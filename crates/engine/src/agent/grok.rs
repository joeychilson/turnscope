//! Grok Build.
//!
//! Each session is a directory, `~/.grok/sessions/<encoded directory>/<session>/`.
//! `summary.json` is rewritten as the session goes on, with its directory,
//! times, branch and generated title. `updates.jsonl` is the session's stream
//! of updates, one JSON-RPC notification a line, and the only place each
//! turn's usage is recorded: a `turn_completed` update per prompt, with the
//! turn's usage per model and what xAI charged for it.
//!
//! **Usage.** A turn's `inputTokens` includes what was read from and written to
//! the cache, and its `outputTokens` includes reasoning, so its total is input
//! plus output. `costUsdTicks` is the charge in xAI's ticks, ten billion to the
//! dollar as xAI documents, and it is the turn's cost as billed. At those
//! rates `grok-4.6-build` cost about 0.17 times the catalog's `grok-4.6`
//! prices (2026-09-23). A turn is many model calls, and Grok Build counts
//! none of them apart, so the prompt that sets its tier is taken as the
//! turn's input over its calls: their average, not their largest.
//!
//! **Grok Build's own totals.** Newer versions also write `usage.json`, with
//! the session's own totals per model, rewritten after each turn, which the
//! turns are checked against. It holds the same fields as a turn's usage,
//! overall and per model, with a list of the turns. Measured 2026-09-26: 5 of
//! 34 sessions, all begun since 2026-09-15, had one, and in all 5 each
//! model's totals equalled the sum of the session's `turn_completed` usage,
//! so none of it is outside the conversation yet.
//!
//! **The conversation** is kept in `chat_history.jsonl` alone. The other
//! files speak of the session without holding it, so a session whose
//! conversation is deleted while its summary stays can't be opened. All 34
//! sessions had `summary.json` and `chat_history.jsonl` on 2026-09-27; 9
//! small ones had no `updates.jsonl`, and recorded no usage anywhere. Each
//! reply names the model that made it in `model_id`, as all 366 assistant
//! lines did on 2026-09-26; `summary.json`'s `current_model_id` is only the
//! model in use now, and reasoning lines name none. A user line Grok Build
//! adds itself, a reminder or word that a subagent or task finished, carries
//! `synthetic_reason` (52 on 2026-09-26), and is not the person's.
//!
//! **What it writes.** The updates are of 17 kinds (2026-09-26), three of
//! which say anything counted or titled. The conversation's lines are of 6
//! types (2026-09-27, 2,331 lines): `user`, with parts of `text` and
//! `image`; `assistant`, whose content is text; and `reasoning`, `system`,
//! `tool_result` and `backend_tool_call`.
//!
//! **Subagents** are sessions of their own, spawned by a parent's
//! `subagent_spawned` update. A parent's turn does not include its subagents'
//! usage: a turn of 8 model calls spawned subagents that made 16. One child
//! session can be resumed by several `subagent_finished` updates, whose
//! `tokens_used` is a different measure from the child's own totals and is
//! never added to them.
//!
//! **Work, for a handoff.** Measured 2026-09-29 over 34 sessions' 366
//! replies: `read_file` made 855 calls, `grep` 232, `search_replace` 128,
//! `list_dir` 91, `run_terminal_command` 83, `todo_write` 7 and `write` 2,
//! among 13 tools. Grok Build marks no result failed, so what a result says
//! is all there is:
//!
//! - A command is `run_terminal_command`'s `command`; its result opens
//!   `exit: N`, or `<task-id>` for one sent to the background (3), which
//!   has no outcome yet.
//! - `search_replace`, given `file_path`, `old_string` and `new_string`,
//!   says `The file … has been updated successfully.` (116), or, given an
//!   empty `old_string`, that it `has been created successfully.` (12);
//!   `write`, given `file_path` and `content`, says the file `has been
//!   created.` (2 of 2). A result in other words is unclear, since it may
//!   be a failure.
//! - The plan is `todo_write`'s `todos`, each an `id`, `content` and
//!   `status`: the whole list, or with `merge`, the steps it names by id.
//!
//! **Which call spawned a subagent.** `subagent_spawned` names no call, but
//! gives the `description` the parent's `spawn_subagent` call gave; the call
//! is marked with it ([`Builder::launched`]) and the link keeps it. A
//! background spawn's result also names the child, but a foreground one's is
//! its answer. Measured 2026-09-27 over 11 spawns: each description is its
//! subagent's, and no parent gave two the same.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{
    Agent, AgentReader, ArtifactKind, Batch, Checkpoint, DiagnosticKind, ModelTotal, Observation,
    ReportScope, SessionReport,
};
use crate::error::{Error, Result};
use crate::handoff::{self, Change, ChangeKind, PlanItem, StepStatus, Work};
use crate::jsonl;
use crate::session::{LinkKind, SessionKey, SessionLink, Title, TitleSource};
use crate::time::Instant;
use crate::transcript::{Builder, Speaker, Titled, Transcript, argument, compact, pieces, text_of};
use crate::usage::{Tokens, Usd};

/// Grok Build's reader.
pub(super) struct Grok;

/// The provider every Grok Build turn is counted under.
const PROVIDER: &str = "xai";

/// The file holding a session's updates.
const UPDATES: &str = "updates.jsonl";

/// The file holding a session's summary.
const SUMMARY: &str = "summary.json";

/// The file holding a session's conversation.
const HISTORY: &str = "chat_history.jsonl";

/// The file holding a session's own totals, which newer versions write.
const USAGE: &str = "usage.json";

