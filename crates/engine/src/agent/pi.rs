//! Pi.
//!
//! Each session is a JSON Lines file,
//! `~/.pi/agent/sessions/<directory>/<time>_<session>.jsonl`, opening with a
//! `session` header that names the session and its working directory. Every
//! later line is an entry with an `id` and the `parentId` it follows, so a
//! session is a tree: going back and trying again starts a new branch, and
//! the abandoned one stays in the file. Its responses were still made and
//! charged, so every branch is counted.
//!
//! **Usage.** An assistant message carries `usage`: `input` excludes cached
//! input, `output` includes `reasoning`, and `cost` is what Pi worked out it
//! cost, broken down by kind. Those costs settle what the counts mean: the
//! output cost divided by `output` alone gives exactly the catalog's output
//! price, so `reasoning` is part of `output`, and `totalTokens` is input plus
//! output plus cache reads and writes, so `input` leaves cached input out. A
//! response is identified by its `responseId`, which a forked session copying
//! the response would carry too, so it is counted once; no forked Pi session
//! existed on this Mac to show it (2026-09-23).
//!
//! **What it writes.** Measured 2026-09-27 over 185 entries: entries of
//! `session`, `message`, `model_change` and `thinking_level_change`;
//! messages of the roles `user`, `assistant` and `toolResult`, with blocks of
//! `text` from the person, `text`, `thinking` and `toolCall` in responses,
//! and `text` and `image` in a tool's result; and a response's `usage.cost`
//! of `input`, `output`, `cacheRead`, `cacheWrite` and `total`.
//!
//! **Titles.** A `session_info` entry holds a name the user gave the session;
//! otherwise the first prompt titles it.
//!
//! **Work, for a handoff.** Measured 2026-09-29 over 8 sessions: `bash` made
//! 59 calls and `read` 19, and no other tool was called. A command is
//! `bash`'s `command`; a failed one's result is marked `isError`, as 1 was,
//! and ends with `Command exited with code N`. Files are changed by `edit`,
//! given a `path`, the `oldText` it replaces and the `newText`, and `write`,
//! given a `path` and `content`, read as Pi's tools describe them since none
//! was here; a write over a file removed an unknown number of lines. Pi
//! keeps no plan.
//!
//! **Providers.** A response names its provider as Pi's own registry does,
//! which is kept as it is: its ChatGPT sign-in is the provider
//! `openai-codex`, kept in `auth.json` under that name, beside `openai` for
//! an API key, so Pi's use of a ChatGPT plan is told from its use of
//! OpenAI's API by the provider its responses name ([`crate::limits`]).
//! Pi's registry lists `openai-codex` as a provider of its own (pi-mono,
//! read 2026-09-29). Measured the same day: this Mac's 60 responses in 8
//! sessions named `opencode-go` (54), `openrouter` (3) and `xai` (3), none
//! `openai-codex`; `auth.json` held `xai` as `oauth`, `openrouter` as
//! `oauth` whose `access` is an OpenRouter key (`sk-or-`) that never
//! expires, and `opencode-go` as `api_key`. Pi is pointed at another folder
//! by `PI_CODING_AGENT_DIR`, its sessions and `auth.json` in it.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use super::{Agent, AgentReader, ArtifactKind, Batch, Checkpoint, DiagnosticKind, Observation};
use crate::error::{Error, Result};
use crate::handoff::{self, Change, ChangeKind, Work};
use crate::jsonl;
use crate::session::{SessionKey, Title, TitleSource};
use crate::time::Instant;
use crate::transcript::{Builder, Speaker, Titled, Transcript, argument, compact, pieces, text_of};
use crate::usage::Tokens;

/// Pi's reader.
pub(super) struct Pi;

/// Entry types that hold nothing counted or said. `model_change` and
/// `thinking_level_change` were measured; the others are the rest of the
/// entry types Pi's session manager writes, none of them written here.
const PASSED_OVER: &[&str] = &[
    "branch_summary",
    "compaction",
    "custom",
    "custom_message",
    "label",
    "model_change",
    "thinking_level_change",
];

