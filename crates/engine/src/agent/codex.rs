//! Codex.
//!
//! Each thread is written to JSON Lines rollouts under `~/.codex/sessions/`
//! (`<yyyy>/<mm>/<dd>/rollout-<time>-<thread>.jsonl`), moved to
//! `~/.codex/archived_sessions/` when archived. Every line is
//! `{timestamp, ordinal, type, payload}`, and a rollout opens with its thread's
//! `session_meta`. A thread resumed later goes on in a new rollout whose
//! ordinals continue from where it stood. Pointed at another folder by
//! `CODEX_HOME`, as a second account is kept, Codex keeps the same there.
//!
//! **Usage** is written two ways. Newer versions write a `token_usage_record`
//! per response, with a unique `response_id`, just before the `token_count`
//! event that brings the thread's running total up to date. Older versions
//! write only the running total. A rollout is counted from its per-response
//! records once it has one, and before that from what the running total grew
//! by, so a rollout begun on an older version and carried on in a newer one is
//! counted in full and once. Growth is identified by the thread, ordinal and
//! time of the record it was measured at, which a rollout keeps when it is
//! archived but two separate starts of a thread never share. Resumed threads
//! copy nothing, and two separate starts of one thread can reuse the same
//! ordinals, which is why the time is part of what identifies growth.
//!
//! On 2026-09-23 there were 48,715 per-response records. The largest rollout,
//! 336 MB, changed format partway through: its first 1.06B tokens exist only
//! as running totals, and the rest as per-response records.
//!
//! A response's `input_tokens` includes its cached input, and its
//! `output_tokens` its reasoning; the cached input is separated here.
//!
//! **Subagents and reviews.** A per-response record always belongs to the
//! rollout it is in: its `thread_id` is the rollout's own. Running totals are
//! where history is shared:
//!
//! - A spawned subagent's rollout opens with a copy of its parent's history,
//!   up to `subagent_history_start_ordinal`, followed by the parent's
//!   `session_meta`. The copy's running totals are the parent's, and only set
//!   where the subagent's own growth is measured from; its prompts and times
//!   are the parent's too.
//! - A review's rollout (Codex asking a model whether to approve an action)
//!   holds its own history before that ordinal, but its first running total
//!   is its parent's, taken over when the review began.
//!
//! Measured 2026-09-23 over 524 reviews and 33 spawned subagents: a review's
//! first running total is its parent's when the review began, 23.0M for a
//! review that itself used 95K, so counting each rollout's running total
//! from zero credits reviews with 0.48B tokens of their parents' usage. Read
//! as described here, Codex's history came to 8.61B tokens, which an
//! independent computation reproduces exactly.
//!
//! Reviews run the model `codex-auto-review`, which the catalog doesn't
//! price: 3,430 responses and 239M tokens on 2026-09-24. They do none of the
//! work, so the cache leaves them out of sessions, usage and search, and the
//! ledger keeps them as read. A review's requests hand the reviewing model
//! the whole transcript under review, so none is said: indexed, they made
//! 168K entries and a 113 MB index where there are 15K without them.
//!
//! **Which call spawned a subagent.** A `spawn_agent` call names a task in
//! `task_name`, and the subagent's `session_meta` has an `agent_path` ending
//! with it, as `/root/<task_name>`; the call is marked with the name
//! ([`Builder::launched`]) and the subagent's link keeps it. Its `message` is
//! encrypted, so nothing else says which it was. Measured 2026-09-27 over the
//! 21 spawned subagents: every one's path ends with a task its parent spawned,
//! and no parent spawned two of one name.
//!
//! **Models and tiers.** Per-response records name no model, so each is
//! counted under the model and service tier in force: the latest
//! `turn_context`'s or `thread_settings_applied`'s, wherever in the rollout
//! they are. A review can record a response before its settings name the
//! model; such usage is counted at once under no model, which is unknown and
//! never guessed, and waits, kept with the rollout's checkpoint, to be
//! counted again under the first model named after it, which takes its
//! place. Only so much waits; the rest stays under no model, noted. On
//! 2026-09-27, 7 responses in 7 rollouts came before their model was named,
//! each named later in its own rollout, and no rollout ended without naming
//! one. About 10% of responses, 4,862 on 2026-09-23, ran at the `priority`
//! service tier, which costs more.
//!
//! **Agents' messages.** An `agent_message` item is what another of Codex's
//! agents sent this one: it has `author`, `recipient` and `content`, a list
//! of `input_text` parts, and no `message` field such as a reply has. Of
//! 1,322 on 2026-09-26, 1,177 also carried `encrypted_content`. The
//! rollout's agent received them rather than said them.
//!
//! **What it sends as the person's.** Codex puts text of its own in
//! messages with the person's role, each opening with a tag
//! ([`crate::transcript::prompt_parts`] tells them apart). Measured
//! 2026-09-30 over 936 rollouts: `environment_context` 906 times,
//! `recommended_plugins` 258, `codex_internal_context` 139, `image` 46,
//! `send_user_message_question_reply` 39, `in-app-browser-context` 25,
//! `external_codex_apps_open_page` 7 (its apps page opened, a message of
//! `{"page_id":null}` alone), `turn_aborted` 4 and `skill` 3; and `<no
//! retained transcript delta entries>` 214 times, all in reviews, which say
//! nothing.
//!
//! **Rate limits.** Every `token_count` also carries the plan's rate-limit
//! readings: plan, percent used, window length and reset time, 65,563 of
//! them on 2026-09-23. They are not read: a log doesn't say which of several
//! accounts signed in to the plan it came from.
//!
//! **What it writes.** Measured 2026-09-27 over 514,675 lines: 8 line types;
//! `event_msg` of 10 kinds, of which `token_count` and
//! `thread_settings_applied` are read for usage, and `item_completed` and
//! `thread_goal_updated` for the work; `response_item` of 8 kinds; message
//! parts of `input_text`, `output_text`, `input_image` and
//! `encrypted_content`; and reasoning summaries of `summary_text` alone.
//! Every `token_count`'s `info` had `total_token_usage`, `last_token_usage`
//! and `model_context_window`, and every `token_usage_record` the same 8
//! fields.
//!
//! **The conversation** is a thread's rollouts read in order, continuations
//! and rewinds resolved, so what was said before a rewind is gone from it
//! though search still holds its words: 43 entries across 15 threads on
//! 2026-09-23.
//!
//! **Titles.** `~/.codex/session_index.jsonl` names threads, a line per naming,
//! the last standing.
//!
//! **Work, for a handoff.** Measured 2026-09-29 over 922 rollouts: Codex
//! runs every tool through one, `exec` (56,299 calls), whose input is a
//! script calling `tools.exec_command` (62,166 times), `tools.apply_patch`
//! (9,042), `tools.write_stdin` and others, so what a call did isn't in the
//! call. It is in the `item_completed` events Codex writes as each finishes,
//! in 873 of the rollouts:
//!
//! - `CommandExecution` (62,530): the `command` as a shell's `-lc`
//!   argument, its `exit_code`, and a `status` of `completed` or `failed`,
//!   every failed one (5,731) with a code other than 0.
//! - `FileChange` (8,633, one of them failed): `changes`, by path, each an
//!   `add` with its `content` (3,603), an `update` with a `unified_diff` of
//!   hunks and a `move_path` (12,197), or a `delete` with the content it
//!   removed (123). A failed change changed nothing.
//! - A thread's goal is set and changed by `thread_goal_updated` events
//!   (169), each with its `objective` and `status`.
//!
//! Older versions' plan tool, `update_plan`, a list of steps each with its
//! `step` and `status`, is read from its calls as Codex's tool describes
//! it; none was in these rollouts, and nor was a plan in any other form. An
//! event of these kinds that doesn't read is counted as unclear. What a
//! rewind takes back of the conversation it takes back of the work too.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use super::{
    Agent, AgentReader, ArtifactKind, Batch, Checkpoint, DiagnosticKind, Object, Observation,
    Resumed, string,
};
use crate::error::{Error, Result};
use crate::handoff::{self, Change, ChangeKind, Goal, PlanItem, StepStatus, Work};
use crate::jsonl;
use crate::session::{LinkKind, SessionKey, SessionLink, Title, TitleSource};
use crate::time::Instant;
use crate::transcript::{
    Builder, Entry, Speaker, Titled, Transcript, argument, compact, pieces, text_of,
};
use crate::usage::Tokens;

/// Codex's reader.
#[derive(Default)]
pub(super) struct Codex {
    /// Threads' conversations read last, to read on from as they grow.
    read: jsonl::Folds<Talk>,
}

/// The provider a thread is counted under when its `session_meta` names none.
const DEFAULT_PROVIDER: &str = "openai";

/// The file that names threads.
const INDEX: &str = "session_index.jsonl";

/// How many responses may wait for a model to be named; the rest stay
/// counted under no model.
const LONGEST_WAIT: usize = 64;

/// How many bytes of their ids the responses waiting for a model may take,
/// since they are kept with the rollout's checkpoint.
const WAITING_BYTES: usize = 8 * 1024;

/// Line types that hold nothing counted or shown about usage or identity.
const PASSED_OVER: &[&str] = &[
    "compacted",
    "inter_agent_communication_metadata",
    "world_state",
];

/// Kinds of `event_msg` that hold nothing counted, said or titled: an item
/// or task begun or finished, a turn aborted, a goal set, and the event
/// stream's copies of messages the rollout's items hold. Measured 2026-09-27.
const EVENTS_PASSED_OVER: &[&str] = &[
    "agent_message",
    "agent_reasoning",
    "item_completed",
    "task_complete",
    "task_started",
    "thread_goal_updated",
    "turn_aborted",
    "user_message",
];

/// Kinds of `response_item`: messages, what another agent sent, reasoning,
/// tool calls and their results, and a compaction's encrypted history.
/// Measured 2026-09-27.
const ITEMS: &[&str] = &[
    "agent_message",
    "compaction",
    "custom_tool_call",
    "custom_tool_call_output",
    "function_call",
    "function_call_output",
    "message",
    "reasoning",
];

/// Kinds of part a message's content holds: text given to a model or
/// written by one, an image, and what another agent sent encrypted.
/// Measured 2026-09-27.
const MESSAGE_PARTS: &[&str] = &[
    "encrypted_content",
    "input_image",
    "input_text",
    "output_text",
];

/// Kinds of part a reasoning item's readable summary holds.
const SUMMARY_PARTS: &[&str] = &["summary_text"];

/// Fields of a `token_count`'s `info`: the thread's running total, the
/// latest request's usage, and the model's context window.
const INFO_FIELDS: &[&str] = &[
    "last_token_usage",
    "model_context_window",
    "total_token_usage",
];

/// Fields of a `token_usage_record`, which a new one could change the
/// meaning of, as a model named on it would.
const RECORD_FIELDS: &[&str] = &[
    "response_id",
    "root_turn_id",
    "session_id",
    "thread_id",
    "thread_token_usage",
    "turn_id",
    "turn_token_usage",
    "usage",
];