/// Kinds of update that hold nothing counted or titled, read and passed over:
/// what the agent streamed and did, its plans and tasks, and its own
/// housekeeping.
const UPDATES_PASSED_OVER: &[&str] = &[
    "agent_message_chunk",
    "agent_thought_chunk",
    "current_mode_update",
    "memory_dream_completed",
    "memory_dream_queued",
    "memory_dream_started",
    "plan",
    "retry_state",
    "session_recap",
    "subagent_finished",
    "task_backgrounded",
    "task_completed",
    "tool_call",
    "tool_call_update",
];

/// Types of conversation line that hold nothing said, for the same reason:
/// the agent's instructions, the model's reasoning, and tools' calls and
/// results.
const HISTORY_PASSED_OVER: &[&str] = &["backend_tool_call", "reasoning", "system", "tool_result"];

/// Kinds of part the person's lines of a conversation hold: what they typed,
/// and images. Measured 2026-09-27.
const USER_PARTS: &[&str] = &["image", "text"];

/// Fields of a turn's usage, and of each model's within it, that this reader
/// understands. Any other is reported, because a new field may be a new kind
/// of token that is charged.
const USAGE_FIELDS: &[&str] = &[
    "apiDurationMs",
    "cacheCreationTokens",
    "cachedReadTokens",
    "costUsdTicks",
    "inputTokens",
    "modelCalls",
    "modelUsage",
    "numTurns",
    "outputTokens",
    "reasoningTokens",
    "totalTokens",
    "usageIsIncomplete",
];

impl AgentReader for Grok {
    fn agent(&self) -> Agent {
        Agent::Grok
    }

    fn version(&self) -> u32 {
        7
    }

    fn roots(&self, folder: &Path) -> Vec<PathBuf> {
        vec![folder.join("sessions")]
    }

    fn classify(&self, root: &Path, path: &Path) -> Option<ArtifactKind> {
        let relative = path.strip_prefix(root).ok()?;
        if relative
            .components()
            .filter(|part| matches!(part, Component::Normal(_)))
            .count()
            != 3
        {
            return None;
        }
        match path.file_name()?.to_str()? {
            UPDATES | HISTORY => Some(ArtifactKind::Log),
            SUMMARY | USAGE => Some(ArtifactKind::Document),
            _ => None,
        }
    }

    fn read(&self, path: &Path, from: &Checkpoint, batch: &mut Batch) -> Result<Checkpoint> {
        let session = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(|name| SessionKey::new(Agent::Grok, name))
            .ok_or_else(|| Error::corrupt("Grok session directory", path.display().to_string()))?;
        let name = path.file_name().and_then(|name| name.to_str());
        // Only the conversation's own file holds it.
        if name == Some(HISTORY) {
            batch.session_mut(&session);
            let offset = super::read_log(path, from.offset, batch, |line, offset, batch| {
                say(line, offset, &session, batch);
            })?;
            return Ok(Checkpoint {
                offset,
                state: Vec::new(),
            });
        }
        batch.hold_only([]);
        if matches!(name, Some(SUMMARY | USAGE)) {
            match jsonl::document(path)? {
                Some(bytes) if name == Some(SUMMARY) => summarize(&bytes, &session, batch),
                Some(bytes) => report(&bytes, &session, batch),
                None => batch.note(
                    DiagnosticKind::Invalid,
                    format!(
                        "a document longer than the longest read, {} MiB",
                        jsonl::LONGEST_LINE >> 20
                    ),
                    0,
                ),
            }
            return Ok(Checkpoint::default());
        }
        let mut state: State = from.state("Grok reader state")?;
        let offset = super::read_log(path, from.offset, batch, |line, offset, batch| {
            read_update(line, offset, &session, &mut state.titled, batch);
        })?;
        Checkpoint::at(offset, &state, "Grok reader state")
    }

    fn conversation(&self, session: &SessionKey, artifacts: &[PathBuf]) -> Result<Transcript> {
        // Of a session's files, only the conversation's own holds it.
        let history = artifacts
            .iter()
            .find(|path| path.file_name().and_then(|name| name.to_str()) == Some(HISTORY))
            .filter(|history| history.exists())
            .ok_or_else(|| Error::Gone(session.to_string()))?;
        let mut builder = Builder::default();
        jsonl::each_line(history, 0, |line, _| converse(line, &mut builder))?;
        Ok(builder.finish())
    }
}

/// Record what the person or the model said on one line of a conversation,
/// at `offset`, for search. Grok records no times on it.
fn say(bytes: &[u8], offset: u64, session: &SessionKey, batch: &mut Batch) {
    let Ok(Value::Object(line)) = serde_json::from_slice::<Value>(bytes) else {
        batch.note(
            DiagnosticKind::Unreadable,
            "a line that is not a JSON object",
            offset,
        );
        return;
    };
    let content = line.get("content").unwrap_or(&Value::Null);
    match line.get("type").and_then(Value::as_str) {
        Some("user") => {
            note_parts(content, offset, batch);
            if !injected(&line) {
                for text in pieces(content) {
                    batch.prompt(session, None, text);
                }
            }
        }
        Some("assistant") => {
            if !matches!(content, Value::String(_) | Value::Null) {
                batch.note(
                    DiagnosticKind::Unreadable,
                    "a reply whose content is not text",
                    offset,
                );
            }
            batch.say(session, None, &text_of(content));
        }
        Some(other) if HISTORY_PASSED_OVER.contains(&other) => {}
        Some(other) => batch.note_unknown("history line type", other, offset),
        None => batch.note(
            DiagnosticKind::Unreadable,
            "a history line with no type",
            offset,
        ),
    }
}

