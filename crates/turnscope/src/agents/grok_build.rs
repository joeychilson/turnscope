//! Grok Build.
//!
//! A session is a directory, `~/.grok/sessions/<encoded directory>/<session>/`,
//! of `summary.json`, rewritten as it goes on, with its directory and title,
//! and `updates.jsonl`, a JSON-RPC notification a line: the only record of
//! usage, a `turn_completed` per prompt with the turn's usage per model and
//! what xAI charged. `chat_history.jsonl` isn't read. A second account's
//! folder is `~/.grok-<name>`, used through `GROK_HOME`, once it holds
//! `auth.json`.
//!
//! Measured on this Mac's history, September and October 2026:
//!
//! - **Usage.** `inputTokens` includes the cache, and `outputTokens`
//!   reasoning. `costUsdTicks` is the bill, ten billion ticks to the dollar;
//!   a turn without one is marked `usageIsIncomplete`. A turn is many model
//!   calls, so the prompt that sets its tier is their average. A `prompt_id`
//!   is unique across sessions.
//! - **Subagents** are sessions of their own, spawned by a parent's
//!   `subagent_spawned`. A parent's turn counts its subagents' turns that
//!   ended in it: in all 13 cases, its own calls and its finished subagents'
//!   added up to its turn's. So it's counted less its subagents' turns that
//!   ended after its last turn did, and no later than it. Taking off each
//!   subagent that finished in it (`subagent_finished`) was dropped: one at
//!   work over two of its turns came off both whole, and one stopped before
//!   it finished came off neither. A parent read again from its start once a
//!   subagent's folder is gone counts that subagent twice.
//! - **Parsing.** A line is parsed into a struct borrowing its text, a
//!   streamed reply held unparsed: 190 ms for a made-up 225 MiB of reply,
//!   against 420 ms as a `Value` (November 2026). Passing over a line naming
//!   no update read, before parsing it, took 57 ms, but would leave a line
//!   that doesn't read uncounted.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use super::{Agent, Credential, Info, Login, Read, Response, Secret, Session, Title, Tokens};
use crate::Result;

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
    login_file: Some("auth.json"),
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
    /// Spawned subagents and how far into each one's `updates.jsonl` turns
    /// have already been subtracted from earlier parent turns.
    #[serde(default)]
    subagents: Vec<Subagent>,
}

#[derive(Serialize, Deserialize)]
struct Subagent {
    id: String,
    #[serde(default)]
    offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    folder: Option<String>,
    #[serde(default)]
    finished: bool,
    #[serde(default)]
    gone: bool,
}

fn safe_child(child: &str) -> bool {
    let mut parts = Path::new(child).components();
    !child.contains(['/', '\\', '\0'])
        && matches!(parts.next(), Some(std::path::Component::Normal(_)))
        && parts.next().is_none()
}

#[derive(Deserialize)]
struct Line<'a> {
    /// In Unix seconds.
    timestamp: Option<i64>,
    #[serde(borrow)]
    params: Option<Params<'a>>,
}

#[derive(Deserialize)]
struct Params<'a> {
    #[serde(borrow)]
    update: Option<Update<'a>>,
}

#[derive(Deserialize)]
struct Update<'a> {
    #[serde(rename = "sessionUpdate", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    child_session_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    prompt_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    usage: Option<&'a RawValue>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct TurnUsage<'a> {
    #[serde(rename = "modelUsage", borrow)]
    models: Option<HashMap<Cow<'a, str>, &'a RawValue>>,
}

impl<'a> Line<'a> {
    fn update(self) -> Option<(Option<i64>, Update<'a>)> {
        Some((self.timestamp, self.params?.update?))
    }
}

impl<'a> Update<'a> {
    /// Each model's usage in a finished turn: `None` for one that can't be
    /// read.
    fn used(&self) -> impl Iterator<Item = (Cow<'a, str>, Option<Used>)> {
        super::parsed::<TurnUsage<'a>>(self.usage)
            .and_then(|usage| usage.models)
            .into_iter()
            .flatten()
            .map(|(model, used)| (model, serde_json::from_str::<Used>(used.get()).ok()))
    }
}

