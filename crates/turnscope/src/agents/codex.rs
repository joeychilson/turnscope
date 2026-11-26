//! Codex.
//!
//! A thread is written to rollouts,
//! `~/.codex/sessions/<yyyy>/<mm>/<dd>/rollout-<time>-<thread>.jsonl`, moved
//! to `archived_sessions/` when archived: JSON Lines of `{timestamp, ordinal,
//! type, payload}`, opening with the thread's `session_meta`.
//! `session_index.jsonl` names threads. A second account's folder is
//! `~/.codex-<name>`, used through `CODEX_HOME`, once it holds `auth.json`.
//!
//! Measured on this Mac's history, September and October 2026:
//!
//! - **Usage.** A `token_usage_record` per response, unique by
//!   `response_id`, comes just before the `token_count` that brings the
//!   thread's running total up to date. Records are counted, and a total's
//!   growth only beyond the records since the total before: none, in 48,823
//!   of 49,260 totals, so a rollout written before records existed is
//!   counted from its totals. A compaction's request is recorded and left
//!   out of the total.
//! - **Subagents.** A spawned subagent's rollout opens with a copy of its
//!   parent's history, up to `subagent_history_start_ordinal`. A record
//!   counts for the thread its `thread_id` names, or else the rollout's.
//! - **Reviews** (`thread_source` `guardian_review`) ask a model whether to
//!   approve an action, and are read as nothing.
//!
//! From Codex's code (openai/codex at `a9b0815`, read 2026-10-12):
//!
//! - **Rewinds.** A thread rewound to an earlier turn goes on in a new
//!   rollout, `rollout-<time>-<thread>_<rollout id>.jsonl`, whose
//!   `session_meta` has a `history_base` kept below `end_ordinal_exclusive`.
//!   What was rewound past was used, and stays counted.
//! - **Forks.** A fork of a paginated thread points at its parent's history
//!   with `history_base`; its own starts at `forked_from_ordinal_exclusive`.
//!   A fork of a legacy thread (`history_mode` `legacy`) copies it: the
//!   copy's records name the parent's thread, and nothing else in it is the
//!   fork's.
//! - **Limits.** A `token_count`'s `rate_limits` are the account's as of that
//!   response, as `RateLimitSnapshot` names them, `resets_at` in Unix seconds.

use std::borrow::Cow;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use super::{Agent, Credential, InBand, Info, Login, Read, Response, Secret, Title, Tokens};
use crate::Result;
use crate::providers;
use crate::time::MINUTE;

pub struct Codex;

static INFO: Info = Info {
    id: "codex",
    name: "Codex",
    command: "codex",
    app: None,
    mcp_add: &["mcp", "add", "turnscope", "--"],
    mcp_remove: Some(&["mcp", "remove", "turnscope"]),
    folder_var: Some("CODEX_HOME"),
    charges: false,
    session_var: None,
    login_file: Some("auth.json"),
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
    #[serde(flatten)]
    copy: Copy,
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

impl Cursor {
    /// The provider the rollout's responses and limits are of: the one its
    /// `session_meta` names, or else OpenAI's.
    fn provider(&self) -> String {
        self.provider.clone().unwrap_or("openai".to_owned())
    }
}

/// Where a rollout holds a copy of its parent's history.
#[derive(Default, Serialize, Deserialize)]
struct Copy {
    /// Where a spawned subagent's own history starts, after its parent's.
    copied_until: Option<i64>,
    legacy_fork: bool,
    /// Within a legacy fork's copy of its parent's history.
    copying: bool,
}

impl Copy {
    /// The copy in the rollout that the `session_meta` `meta` opens.
    fn of(meta: &Payload) -> Copy {
        Copy {
            copied_until: meta
                .parent_thread_id
                .as_ref()
                .and(meta.subagent_history_start_ordinal),
            legacy_fork: meta.legacy_fork(),
            copying: false,
        }
    }

    /// Whether `line`, with `payload`, in a rollout of `thread`, is of the
    /// copy. A legacy fork's copy runs from the parent's `session_meta` to
    /// the `thread_settings_applied` naming `thread`. A Codex before
    /// 2026-08-29 (openai/codex#41567) named no thread there, so the rest of
    /// such a fork reads as the copy until a current Codex resumes it.
    fn holds(&mut self, line: &Line<'_>, payload: &Payload<'_>, thread: &str) -> bool {
        if self.legacy_fork {
            let names = |native: &Option<Cow<'_, str>>| {
                native
                    .as_deref()
                    .is_some_and(|native| INFO.session(native) == thread)
            };
            match (&*line.kind, payload.kind.as_deref()) {
                ("session_meta", _) if !names(&payload.id) => self.copying = true,
                ("event_msg", Some("thread_settings_applied")) if names(&payload.thread_id) => {
                    self.copying = false
                }
                _ => {}
            }
        }
        self.copying
            || line
                .ordinal
                .zip(self.copied_until)
                .is_some_and(|(at, until)| at < until)
    }
}

