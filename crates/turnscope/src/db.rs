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
const REVISIONS: &[&str] = &[r#"
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
    reads INTEGER NOT NULL,        -- reads in a row that failed as `account.problem` says
    why TEXT,                      -- what the last one met, as the log says it
    retry_at INTEGER               -- not read again before then
) STRICT, WITHOUT ROWID;
"#];

/// "TRNS", in the header, so a file that isn't Turnscope's is never written.
const APPLICATION_ID: i32 = 0x5452_4E53;

/// Open the database in `dir`, creating or migrating it. A new one takes in
/// what Turnscope 0.1 and 0.2 kept (`import`).
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
    let latest = REVISIONS.len() as i64;
    if revision(&db)? == latest {
        return Ok(db);
    }
    let _lock = lock(&dir.join("turnscope.lock"))?;
    // Another process may have done it while this one waited.
    let done = revision(&db)?;
    if done == latest {
        return Ok(db);
    }
    let id: i32 = db.pragma_query_value(None, "application_id", |row| row.get(0))?;
    if done == 0 && id != 0 && id != APPLICATION_ID {
        return Err(Error::Failed(format!(
            "{} isn't Turnscope's database",
            path.display()
        )));
    }
    db.pragma_update(None, "journal_mode", "WAL")?;
    let old = dir.join(OLD);
    // Attached outside the transaction, which SQLite requires.
    let importing = done == 0 && old.exists();
    if importing {
        db.execute("ATTACH DATABASE ?1 AS old", [old.to_string_lossy()])?;
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Exclusive)?;
    for (revision, script) in (1..).zip(REVISIONS).skip(done as usize) {
        tx.execute_batch(script)?;
        tx.pragma_update(None, "user_version", revision)?;
    }
    tx.pragma_update(None, "application_id", APPLICATION_ID)?;
    if importing {
        import(&tx).map_err(|error| {
            Error::Failed(format!("{} can't be taken in: {error}", old.display()))
        })?;
    }
    tx.commit()?;
    if importing {
        db.execute("DETACH DATABASE old", [])?;
        for leftover in ["serve.log", "serve.lock", "serve.sock"] {
            let _ = std::fs::remove_file(dir.join(leftover));
        }
    }
    Ok(db)
}

/// The database in `dir` to read, never written: None when it's missing,
/// or at a revision other than this binary's, as for a status line, which
/// runs every few seconds and shouldn't be what creates the database or
/// takes in the old one.
pub fn read(dir: &Path) -> Option<Connection> {
    let flags =
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let db = Connection::open_with_flags(dir.join(FILE), flags).ok()?;
    db.busy_timeout(std::time::Duration::from_secs(1)).ok()?;
    (revision(&db).ok()? == REVISIONS.len() as i64).then_some(db)
}

/// The database of Turnscope 0.1 and 0.2, at any of its six revisions.
const OLD: &str = "turnscope.sqlite";
const OLD_REVISIONS: i64 = 6;

