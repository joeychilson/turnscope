//! Codex.
//!
//! Each thread is written to JSON Lines rollouts,
//! `~/.codex/sessions/<yyyy>/<mm>/<dd>/rollout-<time>-<thread>.jsonl`, moved to
//! `archived_sessions/` when archived. Every line is `{timestamp, ordinal,
//! type, payload}`, and a rollout opens with its thread's `session_meta`. A
//! resumed thread goes on in a new rollout. `session_index.jsonl` names
//! threads. A second account's folder is `~/.codex-<name>`, used through
//! `CODEX_HOME`, once signed in there.
//!
//! What follows was measured on this Mac's history, September and October 2026.
//!
//! **Usage.** Codex writes a `token_usage_record` per response, with a unique
//! `response_id`, just before the `token_count` event that brings the
//! thread's running total up to date. Each record is counted, and a running
//! total's growth only beyond the records written since the total before it:
//! in a rollout with records that is nothing (48,823 of 49,260 totals), but a
//! rollout written before records existed is counted from its totals alone.
//! A compaction's request is recorded and left out of the total, which then
//! grows by less than its records. `input_tokens` includes cached input, and
//! `output_tokens` reasoning.
//!
//! **Subagents.** A spawned subagent's rollout opens with a copy of its
//! parent's history, up to `subagent_history_start_ordinal`: its totals,
//! prompts and times are the parent's, so they're passed over. Records always
//! belong to the rollout they're in.
//!
//! **Reviews** (`thread_source` `guardian_review`) are Codex asking a model
//! whether to approve an action, handing it the whole transcript. They do none
//! of the work and are read as nothing.
//!
//! **Models.** Records name no model: each is counted under the latest
//! `turn_context`'s or `thread_settings_applied`'s, with its service tier. A
//! response before any model is named waits, in the cursor, for the first.
//!
//! **Conversations.** A message sent as the person's comes in parts, its
//! context (`AGENTS.md`, the environment, a goal carried on) in parts of
//! their own, some tags with attributes. Nearly every tool call is `exec`, a
//! script calling Codex's tools (`tools.apply_patch`, `tools.exec_command`),
//! whose patches name the files they change; a script that failed says so
//! as its output opens. A thread resumed goes on in rollouts named
//! `…_<page>.jsonl`, each opening with a `session_meta` whose `history_base`
//! says it's a later page.
//!
//! **Not shown.** Codex encrypts a compaction's summary, so its
//! conversations have none.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use super::{
    Agent, Conversation, Credential, Entry, Info, Kind, Link, Login, Parent, Read, Response,
    Secret, Title, Tokens,
};
use crate::{Error, Result};

pub struct Codex;

static INFO: Info = Info {
    id: "codex",
    name: "Codex",
    command: "codex",
    resume: "codex resume {id}",
    mcp_add: &["mcp", "add", "turnscope", "--"],
    mcp_remove: Some(&["mcp", "remove", "turnscope"]),
    folder_var: Some("CODEX_HOME"),
    charges: false,
    session_var: None,
    cwd_var: None,
};

/// What Codex puts in a message sent as the person's: its context, a
/// skill's instructions, an image's markers.
const INJECTED: &[&str] = &[
    "codex_internal_context",
    "environment_context",
    "external_codex_apps_open_page",
    "image",
    "in-app-browser-context",
    "recommended_plugins",
    "send_user_message_question_reply",
    "skill",
    "turn_aborted",
];

#[derive(Default, Serialize, Deserialize)]
struct Cursor {
    offset: u64,
    /// The rollout's thread, from its own `session_meta`; any later one is
    /// its parent's.
    thread: Option<String>,
    review: bool,
    provider: Option<String>,
    /// Where a spawned subagent's own history starts, after its parent's.
    copied_until: Option<i64>,
    model: Option<String>,
    priority: bool,
    /// The running total as last seen.
    total: Option<Usage>,
    /// What the records since that total counted.
    recorded: Usage,
    titled: bool,
    /// Responses before any model was named.
    waiting: Vec<Response>,
}

#[derive(Deserialize, Serialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(default)]
struct Usage {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
}