/// The input, output and total counts are required, so a renamed one shows
/// in `doctor` as usage skipped. The others, which Codex added later, are 0
/// when left out.
#[derive(Deserialize, Serialize, Default, Clone, Copy, PartialEq, Eq)]
struct Usage {
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    cache_write_input_tokens: u64,
    output_tokens: u64,
    #[serde(default)]
    reasoning_output_tokens: u64,
    total_tokens: u64,
}

impl Usage {
    fn zip(&self, other: &Usage, f: fn(u64, u64) -> Option<u64>) -> Option<Usage> {
        Some(Usage {
            input_tokens: f(self.input_tokens, other.input_tokens)?,
            cached_input_tokens: f(self.cached_input_tokens, other.cached_input_tokens)?,
            cache_write_input_tokens: f(
                self.cache_write_input_tokens,
                other.cache_write_input_tokens,
            )?,
            output_tokens: f(self.output_tokens, other.output_tokens)?,
            reasoning_output_tokens: f(
                self.reasoning_output_tokens,
                other.reasoning_output_tokens,
            )?,
            total_tokens: f(self.total_tokens, other.total_tokens)?,
        })
    }

    fn minus(&self, other: &Usage) -> Option<Usage> {
        self.zip(other, u64::checked_sub)
    }

    fn plus(&self, other: &Usage) -> Usage {
        self.zip(other, u64::checked_add).unwrap_or(*self)
    }

    /// Codex's input includes what was read from and written to the cache,
    /// and its output reasoning.
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
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    cwd: Option<Cow<'a, str>>,
    #[serde(borrow)]
    model_provider: Option<Cow<'a, str>>,
    #[serde(borrow)]
    parent_thread_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    forked_from_id: Option<Cow<'a, str>>,
    forked_from_ordinal_exclusive: Option<i64>,
    #[serde(borrow)]
    history_mode: Option<Cow<'a, str>>,
    subagent_history_start_ordinal: Option<i64>,
    /// The history a rewind's or a paginated fork's rollout goes on from.
    #[serde(borrow)]
    history_base: Option<&'a RawValue>,
    #[serde(borrow)]
    agent_nickname: Option<Cow<'a, str>>,
    #[serde(borrow)]
    thread_source: Option<Cow<'a, str>>,
    // turn_context, and thread_settings_applied's `thread_settings`
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
    #[serde(borrow)]
    thread_settings: Option<&'a RawValue>,
    // token_count
    #[serde(borrow)]
    info: Option<&'a RawValue>,
    #[serde(borrow)]
    rate_limits: Option<&'a RawValue>,
    // token_usage_record, and thread_settings_applied's `thread_id`
    #[serde(borrow)]
    response_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    thread_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    usage: Option<&'a RawValue>,
    // response_item
    #[serde(borrow)]
    role: Option<Cow<'a, str>>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}

impl Payload<'_> {
    /// Whether this `session_meta` opens a fork of a legacy thread, which
    /// copies its parent's history; a subagent's copy ends at its ordinal.
    fn legacy_fork(&self) -> bool {
        self.parent_thread_id.is_none()
            && self.forked_from_id.is_some()
            && self.history_mode.as_deref() == Some("legacy")
    }
}

#[derive(Deserialize)]
struct Base {
    end_ordinal_exclusive: i64,
}

#[derive(Deserialize)]
struct Settings<'a> {
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
    #[serde(borrow)]
    service_tier: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct Totals {
    total_token_usage: Usage,
    last_token_usage: Option<Usage>,
}

#[derive(Deserialize)]
struct RateLimits {
    limit_id: Option<String>,
    limit_name: Option<String>,
    primary: Option<RateWindow>,
    secondary: Option<RateWindow>,
}

#[derive(Deserialize)]
struct RateWindow {
    used_percent: f64,
    window_minutes: Option<i64>,
    resets_at: Option<i64>,
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

impl Agent for Codex {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn folders(&self, home: &Path) -> Vec<PathBuf> {
        super::own_and_found(home, ".codex", "auth.json")
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
        // A name starts with the time its rollout was made, so this is
        // oldest first, and the folder a thread began in is stored before a
        // rewind's.
        files[1..].sort_by(|one, other| one.file_name().cmp(&other.file_name()));
        files
    }

