//! OpenCode.
//!
//! OpenCode keeps every session in one SQLite database,
//! `~/.local/share/opencode/opencode.db`: a row per session in `session_v2`,
//! a row per message in `session_message`, whose `data` holds an assistant
//! message's tokens, cost, provider and model as JSON.
//!
//! What follows was measured on this Mac's history, September and October 2026.
//!
//! **Usage.** `tokens.input` excludes cached input, and `reasoning` is apart
//! from `output`, as OpenCode's own costs bear out.
//!
//! **Messages** are read on from their own `time_updated`, not their
//! session's: OpenCode adds a step to a session without touching the
//! session's.
//!
//! **Not read.** A session's own totals add calls no message records, such as
//! compacting and naming it: a few hundredths of a percent of its tokens.
//!
//! **Logins** are rows of `credential`, `integration_id` naming the provider.
//! Of several to one provider, the latest `active` one is in use.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Agent, Info, Login, Read, Response, Title, Tokens};
use crate::Result;

pub struct OpenCode;

static INFO: Info = Info {
    id: "opencode",
    name: "OpenCode",
    command: "opencode",
    app: None,
    mcp_add: &["mcp", "add", "--global", "turnscope", "--"],
    mcp_remove: None,
    folder_var: None,
    charges: false,
    session_var: None,
};

/// Whether the message `m` of the session `s` is the session's own: a fork's
/// copies of its parent's messages keep the times they were first written,
/// before the fork began. They're the parent's, as OpenCode's own statistics
/// count them. (From OpenCode's code: no fork was on this Mac to measure.)
const OWN: &str = "(s.fork_session_id IS NULL OR m.time_created >= s.time_created)";

#[derive(Default, Serialize, Deserialize)]
struct Cursor {
    /// The latest `time_updated` seen in each table, and no later than the
    /// clock was at that read. Chosen over the latest seen alone, under which
    /// a row stamped while the clock was ahead, as one set by hand and put
    /// right, hid every row stamped at the true time until the clock passed
    /// it, and `doctor` said nothing. On a made-up database with a session
    /// stamped a day ahead, one saved a minute before now was still unread
    /// two days on. Such a row is read again at each read until the clock
    /// passes it, which changes nothing. (No real history was looked at for
    /// such a row.)
    sessions: i64,
    messages: i64,
}

/// How long before the latest `time_updated` seen a read starts. A row can
/// be committed after one stamped later: OpenCode stamps a row as it makes
/// the write, then waits up to 5 s (`busy_timeout`) for another process's
/// write to end. Rows read again change nothing. (From OpenCode's code,
/// `packages/core/src/database/`, read 2026-10-12.)
const LATE: i64 = 10_000;

impl Agent for OpenCode {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn folders(&self, home: &Path) -> Vec<PathBuf> {
        vec![home.join(".local/share/opencode")]
    }

    fn files(&self, folder: &Path) -> Vec<PathBuf> {
        vec![folder.join("opencode.db")]
    }

    fn read(&self, file: &Path, cursor: &str) -> Result<Read> {
        read_at(file, cursor, crate::now())
    }

    fn logins(&self, folder: &Path, _home: &Path) -> Result<Vec<Login>> {
        let file = folder.join("opencode.db");
        if !file.is_file() {
            return Ok(Vec::new());
        }
        let rows: Vec<(String, String)> = super::opened(&file, |db| {
            db.prepare(
                "SELECT integration_id, value FROM credential
                 WHERE integration_id IS NOT NULL AND coalesce(active, 1) != 0
                 ORDER BY time_updated DESC",
            )?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect()
        })?;
        let mut logins: Vec<Login> = Vec::new();
        for (provider, value) in rows {
            if logins.iter().any(|login| login.provider == provider) {
                continue;
            }
            let value: Value = serde_json::from_str(&value).unwrap_or_default();
            logins.push(super::kept_login(&provider, &provider, &value, "key"));
        }
        Ok(logins)
    }

    fn mcp_command(&self, _folder: &Path, home: &Path) -> Option<String> {
        super::json_mcp_command(
            &home.join(".config/opencode/opencode.json"),
            "/mcp/servers/turnscope/command/0",
        )
    }
}

#[derive(Deserialize)]
struct UserText<'a> {
    #[serde(borrow)]
    text: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct MessageData<'a> {
    #[serde(borrow)]
    model: Option<ModelRef<'a>>,
    tokens: Option<MessageTokens>,
    time: Option<MessageTime>,
    #[serde(rename = "providerState", borrow)]
    provider_state: Option<ProviderState<'a>>,
    cost: Option<f64>,
}