/// Kinds of content block the person's messages hold: what they typed.
/// Measured 2026-09-27, as the other kinds of block are.
const USER_BLOCKS: &[&str] = &["text"];

/// Kinds of content block a response holds: its text, thinking and tool
/// calls.
const ASSISTANT_BLOCKS: &[&str] = &["text", "thinking", "toolCall"];

/// Kinds of content block a tool's result holds: its text and images.
const RESULT_BLOCKS: &[&str] = &["image", "text"];

/// Parts of `usage.cost` this reader understands: Pi's cost by kind, and
/// in all. Any other is reported, since it may be a charge `total` leaves
/// out.
const COST_FIELDS: &[&str] = &["cacheRead", "cacheWrite", "input", "output", "total"];

/// Fields of `usage` this reader understands. Any other is reported, because
/// a new field may be a new kind of token that is charged.
const USAGE_FIELDS: &[&str] = &[
    "cacheRead",
    "cacheWrite",
    "cost",
    "input",
    "output",
    "reasoning",
    "totalTokens",
];

impl AgentReader for Pi {
    fn agent(&self) -> Agent {
        Agent::Pi
    }

    fn version(&self) -> u32 {
        4
    }

    fn roots(&self, folder: &Path) -> Vec<PathBuf> {
        vec![folder.join("sessions")]
    }

    fn classify(&self, root: &Path, path: &Path) -> Option<ArtifactKind> {
        let depth = path.strip_prefix(root).ok()?.components().count();
        let name = path.file_name()?.to_str()?;
        (depth == 2 && name.ends_with(".jsonl")).then_some(ArtifactKind::Log)
    }

    fn read(&self, path: &Path, from: &Checkpoint, batch: &mut Batch) -> Result<Checkpoint> {
        let mut state: State = from.state("Pi reader state")?;
        let offset = super::read_log(path, from.offset, batch, |line, offset, batch| {
            read_line(line, offset, &mut state, batch);
        })?;
        Checkpoint::at(offset, &state, "Pi reader state")
    }

    fn conversation(&self, session: &SessionKey, artifacts: &[PathBuf]) -> Result<Transcript> {
        // A Pi session is one file; every branch is shown in the order written.
        let path = artifacts
            .first()
            .ok_or_else(|| Error::Gone(session.to_string()))?;
        let mut builder = Builder::default();
        jsonl::each_line(path, 0, |line, _| converse(line, &mut builder))?;
        Ok(builder.finish())
    }
}

/// Add one entry of a Pi session to its conversation.
fn converse(bytes: &[u8], builder: &mut Builder) {
    let Ok(Value::Object(entry)) = serde_json::from_slice::<Value>(bytes) else {
        return;
    };
    let entry_at = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(Instant::parse);
    match entry.get("type").and_then(Value::as_str) {
        Some("message") => {}
        Some("compaction" | "branch_summary") => {
            let summary = entry
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or_default();
            builder.say(Speaker::System, entry_at, None, summary);
            return;
        }
        _ => return,
    }
    let Some(Value::Object(message)) = entry.get("message") else {
        return;
    };
    let at = message
        .get("timestamp")
        .and_then(Value::as_i64)
        .and_then(Instant::from_millis)
        .or(entry_at);
    let model = message.get("model").and_then(Value::as_str);
    let content = message.get("content").unwrap_or(&Value::Null);
    match message.get("role").and_then(Value::as_str) {
        Some("user") => {
            for text in texts(message) {
                builder.prompt(at, text);
            }
        }
        Some("assistant") => {
            let Some(blocks) = content.as_array() else {
                // Content given as text rather than blocks.
                for text in texts(message) {
                    builder.say(Speaker::Assistant, at, model, text);
                }
                return;
            };
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => builder.say(
                        Speaker::Assistant,
                        at,
                        model,
                        block["text"].as_str().unwrap_or_default(),
                    ),
                    Some("thinking") => builder.say(
                        Speaker::Reasoning,
                        at,
                        model,
                        block["thinking"].as_str().unwrap_or_default(),
                    ),
                    Some("toolCall") => builder.call(
                        block.get("id").and_then(Value::as_str),
                        at,
                        model,
                        block.get("name").and_then(Value::as_str),
                        block.get("arguments").map(compact).unwrap_or_default(),
                    ),
                    _ => {}
                }
            }
        }
        Some("toolResult") => {
            if let Some(id) = message.get("toolCallId").and_then(Value::as_str) {
                let failed = message
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let output = text_of(content);
                // The exit code a failed command ends its output with.
                let exit = output
                    .lines()
                    .next_back()
                    .and_then(|last| handoff::leading_number(last, "Command exited with code "));
                let call = builder
                    .answer(id, output, failed)
                    .map(|call| (call.name.clone(), call.input.clone()));
                if let Some((name, input)) = call {
                    for work in worked(&name, &input, failed, exit) {
                        builder.worked(at, work);
                    }
                }
            }
        }
        _ => builder.say(Speaker::System, at, None, text_of(content)),
    }
}

