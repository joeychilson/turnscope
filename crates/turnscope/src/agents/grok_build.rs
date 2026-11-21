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
//! counts its subagents' turns that ended in it, under the same models: in
//! every one of 13 cases, the parent's own model calls and its finished
//! subagents' added up to its turn's. So a parent's turn is counted less its
//! subagents' turns that ended after its last turn did, and no later than it.
//! Chosen over taking off every turn its updates held of each subagent that
//! finished in it (`subagent_finished`): a subagent at work in two of its
//! parent's turns was taken off each of them whole, and one stopped before
//! it finished was taken off none. (Whether a stopped subagent's turns are
//! in its parent's wasn't measured.)
//!
//! The cost: a parent read again from its start, as after `READERS` is
//! raised, once a subagent's folder is gone, counts that subagent twice, as
//! its turns stay and its parent's turn comes out whole. Chosen over
//! counting a parent's turn whole and its subagents' turns never, which is
//! right in total but shows no subagent's usage of its own. It needs a
//! subagent's folder deleted before its parent's.
//!
//! **Parsing.** A line is parsed into a struct that borrows its text, with
//! what a reply streams held unparsed: 190 ms for a made-up 225 MiB of
//! streamed reply, against 420 ms parsed as a `Value` (November 2026).
//! Passing over a line that names none of the updates read, before parsing
//! it, took 57 ms, but would leave a line that can't be read uncounted.

use std::borrow::Cow;
use std::collections::HashMap;
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
    /// The subagents it spawned.
    subagents: Vec<String>,
    /// When its last turn ended, in Unix seconds.
    ended: Option<i64>,
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
    #[serde(default)]
    usage: Value,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}

impl<'a> Line<'a> {
    /// The update it brings, and when.
    fn update(self) -> Option<(Option<i64>, Update<'a>)> {
        Some((self.timestamp, self.params?.update?))
    }
}

impl Update<'_> {
    /// Each model's usage in a finished turn: `None` for one that can't be
    /// read.
    fn used(&self) -> impl Iterator<Item = (&String, Option<Used>)> {
        let models = self.usage["modelUsage"].as_object();
        models
            .into_iter()
            .flatten()
            .map(|(model, used)| (model, Used::of(used)))
    }
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
        let mut read = Read::default();
        let native = file
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str());
        let Some(native) = native else {
            return Ok(read);
        };
        let session = INFO.session(native);
        let mut cursor: Cursor = serde_json::from_str(cursor).unwrap_or_default();
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
                refresh: None,
            }),
        }])
    }

    fn mcp_command(&self, folder: &Path, _home: &Path) -> Option<String> {
        super::toml_mcp_command(&folder.join("config.toml"))
    }
}

