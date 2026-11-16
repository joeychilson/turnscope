//! Grok Build.
//!
//! Each session is a directory, `~/.grok/sessions/<encoded directory>/<session>/`:
//! `summary.json`, rewritten as the session goes on, with its directory,
//! times and title; and
//! `updates.jsonl`, its stream of updates, one JSON-RPC notification a line,
//! and the only record of usage: a `turn_completed` per prompt with the
//! turn's usage per model and what xAI charged. `chat_history.jsonl`, the
//! conversation, isn't read.
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
//! **Titles.** A session Grok Build gave no title is titled by the first
//! thing the person asked, its first `user_message_chunk`. Their words are
//! in `<user_query>`, beside context Grok Build adds (its rules, the git
//! status, the folder).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Agent, Credential, Info, Login, Read, Response, Secret, Session, Title, Tokens};
use crate::{Error, Result};

pub struct GrokBuild;

static INFO: Info = Info {
    id: "grok-build",
    name: "Grok Build",
    command: "grok",
    app: None,
    mcp_add: &["mcp", "add", "--scope", "user", "turnscope"],
    mcp_remove: Some(&["mcp", "remove", "--scope", "user", "turnscope"]),
    folder_var: Some("GROK_HOME"),
    charges: true,
    session_var: None,
};

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

impl Agent for GrokBuild {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn folders(&self, home: &Path) -> Vec<PathBuf> {
        super::own_and_found(home, ".grok", ".grok-", "auth.json")
    }

    fn files(&self, folder: &Path) -> Vec<PathBuf> {
        super::entries(&folder.join("sessions"))
            .flat_map(|dir| super::entries(&dir))
            .flat_map(|session| ["summary.json", "updates.jsonl"].map(|name| session.join(name)))
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
        let session = INFO.session(native);
        let mut cursor: Cursor = serde_json::from_str(cursor).unwrap_or_default();
        let mut read = Read::default();
        match file.file_name().and_then(|name| name.to_str()) {
            // Rewritten whole.
            Some("summary.json") => {
                let json = super::json_file(file)?.unwrap_or_default();
                // A session opened and left before anything was said.
                if json["num_messages"] == 0 {
                    return Ok(read);
                }
                summary(&json, read.session(&session));
            }
            _ => updates(file, &session, &mut cursor, &mut read)?,
        }
        read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
        Ok(read)
    }