/// Fields of a usage object this reader understands. Any other is reported,
/// because a new field may be a new kind of token that is charged.
const USAGE_FIELDS: &[&str] = &[
    "cache_write_input_tokens",
    "cached_input_tokens",
    "input_tokens",
    "output_tokens",
    "reasoning_output_tokens",
    "total_tokens",
];

impl AgentReader for Codex {
    fn agent(&self) -> Agent {
        Agent::Codex
    }

    fn version(&self) -> u32 {
        11
    }

    fn roots(&self, folder: &Path) -> Vec<PathBuf> {
        vec![
            folder.join("sessions"),
            folder.join("archived_sessions"),
            folder.join(INDEX),
        ]
    }

    fn classify(&self, root: &Path, path: &Path) -> Option<ArtifactKind> {
        let name = path.file_name()?.to_str()?;
        let rollout =
            name.starts_with("rollout-") && name.ends_with(".jsonl") && path.starts_with(root);
        (rollout || (name == INDEX && path == root)).then_some(ArtifactKind::Log)
    }

    fn read(&self, path: &Path, from: &Checkpoint, batch: &mut Batch) -> Result<Checkpoint> {
        if path.file_name().and_then(|name| name.to_str()) == Some(INDEX) {
            // It names threads, whose conversations are in their rollouts.
            batch.hold_only([]);
            let offset = super::read_log(path, from.offset, batch, name_thread)?;
            return Ok(Checkpoint {
                offset,
                state: Vec::new(),
            });
        }
        let mut state: State = from.state("Codex reader state")?;
        let offset = super::read_log(path, from.offset, batch, |line, offset, batch| {
            read_line(line, offset, &mut state, batch);
        })?;
        Checkpoint::at(offset, &state, "Codex reader state")
    }

    fn conversation(&self, session: &SessionKey, artifacts: &[PathBuf]) -> Result<Transcript> {
        // A thread's rollouts, oldest first by the time in their names.
        let mut rollouts: Vec<&Path> = artifacts
            .iter()
            .map(PathBuf::as_path)
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("rollout-"))
            })
            .collect();
        if rollouts.is_empty() {
            return Err(Error::Gone(session.to_string()));
        }
        rollouts.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        self.read.fold(
            &rollouts,
            |talk| talk.opened = false,
            Talk::line,
            Talk::transcript,
        )
    }
}

/// A thread's conversation as its rollouts are read, oldest first.
#[derive(Default)]
struct Talk {
    builder: Builder,
    /// The ordinal and time of the line each entry came from, entry by
    /// entry.
    origins: Vec<(Option<i64>, Option<Instant>)>,
    /// Where each rollout took the thread's history up.
    resumed: Vec<Resumed>,
    /// Whether the rollout being read has opened with its `session_meta`.
    opened: bool,
    /// The ordinal a spawned subagent's own history starts at.
    copied_until: Option<i64>,
    /// Whether the rollout being read is a review's, whose user messages are
    /// Codex's requests for judgment.
    review: bool,
    /// The model in force.
    model: Option<String>,
    /// The work Codex recorded, with the ordinal and time of the line that
    /// recorded it, so a rewind takes it back with what was said.
    work: Vec<(Option<i64>, Option<Instant>, Work)>,
}

impl Talk {
    /// Add one rollout line to the conversation.
    fn line(&mut self, bytes: &[u8]) {
        let Ok(line) = serde_json::from_slice::<Line<Payload>>(bytes) else {
            return;
        };
        let kind = line.kind.as_deref().unwrap_or_default();
        let Some(Object(payload)) = line.payload else {
            return;
        };
        let at = line.timestamp.as_deref().and_then(Instant::parse);
        if kind == "session_meta" {
            if !self.opened
                && let Some(meta) = whole_payload(bytes)
            {
                self.opened = true;
                self.open(&meta, at);
            }
            return;
        }
        let Some(payload) = payload else {
            return;
        };
        if copied(line.ordinal, self.copied_until) {
            return;
        }
        match kind {
            "turn_context" => {
                if let Some(model) = string(payload.model).filter(|model| !model.is_empty()) {
                    self.model = Some(model.into_owned());
                }
            }
            "event_msg" => match string(payload.kind).as_deref() {
                Some("thread_settings_applied") => {
                    if let Some(model) = value(payload.thread_settings)
                        .get("model")
                        .and_then(Value::as_str)
                        .filter(|model| !model.is_empty())
                    {
                        self.model = Some(model.to_owned());
                    }
                }
                Some("item_completed") => {
                    for work in worked(&value(payload.item)) {
                        self.work.push((line.ordinal, at, work));
                    }
                }
                Some("thread_goal_updated") => {
                    let goal = value(payload.goal);
                    let work = match goal.get("objective").and_then(Value::as_str) {
                        Some(objective) => Work::Goal(Goal {
                            objective: objective.to_owned(),
                            status: goal
                                .get("status")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                        }),
                        None => Work::Unclear("thread_goal_updated".to_owned()),
                    };
                    self.work.push((line.ordinal, at, work));
                }
                _ => {}
            },
            "compacted" => {
                let summary = string(payload.message).unwrap_or_default();
                self.builder.say(Speaker::System, at, None, &*summary);
            }
            "response_item" => self.item(&payload, line.ordinal, at),
            // Only those lines say anything a conversation shows.
            _ => return,
        }
        self.origins.resize(self.builder.len(), (line.ordinal, at));
    }

    /// Take in a rollout's own `session_meta`, written at `at`, and where it
    /// takes the thread's history up ([`resumed`]).
    fn open(&mut self, meta: &Map<String, Value>, at: Option<Instant>) {
        self.review = is_review(meta);
        self.copied_until = copied_until(meta);
        if let Some(resumed) = resumed(meta, at) {
            self.resumed.push(resumed);
        }
    }

    /// The conversation: every entry, less those a later rollout took back,
    /// by the rule search follows ([`Resumed::takes_back`]), and the calls
    /// kept that started a subagent, at their places among what is kept.
    fn transcript(&self) -> Transcript {
        let mut entries = Vec::with_capacity(self.builder.len());
        // Where each entry kept went, by where it was.
        let mut moved = HashMap::new();
        for (entry, (ordinal, at)) in self.builder.entries().iter().zip(&self.origins) {
            if !Resumed::takes_back(&self.resumed, *ordinal, *at) {
                // Fewer than the entries there were, which fit.
                let index = u32::try_from(entries.len()).unwrap_or(u32::MAX);
                moved.insert(entry.index, index);
                entries.push(Entry {
                    index,
                    ..entry.clone()
                });
            }
        }
        let launches = self
            .builder
            .launches()
            .iter()
            .filter_map(|(index, key)| Some((*moved.get(index)?, key.clone())))
            .collect();
        let work = self
            .work
            .iter()
            .filter(|(ordinal, at, _)| !Resumed::takes_back(&self.resumed, *ordinal, *at))
            .map(|(_, at, work)| (*at, work.clone()))
            .collect();
        Transcript {
            entries,
            launches,
            work,
        }
    }

    /// Add a response item, from the line numbered `ordinal`.
    fn item(&mut self, item: &Payload, ordinal: Option<i64>, at: Option<Instant>) {
        let model = self.model.as_deref();
        let joined = |field: Option<&RawValue>| text_of(&value(field));
        if let Some(sender) = sender(item.kind, item.role, self.review) {
            let content = value(item.content);
            match sender {
                Sender::Person => {
                    for text in pieces(&content) {
                        self.builder.prompt(at, text);
                    }
                }
                Sender::Model => self
                    .builder
                    .say(Speaker::Assistant, at, model, text_of(&content)),
                Sender::Codex => self
                    .builder
                    .say(Speaker::System, at, None, text_of(&content)),
            }
            return;
        }
        match string(item.kind).as_deref() {
            // Only the readable summary; `encrypted_content` is opaque.
            Some("reasoning") => {
                self.builder
                    .say(Speaker::Reasoning, at, model, joined(item.summary))
            }
            Some("custom_tool_call" | "function_call") => {
                let name = string(item.name);
                let name = match (string(item.namespace), name) {
                    (Some(namespace), Some(name)) => {
                        Some(Cow::Owned(format!("{namespace}.{name}")))
                    }
                    (_, name) => name,
                };
                let input = item
                    .input
                    .or(item.arguments)
                    .map(|input| compact(&value(Some(input))))
                    .unwrap_or_default();
                let id = string(item.call_id);
                let task = (name.as_deref() == Some("spawn_agent"))
                    .then(|| argument(&input, "task_name"))
                    .flatten();
                if name.as_deref() == Some("update_plan") {
                    self.work.push((ordinal, at, planned(&input)));
                }
                self.builder
                    .call(id.as_deref(), at, model, name.as_deref(), input);
                if let Some(task) = task {
                    self.builder.launched(&task);
                }
            }
            // Codex records no failure signal an output can be trusted for, so
            // none is marked and the output speaks for itself.
            Some("custom_tool_call_output" | "function_call_output") => {
                if let Some(id) = string(item.call_id) {
                    let output = item.output.map(|output| match value(Some(output)) {
                        Value::Object(fields) => fields
                            .get("content")
                            .map(text_of)
                            .unwrap_or_else(|| compact(&Value::Object(fields))),
                        other => text_of(&other),
                    });
                    self.builder.answer(&id, output.unwrap_or_default(), false);
                }
            }
            _ => {}
        }
    }
}

/// Who a message item is from, as its conversation shows it and search
/// keeps it: what the person typed and the model replied is said, and what
/// Codex put in is not.
enum Sender {
    /// The person, whose message is divided as [`prompt_parts`] divides it.
    ///
    /// [`prompt_parts`]: crate::transcript::prompt_parts
    Person,
    /// The model.
    Model,
    /// Codex itself: its context, a review's request, or a message from
    /// another of its agents.
    Codex,
}

/// A rollout line, as far as a reader looks: its payload read as `P`, the
/// fields of it that reader needs, in the same pass as the line.
#[derive(Deserialize)]
#[serde(bound(deserialize = "P: Deserialize<'de>"))]
struct Line<'a, P> {
    #[serde(rename = "type", borrow, default)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(default)]
    ordinal: Option<i64>,
    #[serde(default)]
    payload: Option<Object<P>>,
}

/// The fields of a line's payload a conversation shows, held unparsed until
/// one is needed, so the rest, such as a compaction's history or a
/// reasoning item's encrypted content, is skimmed over rather than copied.
/// A field of an unexpected type reads as absent, as it would looked up in
/// parsed JSON.
#[derive(Deserialize)]
struct Payload<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Option<&'a RawValue>,
    #[serde(borrow, default)]
    role: Option<&'a RawValue>,
    #[serde(borrow, default)]
    content: Option<&'a RawValue>,
    #[serde(borrow, default)]
    message: Option<&'a RawValue>,
    #[serde(borrow, default)]
    summary: Option<&'a RawValue>,
    #[serde(borrow, default)]
    name: Option<&'a RawValue>,
    #[serde(borrow, default)]
    namespace: Option<&'a RawValue>,
    #[serde(borrow, default)]
    input: Option<&'a RawValue>,
    #[serde(borrow, default)]
    arguments: Option<&'a RawValue>,
    #[serde(borrow, default)]
    call_id: Option<&'a RawValue>,
    #[serde(borrow, default)]
    output: Option<&'a RawValue>,
    #[serde(borrow, default)]
    model: Option<&'a RawValue>,
    #[serde(borrow, default)]
    thread_settings: Option<&'a RawValue>,
    #[serde(borrow, default)]
    item: Option<&'a RawValue>,
    #[serde(borrow, default)]
    goal: Option<&'a RawValue>,
}

