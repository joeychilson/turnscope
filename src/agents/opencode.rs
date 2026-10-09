//! OpenCode.
//!
//! OpenCode keeps every session in one SQLite database,
//! `~/.local/share/opencode/opencode.db`: a row per session in `session_v2`,
//! a row per message in `session_message`, whose `data` holds an assistant
//! message's tokens, cost, provider and model as JSON. It's read on from the
//! latest `time_updated` seen in each table; rows updated at that mark are
//! read again, which changes nothing.
//!
//! What follows was measured on this Mac's history, September and October 2026.
//!
//! **Usage.** `tokens.input` excludes cached input, and `reasoning` is apart
//! from `output`, as OpenCode's own costs bear out.
//!
//! **Forks** open with copies of their parent's messages, which keep the times
//! they were first written, before the fork's own creation; they're the
//! parent's, as OpenCode's own statistics count them. (No fork was on this
//! Mac to measure: this is from OpenCode's code, which copies them so.)
//!
//! **When.** OpenCode adds a step to a session without touching the session's
//! `time_updated`, so when it worked is its messages': each one's creation
//! and a response's completion.
//!
//! **Not read.** A session's own totals add calls no message records, such as
//! compacting and naming it: a few hundredths of a percent of its tokens.
//!
//! **Logins** are rows of `credential`: `integration_id` names the provider,
//! and `value` is `{type: "key", key}` or `{type: "oauth", access, …}`; an
//! OpenRouter sign-in is kept either way, its token a key. Of several to one
//! provider, `active` marks the one in use.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    Agent, Conversation, Credential, Entry, Info, Kind, Link, Login, Parent, Read, Response, Role,
    Secret, Title, Tokens,
};
use crate::{Error, Result};

pub struct OpenCode;

static INFO: Info = Info {
    id: "opencode",
    name: "OpenCode",
    command: "opencode",
    resume: "opencode --session {id}",
    mcp_add: &["mcp", "add", "--global", "turnscope", "--"],
    mcp_remove: None,
    folder_var: None,
    charges: false,
    session_var: None,
    cwd_var: None,
};

/// Whether the message `m` of the session `s` is the session's own: a fork's
/// copies keep the times they were first written, before the fork began.
const OWN: &str = "(s.fork_session_id IS NULL OR m.time_created >= s.time_created)";

#[derive(Default, Serialize, Deserialize)]
struct Cursor {
    sessions: i64,
    messages: i64,
}

fn id(native: &str) -> String {
    format!("opencode:{native}")
}

