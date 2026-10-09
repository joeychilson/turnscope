//! The database: one SQLite file, `turnscope.sqlite` in the data directory.
//! Each module writes its own SQL against this schema.
//!
//! Times are UTC milliseconds and money is dollars. History read from the
//! agents' files (`file`, `session`, `response`, `said`) can be read again;
//! accounts, readings, sign-ins, prices as they were, alerts and settings
//! exist nowhere else.

use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;

use rusqlite::Connection;

use crate::{Error, Result};

/// The schema, one script per revision. A shipped script is never edited: a
/// change is a new one.
const REVISIONS: &[&str] = &[r#"
CREATE TABLE file (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    agent TEXT NOT NULL,
    folder TEXT NOT NULL,          -- the agent folder it's in
    size INTEGER NOT NULL,
    modified INTEGER NOT NULL,     -- nanoseconds
    inode INTEGER NOT NULL,
    cursor TEXT NOT NULL,          -- where the agent's reader stopped
    skipped INTEGER NOT NULL DEFAULT 0  -- lines or rows it couldn't read
) STRICT;

CREATE TABLE session (
    id TEXT PRIMARY KEY,           -- 'claude-code:0f6e3f6a-…'
    agent TEXT NOT NULL,
    folder TEXT NOT NULL,
    cwd TEXT,
    project TEXT,                  -- the repository's main working tree, as first seen
    branch TEXT,
    title TEXT,
    title_rank INTEGER NOT NULL DEFAULT -1,  -- agents::Title
    parent TEXT,
    link TEXT CHECK (link IN ('subagent', 'fork', 'continuation')),
    started INTEGER,
    last INTEGER
) STRICT;
CREATE INDEX session_last ON session (last);
CREATE INDEX session_parent ON session (parent);
CREATE INDEX session_folder ON session (agent, folder, last);

-- The files that hold a session's conversation.
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
    folder TEXT NOT NULL,
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

-- What the person and the models said, for search. The text is indexed,
-- not kept.
CREATE VIRTUAL TABLE said USING fts5 (
    session UNINDEXED, text,
    content = '', contentless_delete = 1, contentless_unindexed = 1,
    tokenize = 'unicode61 remove_diacritics 2'
);

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
    history_at INTEGER,            -- when the agents' history was last read
    limits_at INTEGER,             -- when any process last read limits
    prices_at INTEGER,             -- when models.dev was last asked
    settings TEXT                  -- alerts::Settings, as JSON
) STRICT;
INSERT INTO state (id) VALUES (1);
"#];

/// "TRNS", in the header, so a file that isn't Turnscope's is never written.
const APPLICATION_ID: i32 = 0x5452_4E53;

/// Open the database in `dir`, creating or migrating it.
///
/// One process at a time creates or migrates it, holding `turnscope.lock`
/// beside it: turning on write-ahead logging, which a new database needs,
/// can't wait for another connection as a query does, and fails at once.
pub fn open(dir: &Path) -> Result<Connection> {
    // The owner's alone: it holds what was said in every session.
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|error| Error::Failed(format!("creating {}: {error}", dir.display())))?;
    let path = dir.join("turnscope.sqlite");
    let mut db = Connection::open(&path)?;
    db.busy_timeout(std::time::Duration::from_secs(10))?;
    db.pragma_update(None, "synchronous", "NORMAL")?;
    let latest = REVISIONS.len() as i64;
    let revision = |db: &Connection| -> Result<i64> {
        let revision: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if revision > latest {
            return Err(Error::Failed(
                "a newer Turnscope has updated this data; update Turnscope".to_owned(),
            ));
        }
        Ok(revision)
    };
    if revision(&db)? == latest {
        return Ok(db);
    }
    let lock = dir.join("turnscope.lock");
    let _lock = std::fs::File::create(&lock)
        .and_then(|file| file.lock().map(|()| file))
        .map_err(|error| Error::Failed(format!("locking {}: {error}", lock.display())))?;
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
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Exclusive)?;
    for (revision, script) in (1..).zip(REVISIONS).skip(done as usize) {
        tx.execute_batch(script)?;
        tx.pragma_update(None, "user_version", revision)?;
    }
    tx.pragma_update(None, "application_id", APPLICATION_ID)?;
    tx.commit()?;
    Ok(db)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_new_database_opened_at_once_from_many_places_opens_in_each() {
        let dir = tempfile::tempdir().unwrap();
        let opened: Vec<bool> = std::thread::scope(|scope| {
            let opening: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| super::open(dir.path()).is_ok()))
                .collect();
            opening
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect()
        });
        assert!(opened.iter().all(|opened| *opened), "{opened:?}");
        let db = super::open(dir.path()).unwrap();
        let mode: String = db
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }
}