/// Note each part of a user line's `content` of a kind not among
/// [`USER_PARTS`].
fn note_parts(content: &Value, offset: u64, batch: &mut Batch) {
    batch.note_parts(
        Some(content),
        USER_PARTS,
        "user content part",
        "a user line whose content is neither text nor parts",
        offset,
    );
}

/// Whether a user line of a conversation is Grok Build's own, sent as the
/// person's: a reminder, or word that a subagent or a task finished.
fn injected(line: &Map<String, Value>) -> bool {
    line.get("synthetic_reason")
        .is_some_and(|reason| !reason.is_null())
}

/// The work a call of Grok Build's tool `name`, given `input`, did, as the
/// start of its result, `head`, says: Grok Build marks no result failed, so
/// its words are all there is.
///
/// Commands are `run_terminal_command`'s, whose result opens with `exit: `
/// and the code, or with `<task-id>` for one sent to the background. Files
/// are changed by `search_replace`, given `file_path`, `old_string` and
/// `new_string`, and by `write`, given `file_path` and `content`, whose
/// result says `The file … has been updated` or `created`. The plan is
/// `todo_write`'s: its whole list, or with `merge`, the steps it names by
/// `id`.
fn worked(name: &str, input: &str, head: &str) -> Vec<Work> {
    let unclear = || vec![Work::Unclear(name.to_owned())];
    let done = head.starts_with("The file ");
    let created = done && head.contains(" has been created");
    match name {
        "run_terminal_command" => {
            let Some(command) = argument(input, "command") else {
                return unclear();
            };
            let exit = handoff::leading_number(head, "exit: ");
            let failed = match exit {
                Some(exit) => Some(exit != 0),
                None if head.starts_with("<task-id>") => None,
                None => return unclear(),
            };
            vec![Work::Ran {
                command,
                exit,
                failed,
            }]
        }
        "search_replace" | "write" if !done => unclear(),
        "search_replace" => {
            let (Some(path), Some(old), Some(new)) = (
                argument(input, "file_path"),
                argument(input, "old_string"),
                argument(input, "new_string"),
            ) else {
                return unclear();
            };
            let change = if created {
                Change::written(&path, &new, true)
            } else {
                Change::new(
                    &path,
                    ChangeKind::Updated,
                    handoff::replaced_lines(&old, &new),
                )
            };
            vec![Work::Changed(change)]
        }
        "write" => {
            let (Some(path), Some(content)) =
                (argument(input, "file_path"), argument(input, "content"))
            else {
                return unclear();
            };
            vec![Work::Changed(Change::written(&path, &content, created))]
        }
        "todo_write" => {
            let Ok(given) = serde_json::from_str::<Value>(input) else {
                return unclear();
            };
            let items: Option<Vec<PlanItem>> = given
                .get("todos")
                .and_then(Value::as_array)
                .and_then(|todos| {
                    todos
                        .iter()
                        .map(|todo| {
                            let status = match todo.get("status") {
                                None => None,
                                Some(status) => Some(StepStatus::from_word(status.as_str()?)?),
                            };
                            Some(PlanItem {
                                id: todo.get("id").and_then(Value::as_str).map(str::to_owned),
                                text: todo
                                    .get("content")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                                status,
                                removed: false,
                            })
                        })
                        .collect()
                });
            match (items, given.get("merge").and_then(Value::as_bool)) {
                (Some(items), Some(true)) => vec![Work::Revised(items)],
                (Some(items), _) if items.iter().all(|item| item.text.is_some()) => {
                    vec![Work::Planned(items)]
                }
                _ => unclear(),
            }
        }
        _ => Vec::new(),
    }
}

/// Add one line of a Grok conversation. Each reply names the model that made
/// it; a reasoning line names none.
fn converse(bytes: &[u8], builder: &mut Builder) {
    let Ok(Value::Object(line)) = serde_json::from_slice::<Value>(bytes) else {
        return;
    };
    let content = line.get("content").unwrap_or(&Value::Null);
    match line.get("type").and_then(Value::as_str) {
        Some("user") if !injected(&line) => {
            for text in pieces(content) {
                builder.prompt(None, text);
            }
        }
        // Grok Build's own words, and those it sent as the person's.
        Some("system" | "user") => builder.say(Speaker::System, None, None, text_of(content)),
        Some("reasoning") => {
            let summary = line.get("summary").map(text_of).unwrap_or_default();
            builder.say(Speaker::Reasoning, None, None, summary);
        }
        Some("assistant") => {
            let model = line.get("model_id").and_then(Value::as_str);
            builder.say(Speaker::Assistant, None, model, text_of(content));
            for call in line
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let name = call.get("name").and_then(Value::as_str);
                let input = call.get("arguments").map(compact).unwrap_or_default();
                let description = (name == Some("spawn_subagent"))
                    .then(|| argument(&input, "description"))
                    .flatten();
                builder.call(
                    call.get("id").and_then(Value::as_str),
                    None,
                    model,
                    name,
                    input,
                );
                if let Some(description) = description {
                    builder.launched(&description);
                }
            }
        }
        Some("tool_result") => {
            if let Some(id) = line.get("tool_call_id").and_then(Value::as_str)
                && let Some(call) = builder.answer(id, text_of(content), false)
            {
                for work in worked(&call.name, &call.input, &call.head()) {
                    builder.worked(None, work);
                }
            }
        }
        _ => {}
    }
}