/// The work a call of Pi's tool `name`, given `input`, did, as whether it
/// failed and the `exit` code a failed command's output ends with say.
/// Commands are `bash`'s; files are changed by `edit`, given a `path`, the
/// `oldText` it replaces and the `newText`, and by `write`, given a `path`
/// and the `content`, which doesn't say what the file held before.
fn worked(name: &str, input: &str, failed: bool, exit: Option<i64>) -> Vec<Work> {
    let unclear = || vec![Work::Unclear(name.to_owned())];
    match name {
        "bash" => match argument(input, "command") {
            Some(command) => vec![Work::Ran {
                command,
                exit: if failed { exit } else { None },
                failed: Some(failed),
            }],
            None => unclear(),
        },
        "edit" | "write" if failed => Vec::new(),
        "edit" => {
            let (Some(path), Some(old), Some(new)) = (
                argument(input, "path"),
                argument(input, "oldText"),
                argument(input, "newText"),
            ) else {
                return unclear();
            };
            let counted = handoff::replaced_lines(&old, &new);
            vec![Work::Changed(Change::new(
                &path,
                ChangeKind::Updated,
                counted,
            ))]
        }
        "write" => {
            let (Some(path), Some(content)) = (argument(input, "path"), argument(input, "content"))
            else {
                return unclear();
            };
            vec![Work::Changed(Change {
                removed: None,
                ..Change::new(
                    &path,
                    ChangeKind::Updated,
                    Some((handoff::lines(&content), 0)),
                )
            })]
        }
        _ => Vec::new(),
    }
}

/// What the reader carries from one read of a session to the next.
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// The session, from its header.
    session: Option<String>,
    /// How far the session's prompts have titled it.
    titled: Titled,
}

/// An entry, as far as this reader looks.
#[derive(Deserialize)]
struct Line<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    cwd: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    name: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    message: Option<&'a RawValue>,
}

