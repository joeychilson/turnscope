//! Reading what's new in the agents' history into the database.
//!
//! Each file keeps where its reader stopped, so only what was appended since
//! is read, and an unchanged file is only stat'ed. A response reported by
//! several files is one row, its reports merged by `Response::merge`, priced,
//! and given the account its folder was signed into when it was made.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};

use crate::agents::{self, Agent, Link, Read, Response, Tokens};
use crate::prices::Prices;
use crate::{Error, Result};

/// Raised when a reader would read a file it read before differently: every
/// file still there is then read again.
const READERS: i64 = 5;

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
        let meta = std::fs::metadata(path).ok()?;
        let mut stat = Stat {
            size: meta.size() as i64,
            modified: meta.mtime() * 1_000_000_000 + meta.mtime_nsec(),
            inode: meta.ino() as i64,
        };
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        if let Ok(wal) = std::fs::metadata(wal) {
            stat.modified = stat
                .modified
                .max(wal.mtime() * 1_000_000_000 + wal.mtime_nsec());
        }
        Some(stat)
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
    let locked = std::fs::File::create(&path).and_then(|lock| {
        lock.lock()?;
        Ok(lock)
    });
    locked.map_err(|error| Error::Failed(format!("locking {}: {error}", path.display())))
}

/// Read what's new in every agent's history under `home`.
pub fn ingest(db: &mut Connection, home: &Path) -> Result<()> {
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
    let gone: Vec<PathBuf> = known
        .keys()
        .filter(|path| !listed.contains(path))
        .cloned()
        .collect();
    let mut moved: HashMap<i64, (PathBuf, Known)> = gone
        .into_iter()
        .filter_map(|path| known.remove(&path).map(|was| (was.stat.inode, (path, was))))
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
        return Ok(());
    }

    let derived = Derived::load(db)?;
    // Files are read in parallel a share at a time, so memory stays bounded,
    // while the share before is written.
    std::thread::scope(|scope| -> Result<()> {
        let (send, reads) = std::sync::mpsc::sync_channel(1);
        scope.spawn(|| {
            for share in due.chunks(256) {
                let read: Vec<Result<Read>> = share
                    .par_iter()
                    .map(|due| due.agent.read(&due.path, &due.cursor))
                    .collect();
                if send.send((share, read)).is_err() {
                    return;
                }
            }
            // Moved in, so the channel closes when reading ends.
            drop(send);
        });
        for (share, read) in reads {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            for (due, read) in share.iter().zip(read) {
                match read {
                    Ok(read) => write(&tx, due, read, home, &derived)?,
                    // A file that can't be read now is tried again next
                    // time, from where it was: its row keeps its cursor and
                    // what it was, and says why for doctor.
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
        }
        Ok(())
    })?;
    // Statistics for the query planner, where tables changed enough.
    db.execute_batch("PRAGMA optimize")?;
    Ok(())
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

    let mut said =
        tx.prepare_cached("INSERT INTO said (session, text, file) VALUES (?1, ?2, ?3)")?;
    for (session, text) in &read.said {
        said.execute(params![session, text, file])?;
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
    let (filter, pairs) = match of {
        Of::Models(pairs) if pairs.is_empty() => return Ok(()),
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
    let _lock = lock(db)?;
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
    tx.execute(
        "DELETE FROM response WHERE file IN (SELECT file FROM again)",
        [],
    )?;
    tx.execute(
        "UPDATE file SET cursor = '', size = -1, skipped = 0 WHERE id IN (SELECT file FROM again)",
        [],
    )?;
    tx.execute(
        "DELETE FROM session WHERE NOT EXISTS (SELECT 1 FROM session_file held
             WHERE held.session = session.id AND held.file NOT IN (SELECT file FROM again))",
        [],
    )?;
    tx.execute(
        "DELETE FROM session_file WHERE file IN (SELECT file FROM again)",
        [],
    )?;
    tx.execute("DROP TABLE again", [])?;
    tx.execute("DELETE FROM said", [])?;
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

    #[test]
    fn history_read_by_a_newer_turnscope_is_left_alone() {
        let (dir, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut db = crate::db::open(dir.path()).unwrap();
        db.execute("UPDATE state SET readers = ?1", [READERS + 1])
            .unwrap();
        assert!(ingest(&mut db, home.path()).is_err());
        let readers: i64 = db
            .query_row("SELECT readers FROM state", [], |row| row.get(0))
            .unwrap();
        assert_eq!(readers, READERS + 1);
    }

    /// A temp data folder and home, with Claude Code's projects in the home,
    /// and a user line of `text` as a log line.
    fn store() -> (tempfile::TempDir, tempfile::TempDir, Connection, PathBuf) {
        let (dir, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let db = crate::db::open(dir.path()).unwrap();
        let projects = home.path().join(".claude/projects");
        std::fs::create_dir_all(&projects).unwrap();
        (dir, home, db, projects)
    }

    fn line(text: &str) -> String {
        let line = serde_json::json!({"type": "user", "sessionId": "s1",
            "timestamp": "2026-09-30T09:00:00Z", "message": {"role": "user", "content": text}});
        format!("{line}\n")
    }

    fn count(db: &Connection, sql: &str) -> i64 {
        db.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    #[test]
    fn a_moved_or_replaced_file_is_counted_once() {
        let (_dir, home, mut db, projects) = store();
        let (first, second) = (projects.join("-work-a"), projects.join("-work-b"));
        std::fs::create_dir_all(&first).unwrap();
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
    fn a_file_that_said_nothing_isnt_marked_as_in_search() {
        let (_dir, home, mut db, projects) = store();
        let folder = projects.join("-work-a");
        std::fs::create_dir_all(&folder).unwrap();
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
        let (_dir, home, mut db, projects) = store();
        let folder = projects.join("-work-a");
        std::fs::create_dir_all(&folder).unwrap();
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
        let (_dir, home, mut db, projects) = store();
        let folder = projects.join("-work-a");
        std::fs::create_dir_all(&folder).unwrap();
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
}