impl Agent for GrokBuild {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn folders(&self, home: &Path) -> Vec<PathBuf> {
        super::own_and_found(home, ".grok", "auth.json")
    }

    fn files(&self, folder: &Path) -> Vec<PathBuf> {
        super::entries(&folder.join("sessions"))
            .flat_map(|dir| super::entries(&dir))
            .flat_map(|session| ["summary.json", "updates.jsonl"].map(|name| session.join(name)))
            .collect()
    }

    fn read(&self, file: &Path, cursor: &str) -> Result<Read> {
        let mut read = Read::default();
        let native = file
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str());
        let Some(native) = native else {
            return Ok(read);
        };
        let session = INFO.session(native);
        match file.file_name().and_then(|name| name.to_str()) {
            // Rewritten whole.
            Some("summary.json") => {
                let json = super::json_file(file)?;
                // A session opened and left before anything was said.
                if json["num_messages"] == 0 {
                    return Ok(read);
                }
                summary(&json, read.session(&session));
            }
            _ => {
                let mut cursor: Cursor = serde_json::from_str(cursor).unwrap_or_default();
                updates(file, &session, &mut cursor, &mut read)?;
                read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
            }
        }
        Ok(read)
    }

    fn logins(&self, folder: &Path, _home: &Path) -> Result<Vec<Login>> {
        let kept = super::json_file(&folder.join("auth.json"))?;
        // The SuperGrok sign-in, under its issuer and client.
        let entry = &kept["https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828"];
        Ok(vec![Login {
            provider: "xai".to_owned(),
            credential: super::non_empty(&entry["key"]).map(|token| Credential {
                provider: "xai",
                secret: Secret::new(token),
                key: false,
                expires: entry["expires_at"].as_str().and_then(crate::time::millis),
                identity: super::non_empty(&entry["user_id"]).map(|id| {
                    (
                        id.to_owned(),
                        super::non_empty(&entry["email"]).map(str::to_owned),
                    )
                }),
                plan: None,
                client: None,
                refresh: None,
            }),
        }])
    }

    fn mcp_command(&self, folder: &Path, _home: &Path) -> Option<String> {
        super::toml_mcp_command(&folder.join("config.toml"))
    }
}

fn summary(summary: &Value, facts: &mut Session) {
    facts.cwd = super::non_empty(&summary["info"]["cwd"]).map(str::to_owned);
    if let Some(title) = super::non_empty(&summary["generated_title"]) {
        let kind = if summary["title_is_manual"] == true {
            Title::Named
        } else {
            Title::Generated
        };
        facts.title(kind, title);
    }
}

fn updates(file: &Path, session: &str, cursor: &mut Cursor, read: &mut Read) -> Result<()> {
    let mut subagents = Subagents {
        own_dir: file.ancestors().nth(2),
        folders: None,
        next_at: HashMap::new(),
    };
    cursor.offset = super::try_lines(file, cursor.offset, |bytes| {
        let Ok(line) = serde_json::from_slice::<Line<'_>>(bytes) else {
            read.skipped += 1;
            return Ok(ControlFlow::Continue(()));
        };
        let Some((at, update)) = line.update() else {
            return Ok(ControlFlow::Continue(()));
        };
        match update.kind.as_deref() {
            Some("turn_completed") => {
                if let Some(at) = at {
                    let theirs = subagents.used(&mut cursor.subagents, at)?;
                    turn(&update, at, session, &theirs, read);
                }
            }
            Some("subagent_spawned") => {
                if let Some(child) = update
                    .child_session_id
                    .as_deref()
                    .filter(|child| safe_child(child))
                {
                    read.session(&INFO.session(child)).parent = Some(session.to_owned());
                    if let Some(known) = cursor.subagents.iter_mut().find(|known| known.id == child)
                    {
                        known.finished = false;
                        known.gone = false;
                        subagents.next_at.remove(child);
                    } else {
                        cursor.subagents.push(Subagent {
                            id: child.to_owned(),
                            offset: 0,
                            folder: None,
                            finished: false,
                            gone: false,
                        });
                    }
                }
            }
            Some("subagent_finished") => {
                if let Some(child) = update
                    .child_session_id
                    .as_deref()
                    .filter(|child| safe_child(child))
                    && let Some(known) = cursor.subagents.iter_mut().find(|known| known.id == child)
                {
                    known.finished = true;
                    known.gone = false;
                }
            }
            Some("user_message_chunk") if !cursor.titled => {
                if let Some(text) = super::parsed::<super::Text<'_>>(update.content)
                    .and_then(|content| content.text)
                {
                    let text = super::typed_in(&text, "user_query", INJECTED);
                    if !text.is_empty() {
                        read.session(session).title(Title::Prompt, &text);
                        cursor.titled = true;
                    }
                }
            }
            _ => {}
        }
        Ok(ControlFlow::Continue(()))
    })?;
    Ok(())
}