/// Read one line into `batch`.
fn read_line(bytes: &[u8], offset: u64, state: &mut State, batch: &mut Batch) {
    let Ok(line) = serde_json::from_slice::<Line>(bytes) else {
        batch.note(
            DiagnosticKind::Unreadable,
            "a line that is not a JSON object",
            offset,
        );
        return;
    };
    let kind = line.kind.as_deref().unwrap_or_default();
    let at = line.timestamp.as_deref().and_then(Instant::parse);
    if kind == "session" {
        let Some(id) = line.id.filter(|id| !id.is_empty()) else {
            batch.note(
                DiagnosticKind::Invalid,
                "a session header with no id",
                offset,
            );
            return;
        };
        let facts = batch.session_mut(&SessionKey::new(Agent::Pi, id.as_ref()));
        if let Some(at) = at {
            facts.saw(at);
        }
        if facts.cwd.is_none() {
            facts.cwd = line.cwd.filter(|cwd| !cwd.is_empty()).map(Cow::into_owned);
        }
        state.session = Some(id.into_owned());
        return;
    }
    let Some(session) = state
        .session
        .clone()
        .map(|id| SessionKey::new(Agent::Pi, id))
    else {
        batch.note(
            DiagnosticKind::Unreadable,
            "an entry before the session header",
            offset,
        );
        return;
    };
    match kind {
        "message" => {
            let Some(raw) = line.message else {
                batch.note(
                    DiagnosticKind::Unreadable,
                    "a message entry with no message",
                    offset,
                );
                return;
            };
            let Ok(Value::Object(message)) = serde_json::from_str::<Value>(raw.get()) else {
                batch.note(
                    DiagnosticKind::Unreadable,
                    "a message that is not an object",
                    offset,
                );
                return;
            };
            match message.get("role").and_then(Value::as_str) {
                Some("assistant") => {
                    note_blocks(&message, ASSISTANT_BLOCKS, offset, batch);
                    for text in texts(&message) {
                        batch.say(&session, at, text);
                    }
                    observe(&message, line.id.as_deref(), at, &session, offset, batch);
                }
                Some("user") => {
                    note_blocks(&message, USER_BLOCKS, offset, batch);
                    for text in texts(&message) {
                        batch.prompt(&session, at, text);
                        if let Some(title) = state.titled.offer(text) {
                            batch.session_mut(&session).offer_title(Some(title));
                        }
                    }
                }
                // A tool's result, which the conversation folds into its call.
                Some("toolResult") => note_blocks(&message, RESULT_BLOCKS, offset, batch),
                Some(other) => batch.note_unknown("message role", other, offset),
                None => batch.note(DiagnosticKind::Unreadable, "a message with no role", offset),
            }
        }
        "session_info" => {
            let title = line
                .name
                .and_then(|name| Title::new(TitleSource::Named, &name));
            batch.session_mut(&session).offer_title(title);
        }
        other if PASSED_OVER.contains(&other) => {}
        "" => {
            batch.note(DiagnosticKind::Unreadable, "an entry with no type", offset);
            return;
        }
        other => {
            batch.note_unknown("entry type", other, offset);
            return;
        }
    }
    if let Some(at) = at {
        batch.session_mut(&session).saw(at);
    }
}

/// Note each block of `message`'s content of a kind not among `known`.
fn note_blocks(message: &Map<String, Value>, known: &[&str], offset: u64, batch: &mut Batch) {
    let blocks = match message.get("content") {
        Some(Value::Array(blocks)) => blocks,
        // Text, or nothing, has no blocks.
        None | Some(Value::Null | Value::String(_)) => return,
        Some(_) => {
            return batch.note(
                DiagnosticKind::Unreadable,
                "a message whose content is neither text nor blocks",
                offset,
            );
        }
    };
    for block in blocks {
        let kind = block.get("type").and_then(Value::as_str);
        batch.note_kind(kind, known, "content block", offset);
    }
}

/// The text a message holds: its content, when that is a string, or the
/// text of each of its text blocks.
fn texts(message: &Map<String, Value>) -> Vec<&str> {
    match message.get("content").unwrap_or(&Value::Null) {
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect(),
        other => pieces(other),
    }
}