    fn logins(&self, folder: &Path, _home: &Path) -> Result<Vec<Login>> {
        let kept = super::json_file(&folder.join("auth.json"))?.unwrap_or_default();
        // The SuperGrok sign-in, under its issuer and client.
        let entry = &kept["https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828"];
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
}

/// Note in `facts` what its session's `summary.json`, `summary`, says.
fn summary(summary: &Value, facts: &mut Session) {
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
    if let Some(title) = text(&summary["generated_title"]) {
        let kind = if summary["title_is_manual"] == true {
            Title::Named
        } else {
            Title::Generated
        };
        facts.title(kind, &title);
    }
}

/// Read `updates.jsonl` past `cursor` into `read`.
fn updates(file: &Path, session: &str, cursor: &mut Cursor, read: &mut Read) -> Result<()> {
    let sessions = file.ancestors().nth(3);
    cursor.offset = super::lines(file, cursor.offset, |bytes| {
        let Ok(line) = serde_json::from_slice::<Value>(bytes) else {
            read.skipped += 1;
            return;
        };
        let at = line["timestamp"].as_i64().map(|seconds| seconds * 1000);
        if let Some(at) = at {
            read.session(session).saw(at);
        }
        let update = &line["params"]["update"];
        match update["sessionUpdate"].as_str() {
            Some("turn_completed") => {
                let finished = std::mem::take(&mut cursor.finished);
                let theirs = subagents(sessions, &finished);
                turn(update, at, session, &theirs, read);
            }
            Some("subagent_spawned") => {
                if let Some(child) = update["child_session_id"].as_str() {
                    read.session(&INFO.session(child)).parent = Some(session.to_owned());
                }
            }
            Some("subagent_finished") => {
                if let Some(child) = update["child_session_id"].as_str() {
                    cursor.finished.push(child.to_owned());
                }
            }
            Some("user_message_chunk") if !cursor.titled => {
                let text = typed(update["content"]["text"].as_str().unwrap_or_default());
                if !text.is_empty() {
                    read.session(session).title(Title::Prompt, &text);
                    cursor.titled = true;
                }
            }
            _ => {}
        }
    })?;
    Ok(())
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

    fn less(self, theirs: Used) -> Used {
        Used {
            input: self.input.saturating_sub(theirs.input),
            cache_read: self.cache_read.saturating_sub(theirs.cache_read),
            cache_write: self.cache_write.saturating_sub(theirs.cache_write),
            output: self.output.saturating_sub(theirs.output),
            reasoning: self.reasoning.saturating_sub(theirs.reasoning),
            calls: self.calls.saturating_sub(theirs.calls),
            // A charge not known in theirs makes what's left unknown.
            ticks: self
                .ticks
                .zip(theirs.ticks)
                .map(|(own, theirs)| own.saturating_sub(theirs)),
        }
    }
}

/// What each turn of the subagents `finished` used, model by model, from
/// their own updates in `sessions`.
fn subagents(sessions: Option<&Path>, finished: &[String]) -> Vec<(String, Used)> {
    let mut used = Vec::new();
    // Most turns finish none, and then the folders aren't listed.
    if finished.is_empty() {
        return used;
    }
    let dirs: Vec<PathBuf> = sessions.into_iter().flat_map(super::entries).collect();
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
                    used.push((model.clone(), each));
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
    subagents: &[(String, Used)],
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
        let used = subagents
            .iter()
            .filter(|(theirs, _)| theirs == model)
            .fold(used, |used, (_, theirs)| used.less(*theirs));
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
        let file = dir
            .join("sessions/%2Fwork%2Fapp")
            .join(session)
            .join("updates.jsonl");
        crate::agents::jsonl(file, lines)
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
        let lines = [
            update(
                1_790_000_000,
                json!({"sessionUpdate": "user_message_chunk", "content": {"text": "<user_query>Speed up the build.</user_query>"}}),
            ),
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
            update(
                1_790_000_100,
                json!({"sessionUpdate": "user_message_chunk", "content": {"text": "<user_query>Now the tests.</user_query>"}}),
            ),
        ];
        let file = updates(dir.path(), "s1", &lines[..3]);
        let first = GrokBuild.read(&file, "").unwrap();
        assert!(first.responses.is_empty());
        updates(dir.path(), "s1", &lines);
        let later = GrokBuild.read(&file, &first.cursor).unwrap();
        let whole = GrokBuild.read(&file, "").unwrap();
        // The turn ended after the read that saw its subagent finish, and is
        // counted less it all the same.
        let own = &later.responses["p1:grok-4.6-build"];
        assert_eq!(own, &whole.responses["p1:grok-4.6-build"]);
        // 30,000 in, 20,000 cached, less the subagent's 10,000 and 5,000.
        assert_eq!((own.tokens.input, own.tokens.cache_read), (5000, 15000));
        assert_eq!((own.prompt, own.cost), (20000_u64.div_ceil(6), Some(3e-8)));
        assert!(later.sessions["grok-build:s1"].title.is_none());
        assert_eq!(
            first.sessions["grok-build:s2"].parent.as_deref(),
            Some("grok-build:s1")
        );
    }

    #[test]
    fn a_tag_the_person_typed_is_theirs_and_grok_builds_own_are_taken_out() {
        let asked = "Give each <agent> its own <dir>, then list the <rules> it reads.";
        // In their query, and in a message with none.
        for sent in [
            format!("<user_info>macOS</user_info><user_query>{asked}</user_query>"),
            format!("<git_status>clean</git_status>{asked}"),
        ] {
            assert_eq!(typed(&sent), asked);
        }
        assert_eq!(
            typed("<rules>Be brief.</rules><git_status>clean</git_status>"),
            ""
        );
    }
}