/// One model's usage in a turn, as `modelUsage` gives it.
#[derive(Clone, Copy, Deserialize)]
struct Used {
    #[serde(rename = "inputTokens")]
    input: u64,
    #[serde(rename = "cachedReadTokens")]
    cache_read: u64,
    #[serde(rename = "cacheCreationTokens")]
    cache_write: u64,
    #[serde(rename = "outputTokens")]
    output: u64,
    #[serde(rename = "reasoningTokens")]
    reasoning: u64,
    #[serde(rename = "modelCalls")]
    calls: u64,
    #[serde(rename = "costUsdTicks", default, deserialize_with = "super::lenient")]
    ticks: Option<u64>,
}

/// Usage split into disjoint input (`uncached` + cache) and output
/// (`non_reasoning` + `reasoning`) counts before subtracting subagents, so
/// saturating subtraction preserves `input >= cached` and `output >= reasoning`.
#[derive(Clone, Copy)]
struct Normalized {
    uncached: u64,
    cache_read: u64,
    cache_write: u64,
    non_reasoning: u64,
    reasoning: u64,
    calls: u64,
    ticks: Option<u64>,
}

impl Used {
    fn normalize(self) -> Option<Normalized> {
        let cached = self.cache_read.checked_add(self.cache_write)?;
        Some(Normalized {
            uncached: self.input.checked_sub(cached)?,
            cache_read: self.cache_read,
            cache_write: self.cache_write,
            non_reasoning: self.output.saturating_sub(self.reasoning),
            reasoning: self.reasoning,
            calls: self.calls,
            ticks: self.ticks,
        })
    }
}