/// The fields of a line's payload that its usage and what was said in it
/// need, as [`Payload`] holds them. The rest, most of a rollout's bytes,
/// such as the output of an item completed, is only skimmed over.
#[derive(Deserialize)]
struct Skimmed<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Option<&'a RawValue>,
    #[serde(borrow, default)]
    role: Option<&'a RawValue>,
    #[serde(borrow, default)]
    content: Option<&'a RawValue>,
    #[serde(borrow, default)]
    summary: Option<&'a RawValue>,
    #[serde(borrow, default)]
    thread_settings: Option<&'a RawValue>,
    #[serde(borrow, default)]
    info: Option<&'a RawValue>,
}

/// Who an item of `kind` and `role` is from, when it is a message, in a
/// rollout that is a review's when `review`. A review's user messages are
/// Codex's requests for judgment, holding the transcript under review;
/// developer and system messages are its context; and a message another of
/// its agents sent this one is received, not said.
fn sender(kind: Option<&RawValue>, role: Option<&RawValue>, review: bool) -> Option<Sender> {
    match string(kind).as_deref() {
        Some("message") => Some(match string(role).as_deref() {
            Some("user") if !review => Sender::Person,
            Some("assistant") => Sender::Model,
            _ => Sender::Codex,
        }),
        Some("agent_message") => Some(Sender::Codex),
        _ => None,
    }
}

/// The payload of the rollout line `bytes`, parsed whole, for a kind of
/// line whose every field may matter: `None` when it is not an object.
fn whole_payload(bytes: &[u8]) -> Option<Map<String, Value>> {
    #[derive(Deserialize)]
    struct Whole<'a> {
        #[serde(borrow, default)]
        payload: Option<&'a RawValue>,
    }
    let whole = serde_json::from_slice::<Whole>(bytes).ok()?;
    serde_json::from_str(whole.payload?.get()).ok()
}

/// The work an `item_completed` event's `item` records: a command run, with
/// its exit code and whether it failed, or the files a change added, updated
/// or deleted. An item of any other kind records none; one of these kinds
/// that doesn't read is unclear.
fn worked(item: &Value) -> Vec<Work> {
    let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
    let unclear = || vec![Work::Unclear(kind.to_owned())];
    match kind {
        "CommandExecution" => {
            // A command is run as a shell's `-lc` argument.
            let command = match item.get("command") {
                Some(Value::Array(words)) => match words.as_slice() {
                    [_, flag, Value::String(command)] if flag == "-lc" || flag == "-c" => {
                        Some(command.clone())
                    }
                    words => words
                        .iter()
                        .map(Value::as_str)
                        .collect::<Option<Vec<&str>>>()
                        .map(|words| words.join(" ")),
                },
                Some(Value::String(command)) => Some(command.clone()),
                _ => None,
            };
            let exit = item.get("exit_code").and_then(Value::as_i64);
            let failed = match item.get("status").and_then(Value::as_str) {
                Some("failed") => Some(true),
                Some("completed") => Some(exit.is_some_and(|exit| exit != 0)),
                _ => None,
            };
            match command {
                Some(command) => vec![Work::Ran {
                    command,
                    exit,
                    failed,
                }],
                None => unclear(),
            }
        }
        // A change that failed changed nothing.
        "FileChange" if item.get("status").and_then(Value::as_str) == Some("failed") => Vec::new(),
        "FileChange" => {
            let Some(changes) = item.get("changes").and_then(Value::as_object) else {
                return unclear();
            };
            changes
                .iter()
                .map(|(path, change)| {
                    let text = |field: &str| change.get(field).and_then(Value::as_str);
                    match (change.get("type").and_then(Value::as_str), text("content")) {
                        (Some("add"), Some(content)) => Work::Changed(Change::new(
                            path,
                            ChangeKind::Created,
                            Some((handoff::lines(content), 0)),
                        )),
                        // What a deleted file held is kept, so its lines are
                        // known.
                        (Some("delete"), Some(content)) => Work::Changed(Change::new(
                            path,
                            ChangeKind::Deleted,
                            Some((0, handoff::lines(content))),
                        )),
                        (Some("update"), _) => match text("unified_diff") {
                            Some(diff) => {
                                let mut change = Change::new(
                                    path,
                                    ChangeKind::Updated,
                                    handoff::diff_lines(diff),
                                );
                                change.moved_to = text("move_path").map(str::to_owned);
                                Work::Changed(change)
                            }
                            None => Work::Unclear(kind.to_owned()),
                        },
                        _ => Work::Unclear(kind.to_owned()),
                    }
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// The plan an `update_plan` call's arguments, `input`, set: its steps, each
/// `step` and `status`, in order.
fn planned(input: &str) -> Work {
    let unclear = || Work::Unclear("update_plan".to_owned());
    let Ok(arguments) = serde_json::from_str::<Value>(input) else {
        return unclear();
    };
    let Some(steps) = arguments.get("plan").and_then(Value::as_array) else {
        return unclear();
    };
    let mut items = Vec::with_capacity(steps.len());
    for step in steps {
        let (Some(text), Some(status)) = (
            step.get("step").and_then(Value::as_str),
            step.get("status").and_then(Value::as_str),
        ) else {
            return unclear();
        };
        let Some(status) = StepStatus::from_word(status) else {
            return unclear();
        };
        items.push(PlanItem {
            text: Some(text.to_owned()),
            status: Some(status),
            ..PlanItem::default()
        });
    }
    Work::Planned(items)
}

/// A field held unparsed, parsed: null where it is absent or isn't JSON.
fn value(field: Option<&RawValue>) -> Value {
    field
        .and_then(|raw| serde_json::from_str(raw.get()).ok())
        .unwrap_or(Value::Null)
}

/// The ordinal a spawned subagent's own history starts at, from its
/// rollout's `session_meta`: before it, the rollout holds a copy of its
/// parent's. A review's history before that ordinal is its own.
fn copied_until(meta: &Map<String, Value>) -> Option<i64> {
    if text(meta, "parent_thread_id").is_none() || is_review(meta) {
        return None;
    }
    meta.get("subagent_history_start_ordinal")
        .and_then(Value::as_i64)
}

/// Where a rollout whose `session_meta`, written at `at`, is `meta` takes its
/// thread's history up: from the ordinal its `history_base` says the thread
/// stood at, so that what came after it in earlier rollouts was taken back,
/// as by a rewind; or, with no base, afresh. `None` when the meta has no
/// time to place it by.
///
/// Of 38 rollouts carrying a thread on here, 28 took back what came before
/// them, 49 user and assistant messages among it; 5 later rollouts with no
/// base numbered their thread afresh from 0 (measured 2026-09-27).
fn resumed(meta: &Map<String, Value>, at: Option<Instant>) -> Option<Resumed> {
    let base = meta
        .get("history_base")
        .and_then(|base| base.get("end_ordinal_exclusive"))
        .and_then(Value::as_i64);
    Some(Resumed { at: at?, base })
}

/// Whether the line at `ordinal` is part of the history a spawned subagent's
/// rollout copied from its parent, which ends before `until`.
fn copied(ordinal: Option<i64>, until: Option<i64>) -> bool {
    matches!((ordinal, until), (Some(ordinal), Some(until)) if ordinal < until)
}

/// What the reader carries from one read of a rollout to the next.
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// Whether the rollout's own `session_meta` has been read. Any after it
    /// are its parent's.
    opened: bool,
    /// The rollout's thread, from its own `session_meta`.
    thread: Option<String>,
    /// The provider the thread's usage is counted under.
    provider: Option<String>,
    /// The ordinal the thread's own history starts at, when it is a spawned
    /// subagent's and opens with a copy of its parent's.
    copied_until: Option<i64>,
    /// Whether the thread is a review, whose first running total is its
    /// parent's.
    review: bool,
    /// The model in force.
    model: Option<String>,
    /// Whether the service tier in force is priority processing.
    priority: bool,
    /// Whether a per-response record has been seen, after which running totals
    /// are not counted.
    per_response: bool,
    /// The running total as last seen.
    total: Option<Totals>,
    /// How far the thread's prompts have titled it. A review's opening
    /// message is Codex's own request for judgment, which titles nothing.
    titled: Titled,
    /// Usage recorded under no model before any model was named, waiting
    /// to be recorded again under the first one named.
    waiting: Vec<Waiting>,
}

/// Usage as it is recorded under the model in force, and kept, while no
/// model has been named, until one is.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Waiting {
    response: String,
    thread: String,
    tokens: Tokens,
    prompt: u64,
    at: i64,
    priority: bool,
}

/// A usage object: one response's, or a running total.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct Totals {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
}

impl Totals {
    /// Read a usage object. `None`, noted, when it is not one or a count is
    /// missing or out of range; fields not understood are noted and passed
    /// over. How much of the output was reasoning, and what was written to
    /// the cache, which newer versions count, may be left out.
    fn read(value: Option<&Value>, offset: u64, batch: &mut Batch) -> Option<Totals> {
        let Some(Value::Object(object)) = value else {
            batch.note(
                DiagnosticKind::Unreadable,
                "a usage object of the wrong shape",
                offset,
            );
            return None;
        };
        batch.note_unknown_fields(object, USAGE_FIELDS, "usage", offset);
        let read = |name: &str, optional: bool, batch: &mut Batch| -> Option<u64> {
            let value = object.get(name);
            let count = if optional {
                super::optional_count(value)
            } else {
                super::count(value)
            };
            if count.is_none() {
                batch.note(DiagnosticKind::Invalid, format!("usage.{name}"), offset);
            }
            count
        };
        Some(Totals {
            input_tokens: read("input_tokens", false, batch)?,
            cached_input_tokens: read("cached_input_tokens", false, batch)?,
            cache_write_input_tokens: read("cache_write_input_tokens", true, batch)?,
            output_tokens: read("output_tokens", false, batch)?,
            reasoning_output_tokens: read("reasoning_output_tokens", true, batch)?,
            total_tokens: read("total_tokens", false, batch)?,
        })
    }

    /// What the running total grew by since `before`, or all of it when there
    /// was no total before or it started again from zero, as it does when a
    /// thread is resumed in a new rollout. `None`, noted, when a count fell
    /// while the total rose, which leaves what it grew by unknown.
    fn since(&self, before: Option<&Totals>, offset: u64, batch: &mut Batch) -> Option<Totals> {
        let Some(before) = before.filter(|before| self.total_tokens >= before.total_tokens) else {
            return Some(*self);
        };
        let mut fell = false;
        let mut grew = |now: u64, then: u64| {
            fell |= now < then;
            now.saturating_sub(then)
        };
        let growth = Totals {
            input_tokens: grew(self.input_tokens, before.input_tokens),
            cached_input_tokens: grew(self.cached_input_tokens, before.cached_input_tokens),
            cache_write_input_tokens: grew(
                self.cache_write_input_tokens,
                before.cache_write_input_tokens,
            ),
            output_tokens: grew(self.output_tokens, before.output_tokens),
            reasoning_output_tokens: grew(
                self.reasoning_output_tokens,
                before.reasoning_output_tokens,
            ),
            total_tokens: grew(self.total_tokens, before.total_tokens),
        };
        if fell {
            batch.note(
                DiagnosticKind::Invalid,
                "a running total count fell while the total rose",
                offset,
            );
            return None;
        }
        Some(growth)
    }

    /// These counts as [`Tokens`], in which no count includes another. `None`,
    /// noted, when the cached input is more than the input or the reasoning
    /// more than the output, which no response can have.
    fn tokens(&self, offset: u64, batch: &mut Batch) -> Option<Tokens> {
        let cached = self
            .cached_input_tokens
            .saturating_add(self.cache_write_input_tokens);
        let (Some(input), true) = (
            self.input_tokens.checked_sub(cached),
            self.reasoning_output_tokens <= self.output_tokens,
        ) else {
            batch.note(
                DiagnosticKind::Invalid,
                "usage whose parts exceed their whole",
                offset,
            );
            return None;
        };
        if self.total_tokens != self.input_tokens.saturating_add(self.output_tokens) {
            batch.note(
                DiagnosticKind::UnknownField,
                "usage.total_tokens is not input plus output",
                offset,
            );
        }
        Some(Tokens {
            input,
            cache_read: self.cached_input_tokens,
            cache_write_5m: self.cache_write_input_tokens,
            cache_write_1h: 0,
            output: self.output_tokens,
            reasoning: self.reasoning_output_tokens,
        })
    }
}

/// Where a line is in its rollout, and when it was written.
struct Position<'a> {
    /// The rollout's thread.
    thread: &'a str,
    /// Its byte offset, for diagnostics.
    offset: u64,
    /// Its time.
    at: Option<Instant>,
    /// Its time as written, which identifies it with its ordinal.
    stamp: Option<&'a str>,
    /// Its ordinal.
    ordinal: Option<i64>,
    /// Whether it is part of a parent's history copied into a subagent's
    /// rollout.
    copied: bool,
}

impl Position<'_> {
    /// Its time in milliseconds, which dates usage recorded on it: `None`,
    /// noted, when it has none.
    fn millis(&self, batch: &mut Batch) -> Option<i64> {
        let at = self.at.map(Instant::millis);
        if at.is_none() {
            batch.note(DiagnosticKind::Invalid, "usage with no time", self.offset);
        }
        at
    }
}

