//! The database: one SQLite file, `turnscope.sqlite` in the data directory.
//! Each module writes its own SQL against this schema.
//!
//! Times are UTC milliseconds and money is dollars. History read from the
//! agents' files (`file`, `session`, `response`) can be read again;
//! accounts, readings, sign-ins, prices as they were, alerts and settings
//! exist nowhere else.

use std::fs::File;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;

use rusqlite::{Connection, Row};

use crate::agents::Tokens;
use crate::{Error, Result};

/// The schema, one script per revision. A shipped script is never edited: a
/// change is a new one.
const REVISIONS: &[&str] = &[
    r#"
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
"#,
    r#"
-- Each row of `said` names its file, so a file read again from the start
-- takes its own rows out first. A table without content can't gain a
-- column, so it's made again, and the history read again to fill it.
-- Dropping it leaves its `said_content` behind (SQLite 3.53, a table
-- keeping unindexed columns but no content), which would stop it being
-- made again.
DROP TABLE said;
DROP TABLE IF EXISTS said_content;
CREATE VIRTUAL TABLE said USING fts5 (
    session UNINDEXED, text, file UNINDEXED,
    content = '', contentless_delete = 1, contentless_unindexed = 1,
    tokenize = 'unicode61 remove_diacritics 2'
);
UPDATE state SET readers = 0;

-- Why a file couldn't be read when it was last tried; NULL once it was.
ALTER TABLE file ADD COLUMN failed TEXT;
-- Whether its reads put rows in `said`: only such a file's are taken out
-- when it's read again, since taking them out by file scans the table.
ALTER TABLE file ADD COLUMN indexed INTEGER NOT NULL DEFAULT 0;
"#,
    r#"
-- Each row of `said` keeps its line's ordinal, where the agent numbers a
-- session's lines, so a session rewound to an earlier point takes what was
-- said after it out. Made again as before, and the history read again.
DROP TABLE said;
DROP TABLE IF EXISTS said_content;
CREATE VIRTUAL TABLE said USING fts5 (
    session UNINDEXED, text, file UNINDEXED, ordinal UNINDEXED,
    content = '', contentless_delete = 1, contentless_unindexed = 1,
    tokenize = 'unicode61 remove_diacritics 2'
);
UPDATE state SET readers = 0;
"#,
    r#"
-- The `ingest::READERS` each file was last read with. One read with an
-- older reader is read again from the start when it's next found: where it
-- was, moved, or back after it was gone. Until then it keeps what it made,
-- as one its agent deleted does.
ALTER TABLE file ADD COLUMN readers INTEGER NOT NULL DEFAULT 0;
UPDATE file SET readers = (SELECT readers FROM state);
"#,
    r#"
-- What was said in sessions is no longer kept: Turnscope stopped searching
-- and reading them (0.2.0). The search index goes, and with it the mark of
-- the files that had rows in it.
--
-- The file is left the size it was, over giving the index's pages back with
-- VACUUM: they're used again as history grows. On a database of 484 MB, with
-- 467,000 responses and 13,800 sessions, the index was 57 MB and dropping
-- it took 1.0 s; VACUUM then took 7.8 s, to 370 MB, holding the write lock
-- all the while (measured 2026-11-16). Whichever command opens the database
-- first runs this, and an agent's hook is one: it can't wait that long.
DROP TABLE said;
DROP TABLE IF EXISTS said_content;
ALTER TABLE file DROP COLUMN indexed;
-- `chat_history.jsonl` was read only for what it said, and no reader lists
-- it now. Its row goes too, so a session it held is held only by files
-- still read: `ingest::read_again` keeps a session that a file not read
-- again holds.
DELETE FROM session_file
WHERE file IN (SELECT id FROM file WHERE path LIKE '%/chat\_history.jsonl' ESCAPE '\');
DELETE FROM file WHERE path LIKE '%/chat\_history.jsonl' ESCAPE '\';

-- Of a session, what usage reads stays: its agent, its project, its title,
-- the session it's a subagent of, and when it was last active. Its agent
-- folder, the folder it worked in, its branch, when it started, and the
-- session a fork or a continuation came from were read only to list, show
-- and hand off sessions, as was a file's agent folder.
--
-- A fork or a continuation was a session of its own already. Now it has no
-- parent, so a parent is a subagent's alone and `link` goes. On that
-- database this and the titles below took 1.1 s more, and usage by session
-- of 10,311 sessions came out the same but for 3,793 titles cut.
DROP INDEX session_last;
DROP INDEX session_folder;
UPDATE session SET parent = NULL WHERE link IS NOT 'subagent';
ALTER TABLE session DROP COLUMN link;
ALTER TABLE session DROP COLUMN branch;
ALTER TABLE session DROP COLUMN started;
ALTER TABLE session DROP COLUMN folder;
ALTER TABLE session DROP COLUMN cwd;
ALTER TABLE file DROP COLUMN folder;

-- A title is kept to its first 200 characters, as `agents::Session::title`
-- cuts one read from now on: 4,206 of 13,795 were longer, and the titles
-- went from 43.4 MB to 1.5 MB.
UPDATE session SET title = substr(title, 1, 200) WHERE length(title) > 200;
"#,
];

/// "TRNS", in the header, so a file that isn't Turnscope's is never written.
const APPLICATION_ID: i32 = 0x5452_4E53;

/// Open the database in `dir`, creating or migrating it.
///
/// One process at a time creates or migrates it, holding `turnscope.lock`
/// beside it: turning on write-ahead logging, which a new database needs,
/// can't wait for another connection as a query does, and fails at once.
pub fn open(dir: &Path) -> Result<Connection> {
    // The owner's alone: it holds what each session cost and was asked.
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
            return Err(newer());
        }
        Ok(revision)
    };
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
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Exclusive)?;
    for (revision, script) in (1..).zip(REVISIONS).skip(done as usize) {
        tx.execute_batch(script)?;
        tx.pragma_update(None, "user_version", revision)?;
    }
    tx.pragma_update(None, "application_id", APPLICATION_ID)?;
    tx.commit()?;
    Ok(db)
}