impl Normalized {
    fn less(self, theirs: Normalized) -> Normalized {
        Normalized {
            uncached: self.uncached.saturating_sub(theirs.uncached),
            cache_read: self.cache_read.saturating_sub(theirs.cache_read),
            cache_write: self.cache_write.saturating_sub(theirs.cache_write),
            non_reasoning: self.non_reasoning.saturating_sub(theirs.non_reasoning),
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

/// Reads each subagent's `updates.jsonl` incrementally from its saved byte
/// offset up to the parent turn's timestamp.
struct Subagents<'a> {
    own_dir: Option<&'a Path>,
    folders: Option<Vec<PathBuf>>,
    /// Timestamp of the next unread `turn_completed` in each subagent, so
    /// earlier parent turns don't reopen it.
    next_at: HashMap<String, i64>,
}

impl Subagents<'_> {
    /// What unread turns of `subagents` that ended no later than `until` used,
    /// advancing each subagent's offset past the turns taken. Fails when a
    /// subagent's updates are there and can't be read, so its parent's turn is
    /// read again later rather than counted whole.
    fn used(
        &mut self,
        subagents: &mut [Subagent],
        until: i64,
    ) -> Result<Vec<(String, Normalized)>> {
        let mut used = Vec::new();
        for child in subagents {
            if child.gone || !safe_child(&child.id) {
                continue;
            }
            if self.next_at.get(&child.id).is_some_and(|&at| at > until) {
                continue;
            }
            let Some((updates, len)) = self.locate(child) else {
                if child.offset > 0 || child.finished {
                    child.gone = true;
                }
                continue;
            };
            if len < child.offset {
                child.offset = 0;
            }
            if len == child.offset {
                if child.finished {
                    self.next_at.insert(child.id.clone(), i64::MAX);
                }
                continue;
            }
            self.next_at.remove(&child.id);
            let mut unread = 0;
            let mut next_at = None;
            let next = super::try_lines(&updates, child.offset, |bytes| {
                let Some((Some(at), update)) = serde_json::from_slice::<Line<'_>>(bytes)
                    .ok()
                    .and_then(Line::update)
                else {
                    return Ok(ControlFlow::Continue(()));
                };
                if update.kind.as_deref() == Some("turn_completed") {
                    if at > until {
                        unread = bytes.len() as u64;
                        next_at = Some(at);
                        return Ok(ControlFlow::Break(()));
                    }
                    for (model, counted) in update.used() {
                        if !model.is_empty()
                            && let Some(counted) = counted.and_then(Used::normalize)
                        {
                            used.push((model.into_owned(), counted));
                        }
                    }
                }
                Ok(ControlFlow::Continue(()))
            })?;
            child.offset = next.saturating_sub(unread);
            if let Some(at) = next_at {
                self.next_at.insert(child.id.clone(), at);
            } else if child.finished && child.offset == len {
                self.next_at.insert(child.id.clone(), i64::MAX);
            }
        }
        Ok(used)
    }

    fn locate(&mut self, child: &mut Subagent) -> Option<(PathBuf, u64)> {
        let sessions = self.own_dir.and_then(Path::parent);
        let at = |folder: &Path| {
            let path = folder.join(&child.id).join("updates.jsonl");
            let meta = std::fs::metadata(&path).ok()?;
            meta.is_file().then_some((path, meta.len()))
        };
        if let Some(folder) = child.folder.as_deref().filter(|name| safe_child(name))
            && let Some(found) = sessions.and_then(|dir| at(&dir.join(folder)))
        {
            return Some(found);
        }
        if let Some(found) = self.own_dir.and_then(at) {
            child.folder = None;
            return Some(found);
        }
        // Beside its parent, or else in any session's folder; one whose
        // folder is gone takes nothing off.
        let (folder, found) = self
            .folders
            .get_or_insert_with(|| sessions.into_iter().flat_map(super::entries).collect())
            .iter()
            .find_map(|folder| Some((folder.clone(), at(folder)?)))?;
        child.folder = folder
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| safe_child(name))
            .map(str::to_owned);
        Some(found)
    }
}