#[derive(Deserialize)]
struct ModelRef<'a> {
    #[serde(rename = "providerID", borrow)]
    provider_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct MessageTokens {
    input: Option<u64>,
    output: Option<u64>,
    reasoning: Option<u64>,
    cache: Option<CacheTokens>,
}

#[derive(Deserialize)]
struct CacheTokens {
    read: Option<u64>,
    write: Option<u64>,
}

#[derive(Deserialize)]
struct MessageTime {
    completed: Option<i64>,
    created: Option<i64>,
}

#[derive(Deserialize)]
struct ProviderState<'a> {
    #[serde(rename = "serviceTier", borrow)]
    service_tier: Option<Cow<'a, str>>,
}

fn read_at(file: &Path, cursor: &str, now: i64) -> Result<Read> {
    let mut cursor: Cursor = serde_json::from_str(cursor).unwrap_or_default();
    super::opened(file, |db| {
        let mut read = Read::default();
        // One read transaction, so sessions and messages are of one moment.
        let tx = db.unchecked_transaction()?;
        sessions(&tx, now, &mut cursor, &mut read)?;
        messages(&tx, now, &mut cursor, &mut read)?;
        read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
        Ok(read)
    })
}

fn sessions(
    db: &Connection,
    now: i64,
    cursor: &mut Cursor,
    read: &mut Read,
) -> rusqlite::Result<()> {
    let mut sessions = db.prepare(&format!(
        "SELECT s.id, nullif(s.parent_id, ''), nullif(s.directory, ''), s.title, s.time_updated,
             CASE WHEN s.title IS NULL OR trim(s.title, ' ' || char(9, 10, 13)) = '' THEN
                 (SELECT m.data FROM session_message m WHERE m.session_id = s.id
                     AND m.type = 'user' AND {OWN} ORDER BY m.seq LIMIT 1)
             END
         FROM session_v2 s WHERE s.time_updated >= ?1"
    ))?;
    let mut rows = sessions.query([cursor.sessions - LATE])?;
    while let Some(row) = rows.next()? {
        let native = row.get_ref(0)?.as_str()?;
        let parent = row.get_ref(1)?.as_str_or_null()?;
        let updated: i64 = row.get(4)?;
        cursor.sessions = cursor.sessions.max(updated.min(now));
        let session = read.session(&INFO.session(native));
        // A subagent's parent. A fork is a session of its own.
        session.parent = parent.map(|parent| INFO.session(parent));
        session.cwd = row.get(2)?;
        if let Some(data) = row.get_ref(5)?.as_str_or_null()?
            && let Ok(user) = serde_json::from_str::<UserText<'_>>(data)
            && let Some(text) = user.text
        {
            session.title(Title::Prompt, &text);
        }
        if let Some(title) = row.get_ref(3)?.as_str_or_null()? {
            session.title(Title::Generated, title);
        }
    }
    Ok(())
}