/// Take in all of the old database, into a new one, by its columns' names:
/// each revision's columns are a subset or superset of the next's. Its rows
/// are left as they were, for a release, in case what's taken in has to be
/// looked at. `serve.lock`, which 0.1 and 0.2 kept beside it, 0.2's
/// `serve.log`, and the `serve.sock` a `serve` that crashed left are
/// removed after: nothing uses them now.
///
/// It runs once, while every process that opens the database waits. A
/// made-up 0.2.1 database of 240 MB, 467,000 responses and 100,000
/// readings, took 6.9 s: 2.3 s of it user and 4.4 s system (November 2026).
///
/// Its schema's revision is raised past 0.2's last, so a Turnscope 0.1 or
/// 0.2 still running, as an agent's MCP server started before the update,
/// writes nothing more there and says to start it again. Left alone, it
/// went on reading history and limits into a file no one else reads.
///
/// Chosen over keeping the old file and its six revisions, two of which
/// dropped tables and columns, so the schema is one script. A copy, not a
/// fresh read of the agents' history: agents delete their own (Claude Code
/// after 30 days), and limit readings, prices, sign-ins, hidden accounts and
/// alerts exist only here.
fn import(tx: &Transaction) -> Result<()> {
    let has = |table: &str, column: &str| -> Result<bool> {
        Ok(tx.query_row(
            "SELECT count(*) FROM pragma_table_info(?1, 'old') WHERE name = ?2",
            [table, column],
            |row| row.get::<_, i64>(0),
        )? > 0)
    };
    let or = |table: &str, column: &str, otherwise: &str| -> Result<String> {
        Ok(match has(table, column)? {
            true => column.to_owned(),
            false => otherwise.to_owned(),
        })
    };
    // Before 0.2.0 a parent was a fork's or a continuation's too, and a
    // session's title was kept whole. `chat_history.jsonl` was read for what
    // was said, and no reader lists it now: a session it alone held would
    // never be read again.
    let parent = match has("session", "link")? {
        true => "iif(link IS 'subagent', parent, NULL)",
        false => "parent",
    };
    let chat = r"path LIKE '%/chat\_history.jsonl' ESCAPE '\'";
    tx.execute_batch(&format!(
        "INSERT INTO file (id, path, agent, size, modified, inode, cursor, skipped, failed, readers)
             SELECT id, path, agent, size, modified, inode, cursor, skipped, {failed}, {readers}
             FROM old.file WHERE NOT {chat};
         INSERT INTO session (id, agent, project, title, title_rank, parent)
             SELECT id, agent, project, substr(title, 1, 200), title_rank, {parent} FROM old.session;
         INSERT INTO session_file (session, file)
             SELECT session, file FROM old.session_file WHERE file IN (SELECT id FROM main.file);
         INSERT INTO response SELECT agent, id, session, copy, file, folder, at, provider, model,
             input, cache_read, cache_write_5m, cache_write_1h, output, reasoning, prompt,
             web_searches, priority, recorded, cost, account FROM old.response;
         INSERT INTO account SELECT id, provider, kind, title, label, hidden, read_at, problem
             FROM old.account;
         INSERT INTO reading SELECT account, limit_key, at, name, scope, used, size, starts, resets
             FROM old.reading;
         INSERT INTO sign_in SELECT agent, folder, provider, since, account, key FROM old.sign_in;
         INSERT INTO alert SELECT id, account, limit_key, kind, window, at, acknowledged, detail
             FROM old.alert;
         INSERT INTO price SELECT provider, model, since, cost, fast FROM old.price;
         UPDATE state SET (readers, history_at, limits_at, prices_at, settings, version) =
             (SELECT readers, history_at, limits_at, prices_at, settings, {version} FROM old.state)
             WHERE EXISTS (SELECT 1 FROM old.state);",
        failed = or("file", "failed", "NULL")?,
        readers = or("file", "readers", "0")?,
        version = or("state", "version", "NULL")?,
    ))?;
    tx.execute_batch(&format!("PRAGMA old.user_version = {}", OLD_REVISIONS + 1))?;
    Ok(())
}

/// Begin a write: a transaction that holds the write lock from its start,
/// with `writing` checked inside it, as a newer Turnscope may have written
/// while this one waited, for the lock or for anything before.
///
/// Every write of more than one statement begins here, over each writer
/// checking by hand, which one missed: costs worked out after waiting out
/// another process's read of history were written over a newer
/// Turnscope's.
pub fn write(db: &mut Connection) -> Result<Transaction<'_>> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    writing(&tx)?;
    Ok(tx)
}

/// Hold the lock file at `path`, made if it's missing, until the file
/// returned is dropped.
pub fn lock(path: &Path) -> Result<File> {
    File::create(path)
        .and_then(|file| file.lock().map(|()| file))
        .map_err(|error| Error::Failed(format!("locking {}: {error}", path.display())))
}

/// `lock`, or None when another process still holds it after `wait`.
pub fn lock_within(path: &Path, wait: std::time::Duration) -> Result<Option<File>> {
    let file = File::create(path)
        .map_err(|error| Error::Failed(format!("locking {}: {error}", path.display())))?;
    let began = std::time::Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) if began.elapsed() < wait => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(Error::Failed(format!(
                    "locking {}: {error}",
                    path.display()
                )));
            }
        }
    }
}

