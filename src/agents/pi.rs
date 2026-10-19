//! Pi.
//!
//! Each session is a JSON Lines file,
//! `~/.pi/agent/sessions/<directory>/<time>_<session>.jsonl`, opening with a
//! `session` header that names it, its working directory, and the session it
//! was forked from (`parentSession`, that session's file). Every later line
//! is an entry following another (`parentId`), so a session is a tree: going
//! back and trying again starts a branch, and the abandoned one stays in the
//! file. Its responses were still charged, so every branch is counted.
//!
//! What follows was measured on this Mac's history, September and October
//! 2026, and read in Pi 1.0.1's session manager where none was written yet.
//!
//! **Usage.** An assistant message carries `usage`: `input` excludes cached
//! input, `output` includes `reasoning`, `cacheWrite1h` is the part of
//! `cacheWrite` kept for an hour, and `cost.total` is what Pi worked out it
//! cost. A response is its `responseId`, its model `responseModel` where the
//! provider named one. Calls a conversation doesn't show carry usage too: a
//! `usage` entry (keeping the cache warm), and the model call that wrote a
//! `compaction` or `branch_summary`.
//!
//! **Forks.** A fork's file copies every entry of its parent's, times and
//! response ids as they were, after a header of its own: an entry from before
//! the header is a copy of the parent's.
//!
//! **Providers.** A response names its provider as Pi's registry does, kept
//! as it is: its ChatGPT sign-in is `openai-codex`, beside `openai` for a key,
//! so its use of a plan is told from its use of the API.
//!
//! **Logins** are `auth.json`, an entry per provider under the name its
//! responses give it: `oauth` with its token `access`, or `api_key` with
//! `key`. OpenRouter's sign-in gives Pi a key (`sk-or-`), kept as `access`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    Agent, Conversation, Entry, Info, Kind, Link, Login, Parent, Read, Response, Role, Title,
    Tokens,
};
use crate::{Error, Result};

pub struct Pi;

static INFO: Info = Info {
    id: "pi",
    name: "Pi",
    command: "pi",
    resume: "pi --session {id}",
    mcp_add: &["mcp", "add", "turnscope", "--"],
    mcp_remove: Some(&["mcp", "remove", "turnscope"]),
    folder_var: None,
    charges: false,
    session_var: None,
    cwd_var: None,
};

#[derive(Default, Serialize, Deserialize)]
struct Cursor {
    offset: u64,
    session: Option<String>,
    titled: bool,
    /// When the session began: an entry from before is a fork's copy.
    began: Option<i64>,
    /// The provider and model of the latest response, which a summary is
    /// written with.
    model: Option<(String, String)>,
}

/// The text of a message: a string, or its text blocks.
fn texts(message: &Value) -> Vec<&str> {
    match &message["content"] {
        Value::String(text) => vec![text],
        content => content
            .as_array()
            .into_iter()
            .flatten()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect(),
    }
}

impl Agent for Pi {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn folders(&self, home: &Path) -> Vec<PathBuf> {
        vec![home.join(".pi/agent")]
    }