    fn read(&self, file: &Path, cursor: &str) -> Result<Read> {
        let mut cursor: Cursor = serde_json::from_str(cursor).unwrap_or_default();
        let mut read = Read::default();
        if file
            .file_name()
            .is_some_and(|name| name == "session_index.jsonl")
        {
            #[derive(Deserialize)]
            struct Named<'a> {
                #[serde(borrow)]
                id: Cow<'a, str>,
                #[serde(borrow)]
                thread_name: Cow<'a, str>,
            }
            cursor.offset =
                super::lines(file, cursor.offset, |bytes| {
                    match serde_json::from_slice::<Named<'_>>(bytes) {
                        Ok(named) => read
                            .session(&INFO.session(&named.id))
                            .title(Title::Generated, &named.thread_name),
                        Err(_) => read.skipped += 1,
                    }
                })?;
        } else if !cursor.review {
            cursor.offset = super::try_lines(file, cursor.offset, |bytes| {
                match serde_json::from_slice::<Line<'_>>(bytes) {
                    Ok(line) => read_line(&line, &mut cursor, &mut read),
                    Err(_) => read.skipped += 1,
                }
                Ok(if cursor.review {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                })
            })?;
        }
        read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
        Ok(read)
    }

    fn logins(&self, folder: &Path, _home: &Path) -> Result<Vec<Login>> {
        let kept = super::json_file(&folder.join("auth.json"))?;
        let (secret, key) = match kept["auth_mode"].as_str() {
            Some("chatgpt") => (super::str_at(&kept, "/tokens/access_token"), false),
            Some("apikey") => (super::str_at(&kept, "/OPENAI_API_KEY"), true),
            _ => (None, false),
        };
        let identity = (!key)
            .then(|| {
                let id = super::str_at(&kept, "/tokens/account_id")?;
                let email = super::str_at(&kept, "/tokens/id_token").and_then(|token| {
                    super::non_empty(&super::claims(token)["email"]).map(str::to_owned)
                });
                Some((id.to_owned(), email))
            })
            .flatten();
        Ok(vec![Login {
            provider: "openai".to_owned(),
            credential: secret.map(|secret| Credential {
                provider: "openai",
                expires: (!key)
                    .then(|| super::claims(secret)["exp"].as_i64())
                    .flatten()
                    .filter(|exp| *exp > 0)
                    .and_then(|exp| exp.checked_mul(1000)),
                secret: Secret::new(secret),
                key,
                identity,
                plan: None,
                refresh: None,
            }),
        }])
    }

    fn mcp_command(&self, folder: &Path, _home: &Path) -> Option<String> {
        super::toml_mcp_command(&folder.join("config.toml"))
    }
}

fn read_line(line: &Line<'_>, cursor: &mut Cursor, read: &mut Read) {
    let empty = Payload::default();
    let payload = line.payload.as_ref().unwrap_or(&empty);
    let Some(thread) = cursor.thread.as_deref() else {
        match (&*line.kind, payload.kind.as_deref()) {
            ("session_meta", _) => open(payload, cursor, read),
            // Usage of a thread that no `session_meta` has named.
            ("token_usage_record", _) => read.skipped += 1,
            ("event_msg", Some("token_count")) if payload.info.is_some() => read.skipped += 1,
            _ => {}
        }
        return;
    };
    let copied = cursor.copy.holds(line, payload, thread);
    match (&*line.kind, payload.kind.as_deref()) {
        // Settings are taken from a copy too: its records were made under them.
        ("turn_context", _) => {
            if !copied {
                read.cwd(thread, payload.cwd.as_deref());
            }
            apply(payload.model.as_deref(), cursor, read);
        }
        // Before any usage, a count has no `info`.
        ("event_msg", Some("token_count")) => {
            if let Some(info) = payload.info {
                match serde_json::from_str(info.get()) {
                    Ok(totals) => total(totals, line, copied, cursor, read),
                    Err(_) => read.skipped += 1,
                }
            }
            if !copied
                && let Some(limits) = super::parsed::<RateLimits>(payload.rate_limits)
                && let Some(at) = line.at()
                && let Some(snapshot) = in_band(limits, cursor, at)
            {
                read.in_band(snapshot);
            }
        }
        // The only line with the tier, in a snapshot of every setting: fast
        // mode is `priority`, and once it's off there's no `service_tier`
        // (openai/codex at `rust-v0.172.0`).
        ("event_msg", Some("thread_settings_applied")) => {
            if let Some(settings) = super::parsed::<Settings>(payload.thread_settings) {
                cursor.priority = settings.service_tier.as_deref() == Some("priority");
                apply(settings.model.as_deref(), cursor, read);
            }
        }
        ("token_usage_record", _) => {
            let (Some(response), Some(usage)) = (
                payload.response_id.as_deref(),
                super::parsed::<Usage>(payload.usage),
            ) else {
                read.skipped += 1;
                return;
            };
            if !copied {
                cursor.recorded = cursor.recorded.plus(&usage);
            }
            let session = payload
                .thread_id
                .as_deref()
                .map_or_else(|| thread.to_owned(), |native| INFO.session(native));
            let prompt = usage.input_tokens;
            match report(response.to_owned(), session, copied, usage, prompt, line) {
                Some(report) => respond(report, cursor, read),
                None => read.skipped += 1,
            }
        }
        // The first thing the person asked titles the thread. Once it has,
        // what they say isn't read.
        ("response_item", Some("message"))
            if !copied && !cursor.titled && payload.role.as_deref() == Some("user") =>
        {
            let texts = payload.content.map(super::texts).unwrap_or_default();
            let asked = texts
                .iter()
                .map(|text| typed(text))
                .find(|text| !text.is_empty());
            if let Some(asked) = asked {
                read.session(thread).title(Title::Prompt, &asked);
                cursor.titled = true;
            }
        }
        _ => {}
    }
}