impl Usage {
    fn each(&self) -> [u64; 6] {
        [
            self.input_tokens,
            self.cached_input_tokens,
            self.cache_write_input_tokens,
            self.output_tokens,
            self.reasoning_output_tokens,
            self.total_tokens,
        ]
    }

    fn of(each: [u64; 6]) -> Usage {
        let [
            input_tokens,
            cached_input_tokens,
            cache_write_input_tokens,
            output_tokens,
            reasoning_output_tokens,
            total_tokens,
        ] = each;
        Usage {
            input_tokens,
            cached_input_tokens,
            cache_write_input_tokens,
            output_tokens,
            reasoning_output_tokens,
            total_tokens,
        }
    }

    /// These counts less `other`'s, or `None` where one would fall below zero.
    fn minus(&self, other: &Usage) -> Option<Usage> {
        let mut each = self.each();
        for (count, less) in each.iter_mut().zip(other.each()) {
            *count = count.checked_sub(less)?;
        }
        Some(Usage::of(each))
    }

    fn plus(&self, other: &Usage) -> Usage {
        let mut each = self.each();
        for (count, more) in each.iter_mut().zip(other.each()) {
            *count += more;
        }
        Usage::of(each)
    }

    /// Codex's input includes what was read from and written to the cache.
    fn tokens(&self) -> Option<Tokens> {
        let cached = self.cached_input_tokens + self.cache_write_input_tokens;
        Some(Tokens {
            input: self.input_tokens.checked_sub(cached)?,
            cache_read: self.cached_input_tokens,
            cache_write_5m: self.cache_write_input_tokens,
            cache_write_1h: 0,
            output: self.output_tokens,
            reasoning: self.reasoning_output_tokens,
        })
    }
}

#[derive(Deserialize)]
struct Line<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    timestamp: Option<Cow<'a, str>>,
    ordinal: Option<i64>,
    #[serde(borrow)]
    payload: Option<Payload<'a>>,
}

impl Line<'_> {
    fn at(&self) -> Option<i64> {
        self.timestamp.as_deref().and_then(crate::time::millis)
    }
}

/// The payload fields of every line kind read, in one pass over the line;
/// objects are held unparsed until their kind is known.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Payload<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    // session_meta
    id: Option<String>,
    cwd: Option<String>,
    #[serde(borrow)]
    git: Option<&'a RawValue>,
    model_provider: Option<String>,
    parent_thread_id: Option<String>,
    forked_from_id: Option<String>,
    subagent_history_start_ordinal: Option<i64>,
    /// A later page of a thread's: where its history before begins.
    #[serde(borrow)]
    history_base: Option<&'a RawValue>,
    agent_nickname: Option<String>,
    thread_source: Option<String>,
    // turn_context, and thread_settings_applied's `thread_settings`
    model: Option<String>,
    service_tier: Option<String>,
    #[serde(borrow)]
    thread_settings: Option<&'a RawValue>,
    // token_count
    #[serde(borrow)]
    info: Option<&'a RawValue>,
    // token_usage_record
    response_id: Option<String>,
    thread_id: Option<String>,
    #[serde(borrow)]
    usage: Option<&'a RawValue>,
    // response_item
    #[serde(borrow)]
    role: Option<Cow<'a, str>>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}

/// A `turn_context`, or a `thread_settings_applied` event's settings.
#[derive(Deserialize)]
struct Settings {
    model: Option<String>,
    service_tier: Option<String>,
}

#[derive(Deserialize)]
struct Totals {
    total_token_usage: Usage,
    last_token_usage: Option<Usage>,
}

/// The text of a message's content: a string, or its parts' text.
fn texts(content: Option<&RawValue>) -> Vec<String> {
    #[derive(Deserialize)]
    struct Part {
        text: Option<String>,
    }
    let Some(content) = content else {
        return Vec::new();
    };
    if let Ok(text) = serde_json::from_str::<String>(content.get()) {
        return vec![text];
    }
    serde_json::from_str::<Vec<Part>>(content.get())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|part| part.text)
        .collect()
}

fn parsed<'a, T: Deserialize<'a>>(raw: Option<&'a RawValue>) -> Option<T> {
    serde_json::from_str(raw?.get()).ok()
}