fn open(file: &Path) -> Result<Connection> {
    let failed =
        |error: rusqlite::Error| Error::Failed(format!("reading {}: {error}", file.display()));
    let db = Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(failed)?;
    db.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(failed)?;
    Ok(db)
}

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
        let mut cursor: Cursor = serde_json::from_str(cursor).unwrap_or_default();
        let db = open(file)?;
        let mut read = Read::default();
        let failed =
            |error: rusqlite::Error| Error::Failed(format!("reading {}: {error}", file.display()));
        // One read transaction, so sessions and messages are of one moment.
        let tx = db.unchecked_transaction().map_err(failed)?;
        let mut sessions = tx
            .prepare(&format!(
                "SELECT s.id, s.parent_id, s.fork_session_id, s.directory, s.title, s.time_created,
                     s.time_updated,
                     (SELECT m.data FROM session_message m WHERE m.session_id = s.id
                         AND m.type = 'user' AND {OWN} ORDER BY m.seq LIMIT 1)
                 FROM session_v2 s WHERE s.time_updated >= ?1"
            ))
            .map_err(failed)?;
        let mut rows = sessions.query([cursor.sessions]).map_err(failed)?;
        while let Some(row) = rows.next().map_err(failed)? {
            let (native, parent, fork): (String, Option<String>, Option<String>) = (
                row.get(0).map_err(failed)?,
                row.get(1).map_err(failed)?,
                row.get(2).map_err(failed)?,
            );
            let (created, updated): (i64, i64) =
                (row.get(5).map_err(failed)?, row.get(6).map_err(failed)?);
            cursor.sessions = cursor.sessions.max(updated);
            let session = read.session(&id(&native));
            session.parent = match (
                parent.filter(|parent| !parent.is_empty()),
                fork.filter(|fork| !fork.is_empty()),
            ) {
                (Some(parent), _) => Some(Parent {
                    id: id(&parent),
                    link: Link::Subagent,
                }),
                (None, Some(fork)) => Some(Parent {
                    id: id(&fork),
                    link: Link::Fork,
                }),
                _ => None,
            };
            session.cwd = row
                .get::<_, Option<String>>(3)
                .map_err(failed)?
                .filter(|cwd| !cwd.is_empty());
            session.saw(created);
            session.saw(updated);
            let first: Option<String> = row.get(7).map_err(failed)?;
            if let Some(text) = first.and_then(|data| serde_json::from_str::<Value>(&data).ok()) {
                session.title(Title::Prompt, text["text"].as_str().unwrap_or_default());
            }
            if let Some(title) = row.get::<_, Option<String>>(4).map_err(failed)? {
                session.title(Title::Generated, &title);
            }
        }
        drop(rows);
        drop(sessions);

        let mut messages = tx
            .prepare(&format!(
                "SELECT m.id, m.session_id, m.type, m.time_created, m.time_updated, m.data
                 FROM session_message m LEFT JOIN session_v2 s ON s.id = m.session_id
                 WHERE m.time_updated >= ?1 AND m.type IN ('user', 'assistant') AND {OWN}"
            ))
            .map_err(failed)?;
        // A message is read again each time it changes; what it says is
        // indexed once, when it's new or a response completes.
        let mark = cursor.messages;
        let mut rows = messages.query(params![mark]).map_err(failed)?;
        while let Some(row) = rows.next().map_err(failed)? {
            let (message, native, kind): (String, String, String) = (
                row.get(0).map_err(failed)?,
                row.get(1).map_err(failed)?,
                row.get(2).map_err(failed)?,
            );
            let (created, updated): (i64, i64) =
                (row.get(3).map_err(failed)?, row.get(4).map_err(failed)?);
            cursor.messages = cursor.messages.max(updated);
            let Ok(data) = serde_json::from_str::<Value>(&row.get::<_, String>(5).map_err(failed)?)
            else {
                read.skipped += 1;
                continue;
            };
            let session = id(&native);
            read.session(&session).saw(created);
            if let Some(completed) = data["time"]["completed"].as_i64() {
                read.session(&session).saw(completed);
            }
            if kind == "user" {
                if created > mark {
                    read.say(&session, data["text"].as_str().unwrap_or_default());
                }
                continue;
            }
            if data["time"]["completed"]
                .as_i64()
                .is_some_and(|completed| completed > mark)
            {
                for part in data["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|part| part["type"] == "text")
                {
                    read.say(&session, part["text"].as_str().unwrap_or_default());
                }
            }
            // A message has no tokens until its response ends.
            let tokens = &data["tokens"];
            let count = |value: &Value| value.as_u64();
            let (Some(input), Some(output), Some(reasoning), Some(cache_read), Some(cache_write)) = (
                count(&tokens["input"]),
                count(&tokens["output"]),
                count(&tokens["reasoning"]),
                count(&tokens["cache"]["read"]),
                count(&tokens["cache"]["write"]),
            ) else {
                continue;
            };
            let (Some(provider), Some(model)) = (
                data["model"]["providerID"].as_str(),
                data["model"]["id"].as_str(),
            ) else {
                read.skipped += 1;
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
            if tokens == Tokens::default() {
                continue;
            }
            read.respond(Response {
                id: message,
                session,
                copy: false,
                at: data["time"]["completed"]
                    .as_i64()
                    .or(data["time"]["created"].as_i64())
                    .unwrap_or(created),
                provider: provider.to_owned(),
                model: model.to_owned(),
                prompt: input + cache_read + cache_write,
                tokens,
                web_searches: 0,
                priority: data["providerState"]["serviceTier"] == "priority",
                cost: data["cost"].as_f64(),
            });
        }
        drop(rows);
        drop(messages);
        read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
        Ok(read)
    }

    fn transcript(&self, native: &str, files: &[PathBuf]) -> Result<Vec<Entry>> {
        let file = files
            .iter()
            .find(|file| file.ends_with("opencode.db"))
            .ok_or_else(|| Error::NotFound("OpenCode's database is gone".to_owned()))?;
        let failed =
            |error: rusqlite::Error| Error::Failed(format!("reading {}: {error}", file.display()));
        let db = open(file)?;
        let mut statement = db
            .prepare(&format!(
                "SELECT m.type, m.time_created, m.data
                 FROM session_message m JOIN session_v2 s ON s.id = m.session_id
                 WHERE m.session_id = ?1 AND {OWN} ORDER BY m.seq"
            ))
            .map_err(failed)?;
        let mut rows = statement.query([native]).map_err(failed)?;
        let mut conversation = Conversation::default();
        while let Some(row) = rows.next().map_err(failed)? {
            let kind: String = row.get(0).map_err(failed)?;
            let at = Some(row.get::<_, i64>(1).map_err(failed)?);
            let Ok(data) = serde_json::from_str::<Value>(&row.get::<_, String>(2).map_err(failed)?)
            else {
                continue;
            };
            // A field that isn't there is no text, not "null".
            let text = |value: &Value| match value {
                Value::String(text) => text.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            match kind.as_str() {
                "user" => conversation.say(Kind::User, at, &text(&data["text"])),
                "assistant" => {
                    for part in data["content"].as_array().into_iter().flatten() {
                        match part["type"].as_str() {
                            Some("text") => {
                                conversation.say(Kind::Assistant, at, &text(&part["text"]))
                            }
                            Some("reasoning") => {
                                conversation.say(Kind::Reasoning, at, &text(&part["text"]))
                            }
                            Some("tool") => {
                                let call = part["id"].as_str();
                                let state = &part["state"];
                                conversation.call(
                                    call,
                                    at,
                                    part["name"].as_str().unwrap_or("tool"),
                                    state["input"].to_string(),
                                );
                                // A finished call's output is its `content`, a
                                // string or a list of parts; a failed one's, its
                                // error's message.
                                let failed = state["status"] == "error"
                                    || state["metadata"]["exit"]
                                        .as_i64()
                                        .is_some_and(|exit| exit != 0);
                                let output = if state["status"] == "error" {
                                    text(state["error"].get("message").unwrap_or(&state["error"]))
                                } else {
                                    match state["content"].as_array() {
                                        Some(parts) => parts
                                            .iter()
                                            .filter_map(|part| part["text"].as_str())
                                            .collect::<Vec<_>>()
                                            .join("\n"),
                                        None => text(&state["content"]),
                                    }
                                };
                                if let Some(call) = call {
                                    conversation.answer(call, output, failed);
                                }
                                // What an edit, a write or a patch changed, as
                                // its result lists them, or its input names.
                                if !failed {
                                    let listed = state["metadata"]["files"].as_array();
                                    let input = &state["input"];
                                    let files: Vec<String> = match listed {
                                        Some(listed) => listed
                                            .iter()
                                            .filter_map(|file| file["file"].as_str())
                                            .map(str::to_owned)
                                            .collect(),
                                        None if matches!(
                                            part["name"].as_str(),
                                            Some("edit" | "write")
                                        ) =>
                                        {
                                            input["filePath"]
                                                .as_str()
                                                .or(input["path"].as_str())
                                                .map(str::to_owned)
                                                .into_iter()
                                                .collect()
                                        }
                                        None => super::patched(
                                            input["patchText"].as_str().unwrap_or_default(),
                                        ),
                                    };
                                    conversation.changed(files);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                // A command the person ran with `!`.
                "shell" => {
                    let call = data["shellID"].as_str();
                    conversation.call(call, at, "shell", text(&data["command"]));
                    if let Some(call) = call {
                        let failed = data["exit"].as_i64().is_some_and(|exit| exit != 0);
                        conversation.answer(call, text(&data["output"]["output"]), failed);
                    }
                }
                "compaction" => conversation.say(Kind::Summary, at, &text(&data["summary"])),
                _ => conversation.say(Kind::System, at, data["text"].as_str().unwrap_or_default()),
            }
        }
        Ok(conversation.entries)
    }

    fn logins(&self, folder: &Path, _home: &Path) -> Result<Vec<Login>> {
        let file = folder.join("opencode.db");
        let mut logins: Vec<Login> = Vec::new();
        if file.is_file() {
            let failed = |error: rusqlite::Error| {
                Error::Failed(format!("reading {}: {error}", file.display()))
            };
            let db = open(&file)?;
            // Of several to one provider, the latest active one is in use.
            let rows: Vec<(String, String)> = db
                .prepare(
                    "SELECT integration_id, value FROM credential
                     WHERE integration_id IS NOT NULL AND coalesce(active, 1) != 0
                     ORDER BY time_updated DESC",
                )
                .and_then(|mut statement| {
                    statement
                        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                        .collect()
                })
                .map_err(failed)?;
            for (provider, value) in rows {
                if logins.iter().any(|login| login.provider == provider) {
                    continue;
                }
                let value: Value = serde_json::from_str(&value).unwrap_or_default();
                let reads = crate::providers::by_id(&provider).map(|provider| provider.info().id);
                let (secret, key) = match value["type"].as_str() {
                    Some("key") => (value["key"].as_str(), true),
                    // OpenRouter's sign-in gives OpenCode a key.
                    Some("oauth") => (value["access"].as_str(), provider == "openrouter"),
                    _ => (None, false),
                };
                logins.push(Login {
                    credential: reads.zip(secret.filter(|secret| !secret.is_empty())).map(
                        |(reads, secret)| Credential {
                            provider: reads,
                            secret: Secret::new(secret),
                            key,
                            expires: value["expires"].as_i64(),
                            identity: None,
                            plan: None,
                        },
                    ),
                    provider,
                });
            }
        }
        Ok(logins)
    }

    fn mcp_command(&self, _folder: &Path, home: &Path) -> Option<String> {
        super::json_mcp_command(
            &home.join(".config/opencode/opencode.json"),
            "/mcp/servers/turnscope/command/0",
        )
    }

    fn role(&self, name: &str) -> Role {
        match name {
            "read" | "grep" | "glob" | "list" | "webfetch" | "websearch" => Role::Read,
            "question" => Role::Ask,
            "task" => Role::Subagent,
            _ => Role::Work,
        }
    }
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
            "tokens": {"input": 100, "output": 20, "reasoning": 5, "cache": {"read": 300, "write": 0}}, "cost": 0.002,
            "content": [{"type": "text", "text": "Fixed."}, {"type": "tool", "id": "c1", "name": "shell",
                "state": {"status": "completed", "input": {"command": "npm test"}, "content": [{"type": "text", "text": "ok"}], "metadata": {"exit": 0}}}]});
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
        assert_eq!((session.started, session.last), (Some(1000), Some(2500)));
        assert!(
            read.sessions["opencode:s2"]
                .parent
                .as_ref()
                .is_some_and(|parent| parent.link == Link::Fork)
        );
        assert_eq!(
            read.said.len(),
            2,
            "the prompt and the reply; the fork's copy isn't its own"
        );
        // Read on, rows at the mark are read again, and their text isn't
        // indexed again.
        let again = OpenCode.read(&file, &read.cursor).unwrap();
        assert!(again.said.is_empty());
        let entries = OpenCode
            .transcript("s1", std::slice::from_ref(&file))
            .unwrap();
        assert_eq!(
            entries.iter().map(|entry| entry.kind).collect::<Vec<_>>(),
            [Kind::User, Kind::Assistant, Kind::Tool]
        );
        assert_eq!(
            entries[2].tool.as_ref().unwrap().output.as_deref(),
            Some("ok")
        );
        let logins = OpenCode.logins(dir.path(), dir.path()).unwrap();
        let go = logins
            .iter()
            .find(|login| login.provider == "opencode-go")
            .unwrap();
        assert!(
            go.credential
                .as_ref()
                .is_some_and(|credential| credential.key)
        );
    }
}
