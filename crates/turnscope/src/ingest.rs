//! Reading what's new in the agents' history into the database, with each
//! response's cost and account. How history is kept: docs/architecture.md#data.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::agents::{self, Agent, Read, Response};
use crate::prices::{History, Prices, in_force};
use crate::{Error, Result};

/// Raised when a reader would read a file it read before differently. A
/// file last read with an older one is read again from the start when it's
/// next found: where it was, moved, or back after it was gone. Chosen over
/// reading again only the files still at their paths, which kept what the
/// older reader made of a file moved since, as Codex moves a thread it
/// archives. A file never found again, as one its agent deleted, keeps what
/// it made.
const READERS: i64 = 8;

/// How long a transaction writes history before it lets the write lock go,
/// and for how long. A first read of 10,785 real files, 11 GB, took 18 to
/// 30 s, longer than another process's write waits (`db::open`'s 10 s).
/// SQLite retries a waiting write at most every 100 ms, so a longer pause
/// lets it in.
///
/// Measured November 2026 on a made-up 616 MB history of 256 files, with a
/// setting saved every half second: one transaction for all held the lock 6
/// to 23 s, and the write failed in 4 runs of 8; one a file let a write in
/// only by chance between commits, after up to 4.3 s; a second, then 110 ms,
/// had no write fail or wait over 1.1 s in 6 runs, and read no slower.
const HOLD: Duration = Duration::from_secs(1);
const PAUSE: Duration = Duration::from_millis(110);

const SESSION: &str = "
INSERT INTO session (id, agent, project, title, title_rank, parent)
VALUES (?1, ?2, ?3, ?4, ?5, ?6)
ON CONFLICT (id) DO UPDATE SET
    project = coalesce(project, excluded.project),
    title = iif(excluded.title_rank >= title_rank, excluded.title, title),
    title_rank = max(title_rank, excluded.title_rank),
    parent = coalesce(parent, excluded.parent)";

const RESPONSE_COLUMNS: &str = "session, copy, at, provider, model, input, cache_read,
    cache_write_5m, cache_write_1h, output, reasoning, prompt, web_searches, priority, recorded, id";

struct Due {
    agent: &'static dyn Agent,
    folder: PathBuf,
    path: PathBuf,
    cursor: String,
    stat: Stat,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stat {
    size: i64,
    modified: i64,
    inode: i64,
}

impl Stat {
    /// A SQLite database's changes reach its file only at a checkpoint, so
    /// its write-ahead log's time counts as its own; not the log's size,
    /// which a checkpoint shrinks. Only a `.db` file is looked beside: of the
    /// files agents list, only it has a log, and a second stat for every file
    /// cost 67M instructions in a catch-up of 5,700 files that read nothing.
    fn of(path: &Path) -> Option<Stat> {
        let modified = |meta: &std::fs::Metadata| meta.mtime() * 1_000_000_000 + meta.mtime_nsec();
        let meta = std::fs::metadata(path).ok()?;
        let wal = match path.extension().is_some_and(|extension| extension == "db") {
            true => {
                let mut wal = path.as_os_str().to_owned();
                wal.push("-wal");
                std::fs::metadata(wal).map_or(i64::MIN, |wal| modified(&wal))
            }
            false => i64::MIN,
        };
        Some(Stat {
            size: meta.size() as i64,
            modified: modified(&meta).max(wal),
            inode: meta.ino() as i64,
        })
    }
}

/// A file as its last read left it.
struct Known {
    id: i64,
    agent: String,
    stat: Stat,
    cursor: String,
    /// The `READERS` it was last read with.
    readers: i64,
}

/// Hold `ingest.lock` beside the database: one process reads history at a
/// time, so a first run isn't read twice at once.
pub fn lock(db: &Connection) -> Result<std::fs::File> {
    crate::db::lock(&lock_path(db))
}

fn lock_path(db: &Connection) -> PathBuf {
    Path::new(db.path().unwrap_or_default()).with_file_name("ingest.lock")
}

/// How long a command waits for `ingest.lock` before it says so, over
/// saying so at once: the app's engine catches up as often as every 5 s
/// while agents work, holding the lock even when it reads nothing, which over a made-up
/// history of 5,000 files took 135 to 153 ms. Said at once, a command run
/// beside the app would now and then say it waits, for a wait no one notices.
const QUIET: Duration = Duration::from_secs(1);

/// What a catch-up is about to wait for, which can take minutes, in words
/// for the command line to say first: another Turnscope reading history,
/// once it has waited `QUIET`, or this one reading all of it, the first time
/// and after `READERS` was raised. Asked before the catch-up, over from
/// inside `ingest`, so only the command line says it: no one reads what
/// the MCP server writes to standard error. The cost: a command
/// that finds the lock held reads the limits and prices due up to `QUIET`
/// later.
pub fn waits(db: &Connection) -> Option<&'static str> {
    match crate::db::lock_within(&lock_path(db), QUIET) {
        Ok(Some(_)) => {}
        Ok(None) => return Some("Waiting while another Turnscope reads your agents' history…"),
        Err(_) => return None,
    }
    matches!(all_read(db), Ok(false)).then_some("Reading your agents' history…")
}

