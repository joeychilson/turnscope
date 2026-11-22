//! Reading what's new in the agents' history into the database, with each
//! response's cost and account. How history is kept: docs/architecture.md#data.

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::agents::{self, Agent, Read, Response};
use crate::prices::{History, Prices, in_force};
use crate::{Error, Result};

/// Raised when a reader would read a file it read before differently. A
/// file last read with an older one is read again from the start when it's
/// next found, moved too: read again only at its path, a thread Codex
/// archives kept what the older reader made of it. A file never found
/// again, as one its agent deleted, keeps what it made.
const READERS: i64 = 8;

/// How long a transaction writes history before it lets the write lock go,
/// and for how long: another process's write waits 10 s (`db::open`), and
/// SQLite retries it at most every 100 ms. On a made-up 616 MB history with
/// a setting saved every half second (November 2026): one transaction held
/// the lock 6 to 23 s and the write failed in 4 runs of 8; one a file let it
/// in after up to 4.3 s; a second, then 110 ms, had none wait over 1.1 s.
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

macro_rules! response_columns {
    () => {
        "session, copy, at, provider, model, input, cache_read,
    cache_write_5m, cache_write_1h, output, reasoning, prompt, web_searches, priority, recorded, id"
    };
}

const SELECT_RESPONSE: &str = concat!(
    "SELECT ",
    response_columns!(),
    " FROM response WHERE agent = ?1 AND id = ?2"
);
const INSERT_RESPONSE_NEW: &str = concat!(
    "INSERT INTO response (agent, folder, file, cost, account, ",
    response_columns!(),
    ") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
     ON CONFLICT (agent, id) DO NOTHING"
);
const REPLACE_RESPONSE: &str = concat!(
    "INSERT OR REPLACE INTO response (agent, folder, file, cost, account, ",
    response_columns!(),
    ") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)"
);
const SELECT_REPRICED: &str = concat!(
    "SELECT ",
    response_columns!(),
    ", agent FROM response
     WHERE cost IS NULL OR EXISTS (
         SELECT 1 FROM repriced
         WHERE repriced.provider = response.provider
           AND repriced.model = response.model
     )"
);