/// Record an assistant message's usage.
fn observe(
    message: &Map<String, Value>,
    entry: Option<&str>,
    at: Option<Instant>,
    session: &SessionKey,
    offset: u64,
    batch: &mut Batch,
) {
    let usage = match message.get("usage") {
        Some(Value::Object(usage)) => usage,
        None | Some(Value::Null) => {
            batch.note(DiagnosticKind::Invalid, "a response with no usage", offset);
            return;
        }
        Some(_) => {
            batch.note(
                DiagnosticKind::Unreadable,
                "a response's usage that is not an object",
                offset,
            );
            return;
        }
    };
    batch.note_unknown_fields(usage, USAGE_FIELDS, "usage", offset);
    // How much of the output was reasoning may be left out; the rest are
    // the usage itself.
    let count = |field: &str| super::count(usage.get(field));
    let counts = (
        count("input"),
        count("cacheRead"),
        count("cacheWrite"),
        count("output"),
        super::optional_count(usage.get("reasoning")),
    );
    let (Some(input), Some(cache_read), Some(cache_write), Some(output), Some(reasoning)) = counts
    else {
        batch.note(
            DiagnosticKind::Invalid,
            "a usage count missing or out of range",
            offset,
        );
        return;
    };
    if reasoning > output {
        batch.note(
            DiagnosticKind::Invalid,
            "usage whose reasoning exceeds its output",
            offset,
        );
        return;
    }
    let tokens = Tokens {
        input,
        cache_read,
        cache_write_5m: cache_write,
        cache_write_1h: 0,
        output,
        reasoning,
    };
    if tokens.total() == 0 {
        return;
    }
    if let Some(Value::Object(cost)) = usage.get("cost") {
        batch.note_unknown_fields(cost, COST_FIELDS, "usage.cost", offset);
    }
    let Some(cost) = super::cost(usage.get("cost").and_then(|cost| cost.get("total"))) else {
        batch.note(DiagnosticKind::Invalid, "a usage cost out of range", offset);
        return;
    };
    let text = |field: &str| {
        message
            .get(field)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
    };
    let (Some(provider), Some(model)) = (text("provider"), text("model")) else {
        batch.note(
            DiagnosticKind::Invalid,
            "usage with no provider or model",
            offset,
        );
        return;
    };
    let response = match (text("responseId"), entry) {
        (Some(response), _) => response.to_owned(),
        (None, Some(entry)) => format!("{}:{entry}", session.native()),
        (None, None) => {
            batch.note(DiagnosticKind::Invalid, "a response with no id", offset);
            return;
        }
    };
    let at = message
        .get("timestamp")
        .and_then(Value::as_i64)
        .and_then(Instant::from_millis)
        .or(at);
    let Some(at) = at else {
        batch.note(DiagnosticKind::Invalid, "a response with no time", offset);
        return;
    };
    batch.observe(Observation {
        response,
        session: session.clone(),
        copy: false,
        at,
        provider: provider.to_owned(),
        model: model.to_owned(),
        prompt: tokens.prompt(),
        tokens,
        web_searches: 0,
        cost,
        priority: false,
    });
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::Pi;
    use crate::Agent;
    use crate::agent::tests::assert_said_as_shown;
    use crate::agent::{AgentReader, Batch, Checkpoint, DiagnosticKind, Observation};
    use crate::session::{SessionKey, TitleSource};
    use crate::transcript::Speaker;
    use crate::usage::{Tokens, Usd};

    const SESSION: &str = "01a099f5-aebf-77c5-8da6-a343af149636";

    /// Write a session of `lines` under a sessions root, and read it.
    fn read(dir: &Path, lines: &[serde_json::Value]) -> (PathBuf, Batch) {
        let path = dir
            .join("--work-turnscope--")
            .join(format!("2026-09-13T08-50-10-240Z_{SESSION}.jsonl"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(&path, text).unwrap();
        assert!(Pi.classify(dir, &path).is_some());
        let mut batch = Batch::default();
        Pi.read(&path, &Checkpoint::default(), &mut batch).unwrap();
        (path, batch)
    }

    fn noted(batch: &Batch) -> Vec<(DiagnosticKind, &str)> {
        batch
            .diagnostics()
            .map(|((kind, detail), _)| (*kind, detail.as_str()))
            .collect()
    }

    fn header() -> serde_json::Value {
        json!({"type": "session", "version": 3, "id": SESSION, "timestamp": "2026-09-13T08:50:10.240Z",
               "cwd": "/work/turnscope"})
    }

    fn assistant(
        id: &str,
        parent: &str,
        response: &str,
        input: u64,
        output: u64,
        reasoning: u64,
    ) -> serde_json::Value {
        json!({"type": "message", "id": id, "parentId": parent, "timestamp": "2026-09-13T08:51:00.000Z",
               "message": {"role": "assistant", "provider": "opencode-go", "model": "glm-5.3",
                           "responseId": response, "timestamp": 1_789_289_428_586u64,
                           "usage": {"input": input, "output": output, "cacheRead": 50, "cacheWrite": 0,
                                     "reasoning": reasoning, "totalTokens": input + output + 50,
                                     "cost": {"input": 0.004_851, "output": 0.000_506, "cacheRead": 0.000_013,
                                              "cacheWrite": 0, "total": 0.005_37}}}})
    }

    #[test]
    fn every_branch_of_a_session_is_counted_with_reasoning_inside_output() {
        let dir = tempfile::tempdir().unwrap();
        let (_, batch) = read(
            dir.path(),
            &[
                header(),
                json!({"type": "message", "id": "u1", "parentId": null, "timestamp": "2026-09-13T08:50:20.000Z",
                   "message": {"role": "user", "content": [{"type": "text", "text": "Index the sessions"}]}}),
                assistant("a1", "u1", "chatcmpl-1", 3_465, 115, 10),
                // Going back to the prompt and trying again: a second branch.
                assistant("a2", "u1", "chatcmpl-2", 3_000, 200, 0),
            ],
        );
        let seen: Vec<&Observation> = batch.observations().collect();
        assert_eq!(seen.len(), 2);
        let first = seen
            .iter()
            .find(|seen| seen.response == "chatcmpl-1")
            .unwrap();
        assert_eq!(
            first.tokens,
            Tokens {
                input: 3_465,
                cache_read: 50,
                cache_write_5m: 0,
                cache_write_1h: 0,
                output: 115,
                reasoning: 10
            }
        );
        assert_eq!(first.cost, Usd::from_nanos(5_370_000));
        assert_eq!(first.at.millis(), 1_789_289_428_586);
        let (_, facts) = batch.sessions().next().unwrap();
        assert_eq!(facts.cwd.as_deref(), Some("/work/turnscope"));
        assert_eq!(facts.title.as_ref().unwrap().text, "Index the sessions");
    }

    #[test]
    fn a_name_the_user_gave_outranks_the_first_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let (_, batch) = read(
            dir.path(),
            &[
                header(),
                json!({"type": "message", "id": "u1", "parentId": null, "timestamp": "2026-09-13T08:50:20.000Z",
                   "message": {"role": "user", "content": "Index the sessions"}}),
                json!({"type": "session_info", "id": "i1", "parentId": "u1", "timestamp": "2026-09-13T08:52:00.000Z",
                   "name": "session index"}),
            ],
        );
        let (_, facts) = batch.sessions().next().unwrap();
        let title = facts.title.as_ref().unwrap();
        assert_eq!(
            (title.source, title.text.as_str()),
            (TitleSource::Named, "session index")
        );
    }

    #[test]
    fn entries_and_messages_of_kinds_not_known_are_noted() {
        let dir = tempfile::tempdir().unwrap();
        // A response with a block of a kind not known beside its text, and
        // a part of its cost not known.
        let mut reply = assistant("a1", "u1", "chatcmpl-1", 3_465, 115, 10);
        reply["message"]["content"] = json!([{"type": "text", "text": "Indexed."},
                                             {"type": "hologram", "data": "?"}]);
        reply["message"]["usage"]["cost"]["hologram"] = json!(0.001);
        let (_, batch) = read(
            dir.path(),
            &[
                header(),
                json!({"type": "hologram", "id": "h1"}),
                json!({"type": "message", "id": "m1", "parentId": null, "timestamp": "2026-09-13T08:50:20.000Z",
                       "message": {"role": "hologram", "content": "?"}}),
                reply,
            ],
        );
        // The response is still counted and said.
        assert_eq!(batch.observations().count(), 1);
        let said: Vec<&str> = batch.said().map(|said| said.text.as_str()).collect();
        assert_eq!(said, ["Indexed."]);
        assert_eq!(
            noted(&batch),
            [
                (DiagnosticKind::UnknownRecord, "content block hologram"),
                (DiagnosticKind::UnknownRecord, "entry type hologram"),
                (DiagnosticKind::UnknownRecord, "message role hologram"),
                (DiagnosticKind::UnknownField, "usage.cost.hologram"),
            ]
        );
    }

    #[test]
    fn usage_missing_out_of_range_or_at_odds_with_itself_is_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        // One more than 2^50 = 1,125,899,906,842,624; a negative count; no
        // count of cache reads, which Pi always writes; and reasoning of 300
        // in an output of 200.
        let mut huge = assistant("a2", "u1", "chatcmpl-2", 3_000, 200, 0);
        huge["message"]["usage"]["input"] = json!(1_125_899_906_842_625_u64);
        let mut negative = assistant("a3", "u1", "chatcmpl-3", 3_000, 200, 0);
        negative["message"]["usage"]["output"] = json!(-1);
        let mut missing = assistant("a5", "u1", "chatcmpl-5", 3_000, 200, 0);
        missing["message"]["usage"]
            .as_object_mut()
            .unwrap()
            .remove("cacheRead");
        // No usage at all, and usage that is not an object.
        let mut unused = assistant("a6", "u1", "chatcmpl-6", 3_000, 200, 0);
        unused["message"].as_object_mut().unwrap().remove("usage");
        let mut misshapen = assistant("a7", "u1", "chatcmpl-7", 3_000, 200, 0);
        misshapen["message"]["usage"] = json!(3_200);
        let (_, batch) = read(
            dir.path(),
            &[
                header(),
                assistant("a1", "u1", "chatcmpl-1", 3_465, 115, 10),
                huge,
                negative,
                assistant("a4", "u1", "chatcmpl-4", 3_000, 200, 300),
                missing,
                unused,
                misshapen,
            ],
        );
        let counted: Vec<&str> = batch
            .observations()
            .map(|seen| seen.response.as_str())
            .collect();
        assert_eq!(counted, ["chatcmpl-1"]);
        assert_eq!(
            noted(&batch),
            [
                (
                    DiagnosticKind::Unreadable,
                    "a response's usage that is not an object"
                ),
                (DiagnosticKind::Invalid, "a response with no usage"),
                (
                    DiagnosticKind::Invalid,
                    "a usage count missing or out of range"
                ),
                (
                    DiagnosticKind::Invalid,
                    "usage whose reasoning exceeds its output"
                )
            ]
        );
    }

    #[test]
    fn a_conversation_shows_messages_of_either_shape_and_each_result_with_its_call() {
        let dir = tempfile::tempdir().unwrap();
        // A reply given as text rather than blocks.
        let mut reply = assistant("a2", "u2", "chatcmpl-1", 3_465, 115, 10);
        reply["message"]["content"] = json!("It reads the ledger first.");
        let (path, _) = read(
            dir.path(),
            &[
                header(),
                json!({"type": "message", "id": "u1", "parentId": null, "timestamp": "2026-09-13T08:50:20.000Z",
                       "message": {"role": "user", "content": [{"type": "text", "text": "Explain this project"}]}}),
                json!({"type": "message", "id": "a1", "parentId": "u1", "timestamp": "2026-09-13T08:50:30.000Z",
                       "message": {"role": "assistant", "model": "glm-5.3", "content": [
                           {"type": "thinking", "thinking": "Look around first."},
                           {"type": "toolCall", "id": "call_1", "name": "bash", "arguments": {"command": "ls"}}]}}),
                json!({"type": "message", "id": "r1", "parentId": "a1", "timestamp": "2026-09-13T08:50:31.000Z",
                       "message": {"role": "toolResult", "toolCallId": "call_1", "isError": false,
                                   "content": [{"type": "text", "text": "README.md"}]}}),
                json!({"type": "message", "id": "u2", "parentId": "r1", "timestamp": "2026-09-13T08:50:40.000Z",
                       "message": {"role": "user", "content": "Where does it start?"}}),
                reply,
            ],
        );
        let session = SessionKey::new(Agent::Pi, SESSION);
        let entries = Pi
            .conversation(&session, std::slice::from_ref(&path))
            .unwrap()
            .entries;
        let shown: Vec<(Speaker, &str)> = entries
            .iter()
            .map(|entry| (entry.speaker, entry.text.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                (Speaker::User, "Explain this project"),
                (Speaker::Reasoning, "Look around first."),
                (Speaker::Tool, ""),
                (Speaker::User, "Where does it start?"),
                (Speaker::Assistant, "It reads the ledger first.")
            ]
        );
        assert_eq!(
            entries[2].tool.as_ref().unwrap().output.as_deref(),
            Some("README.md")
        );
        assert_said_as_shown(
            &Pi,
            &session,
            std::slice::from_ref(&path),
            std::slice::from_ref(&path),
        );
    }
}
