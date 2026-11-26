//! Pi.
//!
//! A session is a JSON Lines file,
//! `~/.pi/agent/sessions/<directory>/<time>_<session>.jsonl`, opening with a
//! `session` header naming it, its working directory and the session it was
//! forked from (`parentSession`). Every later line is an entry following
//! another (`parentId`), so a session is a tree whose abandoned branches stay
//! in the file; they were charged, so every branch is counted.
//!
//! Measured on this Mac's history, September and October 2026, and read in
//! Pi 1.0.1's session manager where none was written yet:
//!
//! - **Usage.** An assistant message's `usage`: `input` leaves out the cache,
//!   `output` includes `reasoning`, `cacheWrite1h` is the part of
//!   `cacheWrite` kept an hour, and `cost.total` is Pi's estimate. A response
//!   is its `responseId`. A `usage` entry, keeping the cache warm, and the
//!   call that wrote a `compaction` or `branch_summary` carry usage too.
//! - **Forks.** A fork's file copies every entry of its parent's, times and
//!   response ids as they were, after a header of its own. The copy's
//!   responses are the parent's, and nothing else in it is the fork's.
//! - **Providers** are named as Pi's registry names them: its ChatGPT sign-in
//!   is `openai-codex`, beside `openai` for a key, which tells a plan's use
//!   from the API's.
//! - **Logins** are `auth.json`, an entry per provider under the name its
//!   responses give it.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use super::{Agent, Info, Login, Read, Response, Title, Tokens};
use crate::Result;

pub struct Pi;

static INFO: Info = Info {
    id: "pi",
    name: "Pi",
    command: "pi",
    app: None,
    mcp_add: &["mcp", "add", "turnscope", "--"],
    mcp_remove: Some(&["mcp", "remove", "turnscope"]),
    folder_var: None,
    charges: false,
    session_var: None,
    login_file: Some("auth.json"),
};

#[derive(Default, Serialize, Deserialize)]
struct Cursor {
    offset: u64,
    session: Option<String>,
    titled: bool,
    began: Option<i64>,
    /// The provider and model of the latest response, which a summary is
    /// written with.
    model: Option<(String, String)>,
}

#[derive(Deserialize)]
struct Line<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "super::lenient")]
    timestamp: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "super::lenient")]
    cwd: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "super::lenient")]
    name: Option<Cow<'a, str>>,
    #[serde(borrow)]
    provider: Option<Cow<'a, str>>,
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
    #[serde(borrow)]
    usage: Option<&'a RawValue>,
    #[serde(borrow)]
    message: Option<Message<'a>>,
}

#[derive(Deserialize)]
struct Message<'a> {
    #[serde(borrow)]
    role: Option<Cow<'a, str>>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
    #[serde(borrow)]
    provider: Option<Cow<'a, str>>,
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
    #[serde(
        rename = "responseModel",
        borrow,
        default,
        deserialize_with = "super::lenient"
    )]
    response_model: Option<Cow<'a, str>>,
    #[serde(
        rename = "responseId",
        borrow,
        default,
        deserialize_with = "super::lenient"
    )]
    response_id: Option<Cow<'a, str>>,
    /// In Unix milliseconds, where it reads as one; else the line's time.
    #[serde(borrow)]
    timestamp: Option<&'a RawValue>,
    #[serde(borrow)]
    usage: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct Usage {
    input: u64,
    #[serde(rename = "cacheRead")]
    cache_read: u64,
    #[serde(rename = "cacheWrite")]
    cache_write: u64,
    #[serde(rename = "cacheWrite1h", default, deserialize_with = "super::lenient")]
    cache_write_1h: Option<u64>,
    output: u64,
    #[serde(default, deserialize_with = "super::lenient")]
    reasoning: Option<u64>,
    #[serde(default, deserialize_with = "super::lenient")]
    cost: Option<Cost>,
}