/// What the person typed in a message Codex sent as theirs: without its
/// context, and of pasted files, only their words after `## My request:`.
fn typed(text: &str) -> String {
    if text.starts_with("# AGENTS.md instructions") {
        return String::new();
    }
    let text = text
        .rsplit_once("## My request:")
        .map_or(text, |(_, request)| request);
    super::without_tags(text, INJECTED)
}

fn id(native: &str) -> String {
    format!("codex:{native}")
}

impl Agent for Codex {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn folders(&self, home: &Path) -> Vec<PathBuf> {
        super::own_and_found(home, ".codex", ".codex-", "auth.json")
    }

    fn files(&self, folder: &Path) -> Vec<PathBuf> {
        fn rollouts(dir: &Path, files: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("rollout-") && name.ends_with(".jsonl") {
                    files.push(path);
                } else if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    rollouts(&path, files);
                }
            }
        }
        let mut files = vec![folder.join("session_index.jsonl")];
        rollouts(&folder.join("sessions"), &mut files);
        rollouts(&folder.join("archived_sessions"), &mut files);
        files
    }

    fn read(&self, file: &Path, cursor: &str) -> Result<Read> {
        let mut cursor: Cursor = serde_json::from_str(cursor).unwrap_or_default();
        let mut read = Read::default();
        if file
            .file_name()
            .is_some_and(|name| name == "session_index.jsonl")
        {
            // A line per naming, the last standing.
            #[derive(Deserialize)]
            struct Named {
                id: String,
                thread_name: String,
            }
            cursor.offset =
                super::lines(file, cursor.offset, |bytes| {
                    match serde_json::from_slice::<Named>(bytes) {
                        Ok(named) => read
                            .session(&id(&named.id))
                            .title(Title::Generated, &named.thread_name),
                        Err(_) => read.skipped += 1,
                    }
                })?;
        } else {
            cursor.offset =
                super::lines(file, cursor.offset, |bytes| {
                    match serde_json::from_slice::<Line>(bytes) {
                        Ok(line) if !cursor.review => read_line(&line, &mut cursor, &mut read),
                        Ok(_) => {}
                        Err(_) => read.skipped += 1,
                    }
                })?;
        }
        read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
        Ok(read)
    }

    fn transcript(&self, _native: &str, files: &[PathBuf]) -> Result<Vec<Entry>> {
        // A thread's rollouts, oldest first by the time in their names.
        let mut rollouts: Vec<&PathBuf> = files
            .iter()
            .filter(|file| {
                file.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("rollout-"))
            })
            .collect();
        rollouts.sort_by_key(|file| file.file_name());
        if rollouts.is_empty() {
            return Err(Error::NotFound("the thread's rollouts are gone".to_owned()));
        }
        let mut conversation = Conversation::default();
        for rollout in rollouts {
            let mut copied_until = None;
            let mut opened = false;
            super::lines(rollout, 0, |bytes| {
                let Ok(line) = serde_json::from_slice::<Line>(bytes) else {
                    return;
                };
                if line.kind == "session_meta" {
                    if !opened {
                        opened = true;
                        copied_until = line.payload.as_ref().and_then(|meta| {
                            meta.parent_thread_id
                                .as_ref()
                                .and(meta.subagent_history_start_ordinal)
                        });
                    }
                    return;
                }
                if line.kind != "response_item"
                    || line
                        .ordinal
                        .zip(copied_until)
                        .is_some_and(|(at, until)| at < until)
                {
                    return;
                }
                converse(&line, bytes, &mut conversation);
            })?;
        }
        Ok(conversation.entries)
    }

    fn logins(&self, folder: &Path, _home: &Path) -> Result<Vec<Login>> {
        let kept = super::json_file(&folder.join("auth.json"))?.unwrap_or_default();
        let text = |pointer: &str| {
            kept.pointer(pointer)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
        };
        let (secret, key) = match kept["auth_mode"].as_str() {
            Some("chatgpt") => (text("/tokens/access_token"), false),
            Some("apikey") => (text("/OPENAI_API_KEY"), true),
            _ => (None, false),
        };
        Ok(vec![Login {
            provider: "openai".to_owned(),
            credential: secret.map(|secret| Credential {
                provider: "openai",
                expires: super::claims(secret)["exp"].as_i64().map(|exp| exp * 1000),
                secret: Secret::new(secret),
                key,
                identity: None,
                plan: None,
            }),
        }])
    }

    fn mcp_command(&self, folder: &Path, _home: &Path) -> Option<String> {
        super::toml_mcp_command(&folder.join("config.toml"))
    }
}