/// The schema's revision in `db`, or the error when it's one a newer
/// Turnscope made: before `db` is opened, or since, as under an agent's
/// MCP server left running through an update.
fn revision(db: &Connection) -> Result<i64> {
    let revision: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if revision > REVISIONS.len() as i64 {
        return Err(newer());
    }
    Ok(revision)
}

/// This Turnscope's version, as `state.version` holds it: major * 1,000,000
/// + minor * 1,000 + patch.
const VERSION: i64 = number(env!("CARGO_PKG_VERSION_MAJOR")) * 1_000_000
    + number(env!("CARGO_PKG_VERSION_MINOR")) * 1_000
    + number(env!("CARGO_PKG_VERSION_PATCH"));

const fn number(digits: &str) -> i64 {
    let digits = digits.as_bytes();
    let (mut number, mut at) = (0, 0);
    while at < digits.len() {
        number = number * 10 + (digits[at] - b'0') as i64;
        at += 1;
    }
    number
}

/// Called first by each function that writes, and again inside each write
/// of more than one statement (`write`): the error when a newer Turnscope has
/// written here, its schema or anything else, and otherwise this one is
/// recorded as the newest that has. An older one still running, as an
/// agent's MCP server started before an update, then only answers.
///
/// By the version, over a number raised by hand when what's written
/// changes, as `ingest::READERS` is: it can't be forgotten. An older one
/// read limits and prices its way among the newer one's, as after a
/// provider changed what it sends. The cost: after each update, such an
/// MCP server says on each answer that it has to be started again, until
/// its agent does, even when nothing it writes changed.
pub fn writing(db: &Connection) -> Result<()> {
    revision(db)?;
    let newest = || -> Result<Option<i64>> {
        Ok(db.query_row("SELECT version FROM state", [], |row| row.get(0))?)
    };
    // Looked at first: raising it waits for the write lock, which a hook
    // shouldn't once this one is recorded.
    let mut newest_now = newest()?;
    if newest_now < Some(VERSION) {
        // Raised, never lowered, so an older one at once can't undo it.
        db.execute(
            "UPDATE state SET version = ?1 WHERE version IS NULL OR version < ?1",
            [VERSION],
        )?;
        newest_now = newest()?;
    }
    match newest_now > Some(VERSION) {
        true => Err(newer()),
        false => Ok(()),
    }
}

/// The file this process runs, as it was when the process started.
static STARTED_FROM: OnceLock<Option<(u64, SystemTime)>> = OnceLock::new();

/// Note the file this process runs, first thing, so `newer` can tell if an
/// update has replaced it since.
pub fn started() {
    STARTED_FROM.get_or_init(binary);
}

/// The file this process runs: its inode and when it was modified.
fn binary() -> Option<(u64, SystemTime)> {
    let file = std::fs::metadata(std::env::current_exe().ok()?).ok()?;
    Some((file.ino(), file.modified().ok()?))
}

/// The error when a newer Turnscope has updated the data: this one would
/// misread it, or undo what that one did. Said so when the file this
/// process runs was replaced since it started, as an update does: it's
/// left over from before, as an agent's MCP server is, and has to be
/// started again, which updating doesn't do. Telling everyone to update
/// told someone who just had.
pub fn newer() -> Error {
    let updated = matches!(
        (STARTED_FROM.get(), binary()),
        (Some(Some(then)), Some(now)) if *then != now
    );
    Error::Newer(
        match updated {
            true => "Turnscope was updated after this one started; start it again",
            false => "a newer Turnscope has updated this data; update Turnscope",
        }
        .to_owned(),
    )
}

/// Tokens from six columns of `row` starting at `from`, in `response`'s
/// order: `input` to `reasoning`. A sum of no rows is NULL, which is 0.
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
mod tests {
    use super::*;

