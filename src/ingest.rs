//! Reading what's new in the agents' history into the database.
//!
//! Each file keeps where its reader stopped, so only what was appended since
//! is read, and an unchanged file is only stat'ed. A response reported by
//! several files is one row, its reports merged by `Response::merge`, priced,
//! and given the account its folder was signed into when it was made.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};

use crate::agents::{self, Agent, Link, Read, Response, Tokens};
use crate::prices::Prices;
use crate::{Error, Result};

/// Raised when a reader would read a file it read before differently: every
/// file still there is then read again.
const READERS: i64 = 5;

/// How long a transaction writes history before it lets the write lock go,
/// and for how long: another process's write waits no more than `db::open`'s
/// 10 s, and SQLite tries a waiting write again every 100 ms at most, so a
/// pause longer than that lets it in.
///
/// Measured on a made-up 616 MB history of 256 files, with `config set` run
/// beside it every half second: one transaction for all 256 held the lock 6
/// to 23 s, and a write failed after 10.4 s in 4 runs of 8. A transaction a
/// file let it go only between commits, so a write got in by chance, after
/// up to 4.3 s. A second and then 110 ms: no write failed or waited over
/// 1.1 s in 6 runs, and reading took as long as before.
const HOLD: Duration = Duration::from_secs(1);
const PAUSE: Duration = Duration::from_millis(110);

/// Merges what a file says of a session: the earliest start, the latest
/// activity and branch, the first folder, and a title as good or better.
const SESSION: &str = "
INSERT INTO session (id, agent, folder, cwd, project, branch, title, title_rank, parent, link,
    started, last)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
ON CONFLICT (id) DO UPDATE SET
    cwd = coalesce(cwd, excluded.cwd),
    project = coalesce(project, excluded.project),
    branch = coalesce(excluded.branch, branch),
    title = iif(excluded.title_rank >= title_rank, excluded.title, title),
    title_rank = max(title_rank, excluded.title_rank),
    parent = coalesce(parent, excluded.parent),
    link = coalesce(link, excluded.link),
    started = coalesce(min(started, excluded.started), started, excluded.started),
    last = coalesce(max(last, excluded.last), last, excluded.last)";

const RESPONSE_COLUMNS: &str = "session, copy, at, provider, model, input, cache_read,
    cache_write_5m, cache_write_1h, output, reasoning, prompt, web_searches, priority, recorded, id";