/// Hold the lock file at `path`, made if it's missing, until the file
/// returned is dropped.
pub fn lock(path: &Path) -> Result<File> {
    File::create(path)
        .and_then(|file| file.lock().map(|()| file))
        .map_err(|error| Error::Failed(format!("locking {}: {error}", path.display())))
}

/// The error when a newer Turnscope has updated the data: this one would
/// misread it, or undo what that one did.
pub fn newer() -> Error {
    Error::Failed("a newer Turnscope has updated this data; update Turnscope".to_owned())
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

    /// A database as 0.1.8 left it, at revision 4, with what was said in a
    /// session indexed.
    #[test]
    fn the_search_index_goes_and_what_usage_reads_stays() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("turnscope.sqlite");
        let old = rusqlite::Connection::open(&path).unwrap();
        for script in &super::REVISIONS[..4] {
            old.execute_batch(script).unwrap();
        }
        old.execute_batch(
            "PRAGMA user_version = 4;
             INSERT INTO file (id, path, agent, folder, size, modified, inode, cursor, indexed) VALUES
                 (1, '/home/.grok/sessions/a/s1/updates.jsonl', 'grok-build', '/home/.grok', 1, 1, 1, '', 0),
                 (2, '/home/.grok/sessions/a/s1/chat_history.jsonl', 'grok-build', '/home/.grok', 1, 1, 2, '', 1),
                 (3, '/home/.grok/sessions/a/s1/chatXhistory.jsonl', 'grok-build', '/home/.grok', 1, 1, 3, '', 0);
             INSERT INTO session (id, agent, folder, cwd, project, branch, title, title_rank, parent, link,
                 started, last) VALUES
                 ('grok-build:s1', 'grok-build', '/home/.grok', '/work/app', '/work/app', 'main',
                     'Add a flag.', 0, NULL, NULL, 1000, 2000),
                 ('grok-build:s2', 'grok-build', '/home/.grok', NULL, NULL, NULL, NULL, -1,
                     'grok-build:s1', 'subagent', 1500, 1800),
                 ('grok-build:s3', 'grok-build', '/home/.grok', NULL, NULL, NULL, NULL, -1,
                     'grok-build:s1', 'fork', 3000, 3000),
                 ('grok-build:s4', 'grok-build', '/home/.grok', NULL, NULL, NULL, NULL, -1,
                     'grok-build:s1', 'continuation', 4000, 4000);
             INSERT INTO session_file (session, file) VALUES ('grok-build:s1', 1), ('grok-build:s1', 2);
             INSERT INTO said (session, text, file) VALUES ('grok-build:s1', 'Add a flag.', 2);",
        )
        .unwrap();
        // A title of 300 characters, the last 100 of them not ASCII.
        let long = "a".repeat(200) + &"é".repeat(100);
        old.execute(
            "INSERT INTO session (id, agent, folder, title, title_rank) VALUES ('grok-build:s5', 'grok-build', '/home/.grok', ?1, 0)",
            [&long],
        )
        .unwrap();
        drop(old);

        let db = super::open(dir.path()).unwrap();
        let count = |sql: &str| -> i64 { db.query_row(sql, [], |row| row.get(0)).unwrap() };
        assert_eq!(
            count("SELECT count(*) FROM sqlite_schema WHERE name LIKE 'said%'"),
            0
        );
        // The file read only for what it said is forgotten, and no other.
        let files: Vec<i64> = db
            .prepare("SELECT id FROM file ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(files, [1, 3]);
        assert_eq!(count("SELECT count(*) FROM session_file"), 1);
        // A session keeps what usage reads, and only a subagent a parent.
        let sessions: Vec<[Option<String>; 5]> = db
            .prepare(
                "SELECT id, project, title, parent, CAST(last AS TEXT) FROM session ORDER BY id",
            )
            .unwrap()
            .query_map([], |row| {
                Ok([0, 1, 2, 3, 4].map(|column| row.get(column).unwrap()))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let some = |text: &str| Some(text.to_owned());
        let cut = "a".repeat(200);
        assert_eq!(
            sessions,
            [
                [
                    some("grok-build:s1"),
                    some("/work/app"),
                    some("Add a flag."),
                    None,
                    some("2000")
                ],
                [
                    some("grok-build:s2"),
                    None,
                    None,
                    some("grok-build:s1"),
                    some("1800")
                ],
                [some("grok-build:s3"), None, None, None, some("3000")],
                [some("grok-build:s4"), None, None, None, some("4000")],
                [some("grok-build:s5"), None, some(&cut), None, None],
            ]
        );
        let columns = |table: &str| -> Vec<String> {
            db.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(
            columns("session"),
            [
                "id",
                "agent",
                "project",
                "title",
                "title_rank",
                "parent",
                "last"
            ]
        );
        assert_eq!(
            columns("file"),
            [
                "id", "path", "agent", "size", "modified", "inode", "cursor", "skipped", "failed",
                "readers"
            ]
        );
    }
}