fn open(meta: &Payload<'_>, cursor: &mut Cursor, read: &mut Read) {
    if meta.thread_source.as_deref() == Some("guardian_review") {
        cursor.review = true;
        return;
    }
    let Some(native) = meta.id.as_deref() else {
        return;
    };
    let thread = INFO.session(native);
    cursor.provider = meta
        .model_provider
        .as_deref()
        .filter(|provider| !provider.is_empty())
        .map(str::to_owned);
    cursor.copy = Copy::of(meta);
    read.cwd(&thread, meta.cwd.as_deref());
    let session = read.session(&thread);
    // A subagent's parent. A fork names the thread it was forked from, and
    // is a thread of its own.
    session.parent = meta
        .parent_thread_id
        .as_deref()
        .map(|parent| INFO.session(parent));
    if let Some(nickname) = meta.agent_nickname.as_deref() {
        session.title(Title::Task, nickname);
    }
    // A rollout that goes on from the thread's own history, as a rewind's
    // does, doesn't hold its first request, so it titles nothing. A fork's
    // own history starts after its parent's.
    cursor.titled = super::parsed::<Base>(meta.history_base).is_some_and(|base| {
        base.end_ordinal_exclusive > meta.forked_from_ordinal_exclusive.unwrap_or(0)
    });
    cursor.thread = Some(thread);
}

fn apply(model: Option<&str>, cursor: &mut Cursor, read: &mut Read) {
    if let Some(model) = model.filter(|model| !model.is_empty()) {
        for mut waiting in cursor.waiting.drain(..) {
            waiting.model = model.to_owned();
            read.respond(waiting);
        }
        cursor.model = Some(model.to_owned());
    }
}

fn total(info: Totals, line: &Line<'_>, copied: bool, cursor: &mut Cursor, read: &mut Read) {
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
        None => Some(
            info.last_token_usage
                .filter(|last| last.total_tokens <= now.total_tokens)
                .unwrap_or(now),
        ),
        // Within a rollout a total falls to what its last request used alone.
        Some(before) if now.total_tokens < before.total_tokens => Some(now),
        Some(before) => now.minus(&before),
    };
    let Some(growth) = growth
        .and_then(|growth| growth.minus(&recorded))
        .filter(|growth| growth.total_tokens > 0)
    else {
        return;
    };
    let Some(stamp) = line.timestamp.as_deref() else {
        read.skipped += 1;
        return;
    };
    let Some(thread) = cursor.thread.as_deref() else {
        return;
    };
    // An older rollout's lines have no ordinal; their times tell them apart.
    let id = match line.ordinal {
        Some(ordinal) => format!("{thread}:{ordinal}:{stamp}"),
        None => format!("{thread}:{stamp}"),
    };
    let session = thread.to_owned();
    // The latest request's prompt, which sets the tier it was charged at.
    let prompt = info
        .last_token_usage
        .map_or(growth.input_tokens, |last| last.input_tokens);
    match report(id, session, false, growth, prompt, line) {
        Some(report) => respond(report, cursor, read),
        None => read.skipped += 1,
    }
}

fn report(
    id: String,
    session: String,
    copy: bool,
    usage: Usage,
    prompt: u64,
    line: &Line<'_>,
) -> Option<Response> {
    Some(Response {
        id,
        session,
        copy,
        at: line.at()?,
        provider: String::new(),
        model: String::new(),
        tokens: usage.tokens()?,
        prompt,
        web_searches: 0,
        priority: false,
        cost: None,
    })
}

/// The limits a `token_count` gives, keyed as the provider's answer keys
/// them, so each takes the place of the one read before: the plan's own
/// windows (`limit_id` `"codex"`, or none), or with a `limit_name` those on
/// some models. None of another limit with no name, which would be taken for
/// the plan's own.
fn in_band(limits: RateLimits, cursor: &Cursor, at: i64) -> Option<InBand> {
    let scope = match (limits.limit_name.as_deref(), limits.limit_id.as_deref()) {
        (_, Some("codex")) => None,
        (Some(name), _) if !name.is_empty() => Some(name),
        (_, None | Some("")) => None,
        _ => return None,
    };
    let readings: Vec<_> = [
        ("primary_window", limits.primary),
        ("secondary_window", limits.secondary),
    ]
    .into_iter()
    .filter_map(|(key, window)| {
        let window = window?;
        let length = window
            .window_minutes
            .filter(|minutes| *minutes > 0)?
            .checked_mul(MINUTE)?;
        let resets = window
            .resets_at
            .filter(|at| *at > 0)
            .and_then(|at| at.checked_mul(1000));
        Some(providers::window(
            key,
            scope,
            window.used_percent,
            length,
            resets,
        ))
    })
    .collect();
    (!readings.is_empty()).then(|| InBand {
        provider: cursor.provider(),
        at,
        readings,
    })
}