/// Whether all of history was read with this Turnscope's readers: not
/// during the first read, nor while all of it is read again after
/// `READERS` was raised. Until then, what's in the data is only part of
/// it, as what an older reader made of a file is taken out first (`due`).
pub fn all_read(db: &Connection) -> Result<bool> {
    Ok(db.query_row(
        "SELECT history_at IS NOT NULL AND readers >= ?1 FROM state",
        [READERS],
        |row| row.get(0),
    )?)
}

/// The files under `home` that are new, changed, or last read with an older
/// reader, with where to read each from. A moved file's row moves with it,
/// and what an older reader made of a file is taken out.
fn due(db: &mut Connection, home: &Path) -> Result<Vec<Due>> {
    // By the path's bytes: a `PathBuf` hashes component by component. With
    // 5,700 files, a catch-up that read nothing ran 60M fewer instructions.
    let mut known: HashMap<OsString, Known> = db
        .prepare("SELECT path, agent, size, modified, inode, cursor, id, readers FROM file")?
        .query_map([], |row| {
            let known = Known {
                id: row.get(6)?,
                agent: row.get(1)?,
                stat: Stat {
                    size: row.get(2)?,
                    modified: row.get(3)?,
                    inode: row.get(4)?,
                },
                cursor: row.get(5)?,
                readers: row.get(7)?,
            };
            Ok((OsString::from(row.get::<_, String>(0)?), known))
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
    // A file at a path that's gone is found again by its inode, so it's read
    // on, not twice: an agent moves a file it archives on the same volume,
    // where it keeps its inode, and APFS numbers new files in order rather
    // than reusing a number.
    let listed: HashSet<&OsStr> = found.iter().map(|(.., path, _)| path.as_os_str()).collect();
    let mut moved: HashMap<i64, (OsString, Known)> = known
        .extract_if(|path, _| !listed.contains(path.as_os_str()))
        .map(|(path, was)| (was.stat.inode, (path, was)))
        .collect();

    let mut due = Vec::new();
    let mut renames = Vec::new();
    let mut older = Vec::new();
    for (agent, folder, path, stat) in found {
        let was = match known.remove(path.as_os_str()) {
            Some(was) => Some(was),
            None => match moved.remove(&stat.inode) {
                Some((from, was)) if was.agent == agent.info().id => {
                    renames.push((
                        from.to_string_lossy().into_owned(),
                        path.to_string_lossy().into_owned(),
                    ));
                    Some(was)
                }
                _ => None,
            },
        };
        let cursor = match &was {
            // Its reader is recorded when the read ends, over when what it
            // made is taken out, so a file taken out and then not read, as
            // when the process ends between the two, never looks read. The
            // cost: one that can't be read yet is taken out again at each
            // catch-up, 23 to 25 ms among 120,000 responses (2026-11-14).
            Some(was) if was.readers < READERS => {
                older.push(was.id);
                String::new()
            }
            Some(was) if was.stat == stat => continue,
            Some(was) if was.stat.inode == stat.inode && stat.size >= was.stat.size => {
                was.cursor.clone()
            }
            // A file replaced or cut short is read from the start.
            Some(_) | None => String::new(),
        };
        due.push(Due {
            agent,
            folder,
            path,
            cursor,
            stat,
        });
    }
    if !renames.is_empty() || !older.is_empty() {
        let tx = crate::db::write(db)?;
        if !renames.is_empty() {
            let mut rename = tx.prepare("UPDATE file SET path = ?1 WHERE path = ?2")?;
            for (from, to) in &renames {
                rename.execute(params![to, from])?;
            }
        }
        if !older.is_empty() {
            read_again(&tx, &older)?;
        }
        tx.commit()?;
    }
    Ok(due)
}

/// Read what's new in every agent's history under `home`; whether any file
/// was read. With `patience`, it gives up with an error when another
/// process has read history for longer.
pub fn ingest(db: &mut Connection, home: &Path, patience: Option<Duration>) -> Result<bool> {
    crate::db::writing(db)?;
    let _lock = match patience {
        None => lock(db)?,
        Some(wait) => crate::db::lock_within(&lock_path(db), wait)?.ok_or_else(|| {
            Error::Busy("another Turnscope is reading your agents' history".to_owned())
        })?,
    };
    // Again: a newer one may have written while this waited for the lock.
    crate::db::writing(db)?;

    let (readers, first): (i64, bool) =
        db.query_row("SELECT readers, history_at IS NULL FROM state", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;

    let due = due(db, home)?;
    // The first read ends by saying so, even with nothing to read.
    if due.is_empty() && !first && readers == READERS {
        return Ok(false);
    }

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
                let tx = crate::db::write(db)?;
                let derived = Derived::load(&tx)?;
                let began = Instant::now();
                while let Some((due, read)) = files.next_if(|_| began.elapsed() < HOLD) {
                    match read {
                        Ok(read) => {
                            write(&tx, due, read, home, &derived)?;
                            wrote = true;
                        }
                        Err(error) => failed(&tx, due, &error)?,
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
    // `history_at` is when the first read finished. Only whether it has is
    // shown, so it's written once: a catch-up after it writes nothing to say
    // it ran. `readers` says, after `READERS` was raised, that all was read
    // again: raised before, a read cut short would go on at the next with
    // no word that it reads all (`waits`).
    if first || readers != READERS {
        let tx = crate::db::write(db)?;
        if first {
            tx.execute("UPDATE state SET history_at = ?1", [crate::now()])?;
        }
        if readers != READERS {
            tx.execute("UPDATE state SET readers = ?1", [READERS])?;
        }
        tx.commit()?;
    }
    Ok(wrote)
}

/// A file that can't be read now is tried again next time, from where it
/// was: its row keeps its cursor and what it was, and says why for doctor.
fn failed(tx: &Transaction, due: &Due, error: &Error) -> Result<()> {
    tx.execute(
        "INSERT INTO file (path, agent, size, modified, inode, cursor, failed, readers)
         VALUES (?1, ?2, -1, 0, ?3, '', ?4, ?5)
         ON CONFLICT (path) DO UPDATE SET failed = excluded.failed",
        params![
            due.path.to_string_lossy(),
            due.agent.info().id,
            due.stat.inode,
            error.to_string(),
            READERS,
        ],
    )?;
    Ok(())
}

fn write(tx: &Transaction, due: &Due, read: Read, home: &Path, derived: &Derived) -> Result<()> {
    let agent = due.agent.info().id;
    let folder = due.folder.to_string_lossy();
    let file: i64 = tx.query_row(
        "INSERT INTO file (path, agent, size, modified, inode, cursor, skipped, readers)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?9)
         ON CONFLICT (path) DO UPDATE SET size = excluded.size, modified = excluded.modified,
             inode = excluded.inode, cursor = excluded.cursor, failed = NULL,
             skipped = iif(?8, excluded.skipped, skipped + excluded.skipped),
             readers = excluded.readers
         RETURNING id",
        params![
            due.path.to_string_lossy(),
            agent,
            due.stat.size,
            due.stat.modified,
            due.stat.inode,
            read.cursor,
            read.skipped as i64,
            due.cursor.is_empty(),
            READERS,
        ],
        |row| row.get(0),
    )?;

    let mut session = tx.prepare_cached(SESSION)?;
    let mut holds =
        tx.prepare_cached("INSERT OR IGNORE INTO session_file (session, file) VALUES (?1, ?2)")?;
    for (id, facts) in &read.sessions {
        session.execute(params![
            id,
            agent,
            facts.cwd.as_deref().map(|cwd| project(cwd, home)),
            facts.title.as_ref().map(|(_, title)| title),
            facts.title.as_ref().map_or(-1, |(rank, _)| *rank as i64),
            facts.parent,
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
        tokens: crate::db::tokens(row, 5)?,
        prompt: count(11)?,
        web_searches: count(12)?,
        priority: row.get(13)?,
        cost: row.get(14)?,
        id: row.get(15)?,
    })
}

/// What a response's cost and account are worked out from: the prices, and
/// what each agent folder held for each provider from when, oldest first.
///
/// Loaded in each write, so a price or sign-in another process wrote since
/// the read began is used: `prices::take` and `limits::record` work out
/// again, in their own writes, only what was written before them, and wait
/// for no read of history. Chosen over loading once a read and taking prices
/// in under `ingest.lock`, which made an agent's tool call wait out the
/// app's read of all of history. The cost: 3.3 ms to load 4,000 prices
/// (November 2026), once a write, and a catch-up that reads for under a
/// second writes once.
struct Derived {
    prices: Prices,
    /// The accounts each agent folder held, by folder and provider: one row
    /// a sign-in, for each folder, of which there are tens.
    signed: HashMap<(String, String), History<Option<String>>>,
}

impl Derived {
    fn load(db: &Connection) -> Result<Derived> {
        let mut signed = HashMap::<_, History<_>>::new();
        let mut rows = db.prepare_cached(
            "SELECT folder, provider, since, account FROM sign_in ORDER BY since",
        )?;
        let mut rows = rows.query([])?;
        while let Some(row) = rows.next()? {
            signed
                .entry((row.get(0)?, row.get(1)?))
                .or_default()
                .push((row.get(2)?, row.get(3)?));
        }
        Ok(Derived {
            prices: Prices::load(db)?,
            signed,
        })
    }

    /// The cost of `response`, and the account it drew on.
    fn of(&self, agent: &str, folder: &str, response: &Response) -> (Option<f64>, Option<String>) {
        let account = self
            .signed
            .get(&(folder.to_owned(), response.provider.clone()))
            .and_then(|held| in_force(held, response.at))
            .and_then(|account| account.clone());
        (self.prices.cost(agent, response), account)
    }
}

/// Price again, in `tx`, every response of `models` (by provider and
/// model), priced for the first time in it. No list price was in force
/// before a model's first, so its usage from before takes that first price,
/// as a read of it now would. Chosen over keeping an agent's own estimate,
/// which left an OpenCode response of 1M input tokens at the $0.001 it
/// put on it, where a fresh read gave $2.00.
pub fn price_again(tx: &Transaction, models: &[(String, String)]) -> Result<()> {
    if models.is_empty() {
        return Ok(());
    }
    // One scan of `response`, over a query per model: `response` has no
    // index on `(provider, model)`, so querying per model scanned the whole
    // table for each. On 50,000 responses and 20 newly priced models, this
    // took 12 ms (11–17 ms) against 72 ms (71–74 ms) with a query per model
    // (2026-11-20).
    tx.execute(
        "CREATE TEMP TABLE repriced (
             provider TEXT NOT NULL,
             model TEXT NOT NULL,
             PRIMARY KEY (provider, model)
         )",
        [],
    )?;
    let mut add = tx.prepare("INSERT OR IGNORE INTO repriced (provider, model) VALUES (?1, ?2)")?;
    for (provider, model) in models {
        add.execute(params![provider, model])?;
    }
    drop(add);
    let rows: Vec<(String, Response)> = tx
        .prepare(&format!(
            "SELECT {RESPONSE_COLUMNS}, agent FROM response
             WHERE EXISTS (
                 SELECT 1 FROM repriced
                 WHERE repriced.provider = response.provider
                   AND repriced.model = response.model
             )"
        ))?
        .query_map([], |row| Ok((row.get(16)?, stored(row)?)))?
        .collect::<rusqlite::Result<_>>()?;
    tx.execute("DROP TABLE repriced", [])?;
    if rows.is_empty() {
        return Ok(());
    }
    let prices = Prices::load(tx)?;
    let mut update = tx.prepare("UPDATE response SET cost = ?3 WHERE agent = ?1 AND id = ?2")?;
    for (agent, response) in &rows {
        update.execute(params![agent, response.id, prices.cost(agent, response)])?;
    }
    Ok(())
}

/// Forget what an older reader made of `files`, so each is read from the
/// start. The sessions only such files hold lose their title and parent, as
/// `SESSION` would keep the older reader's, and keep their project.
///
/// The project is kept as it was worked out when the session was first
/// seen, from the folders then: worked out again, a worktree removed since,
/// as one `git worktree add` made beside its repository, is a project of its
/// own, and the repository's usage splits in two. The cost: a project worked
/// out from a working directory an older reader got wrong stays wrong.
fn read_again(tx: &Transaction, files: &[i64]) -> Result<()> {
    // One transaction, not split as reading is: it held the lock 0.62 to
    // 0.80 s on the history `HOLD` was measured on, 123,140 responses, and
    // 0.75 to 1.24 s on 3,000 files and 120,000 responses (2026-11-14), far
    // under the 10 s another write waits.
    tx.execute("CREATE TEMP TABLE again (file INTEGER PRIMARY KEY)", [])?;
    let mut again = tx.prepare("INSERT INTO again (file) VALUES (?1)")?;
    for file in files {
        again.execute([file])?;
    }
    drop(again);
    // In one statement each: `response.file` has no index, so a statement a
    // file would scan the table once for each.
    tx.execute_batch(
        "DELETE FROM response WHERE file IN (SELECT file FROM again);
         UPDATE session SET title = NULL, title_rank = -1, parent = NULL
             WHERE id IN (SELECT session FROM session_file WHERE file IN (SELECT file FROM again))
             AND NOT EXISTS (SELECT 1 FROM session_file held
                 WHERE held.session = session.id AND held.file NOT IN (SELECT file FROM again));
         DELETE FROM session_file WHERE file IN (SELECT file FROM again);
         DROP TABLE again;",
    )?;
    Ok(())
}

/// The repository `cwd` is in, as its main working tree, so a subdirectory
/// and a worktree belong to it; `cwd` itself when in none, or under `home`
/// only. A worktree an agent keeps in the repository's hidden folder, as
/// Claude Code's `.claude/worktrees/<name>`, belongs to it by its path, even
/// once it and the repository's `.git` are gone.
fn project(cwd: &str, home: &Path) -> String {
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

    /// A request, as Claude Code logs it.
    fn line() -> String {
        let line = serde_json::json!({"type": "user", "sessionId": "s1",
            "timestamp": "2026-09-30T09:00:00Z", "message": {"role": "user", "content": "Fix the parser."}});
        format!("{line}\n")
    }

    /// A turn as Claude Code logs it: a request, and a reply `id` with its
    /// usage.
    fn turn(id: &str) -> String {
        let reply = serde_json::json!({"type": "assistant", "sessionId": "s1", "requestId": format!("req_{id}"),
            "timestamp": "2026-09-30T09:00:05Z", "message": {"id": format!("msg_{id}"), "model": "claude-opus-5",
                "role": "assistant", "content": [], "usage": {"input_tokens": 100, "output_tokens": 20}}});
        format!("{}{reply}\n", line())
    }

    fn count(db: &Connection, sql: &str) -> i64 {
        db.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    /// As if the history had been read with the reader before this one,
    /// which made something else of each file: more output, and another
    /// title for its session, of the highest rank.
    fn read_with_an_older_reader(db: &Connection) {
        db.execute_batch(&format!(
            "UPDATE state SET readers = {older};
             UPDATE file SET readers = {older};
             UPDATE response SET output = output + 1000;
             UPDATE session SET title = 'Misread', title_rank = {named};",
            older = READERS - 1,
            named = agents::Title::Named as i64,
        ))
        .unwrap();
    }

    /// The output counted, and how many sessions are as the older reader
    /// made them.
    fn made(db: &Connection) -> [i64; 2] {
        [
            count(db, "SELECT coalesce(sum(output), 0) FROM response"),
            count(db, "SELECT count(*) FROM session WHERE title = 'Misread'"),
        ]
    }

    #[test]
    fn a_deleted_file_keeps_what_it_made_and_is_read_again_once_its_back() {
        let (_dir, home, mut db, folder) = store();
        std::fs::write(folder.join("s1.jsonl"), turn("a")).unwrap();
        let file = folder.join("s2.jsonl");
        // A session of its own, which no other file holds.
        let log = turn("b").replace(r#""s1""#, r#""s2""#);
        std::fs::write(&file, log).unwrap();
        ingest(&mut db, home.path(), None).unwrap();
        assert_eq!(count(&db, "SELECT count(*) FROM session"), 2);
        read_with_an_older_reader(&db);
        // Gone when history is read again: what the older reader made of it
        // stays.
        let away = home.path().join("s2.jsonl");
        std::fs::rename(&file, &away).unwrap();
        ingest(&mut db, home.path(), None).unwrap();
        assert_eq!(made(&db), [20 + 1020, 1]);
        // Back as it was: read again then.
        std::fs::rename(&away, &file).unwrap();
        ingest(&mut db, home.path(), None).unwrap();
        assert_eq!(made(&db), [40, 0]);
    }

    #[test]
    fn history_read_by_an_older_turnscope_comes_to_a_first_read() {
        let (_dir, home, mut db, folder) = store();
        let price = r#"INSERT INTO price VALUES ('anthropic', 'claude-opus-5', 0, '{"input":5,"output":25}', NULL)"#;
        db.execute(price, []).unwrap();
        for (name, id) in [("s1", "a"), ("s2", "b"), ("s3", "c")] {
            let log = turn(id) + &turn(&format!("{id}2"));
            std::fs::write(folder.join(name).with_extension("jsonl"), log).unwrap();
        }
        ingest(&mut db, home.path(), None).unwrap();
        read_with_an_older_reader(&db);
        // One moved, one grown, one as it was.
        let moved = folder.with_file_name("-work-b");
        std::fs::create_dir_all(&moved).unwrap();
        std::fs::rename(folder.join("s1.jsonl"), moved.join("s1.jsonl")).unwrap();
        let grown = folder.join("s2.jsonl");
        let log = std::fs::read_to_string(&grown).unwrap() + &turn("b3");
        std::fs::write(&grown, log).unwrap();
        ingest(&mut db, home.path(), None).unwrap();

        let totals = |db: &Connection| {
            [
                "SELECT count(*) FROM response",
                "SELECT sum(output) FROM response",
                "SELECT CAST(round(sum(cost) * 1e6) AS INTEGER) FROM response",
                "SELECT count(*) FROM session",
                "SELECT count(*) FROM session_file",
                "SELECT count(*) FROM session WHERE project IS NULL",
                "SELECT min(readers) FROM file",
                "SELECT readers FROM state",
            ]
            .map(|sql| count(db, sql))
        };
        let other = tempfile::tempdir().unwrap();
        let mut fresh = crate::db::open(other.path()).unwrap();
        fresh.execute(price, []).unwrap();
        ingest(&mut fresh, home.path(), None).unwrap();
        assert_eq!(totals(&db), totals(&fresh));
        assert_eq!(totals(&db)[0], 7);
    }

    #[test]
    fn a_wait_for_another_turnscope_is_told_only_once_it_lasts() {
        let (dir, home, mut db, _) = store();
        ingest(&mut db, home.path(), None).unwrap();
        // One catching up holds the lock for a moment: nothing to say.
        let held = lock(&db).unwrap();
        std::thread::scope(|scope| {
            let asked = scope.spawn(|| waits(&crate::db::open(dir.path()).unwrap()));
            std::thread::sleep(QUIET / 10);
            drop(held);
            assert_eq!(asked.join().unwrap(), None);
        });
        // One reading all of history holds it on.
        let _held = lock(&db).unwrap();
        assert!(waits(&db).is_some_and(|wait| wait.starts_with("Waiting")));
    }

    #[test]
    fn a_moved_or_replaced_file_is_counted_once() {
        let (_dir, home, mut db, first) = store();
        let second = first.with_file_name("-work-b");
        std::fs::create_dir_all(&second).unwrap();
        let file = first.join("s1.jsonl");
        std::fs::write(&file, turn("a") + "not a line\n").unwrap();
        ingest(&mut db, home.path(), None).unwrap();
        let cursor = |db: &Connection| -> String {
            db.query_row("SELECT cursor FROM file", [], |row| row.get(0))
                .unwrap()
        };
        let read_to = cursor(&db);

        // Moved, as an agent moves a file it archives: its row moves with
        // it, and it's read on from where it was, not again.
        let moved = second.join("s1.jsonl");
        std::fs::rename(&file, &moved).unwrap();
        assert!(!ingest(&mut db, home.path(), None).unwrap());
        assert_eq!(cursor(&db), read_to);
        let path: String = db
            .query_row("SELECT path FROM file", [], |row| row.get(0))
            .unwrap();
        assert_eq!(Path::new(&path), moved);

        // Replaced, and replaced again: read from the start each time, its
        // responses and what it couldn't read counted once.
        for _ in 0..2 {
            let new = second.join("s1.new");
            std::fs::write(&new, turn("a") + "not a line\n" + &turn("b")).unwrap();
            std::fs::rename(&new, &moved).unwrap();
            ingest(&mut db, home.path(), None).unwrap();
            assert_eq!(made(&db), [40, 0]);
            assert_eq!(count(&db, "SELECT count(*) FROM response"), 2);
            assert_eq!(count(&db, "SELECT skipped FROM file"), 1);
        }

        // Its inode, now listed by Codex, isn't Claude Code's file moved.
        let sessions = home.path().join(".codex/sessions/2026/09/30");
        std::fs::create_dir_all(&sessions).unwrap();
        let codex = sessions.join("rollout-2026-09-30T09-00-00-a.jsonl");
        std::fs::rename(&moved, codex).unwrap();
        ingest(&mut db, home.path(), None).unwrap();
        for agent in ["claude-code", "codex"] {
            let files = format!("SELECT count(*) FROM file WHERE agent = '{agent}'");
            assert_eq!(count(&db, &files), 1, "{agent}");
        }
    }

    #[test]
    fn a_file_that_cant_be_read_says_why_until_it_can() {
        let (_dir, home, mut db, folder) = store();
        std::fs::write(folder.join("s1.jsonl"), line()).unwrap();
        // A subagent's log whose description isn't written yet waits.
        let subagents = folder.join("s1/subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        std::fs::write(subagents.join("agent-x.jsonl"), line()).unwrap();
        ingest(&mut db, home.path(), None).unwrap();
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
        ingest(&mut db, home.path(), None).unwrap();
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
        let reply = |at: &str, n: u8, output: u64| {
            serde_json::json!({"type": "assistant", "sessionId": "s1", "requestId": format!("req_{n}"), "timestamp": at,
                "message": {"id": format!("msg_{n}"), "model": "claude-opus-5", "role": "assistant", "content": [],
                    "usage": {"input_tokens": 100, "output_tokens": output}}})
        };
        let lines = [
            reply("2026-09-30T09:00:05Z", 1, 16),
            // The same response's next block, its usage grown.
            reply("2026-09-30T09:00:09Z", 1, 900),
            reply("2026-09-30T09:01:30Z", 2, 40),
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
        write(&lines[..1]);
        ingest(&mut db, home.path(), None).unwrap();
        write(&lines[1..]);
        ingest(&mut db, home.path(), None).unwrap();
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
    }

    #[test]
    fn a_long_history_read_lets_other_writes_in() {
        const TURNS: usize = 1500;
        let (dir, home, mut db, folder) = store();
        // Seconds of writing, in one share of files.
        for file in 0..200 {
            let mut log = String::new();
            for turn in 0..TURNS {
                log += &format!(
                    r#"{{"type":"assistant","sessionId":"s{file}","timestamp":"2026-09-30T09:00:01Z","requestId":"r{turn}","message":{{"id":"m{file}-{turn}","model":"claude-opus-5","role":"assistant","usage":{{"input_tokens":10,"output_tokens":10}},"content":[]}}}}
"#
                );
            }
            std::fs::write(folder.join(format!("{file}.jsonl")), log).unwrap();
        }
        let other = crate::db::open(dir.path()).unwrap();
        let mut longest = Duration::ZERO;
        std::thread::scope(|scope| {
            let reading = scope.spawn(|| ingest(&mut db, home.path(), None));
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

    /// A session in a worktree `git worktree add` made beside its
    /// repository, read again once the worktree is removed.
    #[test]
    fn a_worktrees_session_keeps_its_repository_as_project_when_read_again() {
        let (_dir, home, mut db, folder) = store();
        // As the repository's `.git` resolves, which `project` gives.
        let root = home.path().canonicalize().unwrap();
        let repository = root.join("work/app");
        let worktree = root.join("work/app-fix");
        let pointer = repository.join(".git/worktrees/app-fix");
        std::fs::create_dir_all(&pointer).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        let gitdir = format!("gitdir: {}\n", pointer.display());
        std::fs::write(worktree.join(".git"), gitdir).unwrap();
        std::fs::write(pointer.join("commondir"), "../..\n").unwrap();
        let cwd = format!(r#""cwd":"{}","sessionId""#, worktree.join("src").display());
        let log = turn("a").replace(r#""sessionId""#, &cwd);
        std::fs::write(folder.join("s1.jsonl"), log).unwrap();
        let project = |db: &Connection| -> String {
            db.query_row("SELECT project FROM session", [], |row| row.get(0))
                .unwrap()
        };
        ingest(&mut db, home.path(), None).unwrap();
        assert_eq!(project(&db), repository.to_string_lossy());

        std::fs::remove_dir_all(&worktree).unwrap();
        std::fs::remove_dir_all(&pointer).unwrap();
        read_with_an_older_reader(&db);
        ingest(&mut db, home.path(), None).unwrap();
        assert_eq!(project(&db), repository.to_string_lossy());
        assert_eq!(made(&db), [20, 0]);
    }

    #[test]
    fn a_read_says_whether_any_file_changed_and_writes_nothing_if_none_did() {
        use std::os::unix::fs::PermissionsExt as _;
        let (dir, home, mut db, folder) = store();
        let log = folder.join("s1.jsonl");
        std::fs::write(&log, line()).unwrap();
        let unread = "SELECT history_at IS NULL FROM state";
        assert_eq!(count(&db, unread), 1);
        assert!(ingest(&mut db, home.path(), None).unwrap());
        assert_eq!(count(&db, unread), 0);
        // Another connection would see any write. Anything but history
        // written in the agent's folder is nothing new.
        let other = crate::db::open(dir.path()).unwrap();
        let before = count(&other, "PRAGMA data_version");
        assert!(!ingest(&mut db, home.path(), None).unwrap());
        std::fs::write(home.path().join(".claude/debug.txt"), "").unwrap();
        assert!(!ingest(&mut db, home.path(), None).unwrap());
        assert_eq!(count(&other, "PRAGMA data_version"), before);
        std::fs::write(&log, line().repeat(2)).unwrap();
        assert!(ingest(&mut db, home.path(), None).unwrap());
        // One that can't be read is tried again each time, and reads nothing.
        let locked = folder.join("s2.jsonl");
        std::fs::write(&locked, line()).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(!ingest(&mut db, home.path(), None).unwrap());
        assert!(!ingest(&mut db, home.path(), None).unwrap());
    }

    #[test]
    fn a_change_only_in_a_databases_write_ahead_log_is_read() {
        let (_dir, home, mut db, _) = store();
        let folder = home.path().join(".local/share/opencode");
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("opencode.db");
        // Held open, as OpenCode holds it, with no checkpoint.
        let opencode = Connection::open(&file).unwrap();
        opencode
            .execute_batch(&format!(
                "PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0; {}",
                include_str!("../tests/fixtures/home/.local/share/opencode/opencode.sql")
            ))
            .unwrap();
        assert!(ingest(&mut db, home.path(), None).unwrap());
        let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
        opencode
            .execute_batch(
                r#"INSERT INTO session_v2 VALUES ('ses_new', NULL, NULL, '/work/web', 'Add a footer',
                       1790748000000, 1790748060000);
                   INSERT INTO session_message VALUES ('msg_new', 'ses_new', 'assistant', 1,
                       1790748000000, 1790748060000,
                       '{"model": {"providerID": "opencode-go", "id": "glm-5.3"},
                         "time": {"created": 1790748000000, "completed": 1790748060000},
                         "tokens": {"input": 100, "output": 20, "reasoning": 0, "cache": {"read": 0, "write": 0}},
                         "content": [{"type": "text", "text": "Added."}]}');"#,
            )
            .unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            modified
        );
        assert!(ingest(&mut db, home.path(), None).unwrap());
        assert_eq!(
            count(&db, "SELECT count(*) FROM session WHERE id LIKE '%ses_new'"),
            1
        );
    }

    /// Another process takes in a model's first price, which holds no lock
    /// a read of history does, after the read began and before it writes.
    #[test]
    fn a_price_taken_in_while_history_is_read_prices_what_the_read_writes() {
        let (dir, home, mut db, folder) = store();
        std::fs::write(folder.join("s1.jsonl"), turn("a")).unwrap();
        crate::db::writing(&db).unwrap();
        let mut other = crate::db::open(dir.path()).unwrap();
        let taking = other
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        std::thread::scope(|scope| {
            let reading = scope.spawn(|| ingest(&mut db, home.path(), None));
            // By then the read has begun, and waits to write.
            std::thread::sleep(Duration::from_millis(300));
            let price = r#"INSERT INTO price VALUES ('anthropic', 'claude-opus-5', 0, '{"input":5,"output":25}', NULL)"#;
            taking.execute(price, []).unwrap();
            taking.commit().unwrap();
            reading.join().unwrap().unwrap();
        });
        // 100 input tokens at $5 a million, and 20 output at $25.
        let cost: Option<f64> = db
            .query_row("SELECT cost FROM response", [], |row| row.get(0))
            .unwrap();
        assert_eq!(cost, Some(0.001));
    }

    #[test]
    fn history_is_not_read_once_a_newer_turnscope_wrote_while_this_waited() {
        let (dir, home, db, folder) = store();
        std::fs::write(folder.join("s1.jsonl"), turn("a")).unwrap();
        // Another process reads history.
        let held = lock(&db).unwrap();
        let (data, home_path) = (dir.path().to_owned(), home.path().to_owned());
        let reading = std::thread::spawn(move || {
            let mut db = crate::db::open(&data).unwrap();
            ingest(&mut db, &home_path, None).map_err(|error| error.to_string())
        });
        // By then it has looked, and waits for the lock. Had it not yet, it
        // stops all the same.
        std::thread::sleep(Duration::from_millis(500));
        db.execute("UPDATE state SET version = ?1", [i64::MAX])
            .unwrap();
        drop(held);
        let error = reading.join().unwrap().unwrap_err();
        assert_eq!(error, crate::db::newer().to_string());
        assert_eq!(count(&db, "SELECT count(*) FROM file"), 0);
    }

    #[test]
    fn moved_files_are_not_renamed_once_a_newer_turnscope_has_written() {
        let (_dir, home, mut db, folder) = store();
        let first = folder.join("s1.jsonl");
        std::fs::write(&first, turn("a")).unwrap();
        ingest(&mut db, home.path(), None).unwrap();

        let moved = folder.join("s2.jsonl");
        std::fs::rename(&first, &moved).unwrap();
        db.execute("UPDATE state SET version = ?1", [i64::MAX])
            .unwrap();
        let error = due(&mut db, home.path()).err().unwrap();
        assert_eq!(error.to_string(), crate::db::newer().to_string());
        let stored: String = db
            .query_row("SELECT path FROM file", [], |row| row.get(0))
            .unwrap();
        assert_eq!(Path::new(&stored), first);
    }

    #[test]
    fn price_again_updates_only_matching_models() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        db.execute_batch(
            "INSERT INTO price VALUES
                 ('anthropic', 'm1', 0, '{\"input\":2,\"output\":10}', NULL),
                 ('anthropic', 'm2', 0, '{\"input\":4,\"output\":20}', NULL);
             INSERT INTO response (agent, id, session, copy, file, folder, at, provider, model,
                 input, cache_read, cache_write_5m, cache_write_1h, output, reasoning, prompt,
                 web_searches, priority, cost) VALUES
                 ('claude-code', 'r1', 's', 0, 1, '/w', 0, 'anthropic', 'm1', 1000000, 0, 0, 0, 0, 0, 1000000, 0, 0, NULL),
                 ('claude-code', 'r2', 's', 0, 1, '/w', 0, 'anthropic', 'm2', 1000000, 0, 0, 0, 0, 0, 1000000, 0, 0, NULL),
                 ('claude-code', 'r3', 's', 0, 1, '/w', 0, 'anthropic', 'm3', 1000000, 0, 0, 0, 0, 0, 1000000, 0, 0, 9.0);",
        )
        .unwrap();
        let tx = crate::db::write(&mut db).unwrap();
        price_again(
            &tx,
            &[
                ("anthropic".to_owned(), "m1".to_owned()),
                ("anthropic".to_owned(), "m2".to_owned()),
            ],
        )
        .unwrap();
        tx.commit().unwrap();
        let costs: Vec<(String, Option<f64>)> = db
            .prepare("SELECT id, cost FROM response ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            costs,
            [
                ("r1".to_owned(), Some(2.0)),
                ("r2".to_owned(), Some(4.0)),
                ("r3".to_owned(), Some(9.0)),
            ]
        );
    }
}