#[derive(Deserialize)]
struct Cost {
    total: Option<f64>,
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
            let Ok(line) = serde_json::from_slice::<Line<'_>>(bytes) else {
                read.skipped += 1;
                return;
            };
            read_line(&line, &mut cursor, &mut read);
        })?;
        read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
        Ok(read)
    }

    fn logins(&self, folder: &Path, _home: &Path) -> Result<Vec<Login>> {
        let kept = super::json_file(&folder.join("auth.json"))?;
        let logins = kept.as_object().into_iter().flatten().map(|(name, entry)| {
            let reads = match name.as_str() {
                "openai-codex" => "openai",
                name => name,
            };
            super::kept_login(name, reads, entry, "api_key")
        });
        Ok(logins.collect())
    }

    fn mcp_command(&self, folder: &Path, _home: &Path) -> Option<String> {
        super::json_mcp_command(&folder.join("mcp.json"), "/mcpServers/turnscope/command")
    }
}

fn read_line(line: &Line<'_>, cursor: &mut Cursor, read: &mut Read) {
    let at = line.timestamp.as_deref().and_then(crate::time::millis);
    if line.kind == "session" {
        let Some(native) = line.id.as_deref() else {
            return;
        };
        let session = INFO.session(native);
        read.session(&session);
        read.cwd(&session, line.cwd.as_deref());
        (cursor.session, cursor.began) = (Some(session), at);
        return;
    }
    let Some(session) = cursor.session.as_deref() else {
        return;
    };
    // An entry from before the session began is a fork's copy of its
    // parent's.
    let copy = at.zip(cursor.began).is_some_and(|(at, began)| at < began);
    // A copy's entry id is its parent's, under the parent's own
    // session, so it can't name a response of this one.
    let entry = || {
        (!copy)
            .then(|| line.id.as_deref().map(|id| format!("{session}:{id}")))
            .flatten()
    };
    let (usage, provider, model, id, at) = match &*line.kind {
        "session_info" if !copy => {
            if let Some(name) = line.name.as_deref() {
                read.session(session).title(Title::Named, name);
            }
            return;
        }
        "usage" => {
            let (Some(provider), Some(model), Some(id)) =
                (line.provider.as_deref(), line.model.as_deref(), entry())
            else {
                return;
            };
            (line.usage, provider.to_owned(), model.to_owned(), id, at)
        }
        "compaction" | "branch_summary" => {
            let Some(usage) = line.usage.filter(|raw| raw.get().starts_with('{')) else {
                return;
            };
            let (Some((provider, model)), Some(id)) = (cursor.model.as_ref(), entry()) else {
                return;
            };
            (Some(usage), provider.clone(), model.clone(), id, at)
        }
        "message" => {
            let Some(message) = &line.message else {
                return;
            };
            match message.role.as_deref() {
                // The first thing the person asked titles the session: its
                // first non-blank text part.
                Some("user") if !copy && !cursor.titled => {
                    let asked = message
                        .content
                        .map(super::texts)
                        .into_iter()
                        .flatten()
                        .find(|text| !text.trim().is_empty());
                    if let Some(asked) = asked {
                        read.session(session).title(Title::Prompt, &asked);
                        cursor.titled = true;
                    }
                    return;
                }
                Some("assistant") => {
                    // The model the provider answered with, where it said.
                    let model = message
                        .response_model
                        .as_deref()
                        .filter(|model| !model.is_empty())
                        .or(message.model.as_deref())
                        .filter(|model| !model.is_empty());
                    let (Some(provider), Some(model)) = (message.provider.as_deref(), model) else {
                        return;
                    };
                    if cursor
                        .model
                        .as_ref()
                        .is_none_or(|(p, m)| p != provider || m != model)
                    {
                        cursor.model = Some((provider.to_owned(), model.to_owned()));
                    }
                    // A copy without a response id couldn't be told
                    // from its parent's.
                    let Some(id) = message
                        .response_id
                        .as_deref()
                        .filter(|id| !id.is_empty())
                        .map(str::to_owned)
                        .or_else(entry)
                    else {
                        return;
                    };
                    let at = message
                        .timestamp
                        .and_then(|raw| raw.get().parse().ok())
                        .or(at);
                    (message.usage, provider.to_owned(), model.to_owned(), id, at)
                }
                _ => return,
            }
        }
        _ => return,
    };
    let (Some(usage), Some(at)) = (super::parsed::<Usage>(usage), at) else {
        read.skipped += 1;
        return;
    };
    let cache_write_1h = usage.cache_write_1h.unwrap_or_default();
    let tokens = Tokens {
        input: usage.input,
        cache_read: usage.cache_read,
        cache_write_5m: usage.cache_write.saturating_sub(cache_write_1h),
        cache_write_1h,
        output: usage.output,
        reasoning: usage.reasoning.unwrap_or_default(),
    };
    read.respond(Response {
        id,
        session: session.to_owned(),
        copy,
        at,
        provider,
        model,
        prompt: tokens.prompt(),
        tokens,
        web_searches: 0,
        priority: false,
        cost: usage.cost.and_then(|cost| cost.total),
    });
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

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
        let logins = Pi.logins(dir.path(), dir.path()).unwrap();
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
                ("made-up-provider", None),
                (
                    "openai-codex",
                    Some(("openai", false, Some(5), "made-up-token"))
                ),
                (
                    "openrouter",
                    Some(("openrouter", true, Some(9), "sk-or-made-up"))
                ),
                ("xai", None),
            ]
        );
        // OpenRouter's sign-in gives Pi a key, so it's the API-key account.
        let openrouter = logins.iter().find(|login| login.provider == "openrouter");
        let credential = openrouter.and_then(|login| login.credential.as_ref());
        let account = crate::providers::by_id("openrouter")
            .and_then(|provider| provider.account(credential?))
            .unwrap();
        assert_eq!(
            (account.id.as_str(), account.kind),
            ("openrouter:api", crate::providers::Kind::ApiKey)
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
                "responseModel": "gpt-5.5-2026-09-01", "responseId": "resp_1", "usage": usage(50, 0.01)}}),
                // Keeping the cache warm, and compacting, are calls too.
                json!({"type": "usage", "id": "e3", "timestamp": "2026-09-30T09:10:00Z", "kind": "cache_warm", "provider": "openai-codex", "model": "gpt-5.5", "usage": usage(5, 0.001)}),
                json!({"type": "compaction", "id": "e4", "timestamp": "2026-09-30T09:20:00Z", "usage": usage(900, 0.02)}),
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
    }

    #[test]
    fn a_growing_file_read_on_from_its_cursor_reads_as_it_does_whole() {
        let dir = tempfile::tempdir().unwrap();
        let reply = |entry: &str, at: &str, response: &str| {
            json!({"type": "message", "id": entry, "timestamp": at, "message": {"role": "assistant", "provider": "openai-codex",
                "model": "gpt-5.5", "responseId": response, "usage": usage(50, 0.01)}})
        };
        let lines = [
            json!({"type": "session", "id": "s1", "cwd": "/work/cli", "timestamp": "2026-09-30T09:00:00Z"}),
            json!({"type": "message", "id": "e0", "timestamp": "2026-09-30T09:00:00Z", "message": {"role": "user", "content": [{"type": "text", "text": "  \n "}]}}),
            json!({"type": "message", "id": "e1", "timestamp": "2026-09-30T09:00:01Z", "message": {"role": "user", "content": "Add a --verbose flag."}}),
            reply("e2", "2026-09-30T09:00:05Z", "resp_1"),
            json!({"type": "message", "id": "e3", "timestamp": "2026-09-30T09:05:00Z", "message": {"role": "user", "content": "Now document it."}}),
            reply("e4", "2026-09-30T09:05:09Z", "resp_2"),
        ];
        let name = "2026-09-30T09-00-00-000Z_s1.jsonl";
        let file = session(dir.path(), name, &lines[..4]);
        let first = Pi.read(&file, "").unwrap();
        session(dir.path(), name, &lines);
        let later = Pi.read(&file, &first.cursor).unwrap();
        let whole = Pi.read(&file, "").unwrap();
        // The later part is the session's, untitled by its request.
        assert_eq!(later.responses.len(), 1);
        assert_eq!(later.responses["resp_2"], whole.responses["resp_2"]);
        assert!(later.sessions["pi:s1"].title.is_none());
        assert_eq!(
            first.sessions["pi:s1"].title.as_ref().unwrap().1,
            "Add a --verbose flag."
        );
    }

    #[test]
    fn a_forks_copies_of_its_parents_entries_are_the_parents() {
        let dir = tempfile::tempdir().unwrap();
        let copied = json!({"type": "message", "id": "e2", "timestamp": "2026-09-30T09:00:05Z", "message": {"role": "assistant",
            "provider": "xai", "model": "grok-4.6", "responseId": "resp_1", "usage": usage(50, 0.01)}});
        let file = session(
            dir.path(),
            "2026-09-30T10-00-00-000Z_s2.jsonl",
            &[
                json!({"type": "session", "id": "s2", "cwd": "/work/cli", "timestamp": "2026-09-30T10:00:00Z",
                    "parentSession": "/Users/me/.pi/agent/sessions/--work-cli--/2026-09-30T09-00-00-000Z_s1.jsonl"}),
                json!({"type": "message", "id": "e1", "timestamp": "2026-09-30T09:00:01Z", "message": {"role": "user", "content": "Add a --verbose flag."}}),
                copied,
                json!({"type": "usage", "id": "e3", "timestamp": "2026-09-30T09:00:06Z", "provider": "xai",
                    "model": "grok-4.6", "usage": usage(20, 0.01)}),
                json!({"type": "session_info", "id": "e4", "timestamp": "2026-09-30T09:00:07Z", "name": "The parent"}),
                json!({"type": "message", "id": "e5", "timestamp": "2026-09-30T10:00:01Z", "message": {"role": "user", "content": "Make it --quiet instead."}}),
            ],
        );
        let read = Pi.read(&file, "").unwrap();
        // The parent's own usage, name and request, copied, aren't the fork's.
        assert_eq!(read.responses.len(), 1);
        assert_eq!(
            read.sessions["pi:s2"].title.as_ref().unwrap().1,
            "Make it --quiet instead."
        );
        let copy = &read.responses["resp_1"];
        assert_eq!((copy.session.as_str(), copy.copy), ("pi:s2", true));
        // A fork is a session of its own.
        assert!(read.sessions["pi:s2"].parent.is_none());
    }

    #[test]
    fn an_odd_field_beside_the_usage_doesnt_lose_the_response() {
        let dir = tempfile::tempdir().unwrap();
        let mut odd = usage(50, 0.0);
        odd["cost"] = json!("n/a");
        odd["reasoning"] = json!("5");
        let file = session(
            dir.path(),
            "2026-09-30T09-00-00-000Z_s1.jsonl",
            &[
                json!({"type": "session", "id": "s1", "timestamp": "2026-09-30T09:00:00Z"}),
                json!({"type": "message", "id": "e1", "timestamp": "2026-09-30T09:00:05Z", "message": {"role": "assistant",
                    "provider": "xai", "model": "grok-4.6", "responseModel": 4.6, "responseId": "resp_1", "timestamp": "soon", "usage": odd}}),
            ],
        );
        let response = &Pi.read(&file, "").unwrap().responses["resp_1"];
        let at = crate::time::millis("2026-09-30T09:00:05Z").unwrap();
        assert_eq!((response.at, response.cost), (at, None));
    }
}
