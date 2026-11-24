//! The database: one SQLite file, `turnscope.db` in the data directory.
//! Each module writes its own SQL against this schema. What it holds:
//! docs/architecture.md#data.

use std::fs::File;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::Path;
use std::sync::OnceLock;
use std::time::SystemTime;

use rusqlite::{Connection, Row, Transaction};

use crate::agents::Tokens;
use crate::{Error, Result};

/// The database's file in the data directory.
const FILE: &str = "turnscope.db";

/// The schema, one script per revision. A shipped script is never edited: a
/// change is a new one.
const REVISIONS: &[&str] = &[
    r#"
-- Each history file an agent keeps, as its last read left it.
CREATE TABLE file (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    agent TEXT NOT NULL,
    size INTEGER NOT NULL,
    modified INTEGER NOT NULL,     -- nanoseconds
    inode INTEGER NOT NULL,
    cursor TEXT NOT NULL,          -- where the agent's reader stopped
    skipped INTEGER NOT NULL DEFAULT 0,  -- lines or rows it couldn't read
    failed TEXT,                   -- why it couldn't be read at the last try
    readers INTEGER NOT NULL       -- the ingest::READERS it was read with
) STRICT;

CREATE TABLE session (
    id TEXT PRIMARY KEY,           -- 'claude-code:0f6e3f6a-…'
    agent TEXT NOT NULL,
    project TEXT,                  -- the repository's main working tree, as first seen
    title TEXT,                    -- at most 200 characters
    title_rank INTEGER NOT NULL DEFAULT -1,  -- agents::Title
    parent TEXT                    -- the session it's a subagent of
) STRICT;
-- What used a limit most, and usage by session, walk each session's
-- subagents. Over 20,000 made-up sessions, 30% of them subagents, that took
-- 14 ms with this index, and 21 ms with the one SQLite made for each query
-- without it (measured 2026-11-18).
CREATE INDEX session_parent ON session (parent);

-- The files that hold a session.
CREATE TABLE session_file (
    session TEXT NOT NULL,
    file INTEGER NOT NULL,
    PRIMARY KEY (session, file)
) STRICT, WITHOUT ROWID;

-- One row per response, whichever files report it: see Response::merge.
CREATE TABLE response (
    agent TEXT NOT NULL,
    id TEXT NOT NULL,
    session TEXT NOT NULL,
    copy INTEGER NOT NULL,         -- only copies have reported it
    file INTEGER NOT NULL,         -- the file that last reported it
    folder TEXT NOT NULL,          -- the agent folder it was made in
    at INTEGER NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    input INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cache_write_5m INTEGER NOT NULL,
    cache_write_1h INTEGER NOT NULL,
    output INTEGER NOT NULL,
    reasoning INTEGER NOT NULL,
    prompt INTEGER NOT NULL,
    web_searches INTEGER NOT NULL,
    priority INTEGER NOT NULL,
    recorded REAL,                 -- what the agent says it cost
    cost REAL,                     -- NULL when no price is known
    account TEXT,
    PRIMARY KEY (agent, id)
) STRICT, WITHOUT ROWID;
CREATE INDEX response_at ON response (at);
CREATE INDEX response_session ON response (session);
CREATE INDEX response_account ON response (account, at);

CREATE TABLE account (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('subscription', 'apiKey')),
    title TEXT NOT NULL,
    label TEXT,
    hidden INTEGER NOT NULL DEFAULT 0,
    read_at INTEGER,               -- the last read that worked
    problem TEXT CHECK (problem IN ('signIn', 'expired', 'unavailable'))
) STRICT, WITHOUT ROWID;

-- Limits as providers reported them.
CREATE TABLE reading (
    account TEXT NOT NULL,
    limit_key TEXT NOT NULL,
    at INTEGER NOT NULL,
    name TEXT NOT NULL,
    scope TEXT,                    -- what it covers, when not everything: 'Opus'
    used REAL NOT NULL,            -- percent
    size REAL,                     -- dollars, for a limit of money
    starts INTEGER,
    resets INTEGER,
    PRIMARY KEY (account, at, limit_key)
) STRICT, WITHOUT ROWID;