fn messages(
    db: &Connection,
    now: i64,
    cursor: &mut Cursor,
    read: &mut Read,
) -> rusqlite::Result<()> {
    let mut messages = db.prepare(&format!(
        "SELECT m.id, m.session_id, m.time_created, m.time_updated, m.data
         FROM session_message m LEFT JOIN session_v2 s ON s.id = m.session_id
         WHERE m.time_updated >= ?1 AND m.type = 'assistant' AND {OWN}"
    ))?;
    let mark = cursor.messages;
    let mut rows = messages.query([mark - LATE])?;
    while let Some(row) = rows.next()? {
        let message: String = row.get(0)?;
        let native = row.get_ref(1)?.as_str()?;
        let (created, updated): (i64, i64) = (row.get(2)?, row.get(3)?);
        cursor.messages = cursor.messages.max(updated.min(now));
        // A row read again only for being within `LATE` was counted, and
        // one stamped ahead of the clock is counted by the read that takes
        // the cursor past it. One committed late, stamped at or before the
        // last read, isn't: knowing it wasn't seen would take every bad
        // row's id in the cursor, for a count only doctor shows.
        let unseen = u64::from(updated > mark && updated <= now);
        let Ok(data) = serde_json::from_str::<MessageData<'_>>(row.get_ref(4)?.as_str()?) else {
            read.skipped += unseen;
            continue;
        };
        // A message has no tokens until its response ends.
        let Some(tokens) = data.tokens else {
            continue;
        };
        let (Some(input), Some(output), Some(reasoning), Some(cache)) =
            (tokens.input, tokens.output, tokens.reasoning, tokens.cache)
        else {
            continue;
        };
        let (Some(cache_read), Some(cache_write)) = (cache.read, cache.write) else {
            continue;
        };
        let Some((Some(provider), Some(model))) = data.model.map(|m| (m.provider_id, m.id)) else {
            read.skipped += unseen;
            continue;
        };
        let tokens = Tokens {
            input,
            cache_read,
            cache_write_5m: cache_write,
            cache_write_1h: 0,
            output: output + reasoning,
            reasoning,
        };
        read.respond(Response {
            id: message,
            session: INFO.session(native),
            copy: false,
            at: data
                .time
                .and_then(|time| time.completed.or(time.created))
                .unwrap_or(created),
            provider: provider.into_owned(),
            model: model.into_owned(),
            prompt: input + cache_read + cache_write,
            tokens,
            web_searches: 0,
            priority: data
                .provider_state
                .and_then(|state| state.service_tier)
                .as_deref()
                == Some("priority"),
            cost: data.cost,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A database shaped as OpenCode's, with a session, its fork, and their
    /// messages.
    fn database(dir: &Path) -> PathBuf {
        let file = dir.join("opencode.db");
        let db = Connection::open(&file).unwrap();
        db.execute_batch(
            "CREATE TABLE session_v2 (id TEXT PRIMARY KEY, parent_id TEXT, fork_session_id TEXT, directory TEXT,
                 title TEXT, time_created INTEGER, time_updated INTEGER);
             CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, seq INTEGER,
                 time_created INTEGER, time_updated INTEGER, data TEXT);
             CREATE TABLE credential (integration_id TEXT, value TEXT, active INTEGER, time_updated INTEGER);
             INSERT INTO session_v2 VALUES ('s1', NULL, NULL, '/work/web', 'Fix the form', 1000, 1000);
             INSERT INTO session_v2 VALUES ('s2', NULL, 's1', '/work/web', NULL, 5000, 5000);",
        )
        .unwrap();
        let assistant = json!({"model": {"providerID": "opencode-go", "id": "glm-5.3"}, "time": {"created": 2000, "completed": 2500},
            "tokens": {"input": 100, "output": 20, "reasoning": 5, "cache": {"read": 300, "write": 0}}, "cost": 0.002});
        for (id, session, kind, seq, created, data) in [
            (
                "m1",
                "s1",
                "user",
                1,
                1500,
                json!({"text": "Fix the form's validation."}),
            ),
            ("m2", "s1", "assistant", 2, 2000, assistant.clone()),
            // The fork's copy of its parent's message, from before it began.
            ("m3", "s2", "assistant", 1, 2000, assistant),
        ] {
            db.execute(
                // Updated last when a reply completed, at 2500.
                "INSERT INTO session_message VALUES (?1, ?2, ?3, ?4, ?5, max(?5, iif(?3 = 'assistant', 2500, 0)), ?6)",
                rusqlite::params![id, session, kind, seq, created, data.to_string()],
            )
            .unwrap();
        }
        db.execute("INSERT INTO credential VALUES ('opencode-go', '{\"type\":\"key\",\"key\":\"sk-go\"}', 1, 1)", []).unwrap();
        file
    }

    #[test]
    fn the_latest_active_sign_in_or_key_to_each_provider_is_its_login() {
        let dir = tempfile::tempdir().unwrap();
        let file = database(dir.path());
        let db = Connection::open(&file).unwrap();
        for (provider, value, active, at) in [
            (
                "openrouter",
                r#"{"type":"oauth","access":"sk-or-made-up","expires":9}"#,
                1,
                2,
            ),
            (
                "anthropic",
                r#"{"type":"key","key":"sk-ant-made-up"}"#,
                1,
                3,
            ),
            ("anthropic", r#"{"type":"key","key":"sk-ant-older"}"#, 1, 1),
            ("xai", r#"{"type":"key","key":"sk-made-up-inactive"}"#, 0, 4),
        ] {
            db.execute(
                "INSERT INTO credential VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![provider, value, active, at],
            )
            .unwrap();
        }
        let logins = OpenCode.logins(dir.path(), dir.path()).unwrap();
        let mut read: Vec<_> = logins
            .iter()
            .map(|login| (login.provider.as_str(), crate::agents::credential(login)))
            .collect();
        read.sort();
        assert_eq!(
            read,
            [
                (
                    "anthropic",
                    Some(("anthropic", true, None, "sk-ant-made-up"))
                ),
                ("opencode-go", Some(("opencode-go", true, None, "sk-go"))),
                (
                    "openrouter",
                    Some(("openrouter", true, Some(9), "sk-or-made-up"))
                ),
            ]
        );
    }

    #[test]
    fn messages_are_read_and_a_forks_copies_are_its_parents() {
        let dir = tempfile::tempdir().unwrap();
        let file = database(dir.path());
        let read = OpenCode.read(&file, "").unwrap();
        assert_eq!(read.responses.len(), 1, "the fork's copy isn't its own");
        let response = &read.responses["m2"];
        assert_eq!(
            response.tokens,
            Tokens {
                input: 100,
                cache_read: 300,
                cache_write_5m: 0,
                cache_write_1h: 0,
                output: 25,
                reasoning: 5
            }
        );
        assert_eq!(
            (response.at, response.cost, response.provider.as_str()),
            (2500, Some(0.002), "opencode-go")
        );
        let session = &read.sessions["opencode:s1"];
        assert_eq!(session.title.as_ref().unwrap().1, "Fix the form");
        // A fork is a session of its own.
        assert!(read.sessions["opencode:s2"].parent.is_none());
        let again = OpenCode.read(&file, &read.cursor).unwrap();
        assert_eq!(again.responses["m2"], read.responses["m2"]);
    }

    #[test]
    fn a_reply_committed_after_a_later_one_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = database(dir.path());
        let first = OpenCode.read(&file, "").unwrap();
        // Another OpenCode process stamped it at 2200, before the last read
        // saw m2's 2500, and committed it after.
        let reply = json!({"model": {"providerID": "opencode-go", "id": "glm-5.3"}, "time": {"created": 2100, "completed": 2200},
            "tokens": {"input": 50, "output": 10, "reasoning": 0, "cache": {"read": 0, "write": 0}}});
        Connection::open(&file)
            .unwrap()
            .execute(
                "INSERT INTO session_message VALUES ('m4', 's1', 'assistant', 3, 2100, 2200, ?1)",
                [reply.to_string()],
            )
            .unwrap();
        let later = OpenCode.read(&file, &first.cursor).unwrap();
        assert_eq!(later.responses["m4"].tokens.input, 50);
    }

    #[test]
    fn a_row_stamped_ahead_of_the_clock_doesnt_stop_later_rows_being_read() {
        let dir = tempfile::tempdir().unwrap();
        let file = database(dir.path());
        let db = Connection::open(&file).unwrap();
        // Saved at 20,000 by a clock an hour ahead: a session, a reply in
        // it, and a row that can't be read.
        let ahead = 3_620_000;
        let reply = json!({"model": {"providerID": "opencode-go", "id": "glm-5.3"}, "time": {"created": ahead, "completed": ahead},
            "tokens": {"input": 50, "output": 10, "reasoning": 0, "cache": {"read": 0, "write": 0}}});
        db.execute_batch(&format!(
            "INSERT INTO session_v2 VALUES ('s3', NULL, NULL, '/work/web', 'Dark theme', {ahead}, {ahead});
             INSERT INTO session_message VALUES ('m4', 's3', 'assistant', 1, {ahead}, {ahead}, '{reply}');
             INSERT INTO session_message VALUES ('m5', 's3', 'assistant', 2, {ahead}, {ahead}, 'not JSON');"
        ))
        .unwrap();
        let first = read_at(&file, "", 20_000).unwrap();
        // The clock put right, a session at 30,000.
        db.execute(
            "INSERT INTO session_v2 VALUES ('s4', NULL, NULL, '/work/web', 'Footer', 30000, 30000)",
            [],
        )
        .unwrap();
        let later = read_at(&file, &first.cursor, 31_000).unwrap();
        let footer = &later.sessions["opencode:s4"];
        assert_eq!(footer.title.as_ref().unwrap().1, "Footer");
        assert_eq!(later.responses["m4"], first.responses["m4"]);
        // The row that can't be read is counted once, when the clock passes
        // it.
        let passed = read_at(&file, &later.cursor, ahead + 1_000).unwrap();
        let after = read_at(&file, &passed.cursor, ahead + 2_000).unwrap();
        assert_eq!(
            [first.skipped, later.skipped, passed.skipped, after.skipped],
            [0, 0, 1, 0]
        );
    }
}