/// What the reader carries from one read of a session's updates to the next.
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// How far the session's prompts have titled it.
    titled: Titled,
}

/// Take in a session's summary.
fn summarize(bytes: &[u8], session: &SessionKey, batch: &mut Batch) {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Summary {
        info: Info,
        created_at: Option<String>,
        updated_at: Option<String>,
        last_active_at: Option<String>,
        generated_title: Option<String>,
        head_branch: Option<String>,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Info {
        cwd: Option<String>,
    }
    let Ok(summary) = serde_json::from_slice::<Summary>(bytes) else {
        batch.note(
            DiagnosticKind::Unreadable,
            "a summary of the wrong shape",
            0,
        );
        return;
    };
    let facts = batch.session_mut(session);
    for at in [
        &summary.created_at,
        &summary.updated_at,
        &summary.last_active_at,
    ]
    .into_iter()
    .flatten()
    .filter_map(|text| Instant::parse(text))
    {
        facts.saw(at);
    }
    facts.cwd = summary.info.cwd.filter(|cwd| !cwd.is_empty());
    facts.branch = summary.head_branch.filter(|branch| !branch.is_empty());
    facts.offer_title(
        summary
            .generated_title
            .and_then(|title| Title::new(TitleSource::Generated, &title)),
    );
}

/// An update line, as far as this reader looks.
#[derive(Deserialize)]
struct Line {
    #[serde(default)]
    timestamp: Option<i64>,
    #[serde(default)]
    params: Option<Params>,
}

#[derive(Deserialize)]
struct Params {
    #[serde(default)]
    update: Option<Map<String, Value>>,
}

/// Read one update into `batch`.
fn read_update(
    bytes: &[u8],
    offset: u64,
    session: &SessionKey,
    titled: &mut Titled,
    batch: &mut Batch,
) {
    let Ok(line) = serde_json::from_slice::<Line>(bytes) else {
        batch.note(
            DiagnosticKind::Unreadable,
            "a line that is not a JSON-RPC notification",
            offset,
        );
        return;
    };
    let at = line.timestamp.and_then(Instant::from_seconds);
    if let Some(at) = at {
        batch.session_mut(session).saw(at);
    }
    let Some(update) = line.params.and_then(|params| params.update) else {
        batch.note(
            DiagnosticKind::Unreadable,
            "a notification with no update",
            offset,
        );
        return;
    };
    match update.get("sessionUpdate").and_then(Value::as_str) {
        Some("turn_completed") => turn(&update, at, session, offset, batch),
        Some("subagent_spawned") => {
            if let Some(child) = update
                .get("child_session_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
            {
                batch.link(SessionLink {
                    child: SessionKey::new(Agent::Grok, child),
                    parent: session.clone(),
                    kind: LinkKind::Subagent,
                    // The description its parent's `spawn_subagent` gave.
                    launch: update
                        .get("description")
                        .and_then(Value::as_str)
                        .filter(|description| !description.is_empty())
                        .map(str::to_owned),
                });
            }
        }
        Some("user_message_chunk") => {
            let text = update
                .get("content")
                .and_then(|content| content.get("text"))
                .and_then(Value::as_str);
            if let Some(title) = text.and_then(|text| titled.offer(text)) {
                batch.session_mut(session).offer_title(Some(title));
            }
        }
        Some(other) if UPDATES_PASSED_OVER.contains(&other) => {}
        Some(other) => batch.note_unknown("update", other, offset),
        None => batch.note(
            DiagnosticKind::Unreadable,
            "an update with no sessionUpdate",
            offset,
        ),
    }
}

/// Record a finished turn's usage, one observation per model it used.
///
/// A turn the person cancelled can finish with no usage at all, though its
/// model calls may have run a while: 3 of 51 turns here, one after 15.5 s,
/// with nothing else in the updates to count them by (measured 2026-09-27).
/// What it used is unknown, not nothing, so each is noted as usage never
/// recorded ([`DiagnosticKind::Unrecorded`]); Grok Build's own totals, where
/// it keeps them, count it outside the conversation. A turn that finished
/// with no usage for any other reason is not understood.
fn turn(
    update: &Map<String, Value>,
    at: Option<Instant>,
    session: &SessionKey,
    offset: u64,
    batch: &mut Batch,
) {
    let Some(Value::Object(usage)) = update.get("usage") else {
        match update.get("stop_reason").and_then(Value::as_str) {
            Some("cancelled") => batch.note(
                DiagnosticKind::Unrecorded,
                "a turn cancelled before its usage was recorded",
                offset,
            ),
            Some(reason) => batch.note_invalid_value(
                "a finished turn with no usage, stop_reason",
                reason,
                offset,
            ),
            None => batch.note(
                DiagnosticKind::Invalid,
                "a finished turn with no usage or stop_reason",
                offset,
            ),
        }
        return;
    };
    batch.note_unknown_fields(usage, USAGE_FIELDS, "usage", offset);
    let Some(Value::Object(models)) = usage.get("modelUsage") else {
        batch.note(
            DiagnosticKind::Invalid,
            "a turn's usage without modelUsage",
            offset,
        );
        return;
    };
    let (Some(prompt), Some(at)) = (update.get("prompt_id").and_then(Value::as_str), at) else {
        batch.note(
            DiagnosticKind::Invalid,
            "a finished turn with no prompt id or time",
            offset,
        );
        return;
    };
    for (model, used) in models {
        let Some(used) = Used::read(used, "usage.modelUsage", offset, batch) else {
            continue;
        };
        if used.tokens.total() == 0 && used.cost.is_none() {
            continue;
        }
        batch.observe(Observation {
            response: format!("{}:{prompt}:{model}", session.native()),
            session: session.clone(),
            copy: false,
            at,
            provider: PROVIDER.to_owned(),
            model: model.clone(),
            prompt: used.prompt,
            tokens: used.tokens,
            web_searches: 0,
            cost: used.cost,
            priority: false,
        });
    }
}

/// What one model used, in a turn or a whole session, as Grok Build counts
/// it.
struct Used {
    tokens: Tokens,
    /// The prompt of one of its calls, on average: its input over its calls,
    /// or all of it when how many calls were made is left out.
    prompt: u64,
    /// What xAI charged for it, when it says.
    cost: Option<Usd>,
}

impl Used {
    /// Read `used`, whose fields are named from `within` in diagnostics.
    /// `None`, noted, when it is not an object, a count is missing or out of
    /// range, or the counts contradict each other. How much of the output was
    /// reasoning, and how many calls were made, may be left out.
    fn read(used: &Value, within: &str, offset: u64, batch: &mut Batch) -> Option<Used> {
        let Value::Object(used) = used else {
            batch.note(
                DiagnosticKind::Unreadable,
                "a model's usage that is not an object",
                offset,
            );
            return None;
        };
        batch.note_unknown_fields(used, USAGE_FIELDS, within, offset);
        let count = |field: &str| super::count(used.get(field));
        let optional = |field: &str| super::optional_count(used.get(field));
        let counts = (
            count("inputTokens"),
            count("cachedReadTokens"),
            count("cacheCreationTokens"),
            count("outputTokens"),
            optional("reasoningTokens"),
            optional("modelCalls"),
        );
        let (
            Some(input),
            Some(cache_read),
            Some(cache_write),
            Some(output),
            Some(reasoning),
            Some(calls),
        ) = counts
        else {
            batch.note(
                DiagnosticKind::Invalid,
                "a model's count missing or out of range",
                offset,
            );
            return None;
        };
        let Some(fresh) = input.checked_sub(cache_read.saturating_add(cache_write)) else {
            batch.note(
                DiagnosticKind::Invalid,
                "usage whose cached input exceeds its input",
                offset,
            );
            return None;
        };
        if reasoning > output {
            batch.note(
                DiagnosticKind::Invalid,
                "usage whose reasoning exceeds its output",
                offset,
            );
            return None;
        }
        let cost = match used.get("costUsdTicks") {
            None | Some(Value::Null) => None,
            Some(ticks) => {
                let Some(cost) = ticks.as_u64().and_then(dollars_of_ticks) else {
                    batch.note(DiagnosticKind::Invalid, "a charge out of range", offset);
                    return None;
                };
                Some(cost)
            }
        };
        let tokens = Tokens {
            input: fresh,
            cache_read,
            cache_write_5m: cache_write,
            cache_write_1h: 0,
            output,
            reasoning,
        };
        Some(Used {
            tokens,
            prompt: input.div_ceil(calls.max(1)),
            cost,
        })
    }
}

/// Take in a session's own totals, per model, from its `usage.json`.
fn report(bytes: &[u8], session: &SessionKey, batch: &mut Batch) {
    #[derive(Deserialize)]
    struct Usage {
        #[serde(rename = "updatedAt", default)]
        updated_at: Option<String>,
        #[serde(default)]
        session: Option<Totals>,
    }
    #[derive(Deserialize)]
    struct Totals {
        #[serde(rename = "modelUsage", default)]
        model_usage: Option<BTreeMap<String, Value>>,
    }
    let Some((updated, models)) = serde_json::from_slice::<Usage>(bytes)
        .ok()
        .and_then(|usage| Some((usage.updated_at, usage.session?.model_usage?)))
    else {
        batch.note(
            DiagnosticKind::Unreadable,
            "a usage.json with no session.modelUsage",
            0,
        );
        return;
    };
    let mut totals = Vec::with_capacity(models.len());
    for (model, used) in models {
        let Some(used) = Used::read(&used, "session.modelUsage", 0, batch) else {
            return;
        };
        totals.push(ModelTotal {
            model: Some(model),
            input: used.tokens.input,
            cache_read: used.tokens.cache_read,
            cache_write: used.tokens.cache_write(),
            output: used.tokens.output,
            web_searches: 0,
            cost: used.cost,
        });
    }
    batch.report(SessionReport {
        session: session.clone(),
        scope: ReportScope::Session,
        at: updated.as_deref().and_then(Instant::parse),
        models: totals,
    });
}

/// A charge in xAI's ticks, ten billion to the dollar, to the nearest
/// nano-dollar: `None` beyond what one record may hold, as for a cost in
/// dollars ([`Usd::of_record_nanos`]).
fn dollars_of_ticks(ticks: u64) -> Option<Usd> {
    Usd::of_record_nanos(ticks.checked_add(5)? / 10)
}

#[cfg(test)]
mod tests {
    use crate::transcript::Speaker;
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::{Grok, dollars_of_ticks};
    use crate::Agent;
    use crate::agent::tests::{assert_said_as_shown, noted, said};
    use crate::agent::{
        AgentReader, ArtifactKind, Batch, Checkpoint, DiagnosticKind, Observation, ReportScope,
    };
    use crate::session::{LinkKind, SessionKey, TitleSource};
    use crate::usage::{Tokens, Usd};

    const SESSION: &str = "01a0797e-fa16-7a30-8ade-373a36755f55";

    fn session_dir(root: &Path) -> PathBuf {
        let dir = root
            .join("%2FUsers%2Fjoey%2FWorkspace%2Fturnscope")
            .join(SESSION);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read(path: &Path) -> Batch {
        let mut batch = Batch::default();
        Grok.read(path, &Checkpoint::default(), &mut batch).unwrap();
        batch
    }

    fn update(timestamp: i64, update: serde_json::Value) -> String {
        format!(
            "{}\n",
            json!({"timestamp": timestamp, "method": "session/update",
                               "params": {"sessionId": SESSION, "update": update}})
        )
    }

    #[test]
    fn a_turn_counts_each_model_with_what_xai_charged() {
        let dir = tempfile::tempdir().unwrap();
        let path = session_dir(dir.path()).join("updates.jsonl");
        let usage = json!({"inputTokens": 198_728, "outputTokens": 1_740, "totalTokens": 200_468,
                           "cachedReadTokens": 70_272, "cacheCreationTokens": 0, "reasoningTokens": 1_569,
                           "modelCalls": 7, "apiDurationMs": 37_971, "costUsdTicks": 514_229_600u64});
        let mut model_usage = usage.clone();
        model_usage["modelUsage"] = json!({"grok-4.6-build": usage});
        model_usage["numTurns"] = json!(7);
        let text = update(
            1_788_837_004,
            json!({"sessionUpdate": "user_message_chunk",
                                                "content": {"type": "text", "text": "Review the frontend"}}),
        ) + &update(
            1_788_837_070,
            json!({"sessionUpdate": "turn_completed", "prompt_id": "01a07efe-8412",
                                            "stop_reason": "end_turn", "usage": model_usage}),
        ) + &update(
            1_788_837_080,
            json!({"sessionUpdate": "subagent_spawned", "subagent_id": "child",
                   "child_session_id": "01a0798a-a9c4", "parent_session_id": SESSION,
                   "description": "[reviewer] routes"}),
        );
        std::fs::write(&path, text).unwrap();
        assert_eq!(Grok.classify(dir.path(), &path), Some(ArtifactKind::Log));
        let batch = read(&path);

        let seen: Vec<&Observation> = batch.observations().collect();
        assert_eq!(seen.len(), 1);
        // Input 198,728 of which 70,272 was read from the cache.
        assert_eq!(
            seen[0].tokens,
            Tokens {
                input: 128_456,
                cache_read: 70_272,
                cache_write_5m: 0,
                cache_write_1h: 0,
                output: 1_740,
                reasoning: 1_569
            }
        );
        // 514,229,600 ticks at ten billion to the dollar is $0.05142296.
        assert_eq!(seen[0].cost, Usd::from_nanos(51_422_960));
        // 198,728 tokens of input over 7 calls is 28,390 a call, rounded up.
        assert_eq!(seen[0].prompt, 28_390);
        assert_eq!(
            (seen[0].provider.as_str(), seen[0].model.as_str()),
            ("xai", "grok-4.6-build")
        );

        let link = batch.links().next().unwrap();
        assert_eq!(
            (link.child.clone(), link.parent.clone(), link.kind),
            (
                SessionKey::new(Agent::Grok, "01a0798a-a9c4"),
                SessionKey::new(Agent::Grok, SESSION),
                LinkKind::Subagent
            )
        );
        // The description the parent's `spawn_subagent` gave, which marks
        // that call in its conversation.
        assert_eq!(link.launch.as_deref(), Some("[reviewer] routes"));
        let (_, facts) = batch.sessions().next().unwrap();
        assert_eq!(facts.title.as_ref().unwrap().text, "Review the frontend");
    }

    #[test]
    fn a_turn_cut_short_counts_what_it_recorded_and_one_with_no_usage_is_noted_with_why() {
        let dir = tempfile::tempdir().unwrap();
        let path = session_dir(dir.path()).join("updates.jsonl");
        // As Grok Build writes a cancelled turn: its usage so far, and no charge.
        let used = json!({"inputTokens": 1_000, "outputTokens": 50, "totalTokens": 1_050,
                          "cachedReadTokens": 800, "cacheCreationTokens": 150, "reasoningTokens": 30,
                          "modelCalls": 2, "apiDurationMs": 9_000});
        let mut usage = used.clone();
        usage["modelUsage"] = json!({"grok-4.6-build": used});
        usage["numTurns"] = json!(2);
        usage["usageIsIncomplete"] = json!(true);
        // Then a turn cancelled after 15.5 s of model calls with no usage at
        // all; one that stopped for a reason not known; and one that says
        // neither what it used nor why it stopped.
        let text = update(
            1_788_603_906,
            json!({"sessionUpdate": "turn_completed", "prompt_id": "p2", "stop_reason": "cancelled",
                   "elapsed_ms": 10_500, "usage": usage}),
        ) + &update(
            1_788_603_930,
            json!({"sessionUpdate": "turn_completed", "prompt_id": "b14f3edf-c51d",
                   "stop_reason": "cancelled", "elapsed_ms": 15_496}),
        ) + &update(
            1_788_603_950,
            json!({"sessionUpdate": "turn_completed", "prompt_id": "c1",
                   "stop_reason": "max_tokens"}),
        ) + &update(
            1_788_603_990,
            json!({"sessionUpdate": "turn_completed", "prompt_id": "c2"}),
        );
        std::fs::write(&path, text).unwrap();
        let batch = read(&path);
        let seen: Vec<&Observation> = batch.observations().collect();
        // Input 1,000 of which 800 was read from the cache and 150 written.
        assert_eq!(
            seen[0].tokens,
            Tokens {
                input: 50,
                cache_read: 800,
                cache_write_5m: 150,
                cache_write_1h: 0,
                output: 50,
                reasoning: 30
            }
        );
        assert_eq!((seen.len(), seen[0].cost), (1, None));
        let noted = noted(&batch);
        // The usage of the turn cancelled without any is known to be
        // missing, and understood; the others' is not.
        assert_eq!(
            noted,
            [
                (
                    DiagnosticKind::Invalid,
                    "a finished turn with no usage or stop_reason"
                ),
                (
                    DiagnosticKind::Invalid,
                    "a finished turn with no usage, stop_reason max_tokens"
                ),
                (
                    DiagnosticKind::Unrecorded,
                    "a turn cancelled before its usage was recorded"
                ),
            ]
        );
    }

    #[test]
    fn usage_missing_out_of_range_or_at_odds_with_itself_is_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        let path = session_dir(dir.path()).join("updates.jsonl");
        let turn = |prompt: &str, used: serde_json::Value| {
            let mut usage = used.clone();
            usage["modelUsage"] = json!({"grok-4.6-build": used});
            update(
                1_788_837_070,
                json!({"sessionUpdate": "turn_completed", "prompt_id": prompt, "usage": usage}),
            )
        };
        let used = |input: u64, cached: u64, output: u64, reasoning: u64| {
            json!({"inputTokens": input, "outputTokens": output, "totalTokens": input + output,
                   "cachedReadTokens": cached, "cacheCreationTokens": 0, "reasoningTokens": reasoning,
                   "modelCalls": 1, "costUsdTicks": 1_000_000})
        };
        // One more than 2^50 = 1,125,899,906,842,624; no count of cache reads;
        // cached input of 900 in an input of 800; and reasoning of 60 in an
        // output of 50.
        let mut huge = used(1_000, 800, 50, 10);
        huge["outputTokens"] = json!(1_125_899_906_842_625_u64);
        let mut missing = used(1_000, 800, 50, 10);
        missing.as_object_mut().unwrap().remove("cachedReadTokens");
        let text = turn("p1", used(1_000, 800, 50, 10))
            + &turn("p2", huge)
            + &turn("p3", missing)
            + &turn("p4", used(800, 900, 50, 10))
            + &turn("p5", used(1_000, 800, 50, 60));
        std::fs::write(&path, text).unwrap();
        let batch = read(&path);

        let counted: Vec<&str> = batch
            .observations()
            .map(|seen| seen.response.as_str())
            .collect();
        assert_eq!(counted, [format!("{SESSION}:p1:grok-4.6-build")]);
        let noted = noted(&batch);
        assert_eq!(
            noted,
            [
                (
                    DiagnosticKind::Invalid,
                    "a model's count missing or out of range"
                ),
                (
                    DiagnosticKind::Invalid,
                    "usage whose cached input exceeds its input"
                ),
                (
                    DiagnosticKind::Invalid,
                    "usage whose reasoning exceeds its output"
                ),
            ]
        );
    }

    #[test]
    fn the_summary_gives_the_directory_branch_and_generated_title() {
        let dir = tempfile::tempdir().unwrap();
        let path = session_dir(dir.path()).join("summary.json");
        std::fs::write(
            &path,
            json!({"info": {"id": SESSION, "cwd": "/Users/joey/Workspace/turnscope"},
                   "created_at": "2026-09-13T07:23:38.466582Z", "updated_at": "2026-09-13T07:23:39.364904Z",
                   "generated_title": "Frontend review", "head_branch": "main", "num_chat_messages": 2})
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            Grok.classify(dir.path(), &path),
            Some(ArtifactKind::Document)
        );
        let batch = read(&path);
        let (_, facts) = batch.sessions().next().unwrap();
        assert_eq!(
            facts.cwd.as_deref(),
            Some("/Users/joey/Workspace/turnscope")
        );
        assert_eq!(facts.branch.as_deref(), Some("main"));
        let title = facts.title.as_ref().unwrap();
        assert_eq!(
            (title.source, title.text.as_str()),
            (TitleSource::Generated, "Frontend review")
        );
    }

    #[test]
    fn grok_builds_own_totals_are_reported_for_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = session_dir(dir.path()).join("usage.json");
        let used = json!({"inputTokens": 69_532, "outputTokens": 1_832, "cachedReadTokens": 5_376,
                          "cacheCreationTokens": 0, "reasoningTokens": 1_456, "totalTokens": 71_364,
                          "modelCalls": 3, "costUsdTicks": 482_772_800u64});
        let mut session = used.clone();
        session["turnCount"] = json!(1);
        session["primaryModelId"] = json!("grok-4.7-build");
        session["modelUsage"] = json!({"grok-4.7-build": used});
        std::fs::write(
            &path,
            json!({"sessionId": SESSION, "updatedAt": "2026-09-24T17:36:09.002507+00:00",
                   "session": session, "turns": []})
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            Grok.classify(dir.path(), &path),
            Some(ArtifactKind::Document)
        );
        let batch = read(&path);

        let report = batch.reports().next().unwrap();
        assert_eq!(
            (report.session.native(), report.scope),
            (SESSION, ReportScope::Session)
        );
        assert_eq!(report.at.unwrap().millis(), 1_790_271_369_002);
        let total = &report.models[0];
        // Input 69,532 of which 5,376 was read from the cache: 64,156 fresh.
        assert_eq!(
            (
                total.model.as_deref(),
                total.input,
                total.cache_read,
                total.cache_write,
                total.output
            ),
            (Some("grok-4.7-build"), 64_156, 5_376, 0, 1_832)
        );
        // 482,772,800 ticks at ten billion to the dollar is $0.04827728.
        assert_eq!(total.cost, Usd::from_nanos(48_277_280));
        assert_eq!(batch.diagnostics().count(), 0);
    }

    #[test]
    fn ticks_are_ten_billion_to_the_dollar() {
        // xAI documents a charge of 37,756,000 ticks as $0.0037756.
        assert_eq!(dollars_of_ticks(37_756_000), Usd::from_nanos(3_775_600));
        assert_eq!(dollars_of_ticks(15), Usd::from_nanos(2));
        // A billion dollars, 10^19 ticks, is the most one record holds, as
        // for a cost in dollars; a nano-dollar more, or $1.8 billion, is
        // out of range.
        assert_eq!(
            dollars_of_ticks(10_000_000_000_000_000_000),
            Usd::from_nanos(1_000_000_000_000_000_000)
        );
        assert_eq!(dollars_of_ticks(10_000_000_000_000_000_010), None);
        assert_eq!(dollars_of_ticks(18_000_000_000_000_000_000), None);
    }

    #[test]
    fn a_conversation_is_read_from_its_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let folder = session_dir(dir.path());
        // The model in use now, which made neither reply.
        std::fs::write(
            folder.join("summary.json"),
            json!({"current_model_id": "grok-4.7-build"}).to_string(),
        )
        .unwrap();
        let lines = [
            json!({"type": "user", "content": [{"type": "text", "text": "Review the frontend"}], "prompt_index": 0}),
            json!({"type": "reasoning", "id": "rs_1", "status": "completed", "encrypted_content": "gAAAAB",
                   "summary": [{"type": "summary_text", "text": "Start with the routes."}]}),
            json!({"type": "assistant", "content": "Reading the routes.", "model_id": "grok-4.6-build",
                   "model_fingerprint": "fp_1", "reasoning_effort": "high",
                   "tool_calls": [{"id": "call_1", "name": "list_dir", "arguments": "{\"target_directory\":\"src\"}"},
                                  {"id": "call_2", "name": "spawn_subagent",
                                   "arguments": "{\"description\":\"[reviewer] routes\",\"prompt\":\"…\"}"}]}),
            json!({"type": "tool_result", "tool_call_id": "call_1", "content": "- routes/"}),
            // Grok's word that a subagent finished, sent as the person's and
            // marked as its own whatever its text.
            json!({"type": "user", "content": [{"type": "text", "text": "Subagent routes_review finished."}],
                   "prompt_index": 1, "synthetic_reason": "subagent_completed"}),
        ];
        std::fs::write(
            folder.join("chat_history.jsonl"),
            lines
                .iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let history = folder.join("chat_history.jsonl");
        let session = SessionKey::new(Agent::Grok, SESSION);
        let transcript = Grok
            .conversation(&session, std::slice::from_ref(&history))
            .unwrap();
        // The spawn is marked by the description its subagent's link records.
        assert_eq!(transcript.launches, [(4, "[reviewer] routes".to_owned())]);
        let entries = transcript.entries;
        assert_said_as_shown(
            &Grok,
            &session,
            std::slice::from_ref(&history),
            std::slice::from_ref(&history),
        );
        let speakers: Vec<Speaker> = entries.iter().map(|entry| entry.speaker).collect();
        assert_eq!(
            speakers,
            [
                Speaker::User,
                Speaker::Reasoning,
                Speaker::Assistant,
                Speaker::Tool,
                Speaker::Tool,
                Speaker::System
            ]
        );
        assert_eq!(
            entries[3].tool.as_ref().unwrap().output.as_deref(),
            Some("- routes/")
        );
        let models: Vec<Option<&str>> =
            entries.iter().map(|entry| entry.model.as_deref()).collect();
        assert_eq!(
            models,
            [
                None,
                None,
                Some("grok-4.6-build"),
                Some("grok-4.6-build"),
                Some("grok-4.6-build"),
                None
            ]
        );
    }

    #[test]
    fn updates_and_conversation_lines_of_kinds_not_known_are_noted() {
        let dir = tempfile::tempdir().unwrap();
        let folder = session_dir(dir.path());
        let updates = folder.join("updates.jsonl");
        std::fs::write(
            &updates,
            update(
                1_788_837_004,
                json!({"sessionUpdate": "tool_call", "toolCallId": "call_1", "status": "pending"}),
            ) + &update(1_788_837_005, json!({"sessionUpdate": "hologram"})),
        )
        .unwrap();
        let history = folder.join("chat_history.jsonl");
        std::fs::write(
            &history,
            format!(
                "{}\n{}\n{}\n",
                json!({"type": "system", "content": "You are Grok Build."}),
                json!({"type": "hologram", "content": "?"}),
                // A part of a kind not known beside what the person typed.
                json!({"type": "user", "content": [{"type": "text", "text": "Review the frontend"},
                                                   {"type": "hologram", "data": "?"}]})
            ),
        )
        .unwrap();
        let mut batch = Batch::default();
        for path in [&updates, &history] {
            Grok.read(path, &Checkpoint::default(), &mut batch).unwrap();
        }
        let said = said(&batch);
        assert_eq!(said, ["Review the frontend"]);
        let noted = noted(&batch);
        assert_eq!(
            noted,
            [
                (DiagnosticKind::UnknownRecord, "history line type hologram"),
                (DiagnosticKind::UnknownRecord, "update hologram"),
                (DiagnosticKind::UnknownRecord, "user content part hologram"),
            ]
        );
    }
}