/// How many responses wait for a model at most, as the cursor holds them.
/// Past that, a response stays under no model, which has no price. (Not
/// measured: how many come before a rollout's first model.)
const WAITING: usize = 64;

/// Take in `response`. Records name no model, so it's under the model,
/// provider and tier in force, or before any model is named, under none, and
/// kept waiting for the first.
fn respond(mut response: Response, cursor: &mut Cursor, read: &mut Read) {
    response.provider = cursor.provider();
    response.model = cursor.model.clone().unwrap_or_default();
    response.priority = cursor.priority;
    if cursor.model.is_none() && cursor.waiting.len() < WAITING {
        cursor.waiting.push(response.clone());
    }
    read.respond(response);
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn rollout(dir: &Path, name: &str, lines: &[Value]) -> PathBuf {
        let file = dir.join(format!("sessions/2026/09/30/rollout-{name}.jsonl"));
        crate::agents::jsonl(file, lines)
    }

    fn line(ordinal: i64, kind: &str, payload: Value) -> Value {
        json!({"timestamp": format!("2026-09-30T09:00:{ordinal:02}Z"), "ordinal": ordinal, "type": kind, "payload": payload})
    }

    fn usage(input: u64, cached: u64, output: u64) -> Value {
        json!({"input_tokens": input, "cached_input_tokens": cached, "cache_write_input_tokens": 0,
            "output_tokens": output, "reasoning_output_tokens": output / 2, "total_tokens": input + output})
    }

    fn settings(ordinal: i64, settings: Value) -> Value {
        line(
            ordinal,
            "event_msg",
            json!({"type": "thread_settings_applied", "thread_settings": settings}),
        )
    }

    fn total(ordinal: i64, total: Value, last: Value) -> Value {
        line(
            ordinal,
            "event_msg",
            json!({"type": "token_count", "info": {"total_token_usage": total, "last_token_usage": last}}),
        )
    }

    fn message(role: &str, text: &str) -> Value {
        json!({"type": "message", "role": role, "content": [{"type": "input_text", "text": text}]})
    }

    #[test]
    fn records_are_counted_and_totals_only_beyond_them() {
        let dir = tempfile::tempdir().unwrap();
        let file = rollout(
            dir.path(),
            "t1",
            &[
                line(0, "session_meta", json!({"id": "t1", "cwd": "/work/api"})),
                settings(1, json!({"model": "gpt-5.5", "service_tier": "priority"})),
                line(
                    2,
                    "token_usage_record",
                    json!({"response_id": "resp_1", "usage": usage(1000, 600, 40)}),
                ),
                total(3, usage(1000, 600, 40), usage(1000, 600, 40)),
                // Fast mode turned off.
                settings(4, json!({"model": "gpt-5.5"})),
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
        assert!(!read.responses["resp_2"].priority);
        assert_eq!(read.sessions["codex:t1"].cwd.as_deref(), Some("/work/api"));
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
    fn a_review_counts_nothing() {
        let dir = tempfile::tempdir().unwrap();
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
    fn a_forks_copy_of_its_parents_history_is_the_parents() {
        let dir = tempfile::tempdir().unwrap();
        let settings = |thread: &str, model: &str| json!({"type": "thread_settings_applied", "thread_id": thread, "thread_settings": {"model": model}});
        let lines = [
            line(
                0,
                "session_meta",
                json!({"id": "t2", "forked_from_id": "t1", "history_mode": "legacy"}),
            ),
            line(1, "session_meta", json!({"id": "t1"})),
            line(2, "turn_context", json!({"model": "gpt-5.4"})),
            line(3, "response_item", message("user", "Plan the migration.")),
            line(
                4,
                "token_usage_record",
                json!({"response_id": "resp_1", "thread_id": "t1", "usage": usage(1000, 0, 10)}),
            ),
            total(5, usage(1000, 0, 10), usage(1000, 0, 10)),
            // A total the parent wrote before it wrote records.
            total(6, usage(3000, 0, 30), usage(2000, 0, 20)),
            line(7, "response_item", message("assistant", "Here's the plan.")),
            line(8, "event_msg", settings("t1", "gpt-5.4")),
            line(9, "event_msg", settings("t2", "gpt-5.5")),
            line(10, "response_item", message("user", "Try it on staging.")),
            line(
                11,
                "token_usage_record",
                json!({"response_id": "resp_2", "thread_id": "t2", "usage": usage(4000, 3000, 40)}),
            ),
            total(12, usage(7000, 3000, 70), usage(4000, 3000, 40)),
        ];
        let file = rollout(dir.path(), "t2", &lines[..9]);
        let copy = Codex.read(&file, "").unwrap();
        rollout(dir.path(), "t2", &lines);
        let own = Codex.read(&file, &copy.cursor).unwrap();

        let mut responses: Vec<(&str, &str, &str)> = [&copy, &own]
            .iter()
            .flat_map(|read| read.responses.values())
            .map(|response| {
                (
                    response.id.as_str(),
                    response.session.as_str(),
                    response.model.as_str(),
                )
            })
            .collect();
        responses.sort();
        assert_eq!(
            responses,
            [
                ("resp_1", "codex:t1", "gpt-5.4"),
                ("resp_2", "codex:t2", "gpt-5.5")
            ]
        );
        assert!(copy.sessions["codex:t2"].title.is_none());
        let fork = &own.sessions["codex:t2"];
        assert_eq!(fork.title.as_ref().unwrap().1, "Try it on staging.");
        // A fork is a thread of its own.
        assert!(copy.sessions["codex:t2"].parent.is_none());
    }

    #[test]
    fn a_subagents_copy_holding_its_parents_session_meta_ends_at_its_ordinal() {
        let dir = tempfile::tempdir().unwrap();
        let file = rollout(
            dir.path(),
            "t2",
            &[
                line(
                    0,
                    "session_meta",
                    json!({"id": "t2", "parent_thread_id": "t1", "forked_from_id": "t1",
                        "history_mode": "legacy", "subagent_history_start_ordinal": 5}),
                ),
                line(1, "session_meta", json!({"id": "t1"})),
                line(2, "turn_context", json!({"model": "gpt-5.4"})),
                line(3, "response_item", message("user", "Plan the migration.")),
                total(4, usage(9000, 0, 90), usage(9000, 0, 90)),
                // As a Codex before 2026-08-29 wrote it, naming no thread.
                settings(5, json!({"model": "gpt-5.5"})),
                line(6, "response_item", message("user", "Check the indexes.")),
                total(7, usage(9500, 0, 95), usage(500, 0, 5)),
            ],
        );
        let read = Codex.read(&file, "").unwrap();
        let counted: Vec<(u64, &str)> = read
            .responses
            .values()
            .map(|response| (response.tokens.input, response.model.as_str()))
            .collect();
        assert_eq!(counted, [(500, "gpt-5.5")]);
        assert_eq!(
            read.sessions["codex:t2"].parent.as_deref(),
            Some("codex:t1")
        );
        assert_eq!(
            read.sessions["codex:t2"].title.as_ref().unwrap().1,
            "Check the indexes."
        );
    }

    #[test]
    fn a_paginated_fork_is_titled_by_its_own_request_and_a_rewind_by_none() {
        let dir = tempfile::tempdir().unwrap();
        let title = |name: &str, meta: Value| {
            let file = rollout(
                dir.path(),
                name,
                &[
                    line(5, "session_meta", meta),
                    line(6, "response_item", message("user", "Try it on staging.")),
                ],
            );
            let read = Codex.read(&file, "").unwrap();
            read.sessions
                .into_values()
                .find_map(|session| session.title)
                .map(|(_, title)| title)
        };
        let base = |rollout: &str| json!({"thread_id": rollout, "end_ordinal_exclusive": 5, "end_byte_offset": 0});
        assert_eq!(
            title(
                "t1_r2",
                json!({"id": "t1", "history_mode": "paginated", "history_base": base("t1")})
            ),
            None
        );
        let fork = |rollout: &str, forked_at: i64| {
            json!({"id": "t2", "forked_from_id": "t1", "forked_from_ordinal_exclusive": forked_at,
                "history_mode": "paginated", "history_base": base(rollout)})
        };
        assert_eq!(
            title("t2", fork("t1", 5)).as_deref(),
            Some("Try it on staging.")
        );
        // A rewind of the fork that keeps some of its own history.
        assert_eq!(title("t2_r2", fork("t2", 3)), None);
    }

    #[test]
    fn a_title_is_the_persons_first_request_without_codexs_context() {
        let dir = tempfile::tempdir().unwrap();
        let asked = "Replace the <image href=\"a.png\"/> in logo.svg with a path, then add a <skill> element to the manifest.";
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
                        {"type": "input_text", "text": "<skill>\n<name>docs</name>\nWrite plainly.\n</skill>"},
                        // An image's tags, each in a part of its own.
                        {"type": "input_text", "text": "<image name=[Image #1]>"},
                        {"type": "input_image", "image_url": "data:image/png;base64,AAAA"},
                        {"type": "input_text", "text": "</image>"},
                        {"type": "input_text", "text": asked}]}),
                ),
                line(2, "response_item", message("assistant", "Fixed.")),
                // The first request titles it, not one after.
                line(3, "response_item", message("user", "Now the tests.")),
            ],
        );
        let read = Codex.read(&file, "").unwrap();
        assert_eq!(read.sessions["codex:t1"].title.as_ref().unwrap().1, asked);
    }

    #[test]
    fn usage_before_a_model_is_named_waits_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            line(0, "session_meta", json!({"id": "t1"})),
            line(
                1,
                "token_usage_record",
                json!({"response_id": "resp_1", "usage": usage(100, 0, 1)}),
            ),
            line(2, "turn_context", json!({"model": "gpt-5.5"})),
        ];
        let file = rollout(dir.path(), "t1", &lines[..2]);
        let first = Codex.read(&file, "").unwrap();
        assert_eq!(first.responses["resp_1"].model, "");
        rollout(dir.path(), "t1", &lines);
        let later = Codex.read(&file, &first.cursor).unwrap();
        assert_eq!(later.responses["resp_1"].model, "gpt-5.5");
    }

    #[test]
    fn usage_without_its_input_or_output_count_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let record = |ordinal: i64, response: &str, usage: Value| {
            line(
                ordinal,
                "token_usage_record",
                json!({"response_id": response, "usage": usage}),
            )
        };
        let file = rollout(
            dir.path(),
            "t1",
            &[
                line(0, "session_meta", json!({"id": "t1"})),
                line(1, "turn_context", json!({"model": "gpt-5.5"})),
                record(
                    2,
                    "resp_1",
                    json!({"prompt_tokens": 900, "completion_tokens": 10, "total_tokens": 910}),
                ),
                record(3, "resp_2", json!({})),
                total(4, json!({"prompt_tokens": 900}), json!(null)),
                // A count before any usage.
                line(5, "event_msg", json!({"type": "token_count", "info": null})),
                // Without a cache or reasoning count, which is 0.
                record(
                    6,
                    "resp_3",
                    json!({"input_tokens": 900, "cached_input_tokens": 600, "output_tokens": 10,
                        "total_tokens": 910}),
                ),
            ],
        );
        let read = Codex.read(&file, "").unwrap();
        assert!(read.responses.keys().eq(["resp_3"]));
        assert_eq!(read.responses["resp_3"].tokens.input, 300);
        assert_eq!(read.skipped, 3);
    }

    #[test]
    fn usage_of_no_known_thread_or_with_no_payload_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let record = line(
            2,
            "token_usage_record",
            json!({"response_id": "resp_1", "usage": usage(100, 0, 10)}),
        );
        let unnamed = rollout(
            dir.path(),
            "t1",
            &[
                line(0, "session_meta", json!({"thread_id": "t1"})),
                line(1, "turn_context", json!({"model": "gpt-5.5"})),
                record.clone(),
                total(3, usage(100, 0, 10), usage(100, 0, 10)),
                // A count before any usage holds none.
                line(4, "event_msg", json!({"type": "token_count", "info": null})),
            ],
        );
        let mut renamed = record;
        let payload = renamed.as_object_mut().unwrap().remove("payload").unwrap();
        renamed["data"] = payload;
        let unloaded = rollout(
            dir.path(),
            "t2",
            &[line(0, "session_meta", json!({"id": "t2"})), renamed],
        );
        for (file, skipped) in [(unnamed, 2), (unloaded, 1)] {
            let read = Codex.read(&file, "").unwrap();
            assert_eq!((read.responses.len(), read.skipped), (0, skipped));
        }
    }

    #[test]
    fn a_record_in_a_subagents_copy_naming_no_thread_is_its_parents() {
        let dir = tempfile::tempdir().unwrap();
        let record = |ordinal: i64| {
            line(
                ordinal,
                "token_usage_record",
                json!({"response_id": "resp_9", "usage": usage(1000, 0, 10)}),
            )
        };
        let parent = rollout(
            dir.path(),
            "z9",
            &[
                line(0, "session_meta", json!({"id": "z9"})),
                line(1, "turn_context", json!({"model": "gpt-5.5"})),
                record(2),
            ],
        );
        let subagent = rollout(
            dir.path(),
            "a1",
            &[
                line(
                    0,
                    "session_meta",
                    json!({"id": "a1", "parent_thread_id": "z9", "subagent_history_start_ordinal": 3}),
                ),
                line(1, "turn_context", json!({"model": "gpt-5.5"})),
                record(2),
            ],
        );
        let mut response = Codex.read(&subagent, "").unwrap().responses["resp_9"].clone();
        response.merge(Codex.read(&parent, "").unwrap().responses["resp_9"].clone());
        assert_eq!(
            (response.session.as_str(), response.copy),
            ("codex:z9", false)
        );
    }

    #[test]
    fn a_chatgpt_sign_in_or_an_api_key_is_the_login() {
        use base64::Engine as _;
        let dir = tempfile::tempdir().unwrap();
        let login = |kept: Value| {
            std::fs::write(dir.path().join("auth.json"), kept.to_string()).unwrap();
            let logins = Codex.logins(dir.path(), dir.path()).unwrap();
            let [login] = logins.as_slice() else {
                panic!("one login");
            };
            assert_eq!(login.provider, "openai");
            crate::agents::credential(login)
                .map(|(provider, key, expires, secret)| (provider, key, expires, secret.to_owned()))
        };
        let claims =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"exp":1790000000}"#);
        let token = format!("made-up.{claims}.signature");
        let kept = json!({"auth_mode": "chatgpt", "OPENAI_API_KEY": "sk-made-up",
            "tokens": {"access_token": token}});
        assert_eq!(
            login(kept),
            Some(("openai", false, Some(1_790_000_000_000), token.clone()))
        );
        let kept = json!({"auth_mode": "apikey", "OPENAI_API_KEY": "sk-made-up",
            "tokens": {"access_token": token}});
        assert_eq!(
            login(kept),
            Some(("openai", true, None, "sk-made-up".to_owned()))
        );
        // Signed out, or signed in some other way.
        assert_eq!(login(json!({"auth_mode": "chatgpt"})), None);
        assert_eq!(login(json!({"OPENAI_API_KEY": "sk-made-up"})), None);
    }

    #[test]
    fn a_rollout_gives_the_last_limits_codex_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let limits = |ordinal: i64, id: &str, name: Option<&str>, primary: f64, secondary: f64| {
            let window = |used: f64, minutes: i64| json!({"used_percent": used, "window_minutes": minutes, "resets_at": 1_790_870_000});
            let limits = json!({"limit_id": id, "limit_name": name, "plan_type": "plus",
                "primary": window(primary, 300), "secondary": window(secondary, 10080)});
            line(
                ordinal,
                "event_msg",
                json!({"type": "token_count", "info": null, "rate_limits": limits}),
            )
        };
        let file = rollout(
            dir.path(),
            "t1",
            &[
                line(
                    0,
                    "session_meta",
                    json!({"id": "t1", "model_provider": "openai"}),
                ),
                limits(1, "codex", None, 15.0, 4.0),
                limits(2, "codex", None, 22.5, 6.0),
                limits(3, "codex_spark", Some("Spark"), 40.0, 10.0),
                // Not the plan's own, nor named for its models.
                limits(4, "other", None, 99.0, 99.0),
            ],
        );
        let read = Codex.read(&file, "").unwrap();
        assert!(read.limits.iter().all(|limits| limits.provider == "openai"));
        assert_eq!(
            read.limits[0].at,
            crate::time::millis("2026-09-30T09:00:02Z").unwrap()
        );
        let readings: Vec<_> = read
            .limits
            .iter()
            .flat_map(|limits| &limits.readings)
            .map(|reading| {
                (
                    reading.key.as_str(),
                    reading.name.as_str(),
                    reading.scope.as_deref(),
                    reading.used,
                )
            })
            .collect();
        assert_eq!(
            readings,
            [
                ("primary_window", "5 hours", None, 22.5),
                ("secondary_window", "Weekly", None, 6.0),
                ("primary_window:Spark", "5 hours", Some("Spark"), 40.0),
                ("secondary_window:Spark", "Weekly", Some("Spark"), 10.0),
            ]
        );
    }

    #[test]
    fn codex_limit_id_stays_unscoped_even_with_a_limit_name() {
        let dir = tempfile::tempdir().unwrap();
        let window = |used: f64, minutes: i64| json!({"used_percent": used, "window_minutes": minutes, "resets_at": 1_790_870_000});
        let file = rollout(
            dir.path(),
            "t1",
            &[
                line(
                    0,
                    "session_meta",
                    json!({"id": "t1", "model_provider": "openai"}),
                ),
                line(
                    1,
                    "event_msg",
                    json!({
                        "type": "token_count",
                        "info": null,
                        "rate_limits": {
                            "limit_id": "codex",
                            "limit_name": "Codex",
                            "plan_type": "plus",
                            "primary": window(25.0, 300),
                            "secondary": window(8.0, 10080),
                        }
                    }),
                ),
            ],
        );
        let read = Codex.read(&file, "").unwrap();
        let readings: Vec<_> = read
            .limits
            .iter()
            .flat_map(|limits| &limits.readings)
            .map(|reading| (reading.key.as_str(), reading.scope.as_deref(), reading.used))
            .collect();
        assert_eq!(
            readings,
            [
                ("primary_window", None, 25.0),
                ("secondary_window", None, 8.0),
            ]
        );
    }
}