fn summary(summary: &Value, facts: &mut Session) {
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

fn updates(file: &Path, session: &str, cursor: &mut Cursor, read: &mut Read) -> Result<()> {
    let mut subagents = Subagents::new(file.ancestors().nth(3));
    let mut failed = None;
    cursor.offset = super::lines(file, cursor.offset, |bytes| {
        if failed.is_some() {
            return;
        }
        let Ok(line) = serde_json::from_slice::<Line>(bytes) else {
            read.skipped += 1;
            return;
        };
        let Some((at, update)) = line.update() else {
            return;
        };
        match update.kind.as_deref() {
            Some("turn_completed") => {
                let Some(at) = at else { return };
                match subagents.used(&cursor.subagents, cursor.ended, at) {
                    Ok(theirs) => turn(&update, at, session, &theirs, read),
                    Err(error) => failed = Some(error),
                }
                cursor.ended = Some(at);
            }
            Some("subagent_spawned") => {
                if let Some(child) = update.child_session_id {
                    read.session(&INFO.session(&child)).parent = Some(session.to_owned());
                    if !cursor.subagents.iter().any(|known| *known == child) {
                        cursor.subagents.push(child.into_owned());
                    }
                }
            }
            Some("user_message_chunk") if !cursor.titled => {
                let content: Value = update
                    .content
                    .and_then(|content| serde_json::from_str(content.get()).ok())
                    .unwrap_or_default();
                let text = typed(content["text"].as_str().unwrap_or_default());
                if !text.is_empty() {
                    read.session(session).title(Title::Prompt, &text);
                    cursor.titled = true;
                }
            }
            _ => {}
        }
    })?;
    failed.map_or(Ok(()), Err)
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

/// A subagent's turn: when it ended, in Unix seconds, and what each model
/// used.
type Turn = (i64, Vec<(String, Used)>);

/// A session's subagents' turns, from their own updates in `sessions`:
/// each subagent's read once a read, however many of its parent's turns
/// end in it.
struct Subagents<'a> {
    sessions: Option<&'a Path>,
    folders: Option<Vec<PathBuf>>,
    turns: HashMap<String, Vec<Turn>>,
}

impl<'a> Subagents<'a> {
    fn new(sessions: Option<&'a Path>) -> Subagents<'a> {
        Subagents {
            sessions,
            folders: None,
            turns: HashMap::new(),
        }
    }

    /// What the turns of `subagents` that ended after `after` and no later
    /// than `until` used, model by model. Fails when a subagent's updates
    /// are there and can't be read, so its parent's turn is read again later
    /// rather than counted whole.
    fn used(
        &mut self,
        subagents: &[String],
        after: Option<i64>,
        until: i64,
    ) -> Result<Vec<(String, Used)>> {
        let mut used = Vec::new();
        for child in subagents {
            if !self.turns.contains_key(child) {
                let turns = self.read(child)?;
                self.turns.insert(child.clone(), turns);
            }
            let ended = |at: i64| after.is_none_or(|after| at > after) && at <= until;
            for (_, theirs) in self.turns[child].iter().filter(|(at, _)| ended(*at)) {
                used.extend(theirs.iter().cloned());
            }
        }
        Ok(used)
    }

    fn read(&mut self, child: &str) -> Result<Vec<Turn>> {
        let sessions = self.sessions;
        let folders = self
            .folders
            .get_or_insert_with(|| sessions.into_iter().flat_map(super::entries).collect());
        // One whose folder is gone takes nothing off.
        let Some(updates) = folders
            .iter()
            .map(|folder| folder.join(child).join("updates.jsonl"))
            .find(|updates| updates.is_file())
        else {
            return Ok(Vec::new());
        };
        let mut turns = Vec::new();
        super::lines(&updates, 0, |bytes| {
            let Some((Some(at), update)) = serde_json::from_slice::<Line>(bytes)
                .ok()
                .and_then(Line::update)
            else {
                return;
            };
            if update.kind.as_deref() == Some("turn_completed") {
                let used = update.used();
                let used = used.filter_map(|(model, used)| Some((model.clone(), used?)));
                turns.push((at, used.collect()));
            }
        })?;
        Ok(turns)
    }
}

/// Record a turn that ended at `at`, in Unix seconds: a response per model,
/// less what its `subagents` used in it.
fn turn(update: &Update, at: i64, session: &str, subagents: &[(String, Used)], read: &mut Read) {
    let Some(prompt) = &update.prompt_id else {
        return;
    };
    for (model, used) in update.used() {
        let Some(used) = used else {
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
        read.respond(Response {
            id: format!("{prompt}:{model}"),
            session: session.to_owned(),
            copy: false,
            at: at * 1000,
            provider: "xai".to_owned(),
            model: model.clone(),
            tokens: Tokens {
                input: uncached,
                cache_read: used.cache_read,
                cache_write_5m: used.cache_write,
                cache_write_1h: 0,
                output: used.output,
                reasoning: used.reasoning,
            },
            prompt: used.input.div_ceil(used.calls.max(1)),
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
        let spawned = |at: i64, child: &str| {
            update(
                at,
                json!({"sessionUpdate": "subagent_spawned", "child_session_id": child}),
            )
        };
        let finished = |at: i64, child: &str| {
            update(
                at,
                json!({"sessionUpdate": "subagent_finished", "child_session_id": child}),
            )
        };
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

    #[test]
    fn a_subagent_that_cant_be_read_fails_its_parents_read() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let file = updates(
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
            typed("<rules>Be brief.</rules><git_status>clean</git_status>Speed up the build."),
            "Speed up the build."
        );
    }
}