fn read_line(line: &Line, cursor: &mut Cursor, read: &mut Read) {
    let Some(payload) = &line.payload else { return };
    if line.kind == "session_meta" {
        if cursor.thread.is_none() {
            open(payload, line.at(), cursor, read);
        }
        return;
    }
    let Some(thread) = cursor.thread.clone() else {
        return;
    };
    let copied = line
        .ordinal
        .zip(cursor.copied_until)
        .is_some_and(|(at, until)| at < until);
    match (&*line.kind, payload.kind.as_deref()) {
        ("turn_context", _) if !copied => {
            let session = read.session(&thread);
            session.cwd = session.cwd.take().or(payload.cwd.clone());
            let settings = Settings {
                model: payload.model.clone(),
                service_tier: payload.service_tier.clone(),
            };
            apply(settings, cursor, read);
        }
        ("event_msg", Some("token_count")) => {
            if let Some(totals) = parsed(payload.info) {
                total(totals, line, &thread, copied, cursor, read);
            }
        }
        ("event_msg", Some("thread_settings_applied")) if !copied => {
            if let Some(settings) = parsed(payload.thread_settings) {
                apply(settings, cursor, read);
            }
        }
        ("token_usage_record", _) => {
            let (Some(response), Some(usage)) =
                (&payload.response_id, parsed::<Usage>(payload.usage))
            else {
                read.skipped += 1;
                return;
            };
            if !copied {
                cursor.recorded = cursor.recorded.plus(&usage);
            }
            let session = payload.thread_id.as_deref().map_or(thread.clone(), id);
            respond(
                response.clone(),
                session,
                usage,
                usage.input_tokens,
                line,
                cursor,
                read,
            );
        }
        ("response_item", Some("message")) if !copied => match payload.role.as_deref() {
            Some("user") => {
                for text in texts(payload.content) {
                    let text = typed(&text);
                    read.say(&thread, &text);
                    if !cursor.titled && !text.is_empty() {
                        read.session(&thread).title(Title::Prompt, &text);
                        cursor.titled = true;
                    }
                }
            }
            Some("assistant") => read.say(&thread, &texts(payload.content).join("\n")),
            _ => {}
        },
        _ => {}
    }
    if let (false, Some(at)) = (copied, line.at()) {
        read.session(&thread).saw(at);
    }
}

/// Take in the rollout's own `session_meta`.
fn open(meta: &Payload, at: Option<i64>, cursor: &mut Cursor, read: &mut Read) {
    if meta.thread_source.as_deref() == Some("guardian_review") {
        cursor.review = true;
        return;
    }
    let Some(native) = &meta.id else { return };
    let thread = id(native);
    cursor.provider = meta.model_provider.clone();
    cursor.copied_until = meta
        .parent_thread_id
        .as_ref()
        .and(meta.subagent_history_start_ordinal);
    let session = read.session(&thread);
    session.parent = match (&meta.parent_thread_id, &meta.forked_from_id) {
        (Some(parent), _) => Some(Parent {
            id: id(parent),
            link: Link::Subagent,
        }),
        (None, Some(fork)) if fork != native => Some(Parent {
            id: id(fork),
            link: Link::Fork,
        }),
        _ => None,
    };
    if let Some(at) = at {
        session.saw(at);
    }
    session.cwd = meta.cwd.clone();
    #[derive(Deserialize)]
    struct Git {
        branch: Option<String>,
    }
    session.branch = parsed::<Git>(meta.git).and_then(|git| git.branch);
    if let Some(nickname) = &meta.agent_nickname {
        session.title(Title::Task, nickname);
    }
    // A thread's first request is on its first page.
    cursor.titled = meta.history_base.is_some();
    cursor.thread = Some(thread);
}