    fn files(&self, folder: &Path) -> Vec<PathBuf> {
        super::entries(&folder.join("sessions"))
            .flat_map(|dir| super::entries(&dir))
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "jsonl")
            })
            .collect()
    }

    fn read(&self, file: &Path, cursor: &str) -> Result<Read> {
        let mut cursor: Cursor = serde_json::from_str(cursor).unwrap_or_default();
        let mut read = Read::default();
        cursor.offset = super::lines(file, cursor.offset, |bytes| {
            let Ok(line) = serde_json::from_slice::<Value>(bytes) else {
                read.skipped += 1;
                return;
            };
            read_line(&line, &mut cursor, &mut read);
        })?;
        read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
        Ok(read)
    }

    fn transcript(&self, _native: &str, files: &[PathBuf]) -> Result<Vec<Entry>> {
        // One file; every branch, in the order written.
        let file = files
            .first()
            .ok_or_else(|| Error::NotFound("the session's file is gone".to_owned()))?;
        let mut conversation = Conversation::default();
        super::lines(file, 0, |bytes| {
            let Ok(line) = serde_json::from_slice::<Value>(bytes) else {
                return;
            };
            let entry_at = line["timestamp"].as_str().and_then(crate::time::millis);
            let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
            let message = match line["type"].as_str() {
                Some("message") => &line["message"],
                Some("compaction" | "branch_summary") => {
                    return conversation.say(Kind::Summary, entry_at, &text(&line["summary"]));
                }
                Some("custom_message") => {
                    return conversation.say(Kind::System, entry_at, &texts(&line).join("\n"));
                }
                _ => return,
            };
            let at = message["timestamp"].as_i64().or(entry_at);
            match message["role"].as_str() {
                Some("user") => conversation.say(Kind::User, at, &texts(message).join("\n")),
                Some("assistant") => {
                    for block in message["content"].as_array().into_iter().flatten() {
                        match block["type"].as_str() {
                            Some("text") => {
                                conversation.say(Kind::Assistant, at, &text(&block["text"]))
                            }
                            Some("thinking") => {
                                conversation.say(Kind::Reasoning, at, &text(&block["thinking"]))
                            }
                            Some("toolCall") => {
                                let name = block["name"].as_str().unwrap_or("tool");
                                let input = block["arguments"].to_string();
                                conversation.call(block["id"].as_str(), at, name, input);
                                if matches!(name, "edit" | "write") {
                                    let file = block["arguments"]["path"].as_str();
                                    conversation.changed(file.map(str::to_owned));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Some("toolResult") => {
                    if let Some(call) = message["toolCallId"].as_str() {
                        let failed = message["isError"].as_bool().unwrap_or(false);
                        conversation.answer(call, texts(message).join("\n"), failed);
                    }
                }
                // A command the person ran with `!`.
                Some("bashExecution") => {
                    let id = line["id"].as_str();
                    conversation.call(id, at, "bash", text(&message["command"]));
                    if let Some(id) = id {
                        let failed = message["exitCode"].as_i64().is_some_and(|exit| exit != 0)
                            || message["cancelled"] == true;
                        conversation.answer(id, text(&message["output"]), failed);
                    }
                }
                Some("compactionSummary" | "branchSummary") => {
                    conversation.say(Kind::Summary, at, &text(&message["summary"]))
                }
                Some("custom") => conversation.say(Kind::System, at, &texts(message).join("\n")),
                _ => {}
            }
        })?;
        Ok(conversation.entries)
    }

    fn logins(&self, folder: &Path, _home: &Path) -> Result<Vec<Login>> {
        let kept = super::json_file(&folder.join("auth.json"))?.unwrap_or_default();
        let logins = kept.as_object().into_iter().flatten().map(|(name, entry)| {
            let reads = match name.as_str() {
                // A ChatGPT sign-in, beside an OpenAI API key's `openai`.
                "openai-codex" => Some("openai"),
                name => crate::providers::by_id(name).map(|provider| provider.info().id),
            };
            super::kept_login(name.clone(), reads, entry, "api_key")
        });
        Ok(logins.collect())
    }

    fn mcp_command(&self, folder: &Path, _home: &Path) -> Option<String> {
        super::json_mcp_command(&folder.join("mcp.json"), "/mcpServers/turnscope/command")
    }

    fn role(&self, name: &str) -> Role {
        match name {
            "read" | "grep" | "find" | "ls" => Role::Read,
            _ => Role::Work,
        }
    }
}

fn read_line(line: &Value, cursor: &mut Cursor, read: &mut Read) {
    let at = line["timestamp"].as_str().and_then(crate::time::millis);
    if line["type"] == "session" {
        let Some(native) = line["id"].as_str() else {
            return;
        };
        let session = INFO.session(native);
        let facts = read.session(&session);
        facts.cwd = line["cwd"]
            .as_str()
            .filter(|cwd| !cwd.is_empty())
            .map(str::to_owned);
        // The parent's file, named for its session after the time.
        facts.parent = line["parentSession"]
            .as_str()
            .and_then(|file| Path::new(file).file_stem()?.to_str()?.split_once('_'))
            .map(|(_, parent)| Parent {
                id: INFO.session(parent),
                link: Link::Fork,
            });
        if let Some(at) = at {
            facts.saw(at);
        }
        (cursor.session, cursor.began) = (Some(session), at);
        return;
    }
    let Some(session) = cursor.session.clone() else {
        return;
    };
    let copy = at.zip(cursor.began).is_some_and(|(at, began)| at < began);
    if let (false, Some(at)) = (copy, at) {
        read.session(&session).saw(at);
    }
    // A copy's entry id is its parent's, under the parent's own
    // session, so it can't name a response of this one.
    let entry = line["id"]
        .as_str()
        .filter(|_| !copy)
        .map(|entry| format!("{session}:{entry}"));
    match line["type"].as_str() {
        Some("session_info") if !copy => {
            if let Some(name) = line["name"].as_str() {
                read.session(&session).title(Title::Named, name);
            }
        }
        Some("usage") => {
            if let (Some(provider), Some(model), Some(id)) =
                (line["provider"].as_str(), line["model"].as_str(), entry)
            {
                let used = (provider, model);
                charge(&line["usage"], used, id, &session, copy, at, read);
            }
        }
        Some("compaction" | "branch_summary") => {
            if let (Some((provider, model)), Some(id)) = (cursor.model.clone(), entry)
                && line["usage"].is_object()
            {
                let used = (provider.as_str(), model.as_str());
                charge(&line["usage"], used, id, &session, copy, at, read);
            }
        }
        Some("message") => {
            let message = &line["message"];
            match message["role"].as_str() {
                Some("user") if !copy => {
                    for text in texts(message) {
                        read.say(&session, text);
                        if !cursor.titled {
                            read.session(&session).title(Title::Prompt, text);
                            cursor.titled = true;
                        }
                    }
                }
                Some("assistant") => {
                    // The model the provider answered with, where it said.
                    let model = message["responseModel"]
                        .as_str()
                        .or(message["model"].as_str());
                    let (Some(provider), Some(model)) = (message["provider"].as_str(), model)
                    else {
                        return;
                    };
                    cursor.model = Some((provider.to_owned(), model.to_owned()));
                    if !copy {
                        read.say(&session, &texts(message).join("\n"));
                    }
                    // A copy without a response id couldn't be told
                    // from its parent's.
                    let id = message["responseId"].as_str().map(str::to_owned).or(entry);
                    if let Some(id) = id {
                        let at = message["timestamp"].as_i64().or(at);
                        let used = (provider, model);
                        charge(&message["usage"], used, id, &session, copy, at, read);
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

/// Record `usage` of `model` from `provider`, as the response `id` of
/// `session`.
fn charge(
    usage: &Value,
    (provider, model): (&str, &str),
    id: String,
    session: &str,
    copy: bool,
    at: Option<i64>,
    read: &mut Read,
) {
    let count = |name: &str| usage[name].as_u64();
    let (Some(input), Some(cache_read), Some(cache_write), Some(output), Some(at)) = (
        count("input"),
        count("cacheRead"),
        count("cacheWrite"),
        count("output"),
        at,
    ) else {
        read.skipped += 1;
        return;
    };
    let cache_write_1h = count("cacheWrite1h").unwrap_or_default();
    let tokens = Tokens {
        input,
        cache_read,
        cache_write_5m: cache_write.saturating_sub(cache_write_1h),
        cache_write_1h,
        output,
        reasoning: count("reasoning").unwrap_or_default(),
    };
    // A response aborted with nothing written records no usage.
    if tokens == Tokens::default() {
        return;
    }
    read.respond(Response {
        id,
        session: session.to_owned(),
        copy,
        at,
        provider: provider.to_owned(),
        model: model.to_owned(),
        prompt: input + cache_read + cache_write,
        tokens,
        web_searches: 0,
        priority: false,
        cost: usage["cost"]["total"].as_f64(),
    });
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn session(dir: &Path, name: &str, lines: &[Value]) -> PathBuf {
        crate::agents::jsonl(dir.join(name), lines)
    }

    fn usage(input: u64, cost: f64) -> Value {
        json!({"input": input, "cacheRead": 400, "cacheWrite": 30, "cacheWrite1h": 10, "output": 20, "reasoning": 8, "cost": {"total": cost}})
    }

    #[test]
    fn a_kept_sign_in_or_key_is_read_as_a_login() {
        let dir = tempfile::tempdir().unwrap();
        let kept = json!({
            "openrouter": {"type": "oauth", "access": "sk-or-made-up", "expires": 9},
            "openai-codex": {"type": "oauth", "access": "made-up-token", "expires": 5},
            "anthropic": {"type": "api_key", "key": "sk-ant-made-up"},
            "xai": {"type": "api_key", "key": ""},
            "made-up-provider": {"type": "api_key", "key": "made-up"},
        });
        std::fs::write(dir.path().join("auth.json"), kept.to_string()).unwrap();
        let mut read: Vec<_> = Pi
            .logins(dir.path(), dir.path())
            .unwrap()
            .into_iter()
            .map(|login| {
                let credential = login.credential.map(|credential| {
                    let secret = credential.secret.expose().to_owned();
                    (
                        credential.provider,
                        credential.key,
                        credential.expires,
                        secret,
                    )
                });
                (login.provider, credential)
            })
            .collect();
        read.sort();
        let given = |provider: &'static str, key, expires, secret: &str| {
            Some((provider, key, expires, secret.to_owned()))
        };
        assert_eq!(
            read,
            [
                (
                    "anthropic".to_owned(),
                    given("anthropic", true, None, "sk-ant-made-up")
                ),
                ("made-up-provider".to_owned(), None),
                (
                    "openai-codex".to_owned(),
                    given("openai", false, Some(5), "made-up-token")
                ),
                (
                    "openrouter".to_owned(),
                    given("openrouter", true, Some(9), "sk-or-made-up")
                ),
                ("xai".to_owned(), None),
            ]
        );
    }

    #[test]
    fn a_sessions_responses_are_read_with_their_hour_long_cache_writes() {
        let dir = tempfile::tempdir().unwrap();
        let file = session(
            dir.path(),
            "2026-09-30T09-00-00-000Z_s1.jsonl",
            &[
                json!({"type": "session", "id": "s1", "cwd": "/work/cli", "timestamp": "2026-09-30T09:00:00Z"}),
                json!({"type": "message", "id": "e1", "timestamp": "2026-09-30T09:00:01Z", "message": {"role": "user", "content": "Add a --verbose flag."}}),
                json!({"type": "message", "id": "e2", "timestamp": "2026-09-30T09:00:05Z", "message": {"role": "assistant", "provider": "openai-codex", "model": "gpt-5.5",
                "responseModel": "gpt-5.5-2026-09-01", "responseId": "resp_1", "content": [{"type": "text", "text": "Added."}], "usage": usage(50, 0.01)}}),
                // Keeping the cache warm, and compacting, are calls too.
                json!({"type": "usage", "id": "e3", "timestamp": "2026-09-30T09:10:00Z", "kind": "cache_warm", "provider": "openai-codex", "model": "gpt-5.5", "usage": usage(5, 0.001)}),
                json!({"type": "compaction", "id": "e4", "timestamp": "2026-09-30T09:20:00Z", "summary": "Added the flag.", "usage": usage(900, 0.02)}),
                json!({"type": "session_info", "id": "e5", "timestamp": "2026-09-30T09:21:00Z", "name": "Verbose flag"}),
            ],
        );
        let read = Pi.read(&file, "").unwrap();
        let response = &read.responses["resp_1"];
        assert_eq!(
            response.tokens,
            Tokens {
                input: 50,
                cache_read: 400,
                cache_write_5m: 20,
                cache_write_1h: 10,
                output: 20,
                reasoning: 8
            }
        );
        assert_eq!(
            (
                response.provider.as_str(),
                response.model.as_str(),
                response.cost
            ),
            ("openai-codex", "gpt-5.5-2026-09-01", Some(0.01))
        );
        assert_eq!(read.responses["pi:s1:e3"].tokens.input, 5);
        assert_eq!(read.responses["pi:s1:e4"].model, "gpt-5.5-2026-09-01");
        assert_eq!(
            read.sessions["pi:s1"].title.as_ref().unwrap().1,
            "Verbose flag"
        );
        let entries = Pi.transcript("s1", &[file]).unwrap();
        let kinds: Vec<Kind> = entries.iter().map(|entry| entry.kind).collect();
        assert_eq!(kinds, [Kind::User, Kind::Assistant, Kind::Summary]);
    }

    #[test]
    fn a_growing_file_read_on_from_its_cursor_reads_as_it_does_whole() {
        let dir = tempfile::tempdir().unwrap();
        let reply = |entry: &str, at: &str, response: &str, text: &str| {
            json!({"type": "message", "id": entry, "timestamp": at, "message": {"role": "assistant", "provider": "openai-codex",
                "model": "gpt-5.5", "responseId": response, "content": [{"type": "text", "text": text}], "usage": usage(50, 0.01)}})
        };
        let lines = [
            json!({"type": "session", "id": "s1", "cwd": "/work/cli", "timestamp": "2026-09-30T09:00:00Z"}),
            json!({"type": "message", "id": "e1", "timestamp": "2026-09-30T09:00:01Z", "message": {"role": "user", "content": "Add a --verbose flag."}}),
            reply("e2", "2026-09-30T09:00:05Z", "resp_1", "Added."),
            json!({"type": "message", "id": "e3", "timestamp": "2026-09-30T09:05:00Z", "message": {"role": "user", "content": "Now document it."}}),
            reply("e4", "2026-09-30T09:05:09Z", "resp_2", "Documented."),
        ];
        let name = "2026-09-30T09-00-00-000Z_s1.jsonl";
        let file = session(dir.path(), name, &lines[..3]);
        let first = Pi.read(&file, "").unwrap();
        session(dir.path(), name, &lines);
        let later = Pi.read(&file, &first.cursor).unwrap();
        let whole = Pi.read(&file, "").unwrap();
        // The later part is the session's, untitled by its request.
        assert_eq!(later.responses.len(), 1);
        assert_eq!(later.responses["resp_2"], whole.responses["resp_2"]);
        assert!(later.sessions["pi:s1"].title.is_none());
        assert_eq!([first.said, later.said].concat(), whole.said);
    }

    #[test]
    fn a_forks_copies_of_its_parents_entries_are_the_parents() {
        let dir = tempfile::tempdir().unwrap();
        let copied = json!({"type": "message", "id": "e2", "timestamp": "2026-09-30T09:00:05Z", "message": {"role": "assistant",
            "provider": "xai", "model": "grok-4.6", "responseId": "resp_1", "content": [], "usage": usage(50, 0.01)}});
        let file = session(
            dir.path(),
            "2026-09-30T10-00-00-000Z_s2.jsonl",
            &[
                json!({"type": "session", "id": "s2", "cwd": "/work/cli", "timestamp": "2026-09-30T10:00:00Z",
                    "parentSession": "/Users/me/.pi/agent/sessions/--work-cli--/2026-09-30T09-00-00-000Z_s1.jsonl"}),
                copied,
                json!({"type": "usage", "id": "e3", "timestamp": "2026-09-30T09:00:06Z", "provider": "xai",
                    "model": "grok-4.6", "usage": usage(20, 0.01)}),
                json!({"type": "session_info", "id": "e4", "timestamp": "2026-09-30T09:00:07Z", "name": "The parent"}),
            ],
        );
        let read = Pi.read(&file, "").unwrap();
        // The parent's own usage and name, copied, aren't the fork's.
        assert_eq!(read.responses.len(), 1);
        assert!(read.sessions["pi:s2"].title.is_none());
        let copy = &read.responses["resp_1"];
        assert_eq!((copy.session.as_str(), copy.copy), ("pi:s2", true));
        let fork = &read.sessions["pi:s2"];
        assert!(
            fork.parent
                .as_ref()
                .is_some_and(|parent| parent.id == "pi:s1" && parent.link == Link::Fork)
        );
        assert_eq!(fork.started, crate::time::millis("2026-09-30T10:00:00Z"));
    }
}