-- What an agent folder held for a provider, as its responses name it, from
-- when: which account a response drew on.
CREATE TABLE sign_in (
    agent TEXT NOT NULL,
    folder TEXT NOT NULL,
    provider TEXT NOT NULL,
    since INTEGER NOT NULL,
    account TEXT,                  -- NULL: nothing Turnscope reads
    key TEXT,                      -- an API key's fingerprint
    PRIMARY KEY (folder, provider, since)
) STRICT, WITHOUT ROWID;

CREATE TABLE alert (
    id INTEGER PRIMARY KEY,
    account TEXT NOT NULL,
    limit_key TEXT NOT NULL,       -- '' for the account itself
    kind TEXT NOT NULL CHECK (kind IN ('runningOut', 'usedUp', 'reset', 'signIn')),
    window INTEGER NOT NULL,       -- the window's reset, or for signIn the last good read
    at INTEGER NOT NULL,
    acknowledged INTEGER NOT NULL DEFAULT 0,
    detail TEXT NOT NULL,          -- JSON: what the notification says
    UNIQUE (account, limit_key, kind, window)
) STRICT;
CREATE INDEX alert_pending ON alert (acknowledged) WHERE acknowledged = 0;

-- models.dev's prices as they were from `since`: each model's `cost`, and
-- its fast mode's. A row is added when a price changes, never changed.
CREATE TABLE price (
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    since INTEGER NOT NULL,
    cost TEXT NOT NULL,
    fast TEXT,
    PRIMARY KEY (provider, model, since)
) STRICT, WITHOUT ROWID;

CREATE TABLE state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    readers INTEGER NOT NULL DEFAULT 0,  -- ingest::READERS the history was read with
    history_at INTEGER,            -- when the first read of history finished
    limits_at INTEGER,             -- when any process last read limits
    prices_at INTEGER,             -- when models.dev was last asked
    settings TEXT,                 -- alerts::Settings, as JSON
    version INTEGER                -- the newest Turnscope that has written: `writing`
) STRICT;
INSERT INTO state (id) VALUES (1);

-- An account whose last read failed, until a read works (`limits::record`).
CREATE TABLE failing (
    account TEXT PRIMARY KEY,
    reads INTEGER NOT NULL,        -- reads in a row that failed as `account.problem` says; of `unavailable`, those backed off
    why TEXT,                      -- what the last one met, as the log says it
    retry_at INTEGER               -- not read again before then
) STRICT, WITHOUT ROWID;
"#,
    r#"
-- When each account's provider was last asked, and when all its limits were
-- last read: limits an agent writes in its history move `read_at` alone
-- (`limits::record_in_band`). Before, both were when it was last read.
ALTER TABLE account ADD COLUMN checked_at INTEGER;
ALTER TABLE account ADD COLUMN full_read_at INTEGER;
UPDATE account SET full_read_at = read_at, checked_at = read_at;

-- 0.3.1 kept an empty folder as a session's project, and the history read
-- again after updating keeps a project (`ingest::read_again`): with none,
-- it's worked out again.
UPDATE session SET project = NULL WHERE project = '';
"#,
];

/// "TRNS", in the header, so a file with another program's is never written.
const APPLICATION_ID: i32 = 0x5452_4E53;

/// Open the database in `dir`, creating or migrating it.
///
/// One process at a time creates or migrates it, holding `turnscope.lock`
/// beside it: turning on write-ahead logging, which a new database needs,
/// can't wait for another connection as a query does, and fails at once.
pub fn open(dir: &Path) -> Result<Connection> {
    // The owner's alone: it holds what each session cost, and its title.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|error| Error::Failed(format!("creating {}: {error}", dir.display())))?;
    let path = dir.join(FILE);
    let mut db = Connection::open(&path)?;
    db.busy_timeout(std::time::Duration::from_secs(10))?;
    db.pragma_update(None, "synchronous", "NORMAL")?;
    // The write-ahead log is cut back to this once checkpointed: without it,
    // one large write left a 359 MB log for as long as a connection stayed
    // open, as the app's does. Above the 4 MB SQLite checkpoints at.
    db.pragma_update(None, "journal_size_limit", 16 << 20)?;
    let latest = REVISIONS.len() as i64;
    // Read `user_version` before `application_id`: a concurrent creator commits
    // both together, so reading `application_id` first could observe 0 followed
    // by a committed `user_version > 0` and falsely reject Turnscope's own DB.
    let current = revision(&db)?;
    let id: i32 = db.pragma_query_value(None, "application_id", |row| row.get(0))?;
    if (id != 0 || current > 0) && id != APPLICATION_ID {
        return Err(Error::Failed(format!(
            "{} isn't Turnscope's database",
            path.display()
        )));
    }
    if current == latest {
        return Ok(db);
    }
    let _lock = lock(&dir.join("turnscope.lock"))?;
    // Another process may have done it while this one waited.
    let done = revision(&db)?;
    if done == latest {
        return Ok(db);
    }
    db.pragma_update(None, "journal_mode", "WAL")?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Exclusive)?;
    for (revision, script) in (1..).zip(REVISIONS).skip(done as usize) {
        tx.execute_batch(script)?;
        tx.pragma_update(None, "user_version", revision)?;
    }
    tx.pragma_update(None, "application_id", APPLICATION_ID)?;
    tx.commit()?;
    Ok(db)
}