/// Put the model and service tier `settings` name in force, and count the
/// responses waiting for a model under it.
fn apply(settings: Settings, cursor: &mut Cursor, read: &mut Read) {
    if let Some(tier) = &settings.service_tier {
        cursor.priority = tier == "priority";
    }
    if let Some(model) = settings.model {
        for mut waiting in cursor.waiting.drain(..) {
            waiting.model = model.clone();
            read.respond(waiting);
        }
        cursor.model = Some(model);
    }
}

/// Count what a running total grew by beyond the records written since the
/// total before it.
fn total(
    info: Totals,
    line: &Line,
    thread: &str,
    copied: bool,
    cursor: &mut Cursor,
    read: &mut Read,
) {
    let now = info.total_token_usage;
    let before = cursor.total.replace(now);
    let recorded = std::mem::take(&mut cursor.recorded);
    // A subagent's copied totals are its parent's, which its own grow from.
    if copied {
        return;
    }
    let growth = match before {
        // A rollout's first total is all it has used, unless it took over its
        // thread's, as a resumed thread's may: then its request used what it
        // says it last used.
        None => info
            .last_token_usage
            .filter(|last| last.total_tokens <= now.total_tokens)
            .unwrap_or(now),
        // Within a rollout a total falls to what its last request used alone.
        Some(before) if now.total_tokens < before.total_tokens => now,
        Some(before) => match now.minus(&before) {
            Some(growth) => growth,
            None => return,
        },
    };
    let Some(growth) = growth
        .minus(&recorded)
        .filter(|growth| growth.total_tokens > 0)
    else {
        return;
    };
    let Some(stamp) = &line.timestamp else {
        return;
    };
    // An older rollout's lines have no ordinal; their times tell them apart.
    let ordinal = line
        .ordinal
        .map_or(String::new(), |ordinal| format!("{ordinal}:"));
    // The latest request's prompt, which sets the tier it was charged at.
    let prompt = info
        .last_token_usage
        .map_or(growth.input_tokens, |last| last.input_tokens);
    respond(
        format!("{thread}:{ordinal}{stamp}"),
        thread.to_owned(),
        growth,
        prompt,
        line,
        cursor,
        read,
    );
}

/// Record usage under the model and provider in force, or before any model is
/// named, under none, and keep it waiting for the first.
fn respond(
    id: String,
    session: String,
    usage: Usage,
    prompt: u64,
    line: &Line,
    cursor: &mut Cursor,
    read: &mut Read,
) {
    let (Some(tokens), Some(at)) = (usage.tokens(), line.at()) else {
        read.skipped += 1;
        return;
    };
    let response = Response {
        id,
        session,
        copy: false,
        at,
        provider: cursor.provider.clone().unwrap_or("openai".to_owned()),
        model: cursor.model.clone().unwrap_or_default(),
        tokens,
        prompt,
        web_searches: 0,
        priority: cursor.priority,
        cost: None,
    };
    if cursor.model.is_none() && cursor.waiting.len() < 64 {
        cursor.waiting.push(response.clone());
    }
    read.respond(response);
}