/// Read one rollout line into `batch`.
fn read_line(bytes: &[u8], offset: u64, state: &mut State, batch: &mut Batch) {
    let Ok(line) = serde_json::from_slice::<Line<Skimmed>>(bytes) else {
        batch.note(
            DiagnosticKind::Unreadable,
            "a line that is not a JSON object",
            offset,
        );
        return;
    };
    let kind = line.kind.as_deref().unwrap_or_default();
    // A payload is looked at only for the kinds that read it, which name it
    // in their diagnostics, and parsed whole only where it counts: most of a
    // rollout's bytes are compactions, events such as an item completed, and
    // reasoning items' encrypted content, which are passed over or skimmed
    // for their type.
    let unreadable = |batch: &mut Batch| {
        batch.note(
            DiagnosticKind::Unreadable,
            format!("a {kind} line whose payload is not an object"),
            offset,
        );
    };
    let skimmed = |batch: &mut Batch| match &line.payload {
        Some(Object(Some(payload))) => Some(payload),
        Some(Object(None)) => {
            unreadable(batch);
            None
        }
        None => {
            batch.note(
                DiagnosticKind::Unreadable,
                format!("a {kind} line with no payload"),
                offset,
            );
            None
        }
    };
    let object = |batch: &mut Batch| {
        skimmed(batch)?;
        let parsed = whole_payload(bytes);
        if parsed.is_none() {
            unreadable(batch);
        }
        parsed
    };
    let at = line.timestamp.as_deref().and_then(Instant::parse);

    if kind == "session_meta" {
        // The first is the rollout's own; a subagent's rollout carries its
        // parent's after it, with the parent's history.
        if !state.opened
            && let Some(meta) = object(batch)
        {
            state.opened = true;
            open(&meta, at, offset, state, batch);
        }
        return;
    }
    let Some(thread) = state.thread.clone() else {
        let detail = if state.opened {
            "a line of a rollout whose session_meta has no id"
        } else {
            "a line before the rollout's session_meta"
        };
        batch.note(DiagnosticKind::Unreadable, detail, offset);
        return;
    };
    let session = SessionKey::new(Agent::Codex, thread);
    let position = Position {
        thread: session.native(),
        offset,
        at,
        stamp: line.timestamp.as_deref(),
        ordinal: line.ordinal,
        copied: copied(line.ordinal, state.copied_until),
    };

    match kind {
        // The settings in a parent's history copied into a subagent's
        // rollout are the parent's, and the subagent's own name its model
        // and tier: all 21 subagents' here did before any usage of theirs
        // (2026-09-27). What it used before its own would be unknown, not
        // the parent's.
        "turn_context" => {
            let Some(payload) = object(batch) else {
                return;
            };
            if !position.copied {
                apply_settings(&payload, offset, state, batch);
                let facts = batch.session_mut(&session);
                if facts.cwd.is_none() {
                    facts.cwd = text(&payload, "cwd");
                }
            }
        }
        "event_msg" => {
            let Some(event) = skimmed(batch) else {
                return;
            };
            match string(event.kind).as_deref() {
                Some("token_count") => running_total(&value(event.info), &position, state, batch),
                Some("thread_settings_applied") => {
                    if !position.copied
                        && let Value::Object(settings) = value(event.thread_settings)
                    {
                        apply_settings(&settings, offset, state, batch);
                    }
                }
                Some(other) if EVENTS_PASSED_OVER.contains(&other) => {}
                Some(other) => batch.note_unknown("event", other, offset),
                None => batch.note(DiagnosticKind::Unreadable, "an event with no type", offset),
            }
        }
        "token_usage_record" => {
            let Some(payload) = object(batch) else {
                return;
            };
            state.per_response = true;
            response(&payload, &position, state, batch);
        }
        "response_item" => {
            if !position.copied {
                let Some(item) = skimmed(batch) else {
                    return;
                };
                note_item(item, offset, batch);
                say(item, &session, &position, state, batch);
            }
        }
        other if PASSED_OVER.contains(&other) => {}
        "" => {
            batch.note(DiagnosticKind::Unreadable, "a line with no type", offset);
            return;
        }
        other => {
            batch.note_unknown("line type", other, offset);
            return;
        }
    }
    if !position.copied
        && let Some(at) = at
    {
        batch.session_mut(&session).saw(at);
    }
}

/// A string field of `object`, when it is a non-empty string.
fn text(object: &Map<String, Value>, field: &str) -> Option<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Take in the rollout's own `session_meta`, at `offset`: whose thread it is,
/// where it came from, and where its own history starts.
fn open(
    meta: &Map<String, Value>,
    at: Option<Instant>,
    offset: u64,
    state: &mut State,
    batch: &mut Batch,
) {
    let Some(thread) = text(meta, "id") else {
        batch.note(
            DiagnosticKind::Unreadable,
            "a session_meta with no id",
            offset,
        );
        return;
    };
    let session = SessionKey::new(Agent::Codex, thread.clone());
    state.provider = text(meta, "model_provider");
    // The thread's parent, when it is a subagent or a review.
    let parent = text(meta, "parent_thread_id");

    let review = is_review(meta);
    state.review = review;
    state.copied_until = copied_until(meta);

    let link = match (parent, text(meta, "forked_from_id")) {
        (Some(parent), _) => Some((
            parent,
            if review {
                LinkKind::Review
            } else {
                LinkKind::Subagent
            },
        )),
        (None, Some(fork)) if fork != thread => Some((fork, LinkKind::Fork)),
        _ => None,
    };
    if let Some((parent, kind)) = link {
        // A spawned subagent's path ends with the task name its parent's
        // `spawn_agent` gave it.
        let launch = (kind == LinkKind::Subagent)
            .then(|| text(meta, "agent_path"))
            .flatten()
            .and_then(|path| path.rsplit('/').next().map(str::to_owned))
            .filter(|task| !task.is_empty());
        batch.link(SessionLink {
            child: session.clone(),
            parent: SessionKey::new(Agent::Codex, parent),
            kind,
            launch,
        });
    }

    if let Some(resumed) = resumed(meta, at) {
        batch.resume(&session, resumed);
    }
    let facts = batch.session_mut(&session);
    if let Some(at) = at {
        facts.saw(at);
    }
    facts.cwd = facts.cwd.take().or_else(|| text(meta, "cwd"));
    facts.origin = facts.origin.take().or_else(|| text(meta, "originator"));
    if let Some(version) = text(meta, "cli_version") {
        facts.version = Some(version);
    }
    if let Some(Value::Object(git)) = meta.get("git")
        && let Some(branch) = text(git, "branch")
    {
        facts.branch = Some(branch);
    }
    let nickname =
        text(meta, "agent_nickname").and_then(|name| Title::new(TitleSource::Description, &name));
    facts.offer_title(nickname);
    state.thread = Some(thread);
}

/// Put the model and service tier that `settings`, at `offset`, name in
/// force, and count any usage that was waiting for a model under it.
fn apply_settings(
    settings: &Map<String, Value>,
    offset: u64,
    state: &mut State,
    batch: &mut Batch,
) {
    if let Some(tier) = settings.get("service_tier").and_then(Value::as_str) {
        state.priority = match tier {
            "priority" => true,
            "default" => false,
            // A tier not known is charged at standard prices, and noted: it
            // may cost more, or less.
            other => {
                batch.note_unknown_value("service_tier", other, offset);
                false
            }
        };
    }
    let Some(model) = text(settings, "model") else {
        return;
    };
    for waiting in state.waiting.drain(..) {
        record(waiting, &model, state.provider.as_deref(), batch);
    }
    state.model = Some(model);
}

impl Waiting {
    /// What keeping it takes of [`WAITING_BYTES`].
    fn bytes(&self) -> usize {
        self.response.len().saturating_add(self.thread.len())
    }
}