/// A file to read: where its last read stopped, and how it stands now.
struct Due {
    agent: &'static dyn Agent,
    folder: PathBuf,
    path: PathBuf,
    cursor: String,
    stat: Stat,
    /// Read before, and now replaced or cut short, so read again from the
    /// start: what its last read said is taken out first.
    again: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stat {
    size: i64,
    modified: i64,
    inode: i64,
}

impl Stat {
    /// A SQLite database's changes reach its file only at a checkpoint, so
    /// when its write-ahead log changed is when it did; the log's size isn't
    /// counted, as a checkpoint shrinks it.
    fn of(path: &Path) -> Option<Stat> {
        let modified = |meta: &std::fs::Metadata| meta.mtime() * 1_000_000_000 + meta.mtime_nsec();
        let meta = std::fs::metadata(path).ok()?;
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        let wal = std::fs::metadata(wal).map_or(i64::MIN, |wal| modified(&wal));
        Some(Stat {
            size: meta.size() as i64,
            modified: modified(&meta).max(wal),
            inode: meta.ino() as i64,
        })
    }
}

/// A file as its last read left it.
struct Known {
    agent: String,
    stat: Stat,
    cursor: String,
}

/// Hold `ingest.lock` beside the database: one process writes history, or
/// works out costs and accounts, at a time, so a first run isn't read twice
/// at once and no response is written with prices or sign-ins gone stale.
fn lock(db: &Connection) -> Result<std::fs::File> {
    let path = Path::new(db.path().unwrap_or_default()).with_file_name("ingest.lock");
    std::fs::File::create(&path)
        .and_then(|lock| lock.lock().map(|()| lock))
        .map_err(|error| Error::Failed(format!("locking {}: {error}", path.display())))
}

/// Read what's new in every agent's history under `home`; whether any file
/// was read.
pub fn ingest(db: &mut Connection, home: &Path) -> Result<bool> {
    let _lock = lock(db)?;

    let readers: i64 = db.query_row("SELECT readers FROM state", [], |row| row.get(0))?;
    // An older Turnscope still running, as an agent's MCP server can be,
    // would read it all again its way, and this one back again.
    if readers > READERS {
        return Err(Error::Failed(
            "a newer Turnscope has updated this data; update Turnscope".to_owned(),
        ));
    }
    if readers < READERS {
        read_again(db)?;
    }

    let mut known: HashMap<PathBuf, Known> = db
        .prepare("SELECT path, agent, size, modified, inode, cursor FROM file")?
        .query_map([], |row| {
            let known = Known {
                agent: row.get(1)?,
                stat: Stat {
                    size: row.get(2)?,
                    modified: row.get(3)?,
                    inode: row.get(4)?,
                },
                cursor: row.get(5)?,
            };
            Ok((PathBuf::from(row.get::<_, String>(0)?), known))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut found = Vec::new();
    for &agent in agents::ALL {
        for folder in agent.folders(home) {
            for path in agent.files(&folder) {
                if let Some(stat) = Stat::of(&path) {
                    found.push((agent, folder.clone(), path, stat));
                }
            }
        }
    }
    // A file read before at a path that's gone, by its inode: an agent that
    // moves a file, as one it archives, moves it on the same disk, where it
    // keeps its inode, and on one volume APFS numbers new files in order
    // rather than reusing a number. Read on from where it was, it's not read
    // twice.
    let listed: HashSet<&PathBuf> = found.iter().map(|(.., path, _)| path).collect();
    let mut moved: HashMap<i64, (PathBuf, Known)> = known
        .extract_if(|path, _| !listed.contains(path))
        .map(|(path, was)| (was.stat.inode, (path, was)))
        .collect();

    let mut due = Vec::new();
    for (agent, folder, path, stat) in found {
        let was = match known.remove(&path) {
            Some(was) => Some(was),
            None => match moved.remove(&stat.inode) {
                Some((from, was)) if was.agent == agent.info().id => {
                    db.execute(
                        "UPDATE file SET path = ?1 WHERE path = ?2",
                        params![path.to_string_lossy(), from.to_string_lossy()],
                    )?;
                    Some(was)
                }
                _ => None,
            },
        };
        let (cursor, again) = match was {
            Some(was) if was.stat == stat => continue,
            Some(was) if was.stat.inode == stat.inode && stat.size >= was.stat.size => {
                (was.cursor, false)
            }
            // A file replaced or cut short is read from the start.
            Some(_) => (String::new(), true),
            None => (String::new(), false),
        };
        due.push(Due {
            agent,
            folder,
            path,
            cursor,
            stat,
            again,
        });
    }
    db.execute("UPDATE state SET history_at = ?1", [crate::now()])?;
    if due.is_empty() {
        return Ok(false);
    }

    let derived = Derived::load(db)?;
    let mut wrote = false;
    // Files are read in parallel a share at a time, so memory stays bounded,
    // while the share before is written.
    std::thread::scope(|scope| -> Result<()> {
        let (send, reads) = std::sync::mpsc::sync_channel(1);
        let shares = due.chunks(256);
        // Moved in, so the channel closes when reading ends.
        scope.spawn(move || {
            for share in shares {
                let read: Vec<Result<Read>> = share
                    .par_iter()
                    .map(|due| due.agent.read(&due.path, &due.cursor))
                    .collect();
                if send.send((share, read)).is_err() {
                    return;
                }
            }
        });
        for (share, read) in reads {
            let mut files = share.iter().zip(read).peekable();
            while files.peek().is_some() {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let began = Instant::now();
                while let Some((due, read)) = files.next_if(|_| began.elapsed() < HOLD) {
                    match read {
                        Ok(read) => {
                            write(&tx, due, read, home, &derived)?;
                            wrote = true;
                        }
                        // A file that can't be read now is tried again next
                        // time, from where it was: its row keeps its cursor
                        // and what it was, and says why for doctor.
                        Err(error) => {
                            tx.execute(
                                "INSERT INTO file (path, agent, folder, size, modified, inode, cursor, failed)
                                 VALUES (?1, ?2, ?3, -1, 0, ?4, '', ?5)
                                 ON CONFLICT (path) DO UPDATE SET failed = excluded.failed",
                                params![
                                    due.path.to_string_lossy(),
                                    due.agent.info().id,
                                    due.folder.to_string_lossy(),
                                    due.stat.inode,
                                    error.to_string(),
                                ],
                            )?;
                        }
                    }
                }
                tx.commit()?;
                if files.peek().is_some() {
                    std::thread::sleep(PAUSE);
                }
            }
        }
        Ok(())
    })?;
    // Statistics for the query planner, where tables changed enough.
    db.execute_batch("PRAGMA optimize")?;
    Ok(wrote)
}

fn write(tx: &Transaction, due: &Due, read: Read, home: &Path, derived: &Derived) -> Result<()> {
    let agent = due.agent.info().id;
    let folder = due.folder.to_string_lossy();
    let (file, indexed): (i64, bool) = tx.query_row(
        "INSERT INTO file (path, agent, folder, size, modified, inode, cursor, skipped)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (path) DO UPDATE SET size = excluded.size, modified = excluded.modified,
             inode = excluded.inode, cursor = excluded.cursor, failed = NULL,
             skipped = iif(?9, excluded.skipped, skipped + excluded.skipped)
         RETURNING id, indexed",
        params![
            due.path.to_string_lossy(),
            agent,
            folder,
            due.stat.size,
            due.stat.modified,
            due.stat.inode,
            read.cursor,
            read.skipped as i64,
            due.cursor.is_empty(),
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    // Only for such a file, and only one whose reads were indexed, since
    // taking rows out by file scans the table.
    if due.again && indexed {
        tx.execute("DELETE FROM said WHERE file = ?1", [file])?;
    }

    let mut session = tx.prepare_cached(SESSION)?;
    let mut holds =
        tx.prepare_cached("INSERT OR IGNORE INTO session_file (session, file) VALUES (?1, ?2)")?;
    for (id, facts) in &read.sessions {
        let link = facts.parent.as_ref().map(|parent| match parent.link {
            Link::Subagent => "subagent",
            Link::Fork => "fork",
            Link::Continuation => "continuation",
        });
        session.execute(params![
            id,
            agent,
            folder,
            facts.cwd,
            facts.cwd.as_deref().map(|cwd| project(cwd, home)),
            // A detached head is on no branch.
            facts.branch.as_deref().filter(|branch| *branch != "HEAD"),
            facts.title.as_ref().map(|(_, title)| title),
            facts.title.as_ref().map_or(-1, |(rank, _)| *rank as i64),
            facts.parent.as_ref().map(|parent| &parent.id),
            link,
            facts.started,
            facts.last,
        ])?;
        holds.execute(params![id, file])?;
    }

    let mut known = tx.prepare_cached(&format!(
        "SELECT {RESPONSE_COLUMNS} FROM response WHERE agent = ?1 AND id = ?2"
    ))?;
    let mut store = tx.prepare_cached(&format!(
        "INSERT OR REPLACE INTO response (agent, folder, file, cost, account, {RESPONSE_COLUMNS})
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)"
    ))?;
    for (_, mut response) in read.responses {
        if let Some(stored) = known
            .query_row(params![agent, response.id], stored)
            .optional()?
        {
            response.merge(stored);
        }
        let (cost, account) = derived.of(agent, &folder, &response);
        let tokens = &response.tokens;
        store.execute(params![
            agent,
            folder,
            file,
            cost,
            account,
            response.session,
            response.copy,
            response.at,
            response.provider,
            response.model,
            tokens.input as i64,
            tokens.cache_read as i64,
            tokens.cache_write_5m as i64,
            tokens.cache_write_1h as i64,
            tokens.output as i64,
            tokens.reasoning as i64,
            response.prompt as i64,
            response.web_searches as i64,
            response.priority,
            response.cost,
            response.id,
        ])?;
    }

    // A rewind takes out what its session said from where it went back to,
    // before this file's own rows, which are past that, go in. Files are
    // written in the order their agent lists them, so what it rewinds is in
    // already. This scans the table: 0.2 to 0.35 s on a made-up index of
    // 250,000 rows, once for each rewind.
    for (session, from) in &read.rewound {
        tx.execute(
            "DELETE FROM said WHERE session = ?1 AND ordinal >= ?2",
            params![session, from],
        )?;
    }
    let mut said = tx.prepare_cached(
        "INSERT INTO said (session, text, file, ordinal) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (session, text, ordinal) in &read.said {
        said.execute(params![session, text, file, ordinal])?;
    }
    let now = !read.said.is_empty() || (indexed && !due.again);
    if now != indexed {
        tx.execute(
            "UPDATE file SET indexed = ?2 WHERE id = ?1",
            params![file, now],
        )?;
    }
    Ok(())
}

/// A response as stored, its columns in `RESPONSE_COLUMNS`' order.
fn stored(row: &Row) -> rusqlite::Result<Response> {
    let count = |at: usize| row.get::<_, i64>(at).map(|count| count as u64);
    Ok(Response {
        session: row.get(0)?,
        copy: row.get(1)?,
        at: row.get(2)?,
        provider: row.get(3)?,
        model: row.get(4)?,
        tokens: Tokens {
            input: count(5)?,
            cache_read: count(6)?,
            cache_write_5m: count(7)?,
            cache_write_1h: count(8)?,
            output: count(9)?,
            reasoning: count(10)?,
        },
        prompt: count(11)?,
        web_searches: count(12)?,
        priority: row.get(13)?,
        cost: row.get(14)?,
        id: row.get(15)?,
    })
}

/// What a response's cost and account are worked out from: the prices, and
/// what each agent folder held for each provider from when, oldest first.
struct Derived {
    prices: Prices,
    signed: Vec<(String, String, i64, Option<String>)>,
}

impl Derived {
    fn load(db: &Connection) -> Result<Derived> {
        let signed = db
            .prepare("SELECT folder, provider, since, account FROM sign_in ORDER BY since")?
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Derived {
            prices: Prices::load(db)?,
            signed,
        })
    }

    /// The cost of `agent`'s `response` in `folder`, and the account it drew
    /// on: the one signed in then, or before the first sign-in recorded, the
    /// first.
    fn of(&self, agent: &str, folder: &str, response: &Response) -> (Option<f64>, Option<String>) {
        let mut held = self.signed.iter().filter(|(held_in, provider, _, _)| {
            held_in == folder && *provider == response.provider
        });
        let first = held.clone().next();
        let then = held
            .rfind(|(_, _, since, _)| *since <= response.at)
            .or(first);
        (
            self.prices.cost(agent, response),
            then.and_then(|(_, _, _, account)| account.clone()),
        )
    }
}

/// The responses whose cost and account to work out again.
pub enum Of {
    /// Of models whose price was first read, by provider and model.
    Models(Vec<(String, String)>),
    /// Made in agent folders whose sign-in to a provider changed, by folder
    /// and provider, from when it changed.
    SignIns(Vec<(String, String, i64)>),
}

/// Work out the cost and account of the responses `of` names again. A cost
/// once known stays as it was: the price in force when a response was made
/// never changes.
pub fn derive(db: &mut Connection, of: Of) -> Result<()> {
    let none = match &of {
        Of::Models(pairs) => pairs.is_empty(),
        Of::SignIns(changed) => changed.is_empty(),
    };
    if none {
        return Ok(());
    }
    // Taken before what's used is looked at: a process reading history may
    // be writing a model's first responses, unpriced. The cost: a price
    // refresh waits out another process's read of history whenever
    // models.dev lists a model it didn't before, used here or not. `guard`
    // refreshes no prices, so a hook never waits; a reporting command,
    // `serve` or the MCP server that claims the six-hourly refresh mid-read
    // does.
    let _lock = lock(db)?;
    let (filter, pairs) = match of {
        Of::Models(pairs) => {
            // Of the thousands of models a catalog lists, the few used here.
            let used: std::collections::HashSet<(String, String)> = db
                .prepare("SELECT DISTINCT provider, model FROM response")?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let pairs = pairs
                .into_iter()
                .filter(|pair| used.contains(pair))
                .map(|(provider, model)| (provider, model, i64::MIN));
            ("provider = ?1 AND model = ?2 AND at >= ?3", pairs.collect())
        }
        Of::SignIns(changed) => ("folder = ?1 AND provider = ?2 AND at >= ?3", changed),
    };
    if pairs.is_empty() {
        return Ok(());
    }
    let derived = Derived::load(db)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut select = tx.prepare(&format!(
        "SELECT {RESPONSE_COLUMNS}, agent, folder FROM response WHERE {filter}"
    ))?;
    let mut update =
        tx.prepare("UPDATE response SET cost = ?3, account = ?4 WHERE agent = ?1 AND id = ?2")?;
    for (first, second, from) in &pairs {
        let rows: Vec<(String, String, Response)> = select
            .query_map(params![first, second, from], |row| {
                Ok((row.get(16)?, row.get(17)?, stored(row)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for (agent, folder, response) in &rows {
            let (cost, account) = derived.of(agent, folder, response);
            update.execute(params![agent, response.id, cost, account])?;
        }
    }
    drop((select, update));
    tx.commit()?;
    Ok(())
}

/// Forget what the readers made of every file still there, so each is read
/// again from the start. A session only such files hold is made anew, so
/// what a reader now makes of its folder, project, parent or title counts;
/// one a deleted file held, and the responses only deleted files reported,
/// are kept. The search index is made anew.
fn read_again(db: &mut Connection) -> Result<()> {
    // One transaction, not split as reading is: on the made-up 616 MB
    // history `HOLD` was measured on, it held the lock 0.62 to 0.80 s, far
    // under the 10 s another write waits. Deleting 123,140 responses took
    // 0.48 to 0.64 s of it.
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let files: Vec<(i64, String)> = tx
        .prepare("SELECT id, path FROM file")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    tx.execute("CREATE TEMP TABLE again (file INTEGER PRIMARY KEY)", [])?;
    for (id, path) in files {
        if Path::new(&path).exists() {
            tx.execute("INSERT INTO again (file) VALUES (?1)", [id])?;
        }
    }
    // In one statement each: `response.file` has no index, so a statement a
    // file would scan the table once for each.
    tx.execute_batch(
        "DELETE FROM response WHERE file IN (SELECT file FROM again);
         UPDATE file SET cursor = '', size = -1, skipped = 0 WHERE id IN (SELECT file FROM again);
         DELETE FROM session WHERE NOT EXISTS (SELECT 1 FROM session_file held
             WHERE held.session = session.id AND held.file NOT IN (SELECT file FROM again));
         DELETE FROM session_file WHERE file IN (SELECT file FROM again);
         DROP TABLE again;",
    )?;
    // `delete-all` empties the index at once, where `DELETE` deletes each
    // of that history's 246,280 rows: 37 to 48 ms against 0.82 to 1.7 s.
    // With `DELETE`, the lock was held 1.9 to 4.3 s.
    tx.execute("INSERT INTO said (said) VALUES ('delete-all')", [])?;
    tx.execute("UPDATE file SET indexed = 0", [])?;
    tx.execute("UPDATE state SET readers = ?1", [READERS])?;
    tx.commit()?;
    Ok(())
}

/// The repository `cwd` is in, as its main working tree, so a subdirectory
/// and a worktree belong to it; `cwd` itself when in none, or under `home`
/// only. A worktree kept inside its repository belongs to it by its path,
/// even once deleted, as does one an agent keeps in the repository's own
/// hidden folder (Claude Code's `.claude/worktrees/<name>`), even once the
/// repository's `.git` is gone.
pub fn project(cwd: &str, home: &Path) -> String {
    let kept = Path::new(cwd).ancestors().find(|dir| {
        dir.file_name() == Some("worktrees".as_ref())
            && dir
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|hidden| hidden.to_string_lossy().starts_with('.'))
    });
    if let Some(repository) = kept
        .and_then(|worktrees| worktrees.parent()?.parent())
        .filter(|repository| *repository != home && repository.parent().is_some())
    {
        return project(&repository.to_string_lossy(), home);
    }
    for dir in Path::new(cwd).ancestors() {
        if dir == home || dir.parent().is_none() {
            break;
        }
        let git = dir.join(".git");
        if git.is_dir() {
            return dir.to_string_lossy().into_owned();
        }
        if git.is_file() {
            // A worktree's `.git` points into the main repository's
            // `.git/worktrees/`, whose `commondir` leads back to `.git`.
            let main = std::fs::read_to_string(&git).ok().and_then(|pointer| {
                let target = dir.join(pointer.trim().strip_prefix("gitdir:")?.trim());
                let common = std::fs::read_to_string(target.join("commondir")).ok()?;
                let common = std::fs::canonicalize(target.join(common.trim())).ok()?;
                (common.file_name()? == ".git").then(|| common.parent().map(Path::to_path_buf))?
            });
            return main
                .unwrap_or(dir.to_path_buf())
                .to_string_lossy()
                .into_owned();
        }
    }
    cwd.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp data folder and home, with a folder of Claude Code's projects
    /// in the home.
    fn store() -> (tempfile::TempDir, tempfile::TempDir, Connection, PathBuf) {
        let (dir, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let db = crate::db::open(dir.path()).unwrap();
        let folder = home.path().join(".claude/projects/-work-a");
        std::fs::create_dir_all(&folder).unwrap();
        (dir, home, db, folder)
    }

    /// A user line of `text`, as Claude Code logs it.
    fn line(text: &str) -> String {
        let line = serde_json::json!({"type": "user", "sessionId": "s1",
            "timestamp": "2026-09-30T09:00:00Z", "message": {"role": "user", "content": text}});
        format!("{line}\n")
    }

    fn count(db: &Connection, sql: &str) -> i64 {
        db.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    #[test]
    fn history_read_by_a_newer_turnscope_is_left_alone() {
        let (_dir, home, mut db, _) = store();
        db.execute("UPDATE state SET readers = ?1", [READERS + 1])
            .unwrap();
        assert!(ingest(&mut db, home.path()).is_err());
        assert_eq!(count(&db, "SELECT readers FROM state"), READERS + 1);
    }

    #[test]
    fn a_moved_or_replaced_file_is_counted_once() {
        let (_dir, home, mut db, first) = store();
        let second = first.with_file_name("-work-b");
        std::fs::create_dir_all(&second).unwrap();
        let file = first.join("s1.jsonl");
        std::fs::write(&file, line("Fix the parser.") + "not a line\n").unwrap();
        ingest(&mut db, home.path()).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM said"), 1);

        // Moved, as an agent moves a file it archives: read on from where it
        // was, not again.
        let moved = second.join("s1.jsonl");
        std::fs::rename(&file, &moved).unwrap();
        ingest(&mut db, home.path()).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM said"), 1);
        assert_eq!(count(&db, "SELECT count(*) FROM file"), 1);

        // Replaced: read from the start, what it said and couldn't read
        // counted once.
        let new = second.join("s1.new");
        std::fs::write(
            &new,
            line("Fix the parser.") + "not a line\n" + &line("And the tests."),
        )
        .unwrap();
        std::fs::rename(&new, &moved).unwrap();
        ingest(&mut db, home.path()).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM said"), 2);
        assert_eq!(count(&db, "SELECT skipped FROM file"), 1);
        assert_eq!(count(&db, "SELECT indexed FROM file"), 1);
    }

    #[test]
    fn a_file_that_cant_be_read_is_no_new_history() {
        use std::os::unix::fs::PermissionsExt as _;
        let (_dir, home, mut db, folder) = store();
        std::fs::write(folder.join("s1.jsonl"), line("Fix the parser.")).unwrap();
        let locked = folder.join("s2.jsonl");
        std::fs::write(&locked, line("Fix the parser.")).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(ingest(&mut db, home.path()).unwrap());
        // Tried again, as it is each time, it reads nothing.
        assert!(!ingest(&mut db, home.path()).unwrap());
    }

    #[test]
    fn a_file_that_said_nothing_isnt_marked_as_in_search() {
        let (_dir, home, mut db, folder) = store();
        std::fs::write(folder.join("s1.jsonl"), line("Fix the parser.")).unwrap();
        let quiet = folder.join("s2.jsonl");
        std::fs::write(&quiet, "not a line\n").unwrap();
        ingest(&mut db, home.path()).unwrap();
        let indexed = |db: &Connection, name: &str| {
            count(
                db,
                &format!("SELECT indexed FROM file WHERE path LIKE '%{name}'"),
            )
        };
        assert_eq!((indexed(&db, "s1.jsonl"), indexed(&db, "s2.jsonl")), (1, 0));
        // Replaced, it's read again, with no scan of search for rows it
        // never had.
        let new = folder.join("s2.new");
        std::fs::write(&new, "not a line\nnor this\n").unwrap();
        std::fs::rename(&new, &quiet).unwrap();
        ingest(&mut db, home.path()).unwrap();
        assert_eq!(indexed(&db, "s2.jsonl"), 0);
        assert_eq!(count(&db, "SELECT count(*) FROM said"), 1);
    }

    #[test]
    fn a_gone_files_inode_under_another_agent_isnt_taken_as_moved() {
        let (_dir, home, mut db, folder) = store();
        let file = folder.join("s1.jsonl");
        std::fs::write(&file, line("Fix the parser.")).unwrap();
        ingest(&mut db, home.path()).unwrap();
        // The same inode, now listed by Codex: not Claude Code's file moved.
        let sessions = home.path().join(".codex/sessions/2026/09/30");
        std::fs::create_dir_all(&sessions).unwrap();
        let other = sessions.join("rollout-2026-09-30T09-00-00-a.jsonl");
        std::fs::rename(&file, &other).unwrap();
        ingest(&mut db, home.path()).unwrap();
        assert_eq!(
            count(&db, "SELECT count(*) FROM file WHERE path LIKE '%s1.jsonl'"),
            1
        );
        assert_eq!(
            count(&db, "SELECT count(*) FROM file WHERE agent = 'codex'"),
            1
        );
    }

    #[test]
    fn a_file_that_cant_be_read_says_why_until_it_can() {
        let (_dir, home, mut db, folder) = store();
        std::fs::write(folder.join("s1.jsonl"), line("Fix the parser.")).unwrap();
        // A subagent's log whose description isn't written yet waits.
        let subagents = folder.join("s1/subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(subagents.join("agent-x.jsonl"), line("Look.")).unwrap();
        ingest(&mut db, home.path()).unwrap();
        let why: String = db
            .query_row(
                "SELECT failed FROM file WHERE path LIKE '%agent-x.jsonl'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(why.contains("waits for its description"), "{why}");
        std::fs::write(
            subagents.join("agent-x.meta.json"),
            r#"{"description": "Look"}"#,
        )
        .unwrap();
        ingest(&mut db, home.path()).unwrap();
        assert_eq!(
            count(&db, "SELECT count(*) FROM file WHERE failed IS NOT NULL"),
            0
        );
    }

    #[test]
    fn a_file_that_grew_between_catch_ups_adds_only_what_was_appended() {
        use std::io::Write as _;

        let (_dir, home, mut db, folder) = store();
        let log = folder.join("s1.jsonl");
        let user = |at: &str, text: &str| {
            serde_json::json!({"type": "user", "sessionId": "s1", "timestamp": at, "cwd": "/work/app",
                "message": {"role": "user", "content": text}})
        };
        let reply = |at: &str, n: u8, output: u64, block: serde_json::Value| {
            serde_json::json!({"type": "assistant", "sessionId": "s1", "requestId": format!("req_{n}"), "timestamp": at,
                "message": {"id": format!("msg_{n}"), "model": "claude-opus-5", "role": "assistant", "content": [block],
                    "usage": {"input_tokens": 100, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0,
                        "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 0}, "output_tokens": output}}})
        };
        let text = |text: &str| serde_json::json!({"type": "text", "text": text});
        let lines = [
            user("2026-09-30T09:00:00Z", "Fix the parser."),
            reply(
                "2026-09-30T09:00:05Z",
                1,
                16,
                text("Looking at the parser."),
            ),
            // The same response's next block, its usage grown.
            reply(
                "2026-09-30T09:00:09Z",
                1,
                900,
                serde_json::json!({"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "cargo test"}}),
            ),
            user("2026-09-30T09:01:00Z", "Add a test too."),
            reply("2026-09-30T09:01:30Z", 2, 40, text("Added one.")),
        ];
        let write = |lines: &[serde_json::Value]| {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
                .unwrap();
            for line in lines {
                writeln!(file, "{line}").unwrap();
            }
        };
        write(&lines[..2]);
        ingest(&mut db, home.path()).unwrap();
        write(&lines[2..]);
        ingest(&mut db, home.path()).unwrap();
        let responses: Vec<(String, i64)> = db
            .prepare("SELECT id, output FROM response ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            responses,
            [
                ("msg_1:req_1".to_owned(), 900),
                ("msg_2:req_2".to_owned(), 40)
            ]
        );
        // Each of the four messages indexed once.
        assert_eq!(count(&db, "SELECT count(*) FROM said"), 4);
    }

    #[test]
    fn history_read_again_is_searched_once() {
        let (_dir, home, mut db, folder) = store();
        std::fs::write(folder.join("s1.jsonl"), line("Quote fields with commas.")).unwrap();
        let found = "SELECT count(*) FROM said WHERE said MATCH 'commas'";
        ingest(&mut db, home.path()).unwrap();
        assert_eq!(count(&db, found), 1);
        read_again(&mut db).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM said"), 0);
        ingest(&mut db, home.path()).unwrap();
        assert_eq!(count(&db, found), 1);
    }

    #[test]
    fn a_long_history_read_lets_other_writes_in() {
        let (dir, home, mut db, folder) = store();
        // Seconds of writing, in one share of files.
        let texts: Vec<String> = (0..100)
            .map(|text| {
                let words: Vec<String> = (0..200)
                    .map(|word| format!("w{}", text * 7 + word))
                    .collect();
                words.join(" ")
            })
            .collect();
        for file in 0..200 {
            let mut log = String::new();
            for (turn, said) in texts.iter().enumerate() {
                log += &format!(
                    r#"{{"type":"user","sessionId":"s{file}","timestamp":"2026-09-30T09:00:00Z","cwd":"/work/app","message":{{"role":"user","content":"{said}"}}}}
{{"type":"assistant","sessionId":"s{file}","timestamp":"2026-09-30T09:00:01Z","requestId":"r{turn}","message":{{"id":"m{file}-{turn}","model":"claude-opus-5","role":"assistant","usage":{{"input_tokens":10,"output_tokens":10}},"content":[{{"type":"text","text":"{said}"}}]}}}}
"#
                );
            }
            std::fs::write(folder.join(format!("{file}.jsonl")), log).unwrap();
        }
        let other = crate::db::open(dir.path()).unwrap();
        let mut longest = Duration::ZERO;
        std::thread::scope(|scope| {
            let reading = scope.spawn(|| ingest(&mut db, home.path()));
            while !reading.is_finished() {
                let began = Instant::now();
                other
                    .execute("UPDATE state SET settings = NULL", [])
                    .unwrap();
                longest = longest.max(began.elapsed());
                std::thread::sleep(Duration::from_millis(100));
            }
            reading.join().unwrap().unwrap();
        });
        assert!(longest < 3 * HOLD, "a write waited {longest:?}");
    }

    #[test]
    fn a_model_first_priced_while_history_is_read_prices_what_was_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        db.execute(
            r#"INSERT INTO price VALUES ('anthropic', 'claude-opus-5', 0, '{"input":5,"output":25}', NULL)"#,
            [],
        )
        .unwrap();
        // Another process reads history, and writes the model's first
        // response unpriced.
        let reading = crate::db::open(dir.path()).unwrap();
        let held = lock(&reading).unwrap();
        std::thread::scope(|scope| {
            let model = ("anthropic".to_owned(), "claude-opus-5".to_owned());
            let deriving = scope.spawn(|| derive(&mut db, Of::Models(vec![model])));
            // Time for `derive` to look at what's used, were it to look first.
            std::thread::sleep(Duration::from_millis(100));
            reading
                .execute(
                    "INSERT INTO response (agent, id, session, copy, file, folder, at, provider,
                         model, input, cache_read, cache_write_5m, cache_write_1h, output,
                         reasoning, prompt, web_searches, priority)
                     VALUES ('claude-code', 'm1', 'claude-code:s1', 0, 1, '/work', 0, 'anthropic',
                         'claude-opus-5', 1000000, 0, 0, 0, 0, 0, 1000000, 0, 0)",
                    [],
                )
                .unwrap();
            drop(held);
            deriving.join().unwrap().unwrap();
        });
        let cost: Option<f64> = db
            .query_row("SELECT cost FROM response", [], |row| row.get(0))
            .unwrap();
        assert_eq!(cost, Some(5.0));
    }

    #[test]
    fn a_worktree_is_its_repositorys_project_even_once_gone() {
        let home = tempfile::tempdir().unwrap();
        let repository = home.path().join("work/app");
        std::fs::create_dir_all(repository.join(".git")).unwrap();
        let project = |cwd: &Path| super::project(&cwd.to_string_lossy(), home.path());
        let app = repository.to_string_lossy().into_owned();
        assert_eq!(project(&repository.join("src")), app);
        // Claude Code's, deleted, in a folder whose `.git` is gone too.
        std::fs::remove_dir(repository.join(".git")).unwrap();
        assert_eq!(
            project(&repository.join(".claude/worktrees/agent-1/src")),
            app
        );
        // An agent's own folder under home isn't a repository.
        let own = home.path().join(".grok/worktrees/w1");
        assert_eq!(project(&own), own.to_string_lossy());
    }

    #[test]
    fn a_read_says_whether_any_file_changed() {
        let (_dir, home, mut db, folder) = store();
        let log = folder.join("s1.jsonl");
        std::fs::write(&log, line("Fix the parser.")).unwrap();
        assert!(ingest(&mut db, home.path()).unwrap());
        // Anything but history written in the agent's folder is nothing new.
        std::fs::write(home.path().join(".claude/debug.txt"), "").unwrap();
        assert!(!ingest(&mut db, home.path()).unwrap());
        std::fs::write(&log, line("Fix the parser.").repeat(2)).unwrap();
        assert!(ingest(&mut db, home.path()).unwrap());
    }
}
