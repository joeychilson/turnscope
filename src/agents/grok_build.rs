//! Grok Build.
//!
//! Each session is a directory, `~/.grok/sessions/<encoded directory>/<session>/`:
//! `summary.json`, rewritten as the session goes on, with its directory,
//! times, branch, title, and the session it was forked from; `updates.jsonl`,
//! its stream of updates, one JSON-RPC notification a line, and the only
//! record of usage: a `turn_completed` per prompt with the turn's usage per
//! model and what xAI charged; and `chat_history.jsonl`, the conversation.
//!
//! What follows was measured on this Mac's history, September and October 2026.
//!
//! **Usage.** A turn's `inputTokens` includes what was read from and written
//! to the cache, and `outputTokens` reasoning. `costUsdTicks` is the charge,
//! ten billion ticks to the dollar: the bill, not an estimate; a turn without
//! one is marked `usageIsIncomplete`, its cost unknown. A turn is many model
//! calls counted together, so the prompt that sets its price tier is their
//! average. A `prompt_id` is unique across sessions.
//!
//! **Subagents** are sessions of their own, spawned by a parent's
//! `subagent_spawned` update, each with turns of its own. A parent's turn also
//! counts the subagents that finished in it (`subagent_finished`), under the
//! same models: in every one of 13 cases, the parent's own model calls and
//! its finished subagents' added up to its turn's. So a parent's turn is
//! counted less what those subagents' turns counted.
//!
//! **What it sends as the person's.** The person's words are in
//! `<user_query>`, beside context Grok Build adds (its rules, the git status,
//! the folder); a line it adds itself, a reminder or word that a task
//! finished, carries `synthetic_reason`. Every conversation opens with the
//! system prompt, which isn't the session's to show.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    Agent, Conversation, Credential, Entry, Info, Kind, Link, Login, Parent, Read, Response, Role,
    Secret, Title, Tokens,
};
use crate::{Error, Result};

pub struct GrokBuild;

static INFO: Info = Info {
    id: "grok-build",
    name: "Grok Build",
    command: "grok",
    resume: "grok --resume {id}",
    mcp_add: &["mcp", "add", "--scope", "user", "turnscope"],
    mcp_remove: Some(&["mcp", "remove", "--scope", "user", "turnscope"]),
    folder_var: Some("GROK_HOME"),
    charges: true,
    session_var: None,
    cwd_var: None,
};

/// Where `auth.json` keeps the SuperGrok sign-in: the issuer and its client.
const SIGN_IN: &str = "https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828";

/// What Grok Build adds to a message sent as the person's.
const INJECTED: &[&str] = &[
    "agent",
    "dir",
    "folder",
    "git_status",
    "image_files",
    "rules",
    "user_info",
    "user_rule",
];

#[derive(Default, Serialize, Deserialize)]
struct Cursor {
    offset: u64,
    titled: bool,
    /// The subagents that finished since the last turn ended, which the next
    /// turn to end counts too.
    finished: Vec<String>,
}

/// What the person typed: their query, or the text without what Grok Build
/// added.
fn typed(text: &str) -> String {
    match super::inner(text, "user_query") {
        Some(query) => query.trim().to_owned(),
        None => super::without_tags(text, INJECTED),
    }
}

/// A line's text: a string, or its text parts.
fn texts(content: &Value) -> Vec<&str> {
    match content {
        Value::String(text) => vec![text],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect(),
        _ => Vec::new(),
    }
}

fn id(native: &str) -> String {
    format!("grok-build:{native}")
}

impl Agent for GrokBuild {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn folders(&self, home: &Path) -> Vec<PathBuf> {
        super::own_and_found(home, ".grok", ".grok-", "auth.json")
    }