fn converse(line: &Line, bytes: &[u8], conversation: &mut Conversation) {
    #[derive(Deserialize)]
    struct Full {
        #[serde(rename = "type")]
        kind: Option<String>,
        role: Option<String>,
        content: Option<Value>,
        summary: Option<Value>,
        name: Option<String>,
        namespace: Option<String>,
        input: Option<Value>,
        arguments: Option<Value>,
        call_id: Option<String>,
        output: Option<Value>,
    }
    #[derive(Deserialize)]
    struct Whole {
        payload: Full,
    }
    let Ok(Whole { payload: item }) = serde_json::from_slice::<Whole>(bytes) else {
        return;
    };
    let at = line.at();
    // A string, a list of parts with text, or an output's `{content}`.
    let text = |value: &Option<Value>| match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Object(fields)) => match fields.get("content") {
            Some(Value::String(text)) => text.clone(),
            Some(content) => content.to_string(),
            None => Value::Object(fields.clone()).to_string(),
        },
        Some(other) => other.to_string(),
        None => String::new(),
    };
    match (item.kind.as_deref(), item.role.as_deref()) {
        // What the person typed, part by part, as Codex sends its context in
        // parts of its own beside theirs.
        (Some("message"), Some("user")) => {
            let parts = match &item.content {
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .map(typed)
                    .collect(),
                content => vec![typed(&text(content))],
            };
            let said: Vec<String> = parts.into_iter().filter(|part| !part.is_empty()).collect();
            if said.is_empty() {
                conversation.say(Kind::System, at, &text(&item.content));
            } else {
                conversation.say(Kind::User, at, &said.join("\n"));
            }
        }
        (Some("message"), Some("assistant")) => {
            conversation.say(Kind::Assistant, at, &text(&item.content))
        }
        (Some("message" | "agent_message"), _) => {
            conversation.say(Kind::System, at, &text(&item.content))
        }
        // Only the readable summary; the rest is encrypted.
        (Some("reasoning"), _) => conversation.say(Kind::Reasoning, at, &text(&item.summary)),
        (Some("function_call" | "custom_tool_call"), _) => {
            let input = item
                .input
                .or(item.arguments)
                .map(|input| match input {
                    Value::String(text) => text,
                    other => other.to_string(),
                })
                .unwrap_or_default();
            let name = match (&item.namespace, &item.name) {
                (Some(namespace), Some(name)) => format!("{namespace}.{name}"),
                // A script that calls Codex's tools, named by those it calls.
                (_, Some(name)) if name == "exec" => {
                    let mut called: Vec<&str> = Vec::new();
                    for (at, _) in input.match_indices("tools.") {
                        let rest = &input[at + "tools.".len()..];
                        let end = rest
                            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                            .unwrap_or(rest.len());
                        if end > 0 && !called.contains(&&rest[..end]) {
                            called.push(&rest[..end]);
                        }
                    }
                    if called.is_empty() {
                        name.clone()
                    } else {
                        format!("exec: {}", called.join(", "))
                    }
                }
                (_, name) => name.clone().unwrap_or("tool".to_owned()),
            };
            let files = super::patched(&input);
            conversation.call(item.call_id.as_deref(), at, &name, input);
            conversation.changed(files);
        }
        (Some("function_call_output" | "custom_tool_call_output"), _) => {
            if let Some(call) = &item.call_id {
                let (output, failed) = script(&text(&item.output));
                conversation.answer(call, output, failed);
            }
        }
        _ => {}
    }
}