/// The database in `dir` to read, never written: None when it's missing,
/// or at a revision other than this binary's, as for a status line, which
/// runs every few seconds and shouldn't be what creates or migrates it.
pub fn read(dir: &Path) -> Option<Connection> {
    let flags =
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let db = Connection::open_with_flags(dir.join(FILE), flags).ok()?;
    db.busy_timeout(std::time::Duration::from_secs(1)).ok()?;
    let id: i32 = db
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .ok()?;
    (id == APPLICATION_ID && revision(&db).ok()? == REVISIONS.len() as i64).then_some(db)
}

/// The data directory `db` is in: None for one in memory.
pub fn dir(db: &Connection) -> Option<&Path> {
    db.path().and_then(|file| Path::new(file).parent())
}

/// Begin a write: a transaction that holds the write lock from its start,
/// with `writing` checked inside it, as a newer Turnscope may have written
/// while this one waited. Every write of more than one statement begins
/// here: checked by hand, one writer missed it.
pub fn write(db: &mut Connection) -> Result<Transaction<'_>> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    writing(&tx)?;
    Ok(tx)
}

/// A periodic job claimed in the `state` table so no two processes run it at once.
#[derive(Clone, Copy)]
pub enum Claim {
    Prices,
    Limits,
}

impl Claim {
    fn column(self) -> &'static str {
        match self {
            Claim::Prices => "prices_at",
            Claim::Limits => "limits_at",
        }
    }
}

/// Claim a job for this process, in one statement, unless another claimed
/// it less than `gap` ago, so no two do it at once: `state`'s column for
/// `job` is set to `to`. A claim more than five minutes ahead of now is of
/// a clock set back since. Looked at first: claiming waits for the write
/// lock, which a hook shouldn't when nothing is due.
pub fn claim(db: &mut Connection, job: Claim, now: i64, gap: i64, to: i64) -> Result<bool> {
    const AHEAD: i64 = 5 * crate::time::MINUTE;
    let column = job.column();
    let last: Option<i64> =
        db.query_row(&format!("SELECT {column} FROM state"), [], |row| row.get(0))?;
    if last.is_some_and(|last| now - gap < last && last <= now + AHEAD) {
        return Ok(false);
    }
    let tx = write(db)?;
    let claimed = tx.execute(
        &format!(
            "UPDATE state SET {column} = ?1
             WHERE {column} IS NULL OR {column} <= ?2 - ?3 OR {column} > ?2 + ?4"
        ),
        rusqlite::params![to, now, gap, AHEAD],
    )?;
    tx.commit()?;
    Ok(claimed > 0)
}

fn open_lock(path: &Path) -> Result<File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| locking(path, error))
}

fn locking(path: &Path, error: std::io::Error) -> Error {
    Error::Failed(format!("locking {}: {error}", path.display()))
}

pub fn lock(path: &Path) -> Result<File> {
    let file = open_lock(path)?;
    file.lock().map_err(|error| locking(path, error))?;
    Ok(file)
}

pub fn lock_within(path: &Path, wait: std::time::Duration) -> Result<Option<File>> {
    let file = open_lock(path)?;
    let began = std::time::Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) if began.elapsed() < wait => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(error)) => return Err(locking(path, error)),
        }
    }
}

fn revision(db: &Connection) -> Result<i64> {
    let revision: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if revision > REVISIONS.len() as i64 {
        return Err(newer());
    }
    Ok(revision)
}

const VERSION: i64 = number(env!("CARGO_PKG_VERSION_MAJOR")) * 1_000_000
    + number(env!("CARGO_PKG_VERSION_MINOR")) * 1_000
    + number(env!("CARGO_PKG_VERSION_PATCH"));