    fn files(&self, folder: &Path) -> Vec<PathBuf> {
        let dirs = std::fs::read_dir(folder.join("sessions"))
            .into_iter()
            .flatten()
            .flatten();
        dirs.flat_map(|dir| {
            std::fs::read_dir(dir.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .flat_map(|session| {
            ["summary.json", "updates.jsonl", "chat_history.jsonl"]
                .map(|name| session.path().join(name))
        })
        .collect()
    }

    fn read(&self, file: &Path, cursor: &str) -> Result<Read> {
        let native = file
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                Error::Failed(format!("{} isn't a Grok Build session's", file.display()))
            })?;
        let session = id(native);
        let mut cursor: Cursor = serde_json::from_str(cursor).unwrap_or_default();
        let mut read = Read::default();
        match file.file_name().and_then(|name| name.to_str()) {
            // Rewritten whole.
            Some("summary.json") => {
                let summary = super::json_file(file)?.unwrap_or_default();
                // A session opened and left before anything was said.
                if summary["num_messages"] == 0 {
                    return Ok(read);
                }
                let facts = read.session(&session);
                for at in ["created_at", "updated_at", "last_active_at"] {
                    if let Some(at) = summary[at].as_str().and_then(crate::time::millis) {
                        facts.saw(at);
                    }
                }
                let text = |field: &Value| {
                    field
                        .as_str()
                        .filter(|text| !text.is_empty())
                        .map(str::to_owned)
                };
                facts.cwd = text(&summary["info"]["cwd"]);
                facts.branch = text(&summary["head_branch"]);
                if let Some(title) = text(&summary["generated_title"]) {
                    let named = summary["title_is_manual"] == true;
                    facts.title(
                        if named {
                            Title::Named
                        } else {
                            Title::Generated
                        },
                        &title,
                    );
                }
                if let Some(parent) = text(&summary["parent_session_id"]) {
                    facts.parent = Some(Parent {
                        id: id(&parent),
                        link: Link::Fork,
                    });
                }
            }
            Some("chat_history.jsonl") => {
                // It holds the session's conversation, which reading the
                // session finds through it.
                read.session(&session);
                cursor.offset = super::lines(file, cursor.offset, |bytes| {
                    let Ok(line) = serde_json::from_slice::<Value>(bytes) else {
                        read.skipped += 1;
                        return;
                    };
                    match line["type"].as_str() {
                        Some("user") if line["synthetic_reason"].is_null() => {
                            for text in texts(&line["content"]) {
                                read.say(&session, &typed(text));
                            }
                        }
                        Some("assistant") => {
                            read.say(&session, &texts(&line["content"]).join("\n"))
                        }
                        _ => {}
                    }
                })?;
            }
            _ => {
                let sessions = file.ancestors().nth(3).map(Path::to_path_buf);
                cursor.offset = super::lines(file, cursor.offset, |bytes| {
                    let Ok(line) = serde_json::from_slice::<Value>(bytes) else {
                        read.skipped += 1;
                        return;
                    };
                    let at = line["timestamp"].as_i64().map(|seconds| seconds * 1000);
                    if let Some(at) = at {
                        read.session(&session).saw(at);
                    }
                    let update = &line["params"]["update"];
                    match update["sessionUpdate"].as_str() {
                        Some("turn_completed") => {
                            let finished = std::mem::take(&mut cursor.finished);
                            let theirs = match &sessions {
                                Some(sessions) => subagents(sessions, &finished),
                                None => BTreeMap::new(),
                            };
                            turn(update, at, &session, &theirs, &mut read);
                        }
                        Some("subagent_spawned") => {
                            if let Some(child) = update["child_session_id"].as_str() {
                                read.session(&id(child)).parent = Some(Parent {
                                    id: session.clone(),
                                    link: Link::Subagent,
                                });
                            }
                        }
                        Some("subagent_finished") => {
                            if let Some(child) = update["child_session_id"].as_str() {
                                cursor.finished.push(child.to_owned());
                            }
                        }
                        Some("user_message_chunk") if !cursor.titled => {
                            let text =
                                typed(update["content"]["text"].as_str().unwrap_or_default());
                            if !text.is_empty() {
                                read.session(&session).title(Title::Prompt, &text);
                                cursor.titled = true;
                            }
                        }
                        _ => {}
                    }
                })?;
            }
        }
        read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
        Ok(read)
    }

    fn transcript(&self, _native: &str, files: &[PathBuf]) -> Result<Vec<Entry>> {
        let file = files
            .iter()
            .find(|file| file.ends_with("chat_history.jsonl"))
            .ok_or_else(|| Error::NotFound("the session's conversation is gone".to_owned()))?;
        let mut conversation = Conversation::default();
        super::lines(file, 0, |bytes| {
            let Ok(line) = serde_json::from_slice::<Value>(bytes) else {
                return;
            };
            let content = texts(&line["content"]).join("\n");
            match line["type"].as_str() {
                Some("user") if line["synthetic_reason"].is_null() => {
                    let said = typed(&content);
                    if said.is_empty() {
                        conversation.say(Kind::System, None, &content);
                    } else {
                        conversation.say(Kind::User, None, &said);
                    }
                }
                // What Grok Build sent as the person's.
                Some("user") => conversation.say(Kind::System, None, &content),
                Some("reasoning") => {
                    conversation.say(Kind::Reasoning, None, &texts(&line["summary"]).join("\n"))
                }
                Some("assistant") => {
                    conversation.say(Kind::Assistant, None, &content);
                    for call in line["tool_calls"].as_array().into_iter().flatten() {
                        let input = match &call["arguments"] {
                            Value::String(text) => text.clone(),
                            other => other.to_string(),
                        };
                        let name = call["name"].as_str().unwrap_or("tool");
                        let file = matches!(name, "search_replace" | "write")
                            .then(|| serde_json::from_str::<Value>(&input).ok())
                            .flatten()
                            .and_then(|input| input["file_path"].as_str().map(str::to_owned));
                        conversation.call(call["id"].as_str(), None, name, input);
                        conversation.changed(file);
                    }
                }
                // Grok Build marks no failure; a command's result opens with
                // its exit code.
                Some("tool_result") => {
                    if let Some(call) = line["tool_call_id"].as_str() {
                        let exit = content.strip_prefix("exit: ").and_then(|rest| {
                            rest.split(|c: char| !c.is_ascii_digit())
                                .next()?
                                .parse::<i64>()
                                .ok()
                        });
                        conversation.answer(
                            call,
                            content.clone(),
                            exit.is_some_and(|exit| exit != 0),
                        );
                    }
                }
                _ => {}
            }
        })?;
        Ok(conversation.entries)
    }

    fn logins(&self, folder: &Path, _home: &Path) -> Result<Vec<Login>> {
        let kept = super::json_file(&folder.join("auth.json"))?.unwrap_or_default();
        let entry = &kept[SIGN_IN];
        let text = |field: &str| entry[field].as_str().filter(|text| !text.is_empty());
        Ok(vec![Login {
            provider: "xai".to_owned(),
            credential: text("key").map(|token| Credential {
                provider: "xai",
                secret: Secret::new(token),
                key: false,
                expires: entry["expires_at"].as_str().and_then(crate::time::millis),
                identity: text("user_id")
                    .map(|id| (id.to_owned(), text("email").map(str::to_owned))),
                plan: None,
            }),
        }])
    }

    fn mcp_command(&self, folder: &Path, _home: &Path) -> Option<String> {
        super::toml_mcp_command(&folder.join("config.toml"))
    }

    fn role(&self, name: &str) -> Role {
        match name {
            "read_file" | "grep" | "list_dir" => Role::Read,
            "spawn_subagent" => Role::Subagent,
            _ => Role::Work,
        }
    }
}

/// One model's usage in a turn, as `modelUsage` gives it.
#[derive(Clone, Copy)]
struct Used {
    input: u64,
    cache_read: u64,
    cache_write: u64,
    output: u64,
    reasoning: u64,
    calls: u64,
    ticks: Option<u64>,
}

impl Used {
    fn of(used: &Value) -> Option<Used> {
        let count = |name: &str| used[name].as_u64();
        Some(Used {
            input: count("inputTokens")?,
            cache_read: count("cachedReadTokens")?,
            cache_write: count("cacheCreationTokens")?,
            output: count("outputTokens")?,
            reasoning: count("reasoningTokens")?,
            calls: count("modelCalls")?,
            ticks: count("costUsdTicks"),
        })
    }