fn turn(
    update: &Update<'_>,
    at: i64,
    session: &str,
    subagents: &[(String, Normalized)],
    read: &mut Read,
) {
    let Some(prompt) = super::non_empty_str(update.prompt_id.as_deref()) else {
        return;
    };
    for (model, used) in update.used() {
        let Some(used) = (!model.is_empty())
            .then_some(used)
            .flatten()
            .and_then(Used::normalize)
        else {
            read.skipped += 1;
            continue;
        };
        let used = subagents
            .iter()
            .filter(|(theirs, _)| theirs == &*model)
            .fold(used, |used, (_, theirs)| used.less(*theirs));
        let tokens = Tokens {
            input: used.uncached,
            cache_read: used.cache_read,
            cache_write_5m: used.cache_write,
            cache_write_1h: 0,
            output: used.non_reasoning.saturating_add(used.reasoning),
            reasoning: used.reasoning,
        };
        read.respond(Response {
            id: format!("{prompt}:{model}"),
            session: session.to_owned(),
            copy: false,
            at: at.saturating_mul(1000),
            provider: "xai".to_owned(),
            model: model.into_owned(),
            prompt: tokens.prompt().div_ceil(used.calls.max(1)),
            tokens,
            web_searches: 0,
            priority: false,
            cost: used.ticks.map(|ticks| ticks as f64 / 1e10),
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

    fn spawned(at: i64, child: &str) -> Value {
        update(
            at,
            json!({"sessionUpdate": "subagent_spawned", "child_session_id": child}),
        )
    }

    fn finished(at: i64, child: &str) -> Value {
        update(
            at,
            json!({"sessionUpdate": "subagent_finished", "child_session_id": child}),
        )
    }

    #[test]
    fn a_parents_turn_is_counted_less_what_its_subagent_used() {
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
            spawned(1_790_000_001, "s2"),
            finished(1_790_000_055, "s2"),
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
        // The turn ended after the read that saw its subagent's, and is
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

    fn completed(at: i64, prompt: &str, input: u64) -> Value {
        update(
            at,
            json!({"sessionUpdate": "turn_completed", "prompt_id": prompt,
                "usage": {"modelUsage": {"grok-4.6-build": used(input, 0, 1, input)}}}),
        )
    }

    fn input(read: &Read, prompt: &str) -> Option<u64> {
        let response = read.responses.get(&format!("{prompt}:grok-4.6-build"))?;
        Some(response.tokens.input)
    }

    #[test]
    fn a_parents_turn_is_counted_less_only_its_subagents_turns_that_ended_in_it() {
        let dir = tempfile::tempdir().unwrap();
        // s2 works in both of s1's turns. s3 is stopped before it finishes.
        let parent = [
            spawned(1_790_000_001, "s2"),
            finished(1_790_000_055, "s2"),
            completed(1_790_000_060, "p1", 20_000),
            spawned(1_790_000_101, "s3"),
            finished(1_790_000_155, "s2"),
            completed(1_790_000_160, "p2", 30_000),
        ];
        let child = [
            completed(1_790_000_050, "c1", 10_000),
            completed(1_790_000_150, "c2", 10_000),
        ];
        let stopped = [completed(1_790_000_120, "c3", 5_000)];
        let file = updates(dir.path(), "s1", &parent[..3]);
        updates(dir.path(), "s2", &child[..1]);
        let first = GrokBuild.read(&file, "").unwrap();
        updates(dir.path(), "s1", &parent);
        updates(dir.path(), "s2", &child);
        updates(dir.path(), "s3", &stopped);
        let later = GrokBuild.read(&file, &first.cursor).unwrap();
        let whole = GrokBuild.read(&file, "").unwrap();
        for (read, p1, p2) in [(&first, Some(10_000), None), (&later, None, Some(15_000))] {
            assert_eq!((input(read, "p1"), input(read, "p2")), (p1, p2));
        }
        assert_eq!(
            (input(&whole, "p1"), input(&whole, "p2")),
            (Some(10_000), Some(15_000))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_subagent_that_cant_be_read_fails_its_parents_read() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let file = updates(
            dir.path(),
            "s1",
            &[
                spawned(1_790_000_001, "s2"),
                finished(1_790_000_055, "s2"),
                completed(1_790_000_060, "p1", 20_000),
            ],
        );
        let child = updates(dir.path(), "s2", &[completed(1_790_000_050, "c1", 10_000)]);
        let readable = std::fs::metadata(&child).unwrap().permissions();
        std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o000)).unwrap();
        let failed = GrokBuild.read(&file, "");
        std::fs::set_permissions(&child, readable).unwrap();
        assert!(failed.is_err());
        assert_eq!(
            input(&GrokBuild.read(&file, "").unwrap(), "p1"),
            Some(10_000)
        );
    }

    #[test]
    fn a_summary_gives_the_title_grok_build_or_the_person_gave_and_none_before_a_message() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sessions/%2Fwork%2Fapp/s1/summary.json");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let title = |summary: Value| {
            std::fs::write(&file, summary.to_string()).unwrap();
            let read = GrokBuild.read(&file, "").unwrap();
            let session = read.sessions.get("grok-build:s1")?;
            Some((session.cwd.clone(), session.title.clone()))
        };
        let summary = |messages: u64, manual: bool| {
            json!({"num_messages": messages, "info": {"cwd": "/work/app"},
                "generated_title": "Speed up the build", "title_is_manual": manual})
        };
        let titled = |kind: Title| {
            Some((
                Some("/work/app".to_owned()),
                Some((kind, "Speed up the build".to_owned())),
            ))
        };
        assert!(title(summary(6, false)) == titled(Title::Generated));
        assert!(title(summary(6, true)) == titled(Title::Named));
        assert!(title(summary(0, false)).is_none());
    }

    #[test]
    fn the_supergrok_sign_in_is_the_login() {
        let dir = tempfile::tempdir().unwrap();
        let login = |entry: Value| {
            let kept = json!({"https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828": entry});
            std::fs::write(dir.path().join("auth.json"), kept.to_string()).unwrap();
            let logins = GrokBuild.logins(dir.path(), dir.path()).unwrap();
            let [login] = logins.as_slice() else {
                panic!("one login");
            };
            let credential = crate::agents::credential(login);
            let identity = login.credential.as_ref().and_then(|c| c.identity.clone());
            (
                login.provider.clone(),
                credential.map(|(provider, key, expires, secret)| {
                    (provider, key, expires, secret.to_owned())
                }),
                identity,
            )
        };
        assert_eq!(
            login(
                json!({"key": "made-up-token", "expires_at": "2026-10-01T00:00:00Z",
                "user_id": "u-1", "email": "someone@example.com"})
            ),
            (
                "xai".to_owned(),
                Some((
                    "xai",
                    false,
                    Some(1_790_812_800_000),
                    "made-up-token".to_owned()
                )),
                Some(("u-1".to_owned(), Some("someone@example.com".to_owned())))
            )
        );
        // Signed out.
        assert_eq!(login(json!({"key": ""})), ("xai".to_owned(), None, None));
    }

    #[test]
    fn a_message_with_no_query_loses_what_grok_build_added() {
        assert_eq!(
            super::super::typed_in(
                "<rules>Be brief.</rules><git_status>clean</git_status>Speed up the build.",
                "user_query",
                super::INJECTED
            ),
            "Speed up the build."
        );
    }

    #[test]
    fn subagent_subtraction_preserves_parent_turn_when_clamped_and_handles_same_second_boundary() {
        let dir = tempfile::tempdir().unwrap();
        // Parent: 1000 input / 800 cached (200 uncached), 100 output / 80 reasoning (20 non-reasoning).
        // Subagent: 900 input / 500 cached (400 uncached), 90 output / 50 reasoning (40 non-reasoning).
        // Subtracting disjoint parts yields 0 uncached, 300 cached, and 30 output / 30 reasoning
        // (preserving both `input >= cached` and `reasoning <= output`).
        let mut child_used = used(900, 500, 1, 50);
        child_used["outputTokens"] = json!(90);
        child_used["reasoningTokens"] = json!(50);
        let mut parent_used = used(1000, 800, 2, 120);
        parent_used["outputTokens"] = json!(100);
        parent_used["reasoningTokens"] = json!(80);
        updates(
            dir.path(),
            "s2",
            &[update(
                1_790_000_050,
                json!({"sessionUpdate": "turn_completed", "prompt_id": "c1",
                    "usage": {"modelUsage": {"grok-4.6-build": child_used}}}),
            )],
        );
        let file = updates(
            dir.path(),
            "s1",
            &[
                spawned(1_790_000_001, "s2"),
                finished(1_790_000_055, "s2"),
                update(
                    1_790_000_060,
                    json!({"sessionUpdate": "turn_completed", "prompt_id": "p1",
                        "usage": {"modelUsage": {"grok-4.6-build": parent_used}}}),
                ),
            ],
        );
        let read = GrokBuild.read(&file, "").unwrap();
        assert_eq!(read.skipped, 0);
        let own = &read.responses["p1:grok-4.6-build"];
        assert_eq!(
            (
                own.tokens.input,
                own.tokens.cache_read,
                own.tokens.output,
                own.tokens.reasoning,
                own.cost
            ),
            (0, 300, 30, 30, Some(7e-9))
        );

        // Same-second boundary: s3 is spawned and completes in the exact same
        // second (60) that p1 ended, before p2 ends at 70. Both incremental
        // and whole reads subtract s3 from p2.
        let parent_lines = [
            completed(1_790_000_060, "p1", 10_000),
            spawned(1_790_000_060, "s3"),
            finished(1_790_000_060, "s3"),
            completed(1_790_000_070, "p2", 25_000),
        ];
        updates(dir.path(), "s3", &[completed(1_790_000_060, "c3", 6_000)]);
        let pfile = updates(dir.path(), "p_same", &parent_lines[..1]);
        let first = GrokBuild.read(&pfile, "").unwrap();
        updates(dir.path(), "p_same", &parent_lines);
        let later = GrokBuild.read(&pfile, &first.cursor).unwrap();
        let whole = GrokBuild.read(&pfile, "").unwrap();
        assert_eq!(input(&first, "p1"), Some(10_000));
        assert_eq!(input(&later, "p2"), Some(19_000));
        assert_eq!(
            (input(&whole, "p1"), input(&whole, "p2")),
            (Some(10_000), Some(19_000))
        );
    }

    #[test]
    fn path_traversal_child_session_id_is_rejected_and_settled_children_stop_reparsing() {
        let dir = tempfile::tempdir().unwrap();
        // Outside sessions folder: must never be read via `..` or absolute path.
        let escaped = dir.path().join("sessions/victim/updates.jsonl");
        crate::agents::jsonl(escaped, &[completed(1_790_000_050, "c1", 5_000)]);
        assert!(!safe_child("bad\0child"));
        let file = updates(
            dir.path(),
            "s1",
            &[
                spawned(1_790_000_001, "../victim"),
                spawned(1_790_000_002, "/etc/passwd"),
                spawned(1_790_000_003, "a/b"),
                spawned(1_790_000_004, "bad\0child"),
                completed(1_790_000_060, "p1", 10_000),
            ],
        );
        let read = GrokBuild.read(&file, "").unwrap();
        assert_eq!(input(&read, "p1"), Some(10_000));
        assert!(!read.sessions.contains_key("grok-build:../victim"));
        assert!(!read.sessions.contains_key("grok-build:bad\0child"));
        let cursor: Cursor = serde_json::from_str(&read.cursor).unwrap();
        assert!(cursor.subagents.is_empty());

        // Settled subagent offset is saved in cursor; once deleted, its `gone`
        // flag prevents rescanning on subsequent turns.
        let child_file = updates(dir.path(), "s2", &[completed(1_790_000_050, "c1", 4_000)]);
        let lines = [
            spawned(1_790_000_001, "s2"),
            finished(1_790_000_055, "s2"),
            completed(1_790_000_060, "p1", 10_000),
            completed(1_790_000_120, "p2", 12_000),
        ];
        let pfile = updates(dir.path(), "p_inc", &lines[..3]);
        let first = GrokBuild.read(&pfile, "").unwrap();
        assert_eq!(input(&first, "p1"), Some(6_000));
        let cursor: Cursor = serde_json::from_str(&first.cursor).unwrap();
        assert!(cursor.subagents[0].offset > 0);
        // Removing the child after Turn 1 marks it `gone` on Turn 2.
        std::fs::remove_file(&child_file).unwrap();
        updates(dir.path(), "p_inc", &lines);
        let second = GrokBuild.read(&pfile, &first.cursor).unwrap();
        assert_eq!(input(&second, "p2"), Some(12_000));
        let cursor: Cursor = serde_json::from_str(&second.cursor).unwrap();
        assert!(cursor.subagents[0].gone);
    }
}