    #[test]
    fn an_older_turnscope_stops_writing_once_a_newer_one_has() {
        let dir = tempfile::tempdir().unwrap();
        let db = open(dir.path()).unwrap();
        let newest = || -> Option<i64> {
            db.query_row("SELECT version FROM state", [], |row| row.get(0))
                .unwrap()
        };
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
        let mode: String = db
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
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

    /// Turnscope 0.1.0's schema, the first of the old database's revisions:
    /// the oldest a database can be taken in from.
    const FIRST: &str = "
        CREATE TABLE file (id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE, agent TEXT NOT NULL,
            folder TEXT NOT NULL, size INTEGER NOT NULL, modified INTEGER NOT NULL,
            inode INTEGER NOT NULL, cursor TEXT NOT NULL, skipped INTEGER NOT NULL DEFAULT 0) STRICT;
        CREATE TABLE session (id TEXT PRIMARY KEY, agent TEXT NOT NULL, folder TEXT NOT NULL,
            cwd TEXT, project TEXT, branch TEXT, title TEXT, title_rank INTEGER NOT NULL DEFAULT -1,
            parent TEXT, link TEXT, started INTEGER, last INTEGER) STRICT;
        CREATE TABLE session_file (session TEXT NOT NULL, file INTEGER NOT NULL,
            PRIMARY KEY (session, file)) STRICT, WITHOUT ROWID;
        CREATE TABLE response (agent TEXT NOT NULL, id TEXT NOT NULL, session TEXT NOT NULL,
            copy INTEGER NOT NULL, file INTEGER NOT NULL, folder TEXT NOT NULL, at INTEGER NOT NULL,
            provider TEXT NOT NULL, model TEXT NOT NULL, input INTEGER NOT NULL,
            cache_read INTEGER NOT NULL, cache_write_5m INTEGER NOT NULL,
            cache_write_1h INTEGER NOT NULL, output INTEGER NOT NULL, reasoning INTEGER NOT NULL,
            prompt INTEGER NOT NULL, web_searches INTEGER NOT NULL, priority INTEGER NOT NULL,
            recorded REAL, cost REAL, account TEXT, PRIMARY KEY (agent, id)) STRICT, WITHOUT ROWID;
        CREATE TABLE account (id TEXT PRIMARY KEY, provider TEXT NOT NULL, kind TEXT NOT NULL,
            title TEXT NOT NULL, label TEXT, hidden INTEGER NOT NULL DEFAULT 0, read_at INTEGER,
            problem TEXT) STRICT, WITHOUT ROWID;
        CREATE TABLE reading (account TEXT NOT NULL, limit_key TEXT NOT NULL, at INTEGER NOT NULL,
            name TEXT NOT NULL, scope TEXT, used REAL NOT NULL, size REAL, starts INTEGER,
            resets INTEGER, PRIMARY KEY (account, at, limit_key)) STRICT, WITHOUT ROWID;
        CREATE TABLE sign_in (agent TEXT NOT NULL, folder TEXT NOT NULL, provider TEXT NOT NULL,
            since INTEGER NOT NULL, account TEXT, key TEXT, PRIMARY KEY (folder, provider, since))
            STRICT, WITHOUT ROWID;
        CREATE TABLE alert (id INTEGER PRIMARY KEY, account TEXT NOT NULL, limit_key TEXT NOT NULL,
            kind TEXT NOT NULL, window INTEGER NOT NULL, at INTEGER NOT NULL,
            acknowledged INTEGER NOT NULL DEFAULT 0, detail TEXT NOT NULL,
            UNIQUE (account, limit_key, kind, window)) STRICT;
        CREATE TABLE price (provider TEXT NOT NULL, model TEXT NOT NULL, since INTEGER NOT NULL,
            cost TEXT NOT NULL, fast TEXT, PRIMARY KEY (provider, model, since)) STRICT, WITHOUT ROWID;
        CREATE TABLE state (id INTEGER PRIMARY KEY CHECK (id = 1), readers INTEGER NOT NULL DEFAULT 0,
            history_at INTEGER, limits_at INTEGER, prices_at INTEGER, settings TEXT) STRICT;
        INSERT INTO state (id) VALUES (1);
        PRAGMA user_version = 1;";

    /// One of each row, as 0.1.0 kept it: a session and a subagent and a
    /// fork of it, a title of 300 characters, and the file that held only
    /// what was said.
    const ROWS: &str = r#"
        INSERT INTO file (id, path, agent, folder, size, modified, inode, cursor) VALUES
            (1, '/home/.grok/sessions/a/s1/updates.jsonl', 'grok-build', '/home/.grok', 10, 1, 1, '9'),
            (2, '/home/.grok/sessions/a/s1/chat_history.jsonl', 'grok-build', '/home/.grok', 1, 1, 2, '');
        INSERT INTO session (id, agent, folder, project, title, title_rank, parent, link) VALUES
            ('grok-build:s1', 'grok-build', '/home/.grok', '/work/app', 'Add a flag.', 0, NULL, NULL),
            ('grok-build:s2', 'grok-build', '/home/.grok', NULL, NULL, -1, 'grok-build:s1', 'subagent'),
            ('grok-build:s3', 'grok-build', '/home/.grok', NULL, NULL, -1, 'grok-build:s1', 'fork');
        INSERT INTO session_file VALUES ('grok-build:s1', 1), ('grok-build:s1', 2);
        INSERT INTO response VALUES ('grok-build', 'r1', 'grok-build:s1', 0, 1, '/home/.grok', 1000,
            'xai', 'grok-5', 100, 0, 0, 0, 20, 0, 100, 0, 0, NULL, 0.5, 'xai:user-1');
        INSERT INTO account VALUES ('xai:user-1', 'xai', 'subscription', 'SuperGrok', NULL, 1, 900, NULL);
        INSERT INTO reading VALUES ('xai:user-1', 'weekly', 900, 'Weekly', NULL, 40, NULL, 0, 5000);
        INSERT INTO sign_in VALUES ('grok-build', '/home/.grok', 'xai', 0, 'xai:user-1', NULL);
        INSERT INTO alert VALUES (7, 'xai:user-1', 'weekly', 'usedUp', 5000, 900, 1, '{}');
        INSERT INTO price VALUES ('xai', 'grok-5', 0, '{"input":3,"output":15}', NULL);
        UPDATE state SET readers = 6, history_at = 800, limits_at = 900, prices_at = 700,
            settings = '{"notify":{"reset":false}}';"#;

    fn rows(db: &Connection, sql: &str) -> Vec<String> {
        db.prepare(sql)
            .unwrap()
            .query_map([], |row| {
                let count = row.as_ref().column_count();
                Ok((0..count)
                    .map(|at| match row.get_ref(at).unwrap() {
                        rusqlite::types::ValueRef::Null => "NULL".to_owned(),
                        rusqlite::types::ValueRef::Integer(n) => n.to_string(),
                        rusqlite::types::ValueRef::Real(n) => n.to_string(),
                        rusqlite::types::ValueRef::Text(text) => {
                            String::from_utf8_lossy(text).into_owned()
                        }
                        rusqlite::types::ValueRef::Blob(_) => "blob".to_owned(),
                    })
                    .collect::<Vec<_>>()
                    .join("|"))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn what_turnscope_0_1_kept_is_taken_in_and_left_where_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let old = Connection::open(dir.path().join(OLD)).unwrap();
        old.execute_batch(FIRST).unwrap();
        old.execute_batch(ROWS).unwrap();
        // A title of 300 characters and 450 bytes: cut by bytes, it would
        // keep fewer than 200 characters.
        let long = "é".repeat(150) + &"a".repeat(150);
        old.execute(
            "INSERT INTO session (id, agent, folder, title) VALUES ('grok-build:s4', 'grok-build', '', ?1)",
            [&long],
        )
        .unwrap();
        drop(old);

        let db = open(dir.path()).unwrap();
        // The file read only for what was said is left out, and with it
        // what it held. 0.1 recorded no reader, so the one kept is read
        // again from the start.
        assert_eq!(
            rows(&db, "SELECT * FROM file"),
            ["1|/home/.grok/sessions/a/s1/updates.jsonl|grok-build|10|1|1|9|0|NULL|0"]
        );
        assert_eq!(rows(&db, "SELECT * FROM session_file"), ["grok-build:s1|1"]);
        // Only a subagent keeps a parent, and a title is cut to 200
        // characters.
        let cut = "é".repeat(150) + &"a".repeat(50);
        assert_eq!(
            rows(
                &db,
                "SELECT id, project, title, parent FROM session ORDER BY id"
            ),
            [
                "grok-build:s1|/work/app|Add a flag.|NULL",
                "grok-build:s2|NULL|NULL|grok-build:s1",
                "grok-build:s3|NULL|NULL|NULL",
                &format!("grok-build:s4|NULL|{cut}|NULL"),
            ]
        );
        for (table, row) in [
            (
                "response",
                "grok-build|r1|grok-build:s1|0|1|/home/.grok|1000|xai|grok-5|100|0|0|0|20|0|100|0|0|NULL|0.5|xai:user-1",
            ),
            (
                "account",
                "xai:user-1|xai|subscription|SuperGrok|NULL|1|900|NULL",
            ),
            (
                "reading",
                "xai:user-1|weekly|900|Weekly|NULL|40|NULL|0|5000",
            ),
            ("sign_in", "grok-build|/home/.grok|xai|0|xai:user-1|NULL"),
            ("alert", "7|xai:user-1|weekly|usedUp|5000|900|1|{}"),
            ("price", r#"xai|grok-5|0|{"input":3,"output":15}|NULL"#),
            (
                "state",
                r#"1|6|800|900|700|{"notify":{"reset":false}}|NULL"#,
            ),
        ] {
            assert_eq!(
                rows(&db, &format!("SELECT * FROM {table}")),
                [row],
                "{table}"
            );
        }
        // The old database's rows are as they were, and its schema is past
        // what 0.1 and 0.2 can write.
        let old = Connection::open(dir.path().join(OLD)).unwrap();
        assert_eq!(rows(&old, "SELECT count(*) FROM file"), ["2"]);
        assert_eq!(rows(&old, "PRAGMA user_version"), ["7"]);
        // Opened again, nothing is taken in twice.
        drop(db);
        let db = open(dir.path()).unwrap();
        assert_eq!(rows(&db, "SELECT count(*) FROM response"), ["1"]);
    }

    /// 0.2.1's last revision: the columns 0.2.0 dropped are gone, and the
    /// ones it and 0.2.1 added are there.
    #[test]
    fn what_turnscope_0_2_kept_is_taken_in_whole() {
        let dir = tempfile::tempdir().unwrap();
        let old = Connection::open(dir.path().join(OLD)).unwrap();
        old.execute_batch(FIRST).unwrap();
        old.execute_batch(
            "ALTER TABLE file ADD COLUMN failed TEXT;
             ALTER TABLE file ADD COLUMN readers INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE file DROP COLUMN folder;
             ALTER TABLE session DROP COLUMN link;
             ALTER TABLE session DROP COLUMN folder;
             ALTER TABLE state ADD COLUMN version INTEGER;
             INSERT INTO file (id, path, agent, size, modified, inode, cursor, failed, readers)
                 VALUES (1, '/home/.claude/projects/-w/s.jsonl', 'claude-code', 5, 2, 3, '4', 'locked', 7);
             INSERT INTO session (id, agent, title, parent) VALUES ('claude-code:s', 'claude-code', 'Fix.', 'claude-code:p');
             UPDATE state SET readers = 7, version = 2001;
             PRAGMA user_version = 6;",
        )
        .unwrap();
        drop(old);
        // What 0.2's `serve` kept beside it.
        let serve = ["serve.log", "serve.lock", "serve.sock"].map(|name| dir.path().join(name));
        for file in &serve {
            std::fs::write(file, "").unwrap();
        }
        let db = open(dir.path()).unwrap();
        assert!(serve.iter().all(|file| !file.exists()));
        assert_eq!(
            rows(&db, "SELECT * FROM file"),
            ["1|/home/.claude/projects/-w/s.jsonl|claude-code|5|2|3|4|0|locked|7"]
        );
        assert_eq!(
            rows(&db, "SELECT id, parent FROM session"),
            ["claude-code:s|claude-code:p"]
        );
        assert_eq!(rows(&db, "SELECT readers, version FROM state"), ["7|2001"]);
    }
}