    fn plus(self, more: Used) -> Used {
        Used {
            input: self.input + more.input,
            cache_read: self.cache_read + more.cache_read,
            cache_write: self.cache_write + more.cache_write,
            output: self.output + more.output,
            reasoning: self.reasoning + more.reasoning,
            calls: self.calls + more.calls,
            // A charge not known in one makes the sum unknown.
            ticks: self.ticks.zip(more.ticks).map(|(own, more)| own + more),
        }
    }

    fn less(self, theirs: Used) -> Used {
        Used {
            input: self.input.saturating_sub(theirs.input),
            cache_read: self.cache_read.saturating_sub(theirs.cache_read),
            cache_write: self.cache_write.saturating_sub(theirs.cache_write),
            output: self.output.saturating_sub(theirs.output),
            reasoning: self.reasoning.saturating_sub(theirs.reasoning),
            calls: self.calls.saturating_sub(theirs.calls),
            ticks: self
                .ticks
                .zip(theirs.ticks)
                .map(|(own, theirs)| own.saturating_sub(theirs)),
        }
    }
}

/// What the turns of the subagents `finished` used, by model, from their own
/// updates in `sessions`.
fn subagents(sessions: &Path, finished: &[String]) -> BTreeMap<String, Used> {
    let mut used: BTreeMap<String, Used> = BTreeMap::new();
    let dirs = std::fs::read_dir(sessions).into_iter().flatten().flatten();
    let dirs: Vec<PathBuf> = dirs.map(|dir| dir.path()).collect();
    for child in finished {
        let Some(updates) = dirs
            .iter()
            .map(|dir| dir.join(child).join("updates.jsonl"))
            .find(|updates| updates.is_file())
        else {
            continue;
        };
        let _ = super::lines(&updates, 0, |bytes| {
            let Ok(line) = serde_json::from_slice::<Value>(bytes) else {
                return;
            };
            let update = &line["params"]["update"];
            if update["sessionUpdate"] != "turn_completed" {
                return;
            }
            for (model, each) in update["usage"]["modelUsage"]
                .as_object()
                .into_iter()
                .flatten()
            {
                if let Some(each) = Used::of(each) {
                    used.entry(model.clone())
                        .and_modify(|total| *total = total.plus(each))
                        .or_insert(each);
                }
            }
        });
    }
    used
}

/// Record a finished turn's usage, a response per model, less what
/// `subagents` that finished in it used.
fn turn(
    update: &Value,
    at: Option<i64>,
    session: &str,
    subagents: &BTreeMap<String, Used>,
    read: &mut Read,
) {
    let (Some(models), Some(prompt), Some(at)) = (
        update["usage"]["modelUsage"].as_object(),
        update["prompt_id"].as_str(),
        at,
    ) else {
        return;
    };
    for (model, used) in models {
        let Some(used) = Used::of(used) else {
            read.skipped += 1;
            continue;
        };
        let used = match subagents.get(model) {
            Some(theirs) => used.less(*theirs),
            None => used,
        };
        let Some(uncached) = used.input.checked_sub(used.cache_read + used.cache_write) else {
            read.skipped += 1;
            continue;
        };
        let tokens = Tokens {
            input: uncached,
            cache_read: used.cache_read,
            cache_write_5m: used.cache_write,
            cache_write_1h: 0,
            output: used.output,
            reasoning: used.reasoning,
        };
        let cost = used.ticks.map(|ticks| ticks as f64 / 1e10);
        if tokens == Tokens::default() && cost.is_none_or(|cost| cost == 0.0) {
            continue;
        }
        read.respond(Response {
            id: format!("{prompt}:{model}"),
            session: session.to_owned(),
            copy: false,
            at,
            provider: "xai".to_owned(),
            model: model.clone(),
            tokens,
            prompt: used.input.div_ceil(used.calls.max(1)),
            web_searches: 0,
            priority: false,
            cost,
        });
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn updates(dir: &Path, session: &str, lines: &[Value]) -> PathBuf {
        let folder = dir.join("sessions/%2Fwork%2Fapp").join(session);
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("updates.jsonl");
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(&file, text).unwrap();
        file
    }

    fn used(input: u64, cached: u64, calls: u64, ticks: u64) -> Value {
        json!({"inputTokens": input, "cachedReadTokens": cached, "cacheCreationTokens": 0, "outputTokens": 100,
            "reasoningTokens": 40, "modelCalls": calls, "costUsdTicks": ticks})
    }

    fn update(at: i64, update: Value) -> Value {
        json!({"timestamp": at, "params": {"update": update}})
    }

    #[test]
    fn a_turns_usage_is_a_response_per_model_at_what_xai_charged() {
        let dir = tempfile::tempdir().unwrap();
        let file = updates(
            dir.path(),
            "s1",
            &[
                update(
                    1_790_000_000,
                    json!({"sessionUpdate": "user_message_chunk", "content": {"text": "<user_info>macOS</user_info><user_query>Speed up the build.</user_query>"}}),
                ),
                update(
                    1_790_000_060,
                    json!({"sessionUpdate": "turn_completed", "prompt_id": "p1", "usage": {"modelUsage": {"grok-4.6-build": used(30000, 20000, 3, 133_892_000)}}}),
                ),
            ],
        );
        let read = GrokBuild.read(&file, "").unwrap();
        let response = &read.responses["p1:grok-4.6-build"];
        assert_eq!(
            (
                response.tokens.input,
                response.tokens.cache_read,
                response.prompt
            ),
            (10000, 20000, 10000)
        );
        assert_eq!(response.cost, Some(0.0133892));
        assert_eq!(
            read.sessions["grok-build:s1"].title.as_ref().unwrap().1,
            "Speed up the build."
        );
    }

    #[test]
    fn a_parents_turn_is_counted_less_the_subagents_that_finished_in_it() {
        let dir = tempfile::tempdir().unwrap();
        updates(
            dir.path(),
            "s2",
            &[update(
                1_790_000_050,
                json!({"sessionUpdate": "turn_completed", "prompt_id": "p2", "usage": {"modelUsage": {"grok-4.6-build": used(10000, 5000, 4, 100)}}}),
            )],
        );
        let parent = updates(
            dir.path(),
            "s1",
            &[
                update(
                    1_790_000_001,
                    json!({"sessionUpdate": "subagent_spawned", "child_session_id": "s2"}),
                ),
                update(
                    1_790_000_055,
                    json!({"sessionUpdate": "subagent_finished", "child_session_id": "s2"}),
                ),
                update(
                    1_790_000_060,
                    json!({"sessionUpdate": "turn_completed", "prompt_id": "p1", "usage": {"modelUsage": {"grok-4.6-build": used(30000, 20000, 10, 400)}}}),
                ),
            ],
        );
        let read = GrokBuild.read(&parent, "").unwrap();
        let own = &read.responses["p1:grok-4.6-build"];
        // 30,000 in, 20,000 cached, less the subagent's 10,000 and 5,000.
        assert_eq!((own.tokens.input, own.tokens.cache_read), (5000, 15000));
        assert_eq!((own.prompt, own.cost), (20000_u64.div_ceil(6), Some(3e-8)));
        assert!(
            read.sessions["grok-build:s2"]
                .parent
                .as_ref()
                .is_some_and(|parent| parent.id == "grok-build:s1" && parent.link == Link::Subagent)
        );
    }

    #[test]
    fn what_grok_build_adds_isnt_the_persons_and_its_system_prompt_isnt_shown() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("sessions/%2Fwork/s1");
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("chat_history.jsonl");
        let lines = [
            json!({"type": "system", "content": "You are Grok Build."}),
            json!({"type": "user", "content": "<rules>Be brief.</rules><git_status>clean</git_status>"}),
            json!({"type": "user", "content": "<user_query>Add a flag.</user_query>"}),
        ];
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(&file, text).unwrap();
        let entries = GrokBuild
            .transcript("s1", std::slice::from_ref(&file))
            .unwrap();
        let kinds: Vec<Kind> = entries.iter().map(|entry| entry.kind).collect();
        assert_eq!(kinds, [Kind::System, Kind::User]);
        assert_eq!(entries[1].text, "Add a flag.");
        // Read, the file is the session's, so its conversation can be found.
        let read = GrokBuild.read(&file, "").unwrap();
        assert!(read.sessions.contains_key("grok-build:s1"));
    }
}