const fn number(digits: &str) -> i64 {
    match i64::from_str_radix(digits, 10) {
        Ok(number) => number,
        Err(_) => panic!("a version's part isn't a number"),
    }
}

/// Refuses writes once a newer Turnscope version has written here, recording
/// this version otherwise. Chosen over a manual counter (like `ingest::READERS`)
/// because it can't be forgotten; the cost is an older MCP server asking to
/// restart after any update.
pub fn writing(db: &Connection) -> Result<()> {
    revision(db)?;
    let newest = || -> Result<Option<i64>> {
        Ok(db
            .prepare_cached("SELECT version FROM state")?
            .query_row([], |row| row.get(0))?)
    };
    // Read first so hooks don't contend on the write lock once recorded.
    let mut newest_now = newest()?;
    if newest_now < Some(VERSION) {
        let updated = db.execute(
            "UPDATE state SET version = ?1 WHERE version IS NULL OR version < ?1",
            [VERSION],
        )?;
        newest_now = if updated > 0 {
            Some(VERSION)
        } else {
            newest()?
        };
    }
    match newest_now > Some(VERSION) {
        true => Err(newer()),
        false => Ok(()),
    }
}

static STARTED_FROM: OnceLock<Option<(u64, SystemTime)>> = OnceLock::new();

pub fn started() {
    STARTED_FROM.get_or_init(binary);
}

fn binary() -> Option<(u64, SystemTime)> {
    let file = std::fs::metadata(std::env::current_exe().ok()?).ok()?;
    Some((file.ino(), file.modified().ok()?))
}

/// Why this Turnscope writes nothing more.
pub fn newer() -> Error {
    Error::Newer {
        replaced: matches!(
            (STARTED_FROM.get(), binary()),
            (Some(Some(then)), Some(now)) if *then != now
        ),
    }
}

pub fn tokens(row: &Row, from: usize) -> rusqlite::Result<Tokens> {
    let count = |at: usize| {
        row.get::<_, Option<i64>>(from + at)
            .map(|count| count.unwrap_or(0) as u64)
    };
    Ok(Tokens {
        input: count(0)?,
        cache_read: count(1)?,
        cache_write_5m: count(2)?,
        cache_write_1h: count(3)?,
        output: count(4)?,
        reasoning: count(5)?,
    })
}

#[cfg(test)]
pub(crate) mod test {
    use rusqlite::Connection;
    use rusqlite::types::{FromSql, ValueRef};

    /// The one value `sql` gives.
    pub(crate) fn one<T: FromSql>(db: &Connection, sql: &str) -> T {
        db.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    /// Each row `sql` gives, its columns joined by `|`.
    pub(crate) fn rows(db: &Connection, sql: &str) -> Vec<String> {
        db.prepare(sql)
            .unwrap()
            .query_map([], |row| {
                Ok((0..row.as_ref().column_count())
                    .map(|at| match row.get_ref(at).unwrap() {
                        ValueRef::Null => "NULL".to_owned(),
                        ValueRef::Integer(n) => n.to_string(),
                        ValueRef::Real(n) => n.to_string(),
                        ValueRef::Text(text) => String::from_utf8_lossy(text).into_owned(),
                        ValueRef::Blob(_) => "blob".to_owned(),
                    })
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// A response of a million input tokens, its session its id, `from` an
    /// agent's folder, a provider and the account it drew on.
    pub(crate) fn response(
        db: &Connection,
        from: (&str, &str, &str, Option<&str>),
        id: &str,
        at: i64,
        model: &str,
        cost: Option<f64>,
    ) {
        let (agent, folder, provider, account) = from;
        db.execute(
            "INSERT INTO response (agent, folder, provider, account, id, at, model, cost, session,
                 copy, file, input, cache_read, cache_write_5m, cache_write_1h, output, reasoning,
                 prompt, web_searches, priority)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?5, 0, 1, 1000000, 0, 0, 0, 0, 0, 1000000, 0, 0)",
            rusqlite::params![agent, folder, provider, account, id, at, model, cost],
        )
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::test::{one, rows};
    use super::*;

    #[test]
    fn an_older_turnscope_stops_writing_once_a_newer_one_has() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path()).unwrap();
        let newest = || one::<Option<i64>>(&db, "SELECT version FROM state");
        assert_eq!(newest(), None);
        writing(&db).unwrap();
        assert_eq!(newest(), Some(VERSION));
        // An older one is raised over.
        db.execute("UPDATE state SET version = ?1", [VERSION - 1])
            .unwrap();
        writing(&db).unwrap();
        assert_eq!(newest(), Some(VERSION));
        // A newer one stays, and this one may not write.
        db.execute("UPDATE state SET version = ?1", [VERSION + 1])
            .unwrap();
        let error = writing(&db).unwrap_err().to_string();
        assert_eq!(error, newer().to_string());
        assert_eq!(newest(), Some(VERSION + 1));
    }

    #[test]
    fn a_new_database_opened_at_once_from_many_places_opens_in_each() {
        let dir = tempfile::tempdir().unwrap();
        let opened: Vec<bool> = std::thread::scope(|scope| {
            let opening: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| open(dir.path()).is_ok()))
                .collect();
            opening
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect()
        });
        assert!(opened.iter().all(|opened| *opened), "{opened:?}");
        let db = open(dir.path()).unwrap();
        assert_eq!(rows(&db, "PRAGMA journal_mode"), ["wal"]);
    }