/// Record `usage` under `model`, or under no model when it is empty, and
/// under `provider` or, when the rollout names none, the provider Codex uses
/// unless told otherwise.
fn record(usage: Waiting, model: &str, provider: Option<&str>, batch: &mut Batch) {
    // Stored from a valid instant, so it converts back.
    let Some(at) = Instant::from_millis(usage.at) else {
        return;
    };
    batch.observe(Observation {
        response: usage.response,
        session: SessionKey::new(Agent::Codex, usage.thread),
        copy: false,
        at,
        provider: provider.unwrap_or(DEFAULT_PROVIDER).to_owned(),
        model: model.to_owned(),
        tokens: usage.tokens,
        prompt: usage.prompt,
        web_searches: 0,
        cost: None,
        priority: usage.priority,
    });
}

/// Count a per-response record.
fn response(
    payload: &Map<String, Value>,
    position: &Position,
    state: &mut State,
    batch: &mut Batch,
) {
    let offset = position.offset;
    batch.note_unknown_fields(payload, RECORD_FIELDS, "token_usage_record", offset);
    let Some(id) = text(payload, "response_id") else {
        batch.note(
            DiagnosticKind::Invalid,
            "a token_usage_record with no response_id",
            offset,
        );
        return;
    };
    let Some(totals) = Totals::read(payload.get("usage"), offset, batch) else {
        return;
    };
    let Some(tokens) = totals.tokens(offset, batch) else {
        return;
    };
    let thread = text(payload, "thread_id").unwrap_or_else(|| position.thread.to_owned());
    let Some(at) = position.millis(batch) else {
        return;
    };
    let usage = Waiting {
        response: id,
        thread,
        tokens,
        prompt: totals.input_tokens,
        at,
        priority: state.priority,
    };
    observe(usage, offset, state, batch);
}

/// Count what a running total grew by, unless the rollout's per-response
/// records count it, or it is part of a copied history.
fn running_total(info: &Value, position: &Position, state: &mut State, batch: &mut Batch) {
    let offset = position.offset;
    // A token_count with no `info` carries only rate limits.
    if info.is_null() {
        return;
    }
    if let Value::Object(fields) = info {
        batch.note_unknown_fields(fields, INFO_FIELDS, "token_count.info", offset);
    }
    let Some(now) = Totals::read(info.get("total_token_usage"), offset, batch) else {
        return;
    };
    let before = state.total.replace(now);
    // Copied totals, and a review's first, are the parent's; once per-response
    // records are written, they count what the totals would.
    if position.copied || state.per_response || (state.review && before.is_none()) {
        return;
    }
    let Some(growth) = now.since(before.as_ref(), offset, batch) else {
        return;
    };
    if growth.total_tokens == 0 {
        return;
    }
    let Some(tokens) = growth.tokens(offset, batch) else {
        return;
    };
    let (Some(ordinal), Some(stamp)) = (position.ordinal, position.stamp) else {
        batch.note(
            DiagnosticKind::Invalid,
            "a running total with no ordinal or time",
            offset,
        );
        return;
    };
    // The latest request's prompt, which sets the tier it was charged at.
    // Codex writes it beside every running total: 65,940 of 65,940 here
    // (2026-09-27). Without it the tier isn't known, so neither is the cost.
    let Some(prompt) = super::count(
        info.get("last_token_usage")
            .and_then(|last| last.get("input_tokens")),
    ) else {
        batch.note(
            DiagnosticKind::Invalid,
            "token_count.info.last_token_usage.input_tokens",
            offset,
        );
        return;
    };
    let Some(at) = position.millis(batch) else {
        return;
    };
    let thread = position.thread;
    let usage = Waiting {
        response: format!("{thread}:{ordinal}:{stamp}"),
        thread: thread.to_owned(),
        tokens,
        prompt,
        at,
        priority: state.priority,
    };
    observe(usage, offset, state, batch);
}

/// Record usage found at `offset` under the model and provider in force.
/// Before any model is named, it is recorded under none, and kept waiting
/// to be recorded again under the first model named, as far as
/// [`LONGEST_WAIT`] and [`WAITING_BYTES`] allow; beyond them it is noted,
/// and stays under none.
fn observe(usage: Waiting, offset: u64, state: &mut State, batch: &mut Batch) {
    if let Some(model) = &state.model {
        record(usage, model, state.provider.as_deref(), batch);
        return;
    }
    let kept: usize = state.waiting.iter().map(Waiting::bytes).sum();
    let room =
        state.waiting.len() < LONGEST_WAIT && kept.saturating_add(usage.bytes()) <= WAITING_BYTES;
    if room {
        state.waiting.push(usage.clone());
    } else {
        batch.note(
            DiagnosticKind::UnknownField,
            "usage before any model is named, beyond what waits for one",
            offset,
        );
    }
    record(usage, "", state.provider.as_deref(), batch);
}

/// The kind of one part of a list, the rest of it skimmed over.
#[derive(Deserialize)]
struct Part<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Option<Cow<'a, str>>,
}

/// Note a response item, at `offset`, of a kind this reader doesn't know,
/// and each part of its content or summary of a kind it doesn't know.
fn note_item(item: &Skimmed, offset: u64, batch: &mut Batch) {
    let kind = string(item.kind);
    let (parts, known, what) = match kind.as_deref() {
        Some("message" | "agent_message") => (item.content, MESSAGE_PARTS, "message part"),
        Some("reasoning") => (item.summary, SUMMARY_PARTS, "reasoning summary part"),
        Some(other) if ITEMS.contains(&other) => return,
        Some(other) => return batch.note_unknown("response item", other, offset),
        None => {
            return batch.note(
                DiagnosticKind::Unreadable,
                "a response item with no type",
                offset,
            );
        }
    };
    let Some(raw) = parts else {
        return;
    };
    let parts = match serde_json::from_str::<Vec<Part>>(raw.get()) {
        Ok(parts) => parts,
        // Content given as text, or as nothing, has no parts.
        Err(_) if string(Some(raw)).is_some() || raw.get() == "null" => return,
        Err(_) => {
            return batch.note(
                DiagnosticKind::Unreadable,
                format!("a {what} list of the wrong shape"),
                offset,
            );
        }
    };
    for part in parts {
        batch.note_kind(part.kind.as_deref(), known, what, offset);
    }
}

/// Record what the person or the model said in a response item at
/// `position`, for search, as its conversation shows it, where it sits in
/// the thread's numbered history, and title the thread from what the person
/// first typed.
fn say(
    item: &Skimmed,
    session: &SessionKey,
    position: &Position,
    state: &mut State,
    batch: &mut Batch,
) {
    let (at, ordinal) = (position.at, position.ordinal);
    match sender(item.kind, item.role, state.review) {
        Some(Sender::Person) => {
            let content = value(item.content);
            for text in pieces(&content) {
                batch.prompt_numbered(session, at, ordinal, text);
                if let Some(title) = state.titled.offer(text) {
                    batch.session_mut(session).offer_title(Some(title));
                }
            }
        }
        Some(Sender::Model) => {
            batch.say_numbered(session, at, ordinal, &text_of(&value(item.content)));
        }
        Some(Sender::Codex) | None => {}
    }
}

/// Whether a rollout's `session_meta` is a review's: Codex asking a model
/// whether to approve an action. Depending on its version, Codex says so in
/// `source` or in `thread_source`.
fn is_review(meta: &Map<String, Value>) -> bool {
    meta.get("source")
        .and_then(|source| source.get("subagent"))
        .and_then(|subagent| subagent.get("other"))
        .and_then(Value::as_str)
        == Some("guardian")
        || text(meta, "thread_source").as_deref() == Some("guardian_review")
}

/// Take a thread's name from a line of the session index.
fn name_thread(bytes: &[u8], offset: u64, batch: &mut Batch) {
    #[derive(Deserialize)]
    struct Named {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        thread_name: Option<String>,
    }
    let Ok(named) = serde_json::from_slice::<Named>(bytes) else {
        batch.note(
            DiagnosticKind::Unreadable,
            "a session index line of the wrong shape",
            offset,
        );
        return;
    };
    let (Some(id), Some(name)) = (named.id.filter(|id| !id.is_empty()), named.thread_name) else {
        batch.note(
            DiagnosticKind::Invalid,
            "a session index line with no id or thread_name",
            offset,
        );
        return;
    };
    let title = Title::new(TitleSource::Generated, &name);
    batch
        .session_mut(&SessionKey::new(Agent::Codex, id))
        .offer_title(title);
}

#[cfg(test)]
mod tests {
    use crate::transcript::Speaker;
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use super::Codex;
    use crate::Agent;
    use crate::agent::tests::assert_said_as_shown;
    use crate::agent::{AgentReader, Batch, Checkpoint, DiagnosticKind, Observation};
    use crate::session::{LinkKind, SessionKey, TitleSource};
    use crate::usage::Tokens;

    const THREAD: &str = "01a06671-ecc2-7ce2-8cde-67cc9b3de001";
    const PARENT: &str = "01a06962-65e1-7332-ba7b-27a8f0b07d4d";

    fn key(native: &str) -> SessionKey {
        SessionKey::new(Agent::Codex, native)
    }

