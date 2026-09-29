//! OpenCode's database, which holds a row for each session and each message.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};
use serde_json::Value;

/// Where OpenCode keeps its database under `home`.
pub fn path(home: &Path) -> PathBuf {
    home.join(".local/share/opencode/opencode.db")
}

/// An OpenCode database at `path`, with OpenCode's `session_v2` and
/// `session_message` tables as it defines them, holding the session `ses_a`,
/// begun in `directory`.
pub fn database(path: &Path, directory: &str) -> Connection {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE session_v2 (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, workspace_id TEXT,
                 parent_id TEXT, fork_session_id TEXT, fork_boundary TEXT, slug TEXT NOT NULL,
                 directory TEXT NOT NULL, path TEXT, title TEXT, version TEXT NOT NULL, share_url TEXT,
                 summary_additions INTEGER, summary_deletions INTEGER, summary_files INTEGER,
                 summary_diffs TEXT, metadata TEXT, cost REAL DEFAULT 0 NOT NULL,
                 tokens_input INTEGER DEFAULT 0 NOT NULL, tokens_output INTEGER DEFAULT 0 NOT NULL,
                 tokens_reasoning INTEGER DEFAULT 0 NOT NULL, tokens_cache_read INTEGER DEFAULT 0 NOT NULL,
                 tokens_cache_write INTEGER DEFAULT 0 NOT NULL, revert TEXT, permission TEXT, agent TEXT,
                 model TEXT, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
                 time_idle INTEGER, time_viewed INTEGER, idle_outcome TEXT, time_compacting INTEGER,
                 time_archived INTEGER, time_suspended INTEGER, resume_attempts INTEGER DEFAULT 0 NOT NULL);
             CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, type TEXT NOT NULL,
                 seq INTEGER NOT NULL, time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
                 data TEXT NOT NULL,
                 FOREIGN KEY (session_id) REFERENCES session_v2 (id) ON DELETE CASCADE);
             CREATE UNIQUE INDEX session_message_session_seq_idx ON session_message (session_id, seq);",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_v2 (id, project_id, slug, directory, version, time_created, time_updated)
             VALUES ('ses_a', 'global', 'ses-a', ?1, '2.0.8', 1000, 1000)",
            [directory],
        )
        .unwrap();
    connection
}

/// Write the message `id` of `ses_a`, of type `kind`, begun at `created`
/// and last changed at `updated`, as OpenCode writes one: after the
/// session's others in `seq`, or in its place when written again as it
/// changes.
pub fn message(
    database: &Connection,
    id: &str,
    kind: &str,
    created: i64,
    updated: i64,
    data: &Value,
) {
    database
        .execute(
            "INSERT INTO session_message (id, session_id, type, seq, time_created, time_updated, data)
             VALUES (?1, 'ses_a', ?2,
                     (SELECT count(*) FROM session_message WHERE session_id = 'ses_a'), ?3, ?4, ?5)
             ON CONFLICT (id) DO UPDATE SET time_updated = excluded.time_updated, data = excluded.data",
            params![id, kind, created, updated, data.to_string()],
        )
        .unwrap();
}