    #[test]
    fn the_write_ahead_log_shrinks_after_a_large_write_while_the_data_is_open() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path()).unwrap();
        let _app = open(dir.path()).unwrap();
        db.execute_batch(
            "CREATE TABLE big (data BLOB);
             WITH RECURSIVE n (i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 32)
             INSERT INTO big SELECT zeroblob(1 << 20) FROM n;
             PRAGMA wal_checkpoint;
             DELETE FROM big;",
        )
        .unwrap();
        let wal = std::fs::metadata(dir.path().join("turnscope.db-wal")).unwrap();
        assert!(wal.len() <= 16 << 20, "{} bytes", wal.len());
    }

    #[test]
    fn a_schema_changed_by_a_newer_turnscope_since_it_was_opened_isnt_known() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path()).unwrap();
        assert_eq!(revision(&db).unwrap(), REVISIONS.len() as i64);
        let newer = Connection::open(dir.path().join(FILE)).unwrap();
        newer
            .pragma_update(None, "user_version", REVISIONS.len() as i64 + 1)
            .unwrap();
        let error = revision(&db).unwrap_err().to_string();
        assert_eq!(
            error,
            "a newer Turnscope has updated this data; update Turnscope"
        );
        // One started now doesn't open it.
        assert_eq!(open(dir.path()).unwrap_err().to_string(), error);
    }

    #[test]
    fn a_database_of_the_revision_before_is_brought_up_to_date() {
        let dir = tempfile::tempdir().unwrap();
        let db = Connection::open(dir.path().join(FILE)).unwrap();
        db.pragma_update(None, "application_id", APPLICATION_ID)
            .unwrap();
        db.execute_batch(REVISIONS[0]).unwrap();
        db.pragma_update(None, "user_version", 1).unwrap();
        db.execute(
            "INSERT INTO account (id, provider, kind, title, read_at)
             VALUES ('anthropic:a', 'anthropic', 'subscription', 'Claude Max', 1234)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO session (id, agent, project)
             VALUES ('claude-code:a', 'claude-code', ''), ('claude-code:b', 'claude-code', '/work/web')",
            [],
        )
        .unwrap();
        drop(db);

        let migrated = open(dir.path()).unwrap();
        assert_eq!(revision(&migrated).unwrap(), REVISIONS.len() as i64);
        assert_eq!(
            rows(
                &migrated,
                "SELECT id, read_at, full_read_at, checked_at FROM account"
            ),
            ["anthropic:a|1234|1234|1234"]
        );
        assert_eq!(
            rows(&migrated, "SELECT id, project FROM session ORDER BY id"),
            ["claude-code:a|NULL", "claude-code:b|/work/web"]
        );
    }

    #[test]
    fn another_apps_database_at_the_same_revision_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let db = Connection::open(dir.path().join(FILE)).unwrap();
        db.pragma_update(None, "user_version", REVISIONS.len() as i64)
            .unwrap();
        db.pragma_update(None, "application_id", 0x1234_5678_i32)
            .unwrap();
        drop(db);
        let error = open(dir.path()).unwrap_err().to_string();
        assert!(error.contains("isn't Turnscope's database"), "{error}");
        assert!(read(dir.path()).is_none());
    }
}