    /// A rollout of `lines`, each given its ordinal in turn and, unless it has
    /// one, a time a second after the start for each ordinal.
    fn rollout(dir: &Path, lines: &[Value]) -> PathBuf {
        let path = dir
            .join("sessions/2026/09/03")
            .join(format!("rollout-2026-09-03T03-45-37-{THREAD}.jsonl"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text: String = lines
            .iter()
            .enumerate()
            .map(|(ordinal, line)| {
                let mut line = line.clone();
                line["ordinal"] = json!(ordinal);
                if line.get("timestamp").is_none() {
                    line["timestamp"] = json!(format!("2026-09-03T07:20:{:02}.000Z", ordinal));
                }
                format!("{line}\n")
            })
            .collect();
        std::fs::write(&path, text).unwrap();
        path
    }

    fn read(path: &Path) -> Batch {
        let mut batch = Batch::default();
        Codex::default()
            .read(path, &Checkpoint::default(), &mut batch)
            .unwrap();
        batch
    }

    fn meta(extra: Value) -> Value {
        let mut payload = json!({"id": THREAD, "cwd": "/work/infergo", "originator": "codex-tui",
                                 "cli_version": "0.152.1", "model_provider": "openai",
                                 "git": {"branch": "main"}});
        for (field, value) in extra.as_object().unwrap() {
            payload[field] = value.clone();
        }
        json!({"type": "session_meta", "payload": payload})
    }

    fn settings(model: &str, tier: &str) -> Value {
        json!({"type": "event_msg", "payload": {"type": "thread_settings_applied",
               "thread_settings": {"model": model, "service_tier": tier}}})
    }

    fn turn(model: &str) -> Value {
        json!({"type": "turn_context", "payload": {"turn_id": "t", "cwd": "/work/infergo", "model": model}})
    }

    fn usage(input: u64, cached: u64, output: u64, reasoning: u64) -> Value {
        json!({"input_tokens": input, "cached_input_tokens": cached, "cache_write_input_tokens": 0,
               "output_tokens": output, "reasoning_output_tokens": reasoning, "total_tokens": input + output})
    }

    fn total(input: u64, cached: u64, output: u64, reasoning: u64) -> Value {
        json!({"type": "event_msg", "payload": {"type": "token_count",
               "info": {"total_token_usage": usage(input, cached, output, reasoning),
                        "last_token_usage": usage(input, cached, output, reasoning)}}})
    }

    fn record(
        id: &str,
        thread: &str,
        input: u64,
        cached: u64,
        output: u64,
        reasoning: u64,
    ) -> Value {
        json!({"type": "token_usage_record", "payload": {"thread_id": thread, "turn_id": "t",
               "response_id": id, "usage": usage(input, cached, output, reasoning)}})
    }

    fn dated(mut line: Value, at: &str) -> Value {
        line["timestamp"] = json!(at);
        line
    }

    fn user(text: &str) -> Value {
        json!({"type": "response_item", "payload": {"type": "message", "role": "user",
               "content": [{"type": "input_text", "text": text}]}})
    }

    fn sum(batch: &Batch) -> Tokens {
        let mut sum = Tokens::default();
        batch.observations().for_each(|seen| sum.add(&seen.tokens));
        sum
    }

    fn tokens(input: u64, cache_read: u64, output: u64, reasoning: u64) -> Tokens {
        Tokens {
            input,
            cache_read,
            output,
            reasoning,
            ..Tokens::default()
        }
    }

    #[test]
    fn a_rollout_that_moves_to_per_response_records_is_counted_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = rollout(
            dir.path(),
            &[
                meta(json!({})),
                turn("gpt-6-astra"),
                user("<environment_context>cwd</environment_context>"),
                user("Make the scan incremental"),
                // Running totals, as an older version wrote them.
                total(1_000, 600, 50, 10),
                total(2_500, 1_800, 120, 30),
                // A newer version: a record before each total, which it counts.
                record("resp_1", THREAD, 3_000, 2_400, 40, 5),
                total(5_500, 4_200, 160, 35),
                record("resp_2", THREAD, 1_000, 900, 20, 0),
                total(6_500, 5_100, 180, 35),
            ],
        );
        let batch = read(&path);

        // Growth 1: input 1,000 of which 600 cached, output 50 (10 reasoning).
        // Growth 2: input 1,500 of which 1,200 cached, output 70 (20 reasoning).
        // resp_1: input 3,000 of which 2,400 cached; resp_2: 1,000 of which 900.
        assert_eq!(batch.observations().count(), 4);
        assert_eq!(
            sum(&batch),
            tokens(
                400 + 300 + 600 + 100,
                600 + 1_200 + 2_400 + 900,
                50 + 70 + 40 + 20,
                10 + 20 + 5
            )
        );
        let first: &Observation = batch
            .observations()
            .find(|seen| seen.response == "resp_1")
            .unwrap();
        assert_eq!(
            (first.model.as_str(), first.prompt, first.priority),
            ("gpt-6-astra", 3_000, false)
        );

        let (_, facts) = batch.sessions().next().unwrap();
        let title = facts.title.as_ref().unwrap();
        assert_eq!(
            (title.source, title.text.as_str()),
            (TitleSource::Prompt, "Make the scan incremental")
        );
        assert_eq!(
            (facts.origin.as_deref(), facts.branch.as_deref()),
            (Some("codex-tui"), Some("main"))
        );
        assert_eq!(batch.diagnostics().count(), 0);
    }

    #[test]
    fn a_spawned_subagent_counts_only_what_it_added_to_its_parents_total() {
        let dir = tempfile::tempdir().unwrap();
        let path = rollout(
            dir.path(),
            &[
                meta(
                    json!({"parent_thread_id": PARENT, "subagent_history_start_ordinal": 4,
                        "source": {"subagent": {"thread_spawn": {"parent_thread_id": PARENT}}}}),
                ),
                // The parent's history, copied with its own earlier times: its
                // total and its prompt.
                dated(
                    total(22_000_000, 20_000_000, 1_000_000, 0),
                    "2026-09-03T06:00:00.000Z",
                ),
                dated(
                    json!({"type": "session_meta", "payload": {"id": PARENT}}),
                    "2026-09-03T06:00:00.000Z",
                ),
                dated(user("The parent's own request"), "2026-09-03T06:05:00.000Z"),
                // The subagent's own.
                turn("gpt-6-astra"),
                user("Review the scanner"),
                total(22_000_250, 20_000_200, 1_000_050, 0),
            ],
        );
        let batch = read(&path);

        // Growth: input 250 of which 200 cached, output 50.
        assert_eq!(sum(&batch), tokens(50, 200, 50, 0));
        let link = batch.links().next().unwrap();
        assert_eq!(
            (link.child.clone(), link.parent.clone(), link.kind),
            (key(THREAD), key(PARENT), LinkKind::Subagent)
        );
        let (_, facts) = batch
            .sessions()
            .find(|(session, _)| **session == key(THREAD))
            .unwrap();
        assert_eq!(facts.title.as_ref().unwrap().text, "Review the scanner");
        // The subagent is dated by its own lines, from its opening at 07:20:00
        // to its last at 07:20:06, not by the parent's copied ones at 06:00.
        let at = |text| crate::time::Instant::parse(text).unwrap();
        assert_eq!(facts.started, Some(at("2026-09-03T07:20:00.000Z")));
        assert_eq!(facts.last, Some(at("2026-09-03T07:20:06.000Z")));
    }

    #[test]
    fn a_subagent_is_not_counted_under_its_parents_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = rollout(
            dir.path(),
            &[
                meta(
                    json!({"parent_thread_id": PARENT, "subagent_history_start_ordinal": 3,
                        "source": {"subagent": {"thread_spawn": {"parent_thread_id": PARENT}}}}),
                ),
                // The parent's settings, copied: another model, at priority.
                dated(
                    settings("gpt-6-parent", "priority"),
                    "2026-09-03T06:00:00.000Z",
                ),
                dated(
                    json!({"type": "session_meta", "payload": {"id": PARENT}}),
                    "2026-09-03T06:00:00.000Z",
                ),
                // A response of the subagent's own before its settings, which
                // waits for its model, and one after.
                record("resp_1", THREAD, 1_000, 600, 50, 10),
                settings("gpt-6-astra", "default"),
                record("resp_2", THREAD, 1_000, 600, 50, 10),
            ],
        );
        let batch = read(&path);
        let counted: Vec<(&str, &str, bool)> = batch
            .observations()
            .map(|seen| (seen.response.as_str(), seen.model.as_str(), seen.priority))
            .collect();
        assert_eq!(
            counted,
            [
                ("resp_1", "gpt-6-astra", false),
                ("resp_2", "gpt-6-astra", false)
            ]
        );
    }