struct Due {
    agent: &'static dyn Agent,
    folder: Arc<str>,
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

/// A file as its last read left it (cursor and agent are fetched by `id`
/// only for changed or moved files).
struct Known {
    id: i64,
    stat: Stat,
    /// The `READERS` it was last read with.
    readers: i64,
}

/// How a file read before is read now.
enum Resume {
    Unchanged,
    /// From its cursor: it only grew.
    On,
    /// It was replaced or cut short.
    FromStart,
}

impl Known {
    /// How the file, read before as this and now `stat`, is read. One an
    /// older reader read is added to `older`, to take out what it made.
    ///
    /// Its reader is recorded when the read ends, over when what it made is
    /// taken out, so a file taken out and then not read, as when the process
    /// ends between the two, never looks read. The cost: one that can't be
    /// read yet is taken out again at each catch-up, 23 to 25 ms among
    /// 120,000 responses (2026-11-14).
    fn resume(&self, stat: &Stat, older: &mut Vec<i64>) -> Resume {
        if self.readers < READERS {
            older.push(self.id);
            Resume::FromStart
        } else if self.stat == *stat {
            Resume::Unchanged
        } else if self.stat.inode == stat.inode && stat.size >= self.stat.size {
            Resume::On
        } else {
            Resume::FromStart
        }
    }
}

/// Hold `ingest.lock` beside the database: one process reads history at a
/// time, so a first run isn't read twice at once.
pub fn lock(db: &Connection) -> Result<std::fs::File> {
    crate::db::lock(&lock_path(db))
}

fn lock_path(db: &Connection) -> PathBuf {
    Path::new(db.path().unwrap_or_default()).with_file_name("ingest.lock")
}

/// How long a command waits for `ingest.lock` before it says so: the app
/// holds it as often as every 5 s while agents work, 135 to 153 ms over a
/// made-up 5,000 files, a wait no one notices.
const QUIET: Duration = Duration::from_secs(1);

/// What a catch-up is about to wait for, which can take minutes, for the
/// command line to say first: another Turnscope reading history, once it has
/// waited `QUIET`, or this one reading all of it.
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
        .prepare_cached("SELECT path, size, modified, inode, id, readers FROM file")?
        .query_map([], |row| {
            let known = Known {
                id: row.get(4)?,
                stat: Stat {
                    size: row.get(1)?,
                    modified: row.get(2)?,
                    inode: row.get(3)?,
                },
                readers: row.get(5)?,
            };
            Ok((OsString::from(row.get::<_, String>(0)?), known))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut due = Vec::new();
    let mut unmatched = Vec::new();
    let mut renames: Vec<(i64, String)> = Vec::new();
    let mut older = Vec::new();
    let load_cursor = |id: i64| -> Result<String> {
        Ok(db
            .prepare_cached("SELECT cursor FROM file WHERE id = ?1")?
            .query_row([id], |row| row.get(0))?)
    };

    for &agent in agents::ALL {
        for folder in agent.folders(home) {
            let folder_key: Arc<str> = Arc::from(folder.to_string_lossy().as_ref());
            for path in agent.files(&folder) {
                let Some(stat) = Stat::of(&path) else {
                    continue;
                };
                if let Some(was) = known.remove(path.as_os_str()) {
                    let cursor = match was.resume(&stat, &mut older) {
                        Resume::Unchanged => continue,
                        Resume::On => load_cursor(was.id)?,
                        Resume::FromStart => String::new(),
                    };
                    due.push(Due {
                        agent,
                        folder: Arc::clone(&folder_key),
                        path,
                        cursor,
                        stat,
                    });
                } else {
                    unmatched.push((agent, Arc::clone(&folder_key), path, stat));
                }
            }
        }
    }

    // A file at a path that's gone is found again by its inode, so it's read
    // on, not twice: an agent moves a file it archives on the same volume,
    // where it keeps its inode, and APFS numbers new files in order rather
    // than reusing a number. Anything still in `known` is a gone path.
    let mut moved: HashMap<i64, Known> = if !unmatched.is_empty() && !known.is_empty() {
        known
            .into_values()
            .map(|was| (was.stat.inode, was))
            .collect()
    } else {
        HashMap::new()
    };
    for (agent, folder, path, stat) in unmatched {
        let mut cursor = String::new();
        if let Some(was) = moved.remove(&stat.inode) {
            let (was_agent, was_cursor): (String, String) = db
                .prepare_cached("SELECT agent, cursor FROM file WHERE id = ?1")?
                .query_row([was.id], |row| Ok((row.get(0)?, row.get(1)?)))?;
            if was_agent == agent.info().id {
                renames.push((was.id, path.to_string_lossy().into_owned()));
                match was.resume(&stat, &mut older) {
                    Resume::Unchanged => continue,
                    Resume::On => cursor = was_cursor,
                    Resume::FromStart => {}
                }
            }
        }
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
            let mut rename = tx.prepare_cached("UPDATE file SET path = ?1 WHERE id = ?2")?;
            for (id, to) in &renames {
                rename.execute(params![to, id])?;
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
    let mut projects = HashMap::new();
    if !due.is_empty() {
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
                    // Each folder's logins, read once a write: Codex writes
                    // limits in every rollout.
                    let mut held = HashMap::new();
                    let began = Instant::now();
                    while let Some((due, read)) = files.next_if(|_| began.elapsed() < HOLD) {
                        match read {
                            Ok(read) => {
                                write(&tx, due, read, home, &derived, &mut projects, &mut held)?;
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
    }
    if wrote {
        // Statistics for the query planner, where tables changed enough.
        db.execute_batch("PRAGMA optimize")?;
    }
    // `history_at` is when the first read finished. Only whether it has is
    // shown, so it's written once: a catch-up after it writes nothing to say
    // it ran. `readers` says, after `READERS` was raised, that all was read
    // again: raised before, a read cut short would go on at the next with
    // no word that it reads all (`waits`).
    if first || readers != READERS {
        let tx = crate::db::write(db)?;
        tx.execute(
            "UPDATE state SET history_at = coalesce(history_at, ?1), readers = ?2",
            params![crate::now(), READERS],
        )?;
        tx.commit()?;
    }
    Ok(wrote)
}

/// A file that can't be read now is tried again next time, from where it
/// was: its row keeps its cursor and what it was, and says why for doctor.
fn failed(tx: &Transaction, due: &Due, error: &Error) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO file (path, agent, size, modified, inode, cursor, failed, readers)
         VALUES (?1, ?2, -1, 0, ?3, '', ?4, ?5)
         ON CONFLICT (path) DO UPDATE SET failed = excluded.failed",
    )?
    .execute(params![
        due.path.to_string_lossy(),
        due.agent.info().id,
        due.stat.inode,
        error.to_string(),
        READERS,
    ])?;
    Ok(())
}

fn write(
    tx: &Transaction,
    due: &Due,
    read: Read,
    home: &Path,
    derived: &Derived,
    projects: &mut HashMap<String, String>,
    held: &mut HashMap<Arc<str>, Vec<(String, String)>>,
) -> Result<()> {
    let agent = due.agent.info().id;
    let folder = due.folder.as_ref();
    let file: i64 = tx
        .prepare_cached(
            "INSERT INTO file (path, agent, size, modified, inode, cursor, skipped, readers)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?9)
             ON CONFLICT (path) DO UPDATE SET size = excluded.size, modified = excluded.modified,
                 inode = excluded.inode, cursor = excluded.cursor, failed = NULL,
                 skipped = iif(?8, excluded.skipped, skipped + excluded.skipped),
                 readers = excluded.readers
             RETURNING id",
        )?
        .query_row(
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
        let project_name = facts.cwd.as_deref().map(|cwd| {
            if !projects.contains_key(cwd) {
                projects.insert(cwd.to_owned(), project(cwd, home));
            }
            projects[cwd].as_str()
        });
        session.execute(params![
            id,
            agent,
            project_name,
            facts.title.as_ref().map(|(_, title)| title),
            facts.title.as_ref().map_or(-1, |(rank, _)| *rank as i64),
            facts.parent,
        ])?;
        holds.execute(params![id, file])?;
    }

    let mut insert_new = tx.prepare_cached(INSERT_RESPONSE_NEW)?;
    let mut known = tx.prepare_cached(SELECT_RESPONSE)?;
    let mut replace = tx.prepare_cached(REPLACE_RESPONSE)?;
    for (_, mut response) in read.responses {
        let (cost, account) = derived.of(agent, folder, &response);
        let inserted = execute_response(
            &mut insert_new,
            agent,
            folder,
            file,
            cost,
            account,
            &response,
        )?;
        if inserted == 0 {
            if let Some(stored) = known
                .query_row(params![agent, response.id], stored)
                .optional()?
            {
                response.merge(stored);
            }
            let (cost, account) = derived.of(agent, folder, &response);
            execute_response(&mut replace, agent, folder, file, cost, account, &response)?;
        }
    }
    if read.limits.is_empty() {
        return Ok(());
    }
    // On the account the folder was signed in to when the agent wrote them,
    // and only while it still holds a login to it: a sign-in to another is
    // in `sign_in` up to `limits::DUE` later, and its limits would go to this.
    let held = held.entry(Arc::clone(&due.folder)).or_insert_with(|| {
        crate::limits::held_in(due.agent, Path::new(folder), home).unwrap_or_default()
    });
    let now = crate::now();
    for limits in &read.limits {
        if let Some(account) = derived.account(folder, &limits.provider, limits.at)
            && held.contains(&(limits.provider.clone(), account.to_owned()))
        {
            crate::limits::record_in_band(tx, account, limits, now)?;
        }
    }
    Ok(())
}

fn execute_response(
    stmt: &mut rusqlite::CachedStatement<'_>,
    agent: &str,
    folder: &str,
    file: i64,
    cost: Option<f64>,
    account: Option<&str>,
    response: &Response,
) -> rusqlite::Result<usize> {
    let tokens = &response.tokens;
    stmt.execute(params![
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
    ])
}

/// A response as stored, its columns in `response_columns!`'s order.
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
/// is used: loaded once and taking prices in under `ingest.lock`, an agent's
/// tool call waited out the app's read of all of history. The cost: 3.3 ms
/// for 4,000 prices (November 2026), once a write.
struct Derived {
    prices: Prices,
    /// The accounts each agent folder held, by folder then provider: one row
    /// a sign-in, for each folder, of which there are tens.
    signed: HashMap<String, HashMap<String, History<Option<String>>>>,
}

impl Derived {
    fn load(db: &Connection) -> Result<Derived> {
        let mut signed: HashMap<String, HashMap<String, History<Option<String>>>> = HashMap::new();
        let mut rows = db.prepare_cached(
            "SELECT folder, provider, since, account FROM sign_in ORDER BY since",
        )?;
        let mut rows = rows.query([])?;
        while let Some(row) = rows.next()? {
            signed
                .entry(row.get(0)?)
                .or_default()
                .entry(row.get(1)?)
                .or_default()
                .push((row.get(2)?, row.get(3)?));
        }
        Ok(Derived {
            prices: Prices::load(db)?,
            signed,
        })
    }

    /// The account `folder` held for `provider` at `at`, if any.
    fn account(&self, folder: &str, provider: &str, at: i64) -> Option<&str> {
        self.signed
            .get(folder)?
            .get(provider)
            .and_then(|held| in_force(held, at))
            .and_then(|account| account.as_deref())
    }

    /// The cost of `response`, and the account it drew on.
    fn of(&self, agent: &str, folder: &str, response: &Response) -> (Option<f64>, Option<&str>) {
        let account = self.account(folder, &response.provider, response.at);
        (self.prices.cost(agent, response), account)
    }
}

/// Price again, in `tx`, every response of `models` (by provider and
/// model), priced for the first time in it, and every response with no
/// known cost, which may take a model's maker's price (`prices::Prices::cost`).
/// A model's usage from before its first price takes it: kept, an agent's
/// own estimate left an OpenCode response of 1M input tokens at $0.001,
/// where a fresh read gave $2.00.
pub fn price_again(tx: &Transaction, models: &[(String, String)]) -> Result<()> {
    // One scan, over a query per model, as `response` has no index on
    // `(provider, model)`: 12 ms against 72 ms on 50,000 responses and 20
    // models (2026-11-20).
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
        .prepare(SELECT_REPRICED)?
        .query_map([], |row| Ok((row.get(16)?, stored(row)?)))?
        .collect::<rusqlite::Result<_>>()?;
    tx.execute("DROP TABLE repriced", [])?;
    if rows.is_empty() {
        return Ok(());
    }
    let prices = Prices::load(tx)?;
    let mut update = tx.prepare("UPDATE response SET cost = ?3 WHERE agent = ?1 AND id = ?2")?;
    for (agent, response) in &rows {
        let cost = prices.cost(agent, response);
        if cost != response.cost {
            update.execute(params![agent, response.id, cost])?;
        }
    }
    Ok(())
}

/// Forget what an older reader made of `files`, so each is read from the
/// start. The sessions only such files hold lose their title and parent, as
/// `SESSION` would keep the older reader's, and keep their project: worked
/// out again, a worktree removed since is a project of its own.
fn read_again(tx: &Transaction, files: &[i64]) -> Result<()> {
    // One transaction, not split as reading is: it held the lock at most
    // 1.24 s on 3,000 files and 120,000 responses (2026-11-14).
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
    use crate::db::test::{one, response, rows};

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
            one::<i64>(db, "SELECT coalesce(sum(output), 0) FROM response"),
            one::<i64>(db, "SELECT count(*) FROM session WHERE title = 'Misread'"),
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
        assert_eq!(one::<i64>(&db, "SELECT count(*) FROM session"), 2);
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
            .map(|sql| one::<i64>(db, sql))
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
        let cursor = |db: &Connection| one::<String>(db, "SELECT cursor FROM file");
        let read_to = cursor(&db);

        // Moved, as an agent moves a file it archives: its row moves with
        // it, and it's read on from where it was, not again.
        let moved = second.join("s1.jsonl");
        std::fs::rename(&file, &moved).unwrap();
        assert!(!ingest(&mut db, home.path(), None).unwrap());
        assert_eq!(cursor(&db), read_to);
        let path = one::<String>(&db, "SELECT path FROM file");
        assert_eq!(Path::new(&path), moved);

        // Replaced, and replaced again: read from the start each time, its
        // responses and what it couldn't read counted once.
        for _ in 0..2 {
            let new = second.join("s1.new");
            std::fs::write(&new, turn("a") + "not a line\n" + &turn("b")).unwrap();
            std::fs::rename(&new, &moved).unwrap();
            ingest(&mut db, home.path(), None).unwrap();
            assert_eq!(made(&db), [40, 0]);
            assert_eq!(one::<i64>(&db, "SELECT count(*) FROM response"), 2);
            assert_eq!(one::<i64>(&db, "SELECT skipped FROM file"), 1);
        }

        // Its inode, now listed by Codex, isn't Claude Code's file moved.
        let sessions = home.path().join(".codex/sessions/2026/09/30");
        std::fs::create_dir_all(&sessions).unwrap();
        let codex = sessions.join("rollout-2026-09-30T09-00-00-a.jsonl");
        std::fs::rename(&moved, codex).unwrap();
        ingest(&mut db, home.path(), None).unwrap();
        for agent in ["claude-code", "codex"] {
            let files = format!("SELECT count(*) FROM file WHERE agent = '{agent}'");
            assert_eq!(one::<i64>(&db, &files), 1, "{agent}");
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
        let why: String = one(
            &db,
            "SELECT failed FROM file WHERE path LIKE '%agent-x.jsonl'",
        );
        assert!(why.contains("waits for its description"), "{why}");
        std::fs::write(
            subagents.join("agent-x.meta.json"),
            r#"{"description": "Look"}"#,
        )
        .unwrap();
        ingest(&mut db, home.path(), None).unwrap();
        assert_eq!(
            one::<i64>(&db, "SELECT count(*) FROM file WHERE failed IS NOT NULL"),
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
        assert_eq!(
            rows(&db, "SELECT id, output FROM response ORDER BY id"),
            ["msg_1:req_1|900", "msg_2:req_2|40"]
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
        let project = |db: &Connection| one::<String>(db, "SELECT project FROM session");
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
        assert!(one::<bool>(&db, unread));
        assert!(ingest(&mut db, home.path(), None).unwrap());
        assert!(!one::<bool>(&db, unread));
        // Another connection would see any write. Anything but history
        // written in the agent's folder is nothing new.
        let other = crate::db::open(dir.path()).unwrap();
        let before = one::<i64>(&other, "PRAGMA data_version");
        assert!(!ingest(&mut db, home.path(), None).unwrap());
        std::fs::write(home.path().join(".claude/debug.txt"), "").unwrap();
        assert!(!ingest(&mut db, home.path(), None).unwrap());
        assert_eq!(one::<i64>(&other, "PRAGMA data_version"), before);
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
            one::<i64>(&db, "SELECT count(*) FROM session WHERE id LIKE '%ses_new'"),
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
        assert_eq!(one::<f64>(&db, "SELECT cost FROM response"), 0.001);
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
        assert_eq!(one::<i64>(&db, "SELECT count(*) FROM file"), 0);
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
        let stored = one::<String>(&db, "SELECT path FROM file");
        assert_eq!(Path::new(&stored), first);
    }

    #[test]
    fn price_again_prices_the_models_listed_and_what_had_no_price() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        db.execute_batch(
            "INSERT INTO price VALUES
                 ('anthropic', 'm1', 0, '{\"input\":2,\"output\":10}', NULL),
                 ('anthropic', 'm2', 0, '{\"input\":4,\"output\":20}', NULL),
                 ('anthropic', 'claude-opus-5-5', 0, '{\"input\":5,\"output\":25}', NULL);",
        )
        .unwrap();
        let claude = ("claude-code", "/w", "anthropic", None);
        let antigravity = ("antigravity", "/g", "google", None);
        for (from, id, model, cost) in [
            (claude, "r1", "m1", None),
            (claude, "r2", "m2", None),
            (claude, "r3", "m3", Some(9.0)),
            (antigravity, "r4", "claude-opus-5-5", None),
            (antigravity, "r5", "gemini-unlisted", None),
        ] {
            response(&db, from, id, 0, model, cost);
        }
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
        assert_eq!(
            rows(&db, "SELECT id, cost FROM response ORDER BY id"),
            [
                "r1|2", "r2|4", "r3|9",
                // Claude served by Google, unpriced since before the maker's
                // price applied to it, at Anthropic's list price.
                "r4|5", "r5|NULL",
            ]
        );
    }

    #[test]
    fn limits_codex_writes_are_taken_in_offline() {
        let (dir, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut db = crate::db::open(dir.path()).unwrap();
        let codex = home.path().join(".codex");
        let sessions = codex.join("sessions/2026/10/01");
        std::fs::create_dir_all(&sessions).unwrap();
        let sign_in = |account: &str| {
            let kept = serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": {"access_token": "token", "account_id": account}
            });
            std::fs::write(codex.join("auth.json"), kept.to_string()).unwrap();
        };
        let now = crate::now();
        let older = now - crate::time::HOUR;
        db.execute(
            "INSERT INTO account (id, provider, kind, title, read_at, full_read_at, checked_at, problem)
             VALUES ('openai:acct-1', 'openai', 'subscription', 'ChatGPT', ?1, ?1, ?1, 'expired')",
            [older],
        )
        .unwrap();
        db.execute(
            "INSERT INTO sign_in (agent, folder, provider, since, account)
             VALUES ('codex', ?1, 'openai', ?2, 'openai:acct-1')",
            params![codex.to_string_lossy(), older],
        )
        .unwrap();
        let rollout = sessions.join("rollout-2026-10-01-t1.jsonl");
        let write = |at: i64, used: f64| {
            let resets = (now + 3 * crate::time::HOUR) / 1000;
            let at = jiff::Timestamp::from_millisecond(at).unwrap().to_string();
            let mut lines = String::new();
            if !rollout.exists() {
                let meta = serde_json::json!({"timestamp": at, "type": "session_meta", "payload": {"id": "t1"}});
                lines += &format!("{meta}\n");
            }
            let count = serde_json::json!({
                "timestamp": at,
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "rate_limits": {
                        "limit_id": "codex",
                        "plan_type": "plus",
                        "primary": {"used_percent": used, "window_minutes": 300, "resets_at": resets},
                        "secondary": {"used_percent": 15.0, "window_minutes": 10080, "resets_at": resets + 86400}
                    }
                }
            });
            lines += &format!("{count}\n");
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&rollout)
                .unwrap();
            std::io::Write::write_all(&mut file, lines.as_bytes()).unwrap();
        };
        let left = |db: &Connection| {
            let status = crate::status::status(db, now).unwrap();
            let account = &status.accounts[0];
            let left: Vec<u8> = account
                .limits
                .iter()
                .map(|limit| limit.left_percent)
                .collect();
            (
                matches!(account.state, crate::status::State::Live { .. }),
                left,
            )
        };

        sign_in("acct-1");
        write(now - 60_000, 30.0);
        assert!(ingest(&mut db, home.path(), None).unwrap());
        // Codex's login works where the one read before had expired.
        assert_eq!(left(&db), (true, vec![70, 85]));
        // Signed in to another account since the logins were gathered, the
        // folder's limits are the other's.
        sign_in("acct-2");
        write(now - 30_000, 90.0);
        assert!(ingest(&mut db, home.path(), None).unwrap());
        assert_eq!(left(&db), (true, vec![70, 85]));
    }
}