/// What a tool's output says, without the wrapper of an `exec` script's:
/// each command's own output, as it printed it, and whether the script or
/// a command in it failed. Codex marks no other failure. A script's output
/// opens with a header, then a JSON chunk a command, its output escaped,
/// or the outcome of a promise holding one (measured 2026-10-09).
fn script(output: &str) -> (String, bool) {
    let mut failed = output.starts_with("Script failed") || output.starts_with("aborted by user");
    let mut said = Vec::new();
    for line in output.lines() {
        if line.starts_with("Script completed")
            || line.starts_with("Wall time")
            || line == "Output:"
        {
            continue;
        }
        let parsed: Option<Value> = serde_json::from_str(line).ok();
        failed |= parsed
            .as_ref()
            .is_some_and(|value| value["status"] == "rejected");
        let chunk = parsed
            .as_ref()
            .and_then(|value| match value.get("chunk_id") {
                Some(_) => Some(value),
                None => value
                    .get("value")
                    .filter(|value| value.get("chunk_id").is_some()),
            });
        let Some(chunk) = chunk else {
            said.push(line.to_owned());
            continue;
        };
        let printed = chunk["output"].as_str().unwrap_or_default().trim_end();
        match chunk["exit_code"].as_i64() {
            Some(code) if code != 0 => {
                failed = true;
                said.push(format!("exit {code}:\n{printed}"));
            }
            _ if !printed.is_empty() => said.push(printed.to_owned()),
            _ => {}
        }
    }
    (said.join("\n"), failed)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn rollout(dir: &Path, name: &str, lines: &[Value]) -> PathBuf {
        let file = dir.join(format!("sessions/2026/09/30/rollout-{name}.jsonl"));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            lines
                .iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>(),
        )
        .unwrap();
        file
    }

    fn line(ordinal: i64, kind: &str, payload: Value) -> Value {
        json!({"timestamp": format!("2026-09-30T09:00:{ordinal:02}Z"), "ordinal": ordinal, "type": kind, "payload": payload})
    }

    fn usage(input: u64, cached: u64, output: u64) -> Value {
        json!({"input_tokens": input, "cached_input_tokens": cached, "cache_write_input_tokens": 0,
            "output_tokens": output, "reasoning_output_tokens": output / 2, "total_tokens": input + output})
    }

    fn total(ordinal: i64, total: Value, last: Value) -> Value {
        line(
            ordinal,
            "event_msg",
            json!({"type": "token_count", "info": {"total_token_usage": total, "last_token_usage": last}}),
        )
    }

    #[test]
    fn a_scripts_output_is_its_commands_and_a_failing_one_fails_it() {
        let output = "Script completed\nWall time 0.4 seconds\nOutput:\n\
            {\"chunk_id\":\"a1\",\"wall_time_seconds\":0.1,\"exit_code\":0,\"original_token_count\":3,\"output\":\"src/lib.rs\\n\"}\n\
            {\"i\":1,\"status\":\"fulfilled\",\"value\":{\"chunk_id\":\"b2\",\"exit_code\":101,\"output\":\"test failed\\n\"}}";
        assert_eq!(
            script(output),
            ("src/lib.rs\nexit 101:\ntest failed".to_owned(), true)
        );
        assert_eq!(script("Edited"), ("Edited".to_owned(), false));
    }

    #[test]
    fn records_are_counted_and_totals_only_beyond_them() {
        let dir = tempfile::tempdir().unwrap();
        let file = rollout(
            dir.path(),
            "t1",
            &[
                line(
                    0,
                    "session_meta",
                    json!({"id": "t1", "cwd": "/work/api", "git": {"branch": "main"}}),
                ),
                line(
                    1,
                    "turn_context",
                    json!({"model": "gpt-5.5", "service_tier": "priority"}),
                ),
                line(
                    2,
                    "response_item",
                    json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "<environment_context>x</environment_context>Add retries."}]}),
                ),
                line(
                    3,
                    "token_usage_record",
                    json!({"response_id": "resp_1", "usage": usage(1000, 600, 40)}),
                ),
                total(4, usage(1000, 600, 40), usage(1000, 600, 40)),
                // A compaction's request: recorded, and left out of the total.
                line(
                    5,
                    "token_usage_record",
                    json!({"response_id": "resp_2", "usage": usage(500, 0, 10)}),
                ),
                total(6, usage(1000, 600, 40), usage(0, 0, 0)),
            ],
        );
        let read = Codex.read(&file, "").unwrap();
        assert_eq!(read.responses.len(), 2, "no growth beyond the records");
        let first = &read.responses["resp_1"];
        assert_eq!(
            first.tokens,
            Tokens {
                input: 400,
                cache_read: 600,
                cache_write_5m: 0,
                cache_write_1h: 0,
                output: 40,
                reasoning: 20
            }
        );
        assert_eq!(
            (first.model.as_str(), first.priority, first.session.as_str()),
            ("gpt-5.5", true, "codex:t1")
        );
        let session = &read.sessions["codex:t1"];
        assert_eq!(
            (session.cwd.as_deref(), session.branch.as_deref()),
            (Some("/work/api"), Some("main"))
        );
        assert_eq!(session.title.as_ref().unwrap().1, "Add retries.");
    }

    #[test]
    fn a_rollout_before_records_is_counted_from_its_totals() {
        let dir = tempfile::tempdir().unwrap();
        let file = rollout(
            dir.path(),
            "t1",
            &[
                line(0, "session_meta", json!({"id": "t1"})),
                line(1, "turn_context", json!({"model": "gpt-5.5"})),
                total(2, usage(1000, 0, 10), usage(1000, 0, 10)),
                total(3, usage(3000, 1500, 30), usage(2000, 1500, 20)),
            ],
        );
        let read = Codex.read(&file, "").unwrap();
        let mut growth: Vec<(u64, u64)> = read
            .responses
            .values()
            .map(|response| (response.tokens.input, response.prompt))
            .collect();
        growth.sort();
        // The second grew by 2,000 input, 1,500 of it cached, at the
        // latest request's prompt.
        assert_eq!(growth, [(500, 2000), (1000, 1000)]);
    }

    #[test]
    fn a_subagents_copy_of_its_parents_totals_and_a_review_count_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let subagent = rollout(
            dir.path(),
            "t2",
            &[
                line(
                    0,
                    "session_meta",
                    json!({"id": "t2", "parent_thread_id": "t1", "subagent_history_start_ordinal": 3}),
                ),
                line(1, "turn_context", json!({"model": "gpt-5.5"})),
                total(2, usage(9000, 0, 90), usage(9000, 0, 90)),
                total(4, usage(9500, 0, 95), usage(500, 0, 5)),
            ],
        );
        let read = Codex.read(&subagent, "").unwrap();
        let counted: Vec<u64> = read
            .responses
            .values()
            .map(|response| response.tokens.input)
            .collect();
        assert_eq!(counted, [500]);
        assert!(
            read.sessions["codex:t2"]
                .parent
                .as_ref()
                .is_some_and(|parent| parent.id == "codex:t1")
        );
        let review = rollout(
            dir.path(),
            "t3",
            &[
                line(
                    0,
                    "session_meta",
                    json!({"id": "t3", "thread_source": "guardian_review"}),
                ),
                line(
                    1,
                    "token_usage_record",
                    json!({"response_id": "resp_9", "usage": usage(100, 0, 1)}),
                ),
            ],
        );
        assert!(Codex.read(&review, "").unwrap().responses.is_empty());
    }

    #[test]
    fn a_conversation_keeps_the_persons_words_and_names_what_a_script_did() {
        let dir = tempfile::tempdir().unwrap();
        let script = "await tools.apply_patch(\"*** Begin Patch\\n*** Update File: src/lib.rs\\n*** End Patch\"); await tools.exec_command({cmd: \"cargo test\"});";
        let file = rollout(
            dir.path(),
            "t1",
            &[
                line(0, "session_meta", json!({"id": "t1"})),
                line(
                    1,
                    "response_item",
                    json!({"type": "message", "role": "user", "content": [
                        {"type": "input_text", "text": "# AGENTS.md instructions for /work\n\nBe brief."},
                        {"type": "input_text", "text": "<environment_context>x</environment_context>"},
                        {"type": "input_text", "text": "Fix the parser."}]}),
                ),
                line(
                    2,
                    "response_item",
                    json!({"type": "custom_tool_call", "name": "exec", "call_id": "c1", "input": script}),
                ),
                line(
                    3,
                    "response_item",
                    json!({"type": "custom_tool_call_output", "call_id": "c1", "output": "Script failed: exit 101"}),
                ),
            ],
        );
        let entries = Codex.transcript("t1", &[file]).unwrap();
        assert_eq!(
            (entries[0].kind, entries[0].text.as_str()),
            (Kind::User, "Fix the parser.")
        );
        let tool = entries[1].tool.as_ref().unwrap();
        assert_eq!(
            (tool.name.as_str(), tool.failed, tool.files.as_slice()),
            (
                "exec: apply_patch, exec_command",
                true,
                ["src/lib.rs".to_owned()].as_slice()
            )
        );
    }

    #[test]
    fn usage_before_a_model_is_named_waits_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = rollout(
            dir.path(),
            "t1",
            &[
                line(0, "session_meta", json!({"id": "t1"})),
                line(
                    1,
                    "token_usage_record",
                    json!({"response_id": "resp_1", "usage": usage(100, 0, 1)}),
                ),
            ],
        );
        let first = Codex.read(&file, "").unwrap();
        assert_eq!(first.responses["resp_1"].model, "");
        let mut text = std::fs::read_to_string(&file).unwrap();
        text += &format!("{}\n", line(2, "turn_context", json!({"model": "gpt-5.5"})));
        std::fs::write(&file, text).unwrap();
        let later = Codex.read(&file, &first.cursor).unwrap();
        assert_eq!(later.responses["resp_1"].model, "gpt-5.5");
    }
}