    #[test]
    fn a_running_total_that_cant_be_counted_is_noted_and_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        // Running totals of input 1,000 more each time, 600 of it cached, and
        // output 50 more, 10 of it reasoning. The second's latest prompt is
        // below zero and the third's missing, so neither is counted; the
        // fifth's cached input falls from 2,400 to 2,300 while the total
        // rises, so what it grew by is unknown.
        let mut negative = total(2_000, 1_200, 100, 20);
        negative["payload"]["info"]["last_token_usage"]["input_tokens"] = json!(-1);
        let mut missing = total(3_000, 1_800, 150, 30);
        missing["payload"]["info"]
            .as_object_mut()
            .unwrap()
            .remove("last_token_usage");
        let path = rollout(
            dir.path(),
            &[
                meta(json!({})),
                turn("gpt-6-astra"),
                total(1_000, 600, 50, 10),
                negative,
                missing,
                total(4_000, 2_400, 200, 40),
                total(5_000, 2_300, 250, 50),
                total(6_000, 2_900, 300, 60),
            ],
        );
        let batch = read(&path);
        // The first, whole, what the fourth added to the third, and what the
        // sixth added to the fifth: each input 1,000 - 600 cached = 400,
        // cached 600, output 50 with 10 reasoning, so 1,200, 1,800, 150 and
        // 30 in all.
        assert_eq!(batch.observations().count(), 3);
        assert_eq!(sum(&batch), tokens(1_200, 1_800, 150, 30));
        let noted: Vec<(DiagnosticKind, &str, u64)> = batch
            .diagnostics()
            .map(|((kind, detail), seen)| (*kind, detail.as_str(), seen.count))
            .collect();
        assert_eq!(
            noted,
            [
                (
                    DiagnosticKind::Invalid,
                    "a running total count fell while the total rose",
                    1
                ),
                (
                    DiagnosticKind::Invalid,
                    "token_count.info.last_token_usage.input_tokens",
                    2
                )
            ]
        );
    }

    #[test]
    fn a_session_meta_with_no_id_is_noted_and_its_parents_not_taken_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut nameless = meta(json!({}));
        nameless["payload"].as_object_mut().unwrap().remove("id");
        let mut parents = meta(json!({}));
        parents["payload"]["id"] = json!("parent");
        let path = rollout(
            dir.path(),
            &[
                nameless,
                turn("gpt-6-astra"),
                total(1_000, 600, 50, 10),
                parents,
                total(2_000, 1_200, 100, 20),
            ],
        );
        let batch = read(&path);
        assert_eq!(batch.observations().count(), 0);
        assert_eq!(batch.sessions().count(), 0);
        let noted: Vec<(DiagnosticKind, &str, u64)> = batch
            .diagnostics()
            .map(|((kind, detail), seen)| (*kind, detail.as_str(), seen.count))
            .collect();
        assert_eq!(
            noted,
            [
                (
                    DiagnosticKind::Unreadable,
                    "a line of a rollout whose session_meta has no id",
                    3
                ),
                (DiagnosticKind::Unreadable, "a session_meta with no id", 1)
            ]
        );
    }

    #[test]
    fn a_message_from_another_agent_is_shown_as_received_and_not_said() {
        let dir = tempfile::tempdir().unwrap();
        let reply = json!({"type": "response_item", "payload": {"type": "message", "role": "assistant",
                           "content": [{"type": "output_text", "text": "I'll read each file once."}]}});
        // As Codex writes a subagent's report to the agent that spawned it.
        let received = json!({"type": "response_item", "payload": {"type": "agent_message",
            "id": "amsg_01a066bd-a493-7070-a4c5-7c452833dbae", "author": "/root/scanner_review",
            "recipient": "/root", "content": [
                {"type": "input_text", "text": "The scanner reads each file twice."},
                {"type": "encrypted_content", "encrypted_content": "gAAAAABo1x"}],
            "internal_chat_message_metadata_passthrough": {"turn_id": "01a0667b-fc79", "create_time": 1_788_430_099.603_1}}});
        let path = rollout(
            dir.path(),
            &[
                meta(json!({})),
                turn("gpt-6-astra"),
                user("Review the scanner"),
                json!({"type": "inter_agent_communication_metadata", "payload": {"trigger_turn": false}}),
                received,
                reply,
            ],
        );
        let entries = Codex::default()
            .conversation(&key(THREAD), std::slice::from_ref(&path))
            .unwrap()
            .entries;
        let shown: Vec<(Speaker, &str)> = entries
            .iter()
            .map(|entry| (entry.speaker, entry.text.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                (Speaker::User, "Review the scanner"),
                (Speaker::System, "The scanner reads each file twice."),
                (Speaker::Assistant, "I'll read each file once.")
            ]
        );
        assert_said_as_shown(
            &Codex::default(),
            &key(THREAD),
            std::slice::from_ref(&path),
            std::slice::from_ref(&path),
        );
    }

    #[test]
    fn a_reviews_request_is_codexs_and_only_its_verdict_is_said() {
        let dir = tempfile::tempdir().unwrap();
        let verdict = json!({"type": "response_item", "payload": {"type": "message", "role": "assistant",
                             "content": [{"type": "output_text", "text": "Approve: the change is safe."}]}});
        let path = rollout(
            dir.path(),
            &[
                meta(json!({"parent_thread_id": PARENT, "thread_source": "guardian_review"})),
                user(">>> TRANSCRIPT START\n[90] user: Fix the parser"),
                verdict,
            ],
        );
        let batch = read(&path);
        let said: Vec<&str> = batch.said().map(|said| said.text.as_str()).collect();
        assert_eq!(said, ["Approve: the change is safe."]);
        let entries = Codex::default()
            .conversation(&key(THREAD), std::slice::from_ref(&path))
            .unwrap()
            .entries;
        let speakers: Vec<Speaker> = entries.iter().map(|entry| entry.speaker).collect();
        assert_eq!(speakers, [Speaker::System, Speaker::Assistant]);
        assert_said_as_shown(
            &Codex::default(),
            &key(THREAD),
            std::slice::from_ref(&path),
            std::slice::from_ref(&path),
        );
    }

    #[test]
    fn a_review_written_before_per_response_records_counts_its_growth_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = rollout(
            dir.path(),
            &[
                meta(json!({"parent_thread_id": PARENT, "thread_source": "guardian_review"})),
                settings("codex-auto-review", "priority"),
                total(23_059_837, 21_000_000, 300_000, 0),
                total(23_154_837, 21_090_000, 300_016, 8),
            ],
        );
        let batch = read(&path);

        // Growth: input 95,000 of which 90,000 cached, output 16 (8 reasoning),
        // served at priority.
        let seen: Vec<&Observation> = batch.observations().collect();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].tokens, tokens(5_000, 90_000, 16, 8));
        assert!(seen[0].priority);
    }

    #[test]
    fn usage_recorded_before_a_model_is_named_waits_for_one_however_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            meta(
                json!({"parent_thread_id": PARENT, "thread_source": "guardian_review",
                    "subagent_history_start_ordinal": 31, "source": {"subagent": {"other": "guardian"}}}),
            ),
            // A review's response, recorded before its settings name its model.
            record("resp_early", THREAD, 95_000, 90_000, 16, 8),
            total(23_059_837, 21_000_000, 300_000, 0),
            settings("codex-auto-review", "priority"),
            record("resp_later", THREAD, 1_000, 500, 10, 0),
            user(
                "The following is the Codex agent history whose request action you are assessing.",
            ),
        ];
        let whole = read(&rollout(dir.path(), &lines));

        // Counted under the model named after it, at the tier in force when
        // it was recorded. Input 95,000 of which 90,000 cached, and 1,000 of
        // which 500.
        let mut seen: Vec<(&str, &str, bool, Tokens)> = whole
            .observations()
            .map(|seen| {
                (
                    seen.response.as_str(),
                    seen.model.as_str(),
                    seen.priority,
                    seen.tokens,
                )
            })
            .collect();
        seen.sort_by_key(|(response, ..)| *response);
        assert_eq!(
            seen,
            [
                (
                    "resp_early",
                    "codex-auto-review",
                    false,
                    tokens(5_000, 90_000, 16, 8)
                ),
                (
                    "resp_later",
                    "codex-auto-review",
                    true,
                    tokens(500, 500, 10, 0)
                ),
            ]
        );
        assert_eq!(whole.links().next().unwrap().kind, LinkKind::Review);
        let (_, facts) = whole.sessions().next().unwrap();
        assert_eq!(facts.title, None);

        // Read as far as the running total first, the early response is
        // counted under no model, and reading on, as a reader started again
        // would from the checkpoint, counts it again under the model named,
        // as reading it whole does.
        let path = rollout(dir.path(), &lines[..3]);
        let mut first = Batch::default();
        let checkpoint = Codex::default()
            .read(&path, &Checkpoint::default(), &mut first)
            .unwrap();
        let early: Vec<(&str, &str, &str, Tokens)> = first
            .observations()
            .map(|seen| {
                (
                    seen.response.as_str(),
                    seen.provider.as_str(),
                    seen.model.as_str(),
                    seen.tokens,
                )
            })
            .collect();
        assert_eq!(
            early,
            [("resp_early", "openai", "", tokens(5_000, 90_000, 16, 8))]
        );
        assert_eq!(first.diagnostics().count(), 0);
        rollout(dir.path(), &lines);
        let mut second = Batch::default();
        Codex::default()
            .read(&path, &checkpoint, &mut second)
            .unwrap();
        let mut parts: Vec<Observation> = second.observations().cloned().collect();
        parts.sort_by(|a, b| a.response.cmp(&b.response));
        let mut at_once: Vec<Observation> = whole.observations().cloned().collect();
        at_once.sort_by(|a, b| a.response.cmp(&b.response));
        assert_eq!(parts, at_once);
    }

    #[test]
    fn usage_beyond_what_waits_for_a_model_stays_under_none_and_is_noted() {
        let dir = tempfile::tempdir().unwrap();
        // Before the model is named, a response whose id is longer than
        // all that waits may be, and then 65 responses, one more than wait.
        let long = "r".repeat(9_000);
        let at = |n: usize| format!("2026-09-03T07:{:02}:{:02}.000Z", 20 + n / 60, n % 60);
        let mut lines = vec![
            meta(json!({})),
            dated(record(&long, THREAD, 10, 0, 1, 0), &at(1)),
        ];
        lines.extend((1..=65).map(|n| {
            dated(
                record(&format!("resp_{n:02}"), THREAD, 10, 0, 1, 0),
                &at(n + 1),
            )
        }));
        lines.push(dated(turn("gpt-6-astra"), &at(67)));
        let path = rollout(dir.path(), &lines);
        let batch = read(&path);

        // Every one of the 66 is counted, 10 input and 1 output each: the 64
        // that waited under the model, the other two under none.
        assert_eq!(sum(&batch), tokens(66 * 10, 0, 66, 0));
        let named = batch
            .observations()
            .filter(|seen| seen.model == "gpt-6-astra")
            .count();
        let mut unnamed: Vec<&str> = batch
            .observations()
            .filter(|seen| seen.model.is_empty())
            .map(|seen| {
                if seen.response == long {
                    "the long one"
                } else {
                    seen.response.as_str()
                }
            })
            .collect();
        unnamed.sort_unstable();
        assert_eq!(named, 64);
        assert_eq!(unnamed, ["resp_65", "the long one"]);
        let noted: Vec<(DiagnosticKind, &str, u64)> = batch
            .diagnostics()
            .map(|((kind, detail), seen)| (*kind, detail.as_str(), seen.count))
            .collect();
        assert_eq!(
            noted,
            [(
                DiagnosticKind::UnknownField,
                "usage before any model is named, beyond what waits for one",
                2
            )]
        );
    }

    #[test]
    fn the_session_index_names_threads_and_the_last_naming_stands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session_index.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                json!({"id": THREAD, "thread_name": "Scan faster", "updated_at": "2026-09-01T22:28:22Z"}),
                json!({"id": THREAD, "thread_name": "Make the scan incremental", "updated_at": "2026-09-02T09:00:00Z"})
            ),
        )
        .unwrap();
        assert_eq!(
            Codex::default().classify(&path, &path),
            Some(crate::agent::ArtifactKind::Log)
        );
        let batch = read(&path);
        let (_, facts) = batch.sessions().next().unwrap();
        let title = facts.title.as_ref().unwrap();
        assert_eq!(
            (title.source, title.text.as_str()),
            (TitleSource::Generated, "Make the scan incremental")
        );
        // It names the thread and holds none of its conversation, which is
        // gone once its rollouts are.
        assert!(batch.holding().is_some_and(|held| held.is_empty()));
        assert!(matches!(
            Codex::default().conversation(&key(THREAD), std::slice::from_ref(&path)),
            Err(crate::Error::Gone(_))
        ));
    }

    #[test]
    fn events_items_parts_and_fields_of_kinds_not_known_are_noted_and_the_rest_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut counted = record("resp_1", THREAD, 1_000, 600, 50, 10);
        counted["payload"]["model"] = json!("gpt-6-astra");
        let mut total = total(1_000, 600, 50, 10);
        total["payload"]["info"]["rate_limit_tier"] = json!("pro");
        let path = rollout(
            dir.path(),
            &[
                meta(json!({})),
                turn("gpt-6-astra"),
                // A tier not known, which is counted at standard prices.
                settings("gpt-6-astra", "flex"),
                json!({"type": "hologram", "payload": {}}),
                json!({"type": "hologram"}),
                json!({"type": "event_msg", "payload": {"type": "task_started", "turn_id": "t"}}),
                json!({"type": "event_msg", "payload": {"type": "hologram_started"}}),
                json!({"type": "response_item", "payload": {"type": "hologram_call", "id": "h_1"}}),
                json!({"type": "response_item", "payload": {"type": "message", "role": "user",
                       "content": [{"type": "input_text", "text": "Make the scan incremental"},
                                   {"type": "input_hologram", "data": "?"}]}}),
                json!({"type": "response_item", "payload": {"type": "reasoning", "encrypted_content": "gAAAAB",
                       "summary": [{"type": "summary_text", "text": "Read ingest."},
                                   {"type": "summary_hologram"}]}}),
                counted,
                total,
            ],
        );
        let batch = read(&path);

        // The record is counted: input 1,000 of which 600 cached, output
        // 50 of which 10 reasoning; and the prompt is said.
        assert_eq!(sum(&batch), tokens(400, 600, 50, 10));
        assert!(!batch.observations().next().unwrap().priority);
        let said: Vec<&str> = batch.said().map(|said| said.text.as_str()).collect();
        assert_eq!(said, ["Make the scan incremental"]);
        let noted: Vec<(DiagnosticKind, &str)> = batch
            .diagnostics()
            .map(|((kind, detail), _)| (*kind, detail.as_str()))
            .collect();
        assert_eq!(
            noted,
            [
                (DiagnosticKind::UnknownRecord, "event hologram_started"),
                (DiagnosticKind::UnknownRecord, "line type hologram"),
                (DiagnosticKind::UnknownRecord, "message part input_hologram"),
                (
                    DiagnosticKind::UnknownRecord,
                    "reasoning summary part summary_hologram"
                ),
                (DiagnosticKind::UnknownRecord, "response item hologram_call"),
                (DiagnosticKind::UnknownField, "service_tier flex"),
                (
                    DiagnosticKind::UnknownField,
                    "token_count.info.rate_limit_tier"
                ),
                (DiagnosticKind::UnknownField, "token_usage_record.model"),
            ]
        );
    }

    #[test]
    fn a_count_missing_or_out_of_range_invalidates_its_record_and_no_other() {
        let dir = tempfile::tempdir().unwrap();
        // One more than 2^50 = 1,125,899,906,842,624, a negative count, and
        // no count of cached input, which Codex always writes.
        let mut huge = record("resp_2", THREAD, 1_000, 600, 50, 10);
        huge["payload"]["usage"]["output_tokens"] = json!(1_125_899_906_842_625_u64);
        let mut negative = record("resp_3", THREAD, 1_000, 600, 50, 10);
        negative["payload"]["usage"]["input_tokens"] = json!(-1);
        let mut missing = record("resp_4", THREAD, 1_000, 600, 50, 10);
        missing["payload"]["usage"]
            .as_object_mut()
            .unwrap()
            .remove("cached_input_tokens");
        let path = rollout(
            dir.path(),
            &[
                meta(json!({})),
                turn("gpt-6-astra"),
                record("resp_1", THREAD, 1_000, 600, 50, 10),
                huge,
                negative,
                missing,
            ],
        );
        let batch = read(&path);
        let counted: Vec<&str> = batch
            .observations()
            .map(|seen| seen.response.as_str())
            .collect();
        assert_eq!(counted, ["resp_1"]);
        let noted: Vec<(DiagnosticKind, &str)> = batch
            .diagnostics()
            .map(|((kind, detail), _)| (*kind, detail.as_str()))
            .collect();
        assert_eq!(
            noted,
            [
                (DiagnosticKind::Invalid, "usage.cached_input_tokens"),
                (DiagnosticKind::Invalid, "usage.input_tokens"),
                (DiagnosticKind::Invalid, "usage.output_tokens")
            ]
        );
    }

    #[test]
    fn what_codex_sends_as_the_persons_on_opening_its_apps_page_is_its_own() {
        // As Codex wrote it on 2026-09-30 when the person opened its apps
        // page, before they asked anything.
        let dir = tempfile::tempdir().unwrap();
        let path = rollout(
            dir.path(),
            &[
                meta(json!({})),
                turn("gpt-6-astra"),
                user("<environment_context>cwd</environment_context>"),
                user(
                    "<external_codex_apps_open_page>{\"page_id\":null}</external_codex_apps_open_page>",
                ),
                user("Fix the failing test"),
            ],
        );
        let entries = Codex::default()
            .conversation(&key(THREAD), std::slice::from_ref(&path))
            .unwrap()
            .entries;
        let speakers: Vec<Speaker> = entries.iter().map(|entry| entry.speaker).collect();
        assert_eq!(speakers, [Speaker::System, Speaker::System, Speaker::User]);
        let paths = [path];
        assert_said_as_shown(&Codex::default(), &key(THREAD), &paths, &paths);
    }

    #[test]
    fn a_resumed_thread_drops_what_it_rewound_past() {
        let dir = tempfile::tempdir().unwrap();
        let call = json!({"type": "response_item", "payload": {"type": "function_call", "name": "exec",
                          "call_id": "call_1", "arguments": "{\"cmd\":\"ls\"}"}});
        let output = json!({"type": "response_item", "payload": {"type": "function_call_output", "call_id": "call_1",
                            "output": "Cargo.toml"}});
        let reply = |text: &str| {
            json!({"type": "response_item", "payload": {"type": "message", "role": "assistant",
                                         "content": [{"type": "output_text", "text": text}]}})
        };
        let first = rollout(
            dir.path(),
            &[
                meta(json!({})),
                turn("gpt-6-astra"),
                user("<environment_context>cwd</environment_context>"),
                user("List the files"),
                call,
                output,
                reply("Here they are."),
                reply("An answer later rewound."),
                user("Rename them all"),
            ],
        );
        // Resumed from before the last reply, in a rollout of its own: the
        // reply at ordinal 7 and the person's message at 8 were taken back.
        let second = dir
            .path()
            .join("sessions/2026/09/04")
            .join(format!("rollout-2026-09-04T01-00-00-{THREAD}.jsonl"));
        std::fs::create_dir_all(second.parent().unwrap()).unwrap();
        let lines = [
            json!({"type": "session_meta", "ordinal": 7, "timestamp": "2026-09-04T01:00:00.000Z",
                   "payload": {"id": THREAD, "history_base": {"thread_id": THREAD, "end_ordinal_exclusive": 7}}}),
            json!({"type": "response_item", "ordinal": 8, "timestamp": "2026-09-04T01:00:01.000Z",
                   "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "A better answer."}]}}),
        ];
        std::fs::write(
            &second,
            lines
                .iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>(),
        )
        .unwrap();

        let entries = Codex::default()
            .conversation(&key(THREAD), &[second.clone(), first.clone()])
            .unwrap()
            .entries;
        let said: Vec<(u32, Speaker, &str)> = entries
            .iter()
            .map(|entry| (entry.index, entry.speaker, entry.text.as_str()))
            .collect();
        assert_eq!(
            said,
            [
                (
                    0,
                    Speaker::System,
                    "<environment_context>cwd</environment_context>"
                ),
                (1, Speaker::User, "List the files"),
                (2, Speaker::Tool, ""),
                (3, Speaker::Assistant, "Here they are."),
                (4, Speaker::Assistant, "A better answer."),
            ]
        );
        assert_eq!(
            entries[2].tool.as_ref().unwrap().output.as_deref(),
            Some("Cargo.toml")
        );
        assert_eq!(entries[3].model.as_deref(), Some("gpt-6-astra"));

        // Search finds what it shows, whichever rollout is read first.
        let both = [first.clone(), second.clone()];
        assert_said_as_shown(&Codex::default(), &key(THREAD), &both, &both);
        let reversed = [second, first];
        assert_said_as_shown(&Codex::default(), &key(THREAD), &reversed, &both);
    }

    #[test]
    fn a_thread_numbered_afresh_is_not_taken_back_past_where_it_began_again() {
        let dir = tempfile::tempdir().unwrap();
        // A thread begun and stopped at once, then begun again from ordinal 0
        // in a rollout with no base, which a third takes up from ordinal 2:
        // what the first said at 2 and later came before the fresh start,
        // and stands; what the second said there is taken back.
        let lines = |at: &str, meta: Value, said: &[(i64, &str, &str)]| -> String {
            let mut lines = vec![
                json!({"type": "session_meta", "timestamp": format!("{at}:00.000Z"),
                                        "payload": meta}),
            ];
            for (ordinal, role, text) in said {
                let kind = if *role == "user" {
                    "input_text"
                } else {
                    "output_text"
                };
                lines.push(json!({"type": "response_item", "ordinal": ordinal,
                                  "timestamp": format!("{at}:{ordinal:02}.000Z"),
                                  "payload": {"type": "message", "role": role,
                                              "content": [{"type": kind, "text": text}]}}));
            }
            lines.iter().map(|line| format!("{line}\n")).collect()
        };
        let write = |name: &str, text: String| {
            let path = dir.path().join("sessions/2026/09/05").join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            path
        };
        let begun = write(
            &format!("rollout-2026-09-05T09-00-00-{THREAD}.jsonl"),
            lines(
                "2026-09-05T09:00",
                json!({"id": THREAD}),
                &[(1, "user", "First try"), (2, "assistant", "First answer")],
            ),
        );
        let again = write(
            &format!("rollout-2026-09-05T09-10-00-{THREAD}_b.jsonl"),
            lines(
                "2026-09-05T09:10",
                json!({"id": THREAD}),
                &[(1, "user", "Second try"), (2, "assistant", "Second answer")],
            ),
        );
        let resumed = write(
            &format!("rollout-2026-09-05T09-20-00-{THREAD}_c.jsonl"),
            lines(
                "2026-09-05T09:20",
                json!({"id": THREAD, "history_base": {"thread_id": THREAD, "end_ordinal_exclusive": 2}}),
                &[(2, "assistant", "Third answer")],
            ),
        );
        let all = [begun, again, resumed];
        let entries = Codex::default()
            .conversation(&key(THREAD), &all)
            .unwrap()
            .entries;
        let said: Vec<&str> = entries.iter().map(|entry| entry.text.as_str()).collect();
        assert_eq!(
            said,
            ["First try", "First answer", "Second try", "Third answer"]
        );
        assert_said_as_shown(&Codex::default(), &key(THREAD), &all, &all);
    }

    #[test]
    fn a_thread_read_again_reads_on_and_comes_to_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let reply = |text: &str| {
            json!({"type": "response_item", "payload": {"type": "message", "role": "assistant",
                                         "content": [{"type": "output_text", "text": text}]}})
        };
        let call = json!({"type": "response_item", "payload": {"type": "function_call", "name": "exec",
                          "call_id": "call_1", "arguments": "{\"cmd\":\"ls\"}"}});
        let output = json!({"type": "response_item", "payload": {"type": "function_call_output", "call_id": "call_1",
                            "output": "Cargo.toml"}});
        let opening = [
            meta(json!({})),
            turn("gpt-6-astra"),
            user("List the files"),
            call,
        ];
        let first = rollout(dir.path(), &opening);
        let reader = Codex::default();
        let read = |reader: &Codex, rollouts: &[PathBuf]| {
            reader.conversation(&key(THREAD), rollouts).unwrap().entries
        };
        let one = [first.clone()];
        assert_eq!(read(&reader, &one), read(&Codex::default(), &one));

        // The rollout grows: the call is answered, and replied to.
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&first)
            .unwrap();
        for line in [output, reply("Here they are.")] {
            std::io::Write::write_all(&mut file, format!("{line}\n").as_bytes()).unwrap();
        }
        drop(file);
        let grown = read(&reader, &one);
        assert_eq!(grown, read(&Codex::default(), &one));
        assert_eq!(
            grown[1]
                .tool
                .as_ref()
                .and_then(|call| call.output.as_deref()),
            Some("Cargo.toml")
        );

        // The thread is resumed in a rollout of its own, after the first.
        let second = dir
            .path()
            .join("sessions/2026/09/04")
            .join(format!("rollout-2026-09-04T01-00-00-{THREAD}.jsonl"));
        std::fs::create_dir_all(second.parent().unwrap()).unwrap();
        std::fs::write(
            &second,
            format!(
                "{}\n{}\n",
                json!({"type": "session_meta", "ordinal": 7, "timestamp": "2026-09-04T01:00:00.000Z",
                       "payload": {"id": THREAD}}),
                reply("And one more thing.")
            ),
        )
        .unwrap();
        let both = [second, first];
        let resumed = read(&reader, &both);
        assert_eq!(resumed, read(&Codex::default(), &both));
        assert_eq!(
            resumed.last().map(|entry| entry.text.as_str()),
            Some("And one more thing.")
        );
    }
}
