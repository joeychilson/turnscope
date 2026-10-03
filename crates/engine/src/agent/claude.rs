//! Claude Code.
//!
//! Each session is one JSON Lines file, `~/.claude/projects/<project>/<session>.jsonl`.
//! Each subagent run is another, `<project>/<session>/subagents/agent-<id>.jsonl`,
//! with `agent-<id>.meta.json` beside it describing the task and naming the
//! agent that started it when that was another subagent. A subagent's lines
//! carry its parent session's `sessionId` and its own `agentId`; the `agentId`
//! is what identifies it.
//!
//! **Usage.** Each assistant line carries `message.usage` as it stood when the
//! line was written. A response is written one line per content block, and its
//! output count grows from line to line, so a response is identified by
//! `message.id` and `requestId`, and its reports merge to the largest. Counting
//! any line but the most complete undercounts: on 2026-09-23, 119,715 lines
//! with usage described 53,928 responses, and for 12,979 of them earlier lines
//! held a partial output count, such as 16, where a later one held the final
//! count, 13,695. Anthropic's `input_tokens` already leaves out what was read
//! from and written to the cache.
//!
//! **Model calls.** A response lists the model calls it was made of in
//! `usage.iterations`, and its own counts are their sum. Measured 2026-09-27:
//! of 184,463 assistant lines with usage, 114,647 list one call, 4 an empty
//! list and 73 null, and none lists more than one call or a call by another
//! model than the response's. On every line with a call, each top-level count
//! is the calls' sum or, on 64 lines, zero, and the split of cache writes is
//! the calls' even then. So zeros stand for the calls' counts, and a top-level
//! count that is neither their sum nor zero contradicts them and invalidates
//! the line. For some responses the calls are the only record of their usage:
//! 5 on 2026-09-23, with 2.96M cache reads.
//!
//! **Forks.** A fork opens with a copy of its parent's history, which keeps
//! the parent's times but carries the fork's `agentId`. Its description says
//! it is a fork (`isFork`), which agent it forked (`parentAgentId`, or none
//! for the session itself), and the call that started it (`toolUseId`); the
//! copy runs through the assistant line making that call, and holds user
//! lines of the parent's. A fork of the session itself also opens with
//! `fork-context-ref`, and its copy is the parent's last response alone,
//! ended by the fork's first user line. The copy is the parent's: its
//! responses are observed as the parent's and marked as copies, so they
//! neither count twice nor belong to the fork, and nothing it said or when is
//! the fork's.
//!
//! Measured 2026-09-26: of 53 forks, 14 forked the session itself and 39 a
//! subagent, whose whole history, prompts included, their logs open with; in
//! all 53 the copy ran through the line making the call that started the
//! fork. Read as the forks' own, the 39 copies made 562 responses claimed by 2
//! to 6 sessions each, put 101 of the subagents' prompts and replies in search
//! again under the forks, and dated each fork from its parent's start. A copy
//! can also hold a partial count, so a response is identified across files,
//! never within one: 534 responses appeared in more than one file on
//! 2026-09-23. Only the description tells a fork's log from another
//! subagent's, so a new log waits for it ([`DESCRIPTION_WAIT`]).
//!
//! **Subagents start subagents.** Of 442 subagent descriptions on
//! 2026-09-27, 153 name the subagent that started them in `parentAgentId`,
//! 42 of those forks of a subagent, and `spawnDepth` put 289 at depth 1,
//! started by the session itself, 137 at 2 and 16 at 3. A subagent that
//! starts others is a parent too, so its usage with its subagents' counts
//! every level below it.
//!
//! **Which call started a subagent.** Its description names the Agent call
//! (once called Task) in `toolUseId`, the call's `id` in the log it was made
//! in; the call is marked with that id ([`Builder::launched`]) and the
//! subagent's link keeps it, so the call names its subagent exactly. Of 519
//! descriptions on 2026-09-27, 516 name a call, found in the session's own
//! log for 395 of the 398 looked for there; the rest were started by a
//! subagent, whose log holds the call. The three that name none were started
//! by no call and stay unnamed.
//!
//! **Claude Code's own totals.** `cost-state` holds what Claude Code counted for
//! the session and every subagent it ran, per model. It includes calls made
//! outside the conversation, such as the Haiku calls behind some features,
//! which no transcript records, so the transcripts are checked against it. It
//! is rewritten as the session goes on, and the last one stands. In the 139
//! sessions that had one on 2026-09-23, the transcripts showed 17.68B of its
//! 21.07B cache reads (16% missing), 57.6M of its 62.2M output tokens (7%)
//! and 0.29M of its 36.0M input tokens (99%). Part of the gap is certainly
//! calls outside the conversation: 18.4M input tokens of Haiku ($29.95) were
//! in `cost-state` and in no transcript at all. The cause of the rest, 1.15M
//! of 59M cache reads in a typical session and 2.75B in the largest, is not
//! established. Its `costUSD` checks the pricing rules, in the sessions whose
//! transcripts match its token counts, as [`crate::price`] records.
//!
//! **Replies it writes itself.** A reply naming the model `<synthetic>` is
//! Claude Code's own, not a model's: 69 assistant lines on 2026-09-26, saying
//! a limit was reached, "No response requested." or an API error. No request
//! was made and their usage is all zeros, so nothing is counted and they show
//! as the agent's.
//!
//! **What it writes.** Measured 2026-09-27 over 512,954 lines: 21 record
//! types; `system` records of 9 subtypes, such as `turn_duration` and
//! `compact_boundary`; responses' content blocks of `text`, `thinking` and
//! `tool_use`; the person's lines as text or as blocks of `text`, `image` and
//! `tool_result`; and `attachment` records, context Claude Code gives the
//! model, of 37 kinds, 85,568 of them `total_tokens_reminder`. Attachments
//! are passed over whole, since none is counted, said or shown, and a new
//! kind comes with most versions. Since 2026-10-02, a `history-suppression`
//! record names a `cause`, such as `chokepoint_veto`, and the account it was
//! against, and nothing said or counted: 312 in one session's log, measured
//! the same day. The conversation is a small part of it: of
//! 1.9 GB on 2026-09-23, 12.7 MB was what was said, 0.9 MB thinking and
//! 631 MB tool input and output, which is what makes indexing what was said
//! affordable.
//!
//! Claude Code deletes transcripts older than its `cleanupPeriodDays`
//! setting, which no file here was yet old enough to show (2026-09-23), so
//! what was read of them has to outlive them.
//!
//! **Titles.** `ai-title` is a title Claude Code generates and rewrites as the
//! conversation develops; `agent-name` is a name the user gave the session.
//!
//! **Work, for a handoff.** Measured 2026-09-29 over 838 logs and 315,012
//! lines with a call or a result: `Bash` made 98,104 calls (1,716 marked
//! failed), `Edit` 9,412, `Write` 4,832, `TaskCreate` 31 and `TaskUpdate`
//! 50, in 3 logs; no `TodoWrite`, `MultiEdit` or `NotebookEdit` call was
//! there. Every one of the 127,917 lines holding a result held one, with
//! Claude Code's own account of it beside it in `toolUseResult`. So:
//!
//! - A command is `Bash`'s `command`. A failed one's result opens `Exit code
//!   N` when it ran and exited so; other failures, as a call refused before
//!   it ran, say no code. One sent to the background opens `Command running
//!   in background` (1,267 results) and has no outcome yet.
//! - Every successful `Edit`'s account (9,345) holds its change as hunks,
//!   `structuredPatch`, each a list of lines marked `+`, `-` or ` `, which
//!   give its lines exactly. A `Write`'s says whether it created the file
//!   (`type`, 3,865 `create`, whose hunks are empty, and 925 `update`, whose
//!   hunks hold the change). Without an account, an edit is counted from
//!   its old and new text, and a write over a file removed an unknown number
//!   of lines. A failed edit changed nothing.
//! - The plan is the tasks `TaskCreate` adds, whose account gives the id
//!   Claude Code assigned (`task.id`, as `Task #1 created successfully`
//!   says too), and `TaskUpdate` changes by `taskId`; or `TodoWrite`'s
//!   whole list, read as its tool describes it, since none was here.
//!
//! A call of these tools whose input or result isn't so is counted as
//! unclear, by the tool's name, never skipped.
//!
//! **Whose account.** Measured 2026-09-29: of 221 sessions' logs, 92 held
//! `bridge-session` records, 9,489 in all, naming the account and
//! organization signed in (`ownerAccountUuid`, `ownerOrganizationUuid`), all
//! the one account here, and 111 `artifact-autoreact-ledger` records named
//! an `accountUuid`; no response names one. Which account a session drew on
//! is found from where Claude Code was signed in when it ran
//! ([`crate::limits`]), which every session has, rather than from its log.
//! Pointed at another folder by `CLAUDE_CONFIG_DIR`, Claude Code keeps its
//! logs under that folder's `projects`, read as its own are.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use super::{
    Agent, AgentReader, ArtifactKind, Batch, Checkpoint, DiagnosticKind, ModelTotal, Object,
    Observation, ReportScope, SessionReport, owned, string, value,
};
use crate::error::{Error, Result};
use crate::handoff::{self, Change, ChangeKind, PlanItem, StepStatus, Work};
use crate::jsonl;
use crate::session::{LinkKind, SessionKey, SessionLink, Title, TitleSource};
use crate::time::Instant;
use crate::transcript::{Builder, Speaker, Titled, Transcript, argument, compact, pieces, text_of};
use crate::usage::{LARGEST_COUNT, Tokens};

/// Claude Code's reader.
#[derive(Default)]
pub(super) struct ClaudeCode {
    /// Conversations read last, to read on from as they grow.
    read: jsonl::Folds<Conversing>,
}

/// A session's conversation as its file is read.
#[derive(Default)]
struct Conversing {
    /// While the lines read are a fork's copy of its parent's, the copy.
    copying: Option<Copied>,
    /// Whether the person has said anything yet.
    prompted: bool,
    builder: Builder,
}

/// A fork's copy of its parent's history, while its lines are read.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Copied {
    /// The session whose history it is.
    parent: String,
    /// The call that started the fork, made on the copy's last line. Without
    /// one, the fork's first user line ends the copy.
    through: Option<String>,
}

impl Copied {
    /// The copy a `fork-context-ref` line opens, of its parent session's last
    /// response.
    fn referred(line: &Line) -> Option<Copied> {
        owned(line.parent_session_id).map(|parent| Copied {
            parent,
            through: None,
        })
    }

    /// Where the line of `kind` stands against a fork's copy: with the copy
    /// being read, `copying`, and whether the file's own person has said
    /// anything yet, `prompted`, brought up to date. Reading and conversing
    /// take this one step, so search and the conversation leave out the
    /// same lines.
    fn stand(copying: &mut Option<Copied>, prompted: &mut bool, kind: &str, line: &Line) -> Stand {
        if let Some(parent) = copying.as_ref().map(|copy| copy.parent.clone())
            && Copied::holds(copying, kind, line.message())
        {
            return Stand::Copied(parent);
        }
        match kind {
            "fork-context-ref" => {
                if !*prompted && copying.is_none() {
                    *copying = Copied::referred(line);
                }
                Stand::Opening
            }
            "user" => {
                *prompted = true;
                Stand::Own
            }
            _ => Stand::Own,
        }
    }

    /// Whether the line of `kind` carrying `message` belongs to the copy
    /// `copying`, which ends, becoming `None`, with the copy's last line.
    fn holds(copying: &mut Option<Copied>, kind: &str, message: Option<&Message>) -> bool {
        let Some(copy) = copying else {
            return false;
        };
        match (kind, copy.through.as_deref()) {
            ("user", None) => {
                *copying = None;
                false
            }
            ("assistant", Some(call)) => {
                if message.is_some_and(|message| message.calls(call)) {
                    *copying = None;
                }
                true
            }
            ("user" | "assistant" | "system" | "attachment", _) => true,
            _ => false,
        }
    }
}

/// Where a line stands against a fork's copy of its parent's history.
enum Stand {
    /// The file's own.
    Own,
    /// The copy's: the session named's history.
    Copied(String),
    /// A `fork-context-ref`, which opens a copy before the file's first
    /// prompt, and holds nothing itself.
    Opening,
}

/// How long a subagent's log waits to be read for the description Claude Code
/// writes beside it, which says whether the log opens with a copy. Of 393
/// descriptions on 2026-09-26, 6 were written after their logs were begun,
/// the last 1.5 s after.
const DESCRIPTION_WAIT: Duration = Duration::from_secs(10);

/// The provider every Claude Code response is counted under.
const PROVIDER: &str = "anthropic";

/// The model Claude Code names for a message it wrote itself, such as an error
/// shown in place of a response. No request was made, so nothing is counted,
/// and nothing a model said: it shows as the agent's.
const SYNTHETIC: &str = "<synthetic>";

/// Record kinds that hold nothing counted or shown about a session's usage or
/// identity, read and passed over.
const PASSED_OVER: &[&str] = &[
    "artifact-autoreact-ledger",
    "artifact-comment-monitor",
    "atis-latch",
    "bridge-session",
    "file-history-delta",
    "file-history-snapshot",
    "frame-link",
    "history-suppression",
    "last-prompt",
    "mode",
    "permission-mode",
    "pr-link",
    "queue-operation",
    "summary",
];

/// Kinds of `system` record, which Claude Code writes of itself: how long a
/// turn took, a compaction, a command's output and the like. None is counted
/// or said, and a conversation shows none, but a new kind is noted so a
/// change in what Claude Code records is noticed. Measured 2026-09-27.
const SYSTEM_KINDS: &[&str] = &[
    "agents_killed",
    "away_summary",
    "bridge_status",
    "compact_boundary",
    "informational",
    "local_command",
    "scheduled_task_fire",
    "stop_hook_summary",
    "turn_duration",
];

/// Kinds of content block a user line holds: what the person typed, an
/// image, and a tool's result. Measured 2026-09-27.
const USER_BLOCKS: &[&str] = &["image", "text", "tool_result"];

/// Kinds of content block a response holds: its text, its thinking, and its
/// tool calls. Measured 2026-09-27.
const ASSISTANT_BLOCKS: &[&str] = &["text", "thinking", "tool_use"];

/// Fields of `message.usage` this reader understands. Any other is reported,
/// because a new field may be a new kind of token that is charged.
/// `fallback_credit` has only ever been null: 1,789 responses in 17 files
/// since Claude Code 2.1.285 (2026-09-29, measured 2026-09-30); what any
/// other value would charge isn't known, so one is reported.
const USAGE_FIELDS: &[&str] = &[
    "cache_creation",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
    "fallback_credit",
    "inference_geo",
    "input_tokens",
    "iterations",
    "output_tokens",
    "output_tokens_details",
    "server_tool_use",
    "service_tier",
    "speed",
];

/// Fields of `message.usage.server_tool_use` this reader understands: the
/// web searches the provider ran, which are charged apart from tokens, and
/// the pages it fetched, which cost only the tokens they add. Any other is
/// reported, because it may be a tool that is charged. Measured 2026-09-27:
/// 116,855 of 189,212 responses here give both, and the rest null.
const SERVER_TOOL_FIELDS: &[&str] = &["web_fetch_requests", "web_search_requests"];

/// Fields of `message.usage.output_tokens_details` this reader understands:
/// how much of the output was thinking. Any other is reported, because it
/// may be a kind of output charged apart. Measured 2026-09-27: 116,782 of
/// 189,212 responses here give it, and the rest null.
const OUTPUT_DETAIL_FIELDS: &[&str] = &["thinking_tokens"];

/// Fields of a model's totals in `cost-state` this reader understands. Any
/// other is reported, because a new field may be a kind of token that is
/// charged; `thinkingTokens` is part of `outputTokens`.
const COST_FIELDS: &[&str] = &[
    "cacheCreationInputTokens",
    "cacheReadInputTokens",
    "costUSD",
    "inputTokens",
    "outputTokens",
    "thinkingTokens",
    "webSearchRequests",
];

/// Fields of a model call in `usage.iterations` this reader understands, for
/// the same reason.
const ITERATION_FIELDS: &[&str] = &[
    "cache_creation",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
    "input_tokens",
    "model",
    "output_tokens",
    "type",
];

impl AgentReader for ClaudeCode {
    fn agent(&self) -> Agent {
        Agent::ClaudeCode
    }

    fn version(&self) -> u32 {
        9
    }

    fn roots(&self, folder: &Path) -> Vec<PathBuf> {
        vec![folder.join("projects")]
    }

    fn classify(&self, root: &Path, path: &Path) -> Option<ArtifactKind> {
        Place::of(root, path).map(|_| ArtifactKind::Log)
    }

    fn read(&self, path: &Path, from: &Checkpoint, batch: &mut Batch) -> Result<Checkpoint> {
        let place = Place::from_file(path).ok_or_else(|| {
            Error::corrupt("Claude Code artifact path", path.display().to_string())
        })?;
        let mut state: State = from.state("Claude Code reader state")?;

        if let Place::Subagent { agent, session } = &place
            && !state.described
        {
            let found = Meta::beside(path, agent);
            // Looked for again until found; one that does not read is not.
            let described = found.is_some();
            let meta = match found {
                Some(Ok(meta)) => Some(meta),
                // A log read without its description would take a fork's copy
                // of its parent's history as the fork's own, so a new log
                // waits for one, or for one still being written.
                _ if from.offset == 0 && begun_lately(path) => return Ok(from.clone()),
                Some(Err(_)) => {
                    batch.note(
                        DiagnosticKind::Unreadable,
                        "a subagent description of the wrong shape",
                        0,
                    );
                    None
                }
                None => None,
            };
            if let Some(meta) = &meta
                && from.offset == 0
            {
                state.copying = meta.copy(session);
            }
            describe(meta, agent, session, batch);
            state.described = described;
        }

        let offset = super::read_log(path, from.offset, batch, |line, offset, batch| {
            read_line(line, offset, &place, &mut state, batch);
        })?;
        Checkpoint::at(offset, &state, "Claude Code reader state")
    }

    fn conversation(&self, session: &SessionKey, artifacts: &[PathBuf]) -> Result<Transcript> {
        // The session's own file: named for it, or for the subagent it is.
        let own = artifacts
            .iter()
            .find(|path| Place::from_file(path).is_some_and(|place| place.owner() == *session));
        // Another file's copy of its responses holds none of its conversation.
        let path = own.ok_or_else(|| Error::Gone(session.to_string()))?;
        let copy = match Place::from_file(path) {
            Some(Place::Subagent { agent, session }) => Meta::beside(path, &agent)
                .and_then(Result::ok)
                .and_then(|meta| meta.copy(&session)),
            _ => None,
        };
        self.read.fold(
            &[path.as_path()],
            |talk| talk.copying = copy.clone(),
            Conversing::line,
            |talk| talk.builder.transcript(),
        )
    }
}

impl Conversing {
    /// Add one line of a session's file to its conversation. A fork's copy of
    /// its parent's history is the parent's and is left out.
    fn line(&mut self, bytes: &[u8]) {
        let Ok(line) = serde_json::from_slice::<Line>(bytes) else {
            return;
        };
        let kind = string(line.kind).unwrap_or_default();
        let stand = Copied::stand(&mut self.copying, &mut self.prompted, &kind, &line);
        if !matches!(stand, Stand::Own) || !matches!(kind.as_ref(), "user" | "assistant") {
            return;
        }
        let Some(message) = line.message() else {
            return;
        };
        let at = string(line.timestamp).and_then(|stamp| Instant::parse(&stamp));
        let synthetic = message.synthetic();
        let model = string(message.model);
        let model = model.as_deref().filter(|_| !synthetic);
        let content = &value(message.content);
        let blocks = content.as_array().map(Vec::as_slice).unwrap_or_default();
        let builder = &mut self.builder;
        // A result names the call it answers, and is folded into it. What
        // the call did is read from its result, with Claude Code's own
        // account of it beside it on a line holding one result, as every
        // line holding a result does.
        let results = blocks
            .iter()
            .filter(|block| block["type"] == "tool_result")
            .count();
        for block in blocks.iter().filter(|block| block["type"] == "tool_result") {
            if let Some(id) = block["tool_use_id"].as_str() {
                let failed = block["is_error"].as_bool().unwrap_or(false);
                if let Some(call) = builder.answer(id, text_of(&block["content"]), failed) {
                    let account = || {
                        if results == 1 {
                            value(line.tool_use_result)
                        } else {
                            Value::Null
                        }
                    };
                    for work in worked(&call.name, &call.input, &call.head(), failed, account) {
                        builder.worked(at, work);
                    }
                }
            }
        }
        if string(message.role).as_deref() == Some("assistant") {
            // What Claude Code wrote itself, such as that a limit was reached.
            let speaker = if synthetic {
                Speaker::System
            } else {
                Speaker::Assistant
            };
            builder.say(speaker, at, model, text_of(content));
        } else if flag(line.is_meta) || flag(line.is_compact_summary) {
            // Claude Code's own additions: a skill's instructions, or the
            // summary a compacted conversation carries on from.
            for text in pieces(content) {
                builder.say(Speaker::System, at, None, text);
            }
        } else {
            for text in pieces(content) {
                builder.prompt(at, text);
            }
        }
        for block in blocks {
            match block["type"].as_str() {
                Some("thinking") => builder.say(
                    Speaker::Reasoning,
                    at,
                    model,
                    block["thinking"].as_str().unwrap_or_default(),
                ),
                Some("tool_use") => {
                    let id = block["id"].as_str();
                    let name = block["name"].as_str();
                    builder.call(id, at, model, name, compact(&block["input"]));
                    if matches!(name, Some("Agent" | "Task")) {
                        builder.launched(id.unwrap_or_default());
                    }
                }
                _ => {}
            }
        }
    }
}

/// The work a call of Claude Code's tool `name`, given `input`, did, as the
/// start of its result, `head`, whether that was a failure, and Claude
/// Code's own account of the result, `account`, say.
///
/// Commands are `Bash`'s: one that failed opens its result with its exit
/// code, and one sent to the background has no outcome yet. Files are
/// changed by `Edit`, `Write` and `MultiEdit`, whose account holds the
/// change as hunks (`structuredPatch`) and says whether a write created the
/// file; without one, the lines are counted from what the call was given.
/// The plan is `TodoWrite`'s whole list, or the tasks `TaskCreate` adds,
/// with the id its account gives, and `TaskUpdate` changes.
fn worked(
    name: &str,
    input: &str,
    head: &str,
    failed: bool,
    account: impl FnOnce() -> Value,
) -> Vec<Work> {
    let unclear = || vec![Work::Unclear(name.to_owned())];
    match name {
        "Bash" => {
            let Some(command) = argument(input, "command") else {
                return unclear();
            };
            let (exit, failed) = if failed {
                (handoff::leading_number(head, "Exit code "), Some(true))
            } else if head.starts_with("Command running in background") {
                (None, None)
            } else {
                (None, Some(false))
            };
            vec![Work::Ran {
                command,
                exit,
                failed,
            }]
        }
        // A change that failed changed nothing.
        "Edit" | "Write" | "MultiEdit" if failed => Vec::new(),
        "Edit" | "Write" | "MultiEdit" => {
            let Ok(given) = serde_json::from_str::<Value>(input) else {
                return unclear();
            };
            let account = account();
            let path = account
                .get("filePath")
                .or_else(|| given.get("file_path"))
                .and_then(Value::as_str);
            let Some(path) = path else {
                return unclear();
            };
            let created = match account.get("type").and_then(Value::as_str) {
                Some(kind) => kind == "create",
                None => head.starts_with("File created successfully"),
            };
            let content = given.get("content").and_then(Value::as_str);
            let kind = if created {
                ChangeKind::Created
            } else {
                ChangeKind::Updated
            };
            let change = match (content, account.get("structuredPatch")) {
                (Some(content), _) if created => Change::written(path, content, true),
                (_, Some(hunks)) if hunks.is_array() => Change::new(path, kind, patched(hunks)),
                // Without Claude Code's account, a file written whole is
                // known by what was written.
                (Some(content), _) if name == "Write" => Change::written(path, content, false),
                // Otherwise by what the call was given.
                _ => Change::new(path, kind, given_lines(name, &given)),
            };
            vec![Work::Changed(change)]
        }
        "TodoWrite" if !failed => {
            let todos = serde_json::from_str::<Value>(input)
                .ok()
                .and_then(|given| given.get("todos").and_then(Value::as_array).cloned());
            let items: Option<Vec<PlanItem>> = todos.map(|todos| {
                todos
                    .iter()
                    .map(|todo| PlanItem {
                        text: todo
                            .get("content")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        status: todo
                            .get("status")
                            .and_then(Value::as_str)
                            .and_then(StepStatus::from_word),
                        ..PlanItem::default()
                    })
                    .collect()
            });
            match items {
                Some(items)
                    if items
                        .iter()
                        .all(|item| item.text.is_some() && item.status.is_some()) =>
                {
                    vec![Work::Planned(items)]
                }
                _ => unclear(),
            }
        }
        "TaskCreate" if !failed => {
            let account = account();
            let task = account.get("task");
            let id = task
                .and_then(|task| task.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    handoff::leading_number(head, "Task #").map(|number| number.to_string())
                });
            let subject = task
                .and_then(|task| task.get("subject"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| argument(input, "subject"));
            match (id, subject) {
                (Some(id), Some(subject)) => vec![Work::Revised(vec![PlanItem {
                    id: Some(id),
                    text: Some(subject),
                    status: Some(StepStatus::Pending),
                    removed: false,
                }])],
                _ => unclear(),
            }
        }
        "TaskUpdate" if !failed => {
            let Some(id) = argument(input, "taskId") else {
                return unclear();
            };
            let word = argument(input, "status");
            let status = word.as_deref().and_then(StepStatus::from_word);
            let removed = word.as_deref() == Some("deleted");
            if word.is_some() && status.is_none() && !removed {
                return unclear();
            }
            vec![Work::Revised(vec![PlanItem {
                id: Some(id),
                text: argument(input, "subject"),
                status,
                removed,
            }])]
        }
        _ => Vec::new(),
    }
}

/// The lines Claude Code's hunks of a change, each a list of `lines` marked
/// `+`, `-` or ` `, add and remove. `None` when they don't read.
fn patched(hunks: &Value) -> Option<(u64, u64)> {
    let (mut added, mut removed) = (0u64, 0u64);
    for hunk in hunks.as_array()? {
        for line in hunk.get("lines")?.as_array()? {
            match line.as_str()?.as_bytes().first() {
                Some(b'+') => added = added.saturating_add(1),
                Some(b'-') => removed = removed.saturating_add(1),
                _ => {}
            }
        }
    }
    Some((added, removed))
}

/// The lines a change adds and removes, from what the call of `name` was
/// `given` alone: an edit's old and new text compared, or each of a
/// `MultiEdit`'s. `None` for a write over a file, whose old lines only
/// Claude Code's account holds, and for an edit made everywhere its text
/// is, which doesn't say in how many places that was.
fn given_lines(name: &str, given: &Value) -> Option<(u64, u64)> {
    let replaced = |edit: &Value| -> Option<(u64, u64)> {
        if edit.get("replace_all").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        handoff::replaced_lines(
            edit.get("old_string")?.as_str()?,
            edit.get("new_string")?.as_str()?,
        )
    };
    match name {
        "Edit" => replaced(given),
        "MultiEdit" => given.get("edits")?.as_array()?.iter().try_fold(
            (0u64, 0u64),
            |(added, removed), edit| {
                let (more, fewer) = replaced(edit)?;
                Some((added.saturating_add(more), removed.saturating_add(fewer)))
            },
        ),
        _ => None,
    }
}

/// Whether a field held unparsed is `true`.
fn flag(field: Option<&RawValue>) -> bool {
    field.is_some_and(|raw| raw.get() == "true")
}

/// Where an artifact sits, which says whose history it holds.
enum Place {
    /// A session's own file, named for the session.
    Session(String),
    /// A subagent's file.
    Subagent {
        /// The subagent's id, from its file name.
        agent: String,
        /// The session it ran in, from its folder.
        session: String,
    },
}

impl Place {
    /// Where `path`, under `root`, sits, when it is a session's or a subagent's
    /// file.
    fn of(root: &Path, path: &Path) -> Option<Place> {
        let relative = path.strip_prefix(root).ok()?;
        let parts: Vec<&str> = relative
            .components()
            .map(|part| match part {
                Component::Normal(name) => name.to_str(),
                _ => None,
            })
            .collect::<Option<_>>()?;
        match parts.as_slice() {
            [_project, file] => {
                let session = file.strip_suffix(".jsonl")?;
                (!session.is_empty()).then(|| Place::Session(session.to_owned()))
            }
            [_project, session, "subagents", file] => {
                let agent = file.strip_prefix("agent-")?.strip_suffix(".jsonl")?;
                (!agent.is_empty() && !agent.contains('.')).then(|| Place::Subagent {
                    agent: agent.to_owned(),
                    session: (*session).to_owned(),
                })
            }
            _ => None,
        }
    }

    /// Where the file at `path` sits, judged from its own folder rather than
    /// the root it was found under.
    fn from_file(path: &Path) -> Option<Place> {
        let folder = path.parent()?;
        if folder.file_name().and_then(|name| name.to_str()) == Some("subagents") {
            let root = folder.parent()?.parent()?.parent()?;
            Place::of(root, path)
        } else {
            Place::of(folder.parent()?, path)
        }
    }

    /// The session a line without an `agentId` or `sessionId` belongs to.
    fn owner(&self) -> SessionKey {
        match self {
            Place::Session(session) => SessionKey::new(Agent::ClaudeCode, session.clone()),
            Place::Subagent { agent, .. } => SessionKey::new(Agent::ClaudeCode, agent.clone()),
        }
    }
}

/// What the reader carries from one read of a file to the next.
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// The latest instant any line gave. `cost-state` records give none of
    /// their own, and are dated by this.
    last_at: Option<i64>,
    /// While a fork's copy of its parent's history is being read, the copy.
    copying: Option<Copied>,
    /// Whether a user line of the file's own has been seen, after which a
    /// `fork-context-ref` opens no copy.
    prompted: bool,
    /// How far the file's first prompts have titled its session; later
    /// prompts are the conversation going on.
    titled: Titled,
    /// Whether a subagent's description has been read. Claude Code may write
    /// it after the subagent's first lines, so it is looked for until found.
    described: bool,
}

/// A line of a session or subagent file, as far as this reader looks.
///
/// Fields are held unparsed and read only as a record's kind needs them, so a
/// field of an unexpected type costs that one field, not the whole line.
#[derive(Deserialize)]
struct Line<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Option<&'a RawValue>,
    #[serde(rename = "sessionId", borrow, default)]
    session_id: Option<&'a RawValue>,
    #[serde(rename = "agentId", borrow, default)]
    agent_id: Option<&'a RawValue>,
    #[serde(borrow, default)]
    timestamp: Option<&'a RawValue>,
    #[serde(borrow, default)]
    cwd: Option<&'a RawValue>,
    #[serde(rename = "gitBranch", borrow, default)]
    git_branch: Option<&'a RawValue>,
    #[serde(borrow, default)]
    version: Option<&'a RawValue>,
    #[serde(borrow, default)]
    entrypoint: Option<&'a RawValue>,
    #[serde(rename = "requestId", borrow, default)]
    request_id: Option<&'a RawValue>,
    #[serde(rename = "isMeta", borrow, default)]
    is_meta: Option<&'a RawValue>,
    #[serde(rename = "isCompactSummary", borrow, default)]
    is_compact_summary: Option<&'a RawValue>,
    #[serde(borrow, default)]
    message: Option<Object<Message<'a>>>,
    #[serde(rename = "aiTitle", borrow, default)]
    ai_title: Option<&'a RawValue>,
    #[serde(rename = "agentName", borrow, default)]
    agent_name: Option<&'a RawValue>,
    #[serde(rename = "modelUsage", borrow, default)]
    model_usage: Option<&'a RawValue>,
    #[serde(rename = "continuedInSessionId", borrow, default)]
    continued_in: Option<&'a RawValue>,
    #[serde(rename = "parentSessionId", borrow, default)]
    parent_session_id: Option<&'a RawValue>,
    #[serde(borrow, default)]
    subtype: Option<&'a RawValue>,
    #[serde(rename = "toolUseResult", borrow, default)]
    tool_use_result: Option<&'a RawValue>,
}

impl<'a> Line<'a> {
    /// Its message, when it is an object.
    fn message(&self) -> Option<&Message<'a>> {
        self.message.as_ref()?.0.as_ref()
    }
}

/// The fields of a line's message this reader looks at, held unparsed.
#[derive(Deserialize)]
struct Message<'a> {
    #[serde(borrow, default)]
    role: Option<&'a RawValue>,
    #[serde(borrow, default)]
    id: Option<&'a RawValue>,
    #[serde(borrow, default)]
    model: Option<&'a RawValue>,
    #[serde(borrow, default)]
    usage: Option<&'a RawValue>,
    #[serde(borrow, default)]
    content: Option<&'a RawValue>,
}

impl<'a> Message<'a> {
    /// Its content, when it has content of either shape.
    fn content(&self) -> Option<Content<'a>> {
        Content::parse(self.content?)
    }

    /// Whether Claude Code wrote it itself, such as to say a limit was
    /// reached, rather than a model.
    fn synthetic(&self) -> bool {
        string(self.model).as_deref() == Some(SYNTHETIC)
    }

    /// Whether it calls a tool with the call id `id`.
    fn calls(&self, id: &str) -> bool {
        matches!(self.content(), Some(Content::Blocks(blocks))
        if blocks.iter().any(|block| {
            block.kind.as_deref() == Some("tool_use") && block.id.as_deref() == Some(id)
        }))
    }
}

/// Read one line into `batch`.
fn read_line(bytes: &[u8], offset: u64, place: &Place, state: &mut State, batch: &mut Batch) {
    let Ok(line) = serde_json::from_slice::<Line>(bytes) else {
        batch.note(
            DiagnosticKind::Unreadable,
            "a line that is not a JSON object",
            offset,
        );
        return;
    };
    let kind = string(line.kind).unwrap_or_default();
    let at = string(line.timestamp).and_then(|stamp| Instant::parse(&stamp));
    if let Some(at) = at {
        state.last_at = state.last_at.max(Some(at.millis()));
    }
    // A subagent's lines name the session they ran in; the agent id says whose
    // they are.
    let owner = owned(line.agent_id)
        .or_else(|| owned(line.session_id))
        .map_or_else(
            || place.owner(),
            |id| SessionKey::new(Agent::ClaudeCode, id),
        );

    // A fork's copy of its parent's history: its responses are the parent's,
    // copied before they finished, and its dates, directory and words are the
    // parent's too, so they say nothing of the fork.
    match Copied::stand(&mut state.copying, &mut state.prompted, &kind, &line) {
        Stand::Own => {}
        Stand::Copied(parent) => {
            if kind == "assistant"
                && let Some(reply) = Reply::read(line.message.as_ref(), offset, batch)
            {
                let parent = SessionKey::new(Agent::ClaudeCode, parent);
                observe(&reply, line.request_id, at, parent, true, offset, batch);
            }
            return;
        }
        Stand::Opening => return,
    }

    match kind.as_ref() {
        "user" => {
            let content = line.message().and_then(Message::content);
            if let Some(Content::Blocks(blocks)) = &content {
                note_blocks(blocks, USER_BLOCKS, "user content block", offset, batch);
            }
            // A skill's instructions, and a compacted conversation's summary,
            // are Claude Code's, not the person's, though they arrive as theirs.
            if !flag(line.is_meta) && !flag(line.is_compact_summary) {
                let texts = content.as_ref().map(Content::texts).unwrap_or_default();
                for text in texts {
                    batch.prompt(&owner, at, text);
                    if let Some(title) = state.titled.offer(text) {
                        batch.session_mut(&owner).offer_title(Some(title));
                    }
                }
            }
        }
        "assistant" => {
            if let Some(reply) = Reply::read(line.message.as_ref(), offset, batch) {
                observe(
                    &reply,
                    line.request_id,
                    at,
                    owner.clone(),
                    false,
                    offset,
                    batch,
                );
            }
            // The conversation shows a reply of the wrong shape too.
            let message = line.message();
            let content = message.and_then(Message::content);
            if let Some(Content::Blocks(blocks)) = &content {
                note_blocks(
                    blocks,
                    ASSISTANT_BLOCKS,
                    "assistant content block",
                    offset,
                    batch,
                );
            }
            // One reply, as the conversation shows it, however many blocks;
            // what Claude Code wrote itself is not a model's.
            let texts = content.as_ref().map(Content::texts).unwrap_or_default();
            if !texts.is_empty() && !message.is_some_and(Message::synthetic) {
                batch.say(&owner, at, &texts.join("\n"));
            }
        }
        "system" => match string(line.subtype) {
            Some(kind) if SYSTEM_KINDS.contains(&kind.as_ref()) => {}
            Some(kind) => batch.note_unknown("system record", &kind, offset),
            None => batch.note(
                DiagnosticKind::Unreadable,
                "a system record with no subtype",
                offset,
            ),
        },
        // What Claude Code attaches to the conversation for the model, of
        // dozens of kinds, none counted, said or shown.
        "attachment" => {}
        "ai-title" => {
            let title =
                string(line.ai_title).and_then(|title| Title::new(TitleSource::Generated, &title));
            batch.session_mut(&owner).offer_title(title);
            return;
        }
        "agent-name" => {
            let title =
                string(line.agent_name).and_then(|name| Title::new(TitleSource::Named, &name));
            batch.session_mut(&owner).offer_title(title);
            return;
        }
        "cost-state" => {
            report(&line, state, owner, offset, batch);
            return;
        }
        "continued-in" => {
            if let Some(next) = owned(line.continued_in) {
                batch.link(SessionLink {
                    child: SessionKey::new(Agent::ClaudeCode, next),
                    parent: owner,
                    kind: LinkKind::Continuation,
                    launch: None,
                });
            }
            return;
        }
        other if PASSED_OVER.contains(&other) => return,
        "" => {
            batch.note(DiagnosticKind::Unreadable, "a record with no type", offset);
            return;
        }
        other => {
            batch.note_unknown("record type", other, offset);
            return;
        }
    }

    // A user, assistant, system, or attachment line of the session's own.
    let facts = batch.session_mut(&owner);
    if let Some(at) = at {
        facts.saw(at);
    }
    if facts.cwd.is_none() {
        facts.cwd = owned(line.cwd);
    }
    if let Some(branch) = owned(line.git_branch) {
        facts.branch = Some(branch);
    }
    if let Some(version) = owned(line.version) {
        facts.version = Some(version);
    }
    if facts.origin.is_none() {
        facts.origin = owned(line.entrypoint);
    }
}

/// What a message's content holds, as far as this reader looks.
enum Content<'a> {
    /// The content, when it is a string.
    Text(Cow<'a, str>),
    /// Its blocks, whose own contents, such as a tool's result, are passed
    /// over without being read into memory.
    Blocks(Vec<Block<'a>>),
}

/// A block of a message's content.
#[derive(Deserialize)]
struct Block<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    text: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    id: Option<Cow<'a, str>>,
}

impl<'a> Content<'a> {
    /// `content`, when it is of either shape.
    fn parse(content: &'a RawValue) -> Option<Content<'a>> {
        if let Some(text) = string(Some(content)) {
            return Some(Content::Text(text));
        }
        serde_json::from_str(content.get())
            .ok()
            .map(Content::Blocks)
    }

    /// The text it holds: itself when it is a string, or its text blocks.
    fn texts(&self) -> Vec<&str> {
        match self {
            Content::Text(text) => vec![text],
            Content::Blocks(blocks) => blocks
                .iter()
                .filter(|block| block.kind.as_deref() == Some("text"))
                .filter_map(|block| block.text.as_deref())
                .collect(),
        }
    }
}

/// Note each of `blocks`, of a line's content, of a kind not among `known`,
/// as `what` and its kind.
fn note_blocks(blocks: &[Block], known: &[&str], what: &str, offset: u64, batch: &mut Batch) {
    for block in blocks {
        batch.note_kind(block.kind.as_deref(), known, what, offset);
    }
}

/// What an assistant line's message says of its usage.
struct Reply<'a> {
    id: Option<Cow<'a, str>>,
    model: Option<Cow<'a, str>>,
    usage: Option<Map<String, Value>>,
}

impl<'a> Reply<'a> {
    /// What the message of an assistant line at `offset` says of its usage:
    /// `None` when it has none, and, noted, when it is of the wrong shape.
    fn read(
        message: Option<&Object<Message<'a>>>,
        offset: u64,
        batch: &mut Batch,
    ) -> Option<Reply<'a>> {
        let text = |field: Option<&'a RawValue>| match field {
            None => Some(None),
            Some(raw) => string(Some(raw)).map(Some),
        };
        let reply = message?.0.as_ref().and_then(|message| {
            Some(Reply {
                id: text(message.id)?,
                model: text(message.model)?,
                usage: match message.usage {
                    None => None,
                    Some(raw) => Some(serde_json::from_str(raw.get()).ok()?),
                },
            })
        });
        if reply.is_none() {
            batch.note(
                DiagnosticKind::Unreadable,
                "an assistant message of the wrong shape",
                offset,
            );
        }
        reply
    }

    /// Whether Claude Code wrote it itself, such as to say a limit was
    /// reached, rather than a model.
    fn synthetic(&self) -> bool {
        self.model.as_deref() == Some(SYNTHETIC)
    }
}

/// Record the usage `reply`, on an assistant line with the request id
/// `request`, reports, as `session`'s.
fn observe(
    reply: &Reply,
    request: Option<&RawValue>,
    at: Option<Instant>,
    session: SessionKey,
    copy: bool,
    offset: u64,
    batch: &mut Batch,
) {
    // Claude Code's own message made no request, so nothing is counted.
    if reply.synthetic() {
        return;
    }
    let Some(usage) = &reply.usage else {
        batch.note(DiagnosticKind::Invalid, "a response with no usage", offset);
        return;
    };
    let model = reply.model.as_deref().unwrap_or_default();
    let (Some(id), Some(at)) = (reply.id.as_deref().filter(|id| !id.is_empty()), at) else {
        batch.note(
            DiagnosticKind::Invalid,
            "a response with no message id or time",
            offset,
        );
        return;
    };
    if model.is_empty() {
        batch.note(DiagnosticKind::Invalid, "a response with no model", offset);
        return;
    }
    let Some(used) = Used::read(usage, model, offset, batch) else {
        return;
    };
    let response = match string(request) {
        Some(request) if !request.is_empty() => format!("{id}:{request}"),
        _ => id.to_owned(),
    };
    batch.observe(Observation {
        response,
        session,
        copy,
        at,
        provider: PROVIDER.to_owned(),
        model: model.to_owned(),
        tokens: used.tokens,
        prompt: used.prompt,
        web_searches: used.web_searches,
        cost: None,
        priority: used.fast,
    });
}

/// The value at `path` within `usage`, when there is one.
fn nested<'a>(usage: &'a Map<String, Value>, path: &[&str]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    rest.iter()
        .try_fold(usage.get(*first)?, |value, key| value.get(key))
}

/// The token counts of one usage object: a response's own, or one of its
/// iterations'.
#[derive(Clone, Copy, Default)]
struct Counted {
    input: u64,
    cache_read: u64,
    /// Everything written to the cache.
    written: u64,
    /// Of that, what was kept for five minutes and for an hour, as far as the
    /// object splits it.
    five: u64,
    hour: u64,
    output: u64,
}

impl Counted {
    /// The prompt the call or calls were given: their input however it was
    /// cached.
    fn prompt(&self) -> u64 {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.written)
    }

    /// Each count, in order.
    fn each(self) -> [u64; 6] {
        [
            self.input,
            self.cache_read,
            self.written,
            self.five,
            self.hour,
            self.output,
        ]
    }

    /// These counts and `other`'s together, or `None` when one comes to more
    /// than any count can be.
    fn plus(self, other: Counted) -> Option<Counted> {
        let sum = |a: u64, b: u64| a.checked_add(b).filter(|sum| *sum <= LARGEST_COUNT);
        Some(Counted {
            input: sum(self.input, other.input)?,
            cache_read: sum(self.cache_read, other.cache_read)?,
            written: sum(self.written, other.written)?,
            five: sum(self.five, other.five)?,
            hour: sum(self.hour, other.hour)?,
            output: sum(self.output, other.output)?,
        })
    }

    /// Whether these counts, a response's own, are those of `calls`, the
    /// sum of the model calls it lists: each the same, or left at zero.
    fn made_of(self, calls: Counted) -> bool {
        self.each()
            .into_iter()
            .zip(calls.each())
            .all(|(own, calls)| own == calls || own == 0)
    }
}

/// Read the counts of one usage object, whose fields are named from `within`
/// in diagnostics. `None`, noted, when a count is missing or out of range.
///
/// Anthropic's API always gives the input and output, and gives what was
/// read from and written to the cache only where caching applies, so those
/// may be left out. Older versions write no split of what was written to the
/// cache by how long it is kept, which is then all counted as kept for five
/// minutes; a split, when written, has both parts.
fn counted(
    object: &Map<String, Value>,
    within: &str,
    offset: u64,
    batch: &mut Batch,
) -> Option<Counted> {
    let read = |path: &[&str], optional: bool, batch: &mut Batch| {
        let value = nested(object, path);
        let value = if optional {
            super::optional_count(value)
        } else {
            super::count(value)
        };
        if value.is_none() {
            batch.note(
                DiagnosticKind::Invalid,
                format!("{within}.{}", path.join(".")),
                offset,
            );
        }
        value
    };
    let mut counts = Counted {
        input: read(&["input_tokens"], false, batch)?,
        cache_read: read(&["cache_read_input_tokens"], true, batch)?,
        written: read(&["cache_creation_input_tokens"], true, batch)?,
        output: read(&["output_tokens"], false, batch)?,
        ..Counted::default()
    };
    if let Some(Value::Object(split)) = object.get("cache_creation") {
        batch.note_unknown_fields(
            split,
            &["ephemeral_1h_input_tokens", "ephemeral_5m_input_tokens"],
            &format!("{within}.cache_creation"),
            offset,
        );
        counts.five = read(
            &["cache_creation", "ephemeral_5m_input_tokens"],
            false,
            batch,
        )?;
        counts.hour = read(
            &["cache_creation", "ephemeral_1h_input_tokens"],
            false,
            batch,
        )?;
    } else {
        counts.five = counts.written;
    }
    Some(counts)
}

/// A response's usage, as its `usage` gives it.
struct Used {
    tokens: Tokens,
    /// The largest prompt of the model calls it was made of.
    prompt: u64,
    web_searches: u64,
    /// Whether it was served in fast mode, which is charged as priority.
    fast: bool,
}

impl Used {
    /// Bring the `usage` of a response from `model` to what it used. `None`,
    /// noted, when a count is out of range or the counts contradict each
    /// other.
    ///
    /// A response made of several model calls lists each in `iterations`,
    /// and the response's own counts are their sum. Some lines give the
    /// iterations and zeros for the response's own counts, so the counts are
    /// the iterations'; a count of the response's own that is neither their
    /// sum nor zero contradicts them. Each call's prompt sets the price tier
    /// it was charged at, so where the iterations list the calls the prompt
    /// is the largest of theirs, never their sum, and never taken from the
    /// response's own counts, which add the calls up. Without them, the
    /// response is one call.
    fn read(
        usage: &Map<String, Value>,
        model: &str,
        offset: u64,
        batch: &mut Batch,
    ) -> Option<Used> {
        batch.note_unknown_fields(usage, USAGE_FIELDS, "usage", offset);
        for (field, known) in [
            ("server_tool_use", SERVER_TOOL_FIELDS),
            ("output_tokens_details", OUTPUT_DETAIL_FIELDS),
        ] {
            if let Some(Value::Object(details)) = usage.get(field) {
                batch.note_unknown_fields(details, known, &format!("usage.{field}"), offset);
            }
        }
        let own = counted(usage, "usage", offset, batch)?;
        let mut calls: Vec<Counted> = Vec::new();
        match usage.get("iterations") {
            None | Some(Value::Null) => {}
            Some(Value::Array(items)) => {
                for item in items {
                    let Value::Object(item) = item else {
                        batch.note(
                            DiagnosticKind::Unreadable,
                            "usage.iterations holds a non-object",
                            offset,
                        );
                        return None;
                    };
                    batch.note_unknown_fields(item, ITERATION_FIELDS, "usage.iterations", offset);
                    match item.get("type").and_then(Value::as_str) {
                        None | Some("message") => {}
                        Some(other) => {
                            batch.note_unknown_value("usage.iterations type", other, offset)
                        }
                    }
                    // A call by another model would be charged at its prices,
                    // not the response's.
                    if let Some(called) = item.get("model").and_then(Value::as_str)
                        && called != model
                    {
                        batch.note(
                            DiagnosticKind::UnknownField,
                            "usage.iterations model other than the message's",
                            offset,
                        );
                    }
                    calls.push(counted(item, "usage.iterations", offset, batch)?);
                }
            }
            Some(_) => {
                batch.note(
                    DiagnosticKind::Unreadable,
                    "usage.iterations is not a list",
                    offset,
                );
                return None;
            }
        }
        let (counts, prompt) = match calls.iter().map(|call| call.prompt()).max() {
            None => (own, own.prompt()),
            Some(largest) => {
                let Some(sum) = calls
                    .iter()
                    .try_fold(Counted::default(), |sum, call| sum.plus(*call))
                else {
                    batch.note(
                        DiagnosticKind::Invalid,
                        "usage.iterations add up to more than any count",
                        offset,
                    );
                    return None;
                };
                if !own.made_of(sum) {
                    batch.note(
                        DiagnosticKind::Invalid,
                        "usage whose counts are not its iterations' sum",
                        offset,
                    );
                    return None;
                }
                (sum, largest)
            }
        };

        // Newer versions only write thinking and web searches; 57,548 of
        // 163,032 responses here have neither (2026-09-26).
        let optional = |path: &[&str]| super::optional_count(nested(usage, path));
        let reasoning = optional(&["output_tokens_details", "thinking_tokens"]);
        let web_searches = optional(&["server_tool_use", "web_search_requests"]);
        let (Some(reasoning), Some(web_searches)) = (reasoning, web_searches) else {
            batch.note(
                DiagnosticKind::Invalid,
                "usage.output_tokens_details or usage.server_tool_use",
                offset,
            );
            return None;
        };
        // Everything written to the cache is split by how long it is kept, and
        // reasoning is part of the output; counts that say otherwise are wrong
        // somewhere, and which is not known.
        if counts.five.saturating_add(counts.hour) != counts.written {
            batch.note(
                DiagnosticKind::Invalid,
                "usage.cache_creation does not add up to cache_creation_input_tokens",
                offset,
            );
            return None;
        }
        if reasoning > counts.output {
            batch.note(
                DiagnosticKind::Invalid,
                "usage whose thinking exceeds its output",
                offset,
            );
            return None;
        }

        let fast = match usage.get("speed").and_then(Value::as_str) {
            None | Some("standard") => false,
            Some("fast") => true,
            Some(other) => {
                batch.note_unknown_value("usage.speed", other, offset);
                false
            }
        };
        if let Some(tier) = usage.get("service_tier").and_then(Value::as_str)
            && tier != "standard"
        {
            batch.note_unknown_value("usage.service_tier", tier, offset);
        }
        if let Some(credit) = usage
            .get("fallback_credit")
            .filter(|credit| !credit.is_null())
        {
            batch.note_unknown_value("usage.fallback_credit", &credit.to_string(), offset);
        }

        let tokens = Tokens {
            input: counts.input,
            cache_read: counts.cache_read,
            cache_write_5m: counts.five,
            cache_write_1h: counts.hour,
            output: counts.output,
            reasoning,
        };
        Some(Used {
            tokens,
            prompt,
            web_searches,
            fast,
        })
    }
}

/// Record Claude Code's own totals for `session`.
fn report(line: &Line, state: &State, session: SessionKey, offset: u64, batch: &mut Batch) {
    let Some(raw) = line.model_usage else {
        batch.note(
            DiagnosticKind::Unreadable,
            "a cost-state with no modelUsage",
            offset,
        );
        return;
    };
    // By model, which is the order the report keeps them in.
    let Ok(models) = serde_json::from_str::<BTreeMap<String, Map<String, Value>>>(raw.get()) else {
        batch.note(
            DiagnosticKind::Unreadable,
            "a cost-state whose modelUsage is the wrong shape",
            offset,
        );
        return;
    };
    let mut totals = Vec::with_capacity(models.len());
    for (model, usage) in models {
        batch.note_unknown_fields(&usage, COST_FIELDS, "modelUsage", offset);
        let count = |field: &str| super::count(usage.get(field));
        let counts = (
            count("inputTokens"),
            count("cacheReadInputTokens"),
            count("cacheCreationInputTokens"),
            count("outputTokens"),
            count("webSearchRequests"),
        );
        let (Some(input), Some(cache_read), Some(cache_write), Some(output), Some(web_searches)) =
            counts
        else {
            batch.note(
                DiagnosticKind::Invalid,
                "a cost-state count missing or out of range",
                offset,
            );
            return;
        };
        let Some(cost) = super::cost(usage.get("costUSD")) else {
            batch.note(
                DiagnosticKind::Invalid,
                "a cost-state cost out of range",
                offset,
            );
            return;
        };
        totals.push(ModelTotal {
            model: Some(model),
            input,
            cache_read,
            cache_write,
            output,
            web_searches,
            cost,
        });
    }
    batch.report(SessionReport {
        session,
        scope: ReportScope::Tree,
        at: state.last_at.and_then(Instant::from_millis),
        models: totals,
    });
}

/// A subagent's description, `agent-<id>.meta.json` beside its log.
#[derive(Deserialize)]
struct Meta {
    /// What it was asked to do.
    #[serde(default)]
    description: Option<String>,
    /// The subagent that started it, when the session itself did not.
    #[serde(rename = "parentAgentId", default)]
    parent_agent_id: Option<String>,
    /// Whether it is a fork, whose log opens with a copy of its parent's
    /// history.
    #[serde(rename = "isFork", default)]
    is_fork: bool,
    /// The call that started it.
    #[serde(rename = "toolUseId", default)]
    tool_use_id: Option<String>,
}

impl Meta {
    /// The description of the subagent `agent` whose log is at `path`: `None`
    /// when there is none, and an error when it does not read.
    fn beside(path: &Path, agent: &str) -> Option<serde_json::Result<Meta>> {
        let document = jsonl::document(&path.with_file_name(format!("agent-{agent}.meta.json")));
        // One too long to read is one that doesn't read: nothing is kept of
        // it, so it is read as no document at all is, which fails.
        let bytes = document.ok()?.unwrap_or_default();
        Some(serde_json::from_slice(&bytes))
    }

    /// The session or subagent that started it, which ran in `session`.
    fn parent(&self, session: &str) -> String {
        self.parent_agent_id
            .clone()
            .filter(|parent| !parent.is_empty())
            .unwrap_or_else(|| session.to_owned())
    }

    /// The copy of its parent's history its log opens with, for a fork that
    /// ran in `session`.
    fn copy(&self, session: &str) -> Option<Copied> {
        let call = self.tool_use_id.clone().filter(|call| !call.is_empty());
        (self.is_fork && call.is_some()).then(|| Copied {
            parent: self.parent(session),
            through: call,
        })
    }
}

/// Record what a subagent's description, `meta`, says of the subagent
/// `agent`: what it was asked to do, and which agent started it.
///
/// Without a description, the subagent is linked to the session it ran in,
/// which is right for every subagent started by the session itself.
fn describe(meta: Option<Meta>, agent: &str, session: &str, batch: &mut Batch) {
    let child = SessionKey::new(Agent::ClaudeCode, agent);
    let parent = meta
        .as_ref()
        .map_or_else(|| session.to_owned(), |meta| meta.parent(session));
    batch.link(SessionLink {
        child: child.clone(),
        parent: SessionKey::new(Agent::ClaudeCode, parent),
        kind: LinkKind::Subagent,
        // The id of the Agent call that started it, which its parent's
        // conversation records on the call.
        launch: meta
            .as_ref()
            .and_then(|meta| meta.tool_use_id.clone())
            .filter(|call| !call.is_empty()),
    });
    let title = meta
        .and_then(|meta| meta.description)
        .and_then(|description| Title::new(TitleSource::Description, &description));
    batch.session_mut(&child).offer_title(title);
}

/// Whether the log at `path` was begun less than [`DESCRIPTION_WAIT`] ago. A
/// log whose beginning the file system does not keep is taken as older.
fn begun_lately(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|file| file.created())
        .ok()
        .and_then(|begun| begun.elapsed().ok())
        .is_some_and(|age| age < DESCRIPTION_WAIT)
}

#[cfg(test)]
mod tests {
    use std::os::macos::fs::FileTimesExt as _;
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use super::ClaudeCode;
    use crate::agent::tests::{assert_said_as_shown, noted, said};
    use crate::agent::{AgentReader, Batch, Checkpoint, DiagnosticKind, Observation, ReportScope};
    use crate::session::{LinkKind, SessionKey, TitleSource};
    use crate::time::Instant;
    use crate::transcript::Speaker;
    use crate::usage::Tokens;
    use crate::{Agent, Usd};

    const SESSION: &str = "0f6e3f6a-713c-4bad-8f6d-f04fe41bbd84";

    fn key(native: &str) -> SessionKey {
        SessionKey::new(Agent::ClaudeCode, native)
    }

    fn at(text: &str) -> Instant {
        Instant::parse(text).unwrap()
    }

    /// A session's file under a projects root, holding `lines`.
    fn session_file(root: &Path, lines: &[Value]) -> PathBuf {
        let path = root.join("-work-ledger").join(format!("{SESSION}.jsonl"));
        write(&path, lines);
        path
    }

    fn write(path: &Path, lines: &[Value]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(path, text).unwrap();
    }

    fn read(path: &Path) -> Batch {
        let mut batch = Batch::default();
        ClaudeCode::default()
            .read(path, &Checkpoint::default(), &mut batch)
            .unwrap();
        batch
    }

    fn observations(batch: &Batch) -> Vec<&Observation> {
        batch.observations().collect()
    }

    fn user(session: &str, time: &str, text: &str) -> Value {
        json!({"type": "user", "sessionId": session, "timestamp": time, "cwd": "/work/ledger",
               "gitBranch": "main", "version": "2.1.200", "entrypoint": "cli",
               "message": {"role": "user", "content": text}})
    }

    fn assistant(session: &str, time: &str, id: &str, usage: Value) -> Value {
        json!({"type": "assistant", "sessionId": session, "timestamp": time, "requestId": format!("req_{id}"),
               "cwd": "/work/ledger", "gitBranch": "main", "version": "2.1.200",
               "message": {"id": format!("msg_{id}"), "model": "claude-opus-5", "role": "assistant",
                           "content": [{"type": "text", "text": "On it."}], "usage": usage}})
    }

    fn usage(input: u64, cache_read: u64, five: u64, hour: u64, output: u64) -> Value {
        json!({"input_tokens": input, "cache_read_input_tokens": cache_read,
               "cache_creation_input_tokens": five + hour, "output_tokens": output,
               "cache_creation": {"ephemeral_5m_input_tokens": five, "ephemeral_1h_input_tokens": hour},
               "service_tier": "standard", "speed": "standard"})
    }

    #[test]
    fn a_streamed_response_counts_once_at_its_most_complete() {
        let dir = tempfile::tempdir().unwrap();
        let path = session_file(
            dir.path(),
            &[
                user(
                    SESSION,
                    "2026-09-14T12:58:00.000Z",
                    "Add idempotency keys\nto the payment endpoints",
                ),
                assistant(
                    SESSION,
                    "2026-09-14T12:58:10.000Z",
                    "1",
                    usage(2, 600, 100, 200, 10),
                ),
                assistant(
                    SESSION,
                    "2026-09-14T12:58:12.000Z",
                    "1",
                    usage(2, 600, 100, 200, 98),
                ),
                assistant(
                    SESSION,
                    "2026-09-14T12:58:11.000Z",
                    "1",
                    usage(2, 600, 100, 200, 40),
                ),
            ],
        );
        let batch = read(&path);

        let seen = observations(&batch);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].response, "msg_1:req_1");
        assert_eq!(seen[0].session, key(SESSION));
        assert!(!seen[0].copy);
        assert_eq!(seen[0].at, at("2026-09-14T12:58:12.000Z"));
        assert_eq!(
            seen[0].tokens,
            Tokens {
                input: 2,
                cache_read: 600,
                cache_write_5m: 100,
                cache_write_1h: 200,
                output: 98,
                reasoning: 0
            }
        );

        let (_, facts) = batch
            .sessions()
            .find(|(session, _)| **session == key(SESSION))
            .unwrap();
        assert_eq!(facts.started, Some(at("2026-09-14T12:58:00.000Z")));
        assert_eq!(facts.last, Some(at("2026-09-14T12:58:12.000Z")));
        assert_eq!(facts.cwd.as_deref(), Some("/work/ledger"));
        assert_eq!(facts.origin.as_deref(), Some("cli"));
        let title = facts.title.as_ref().unwrap();
        assert_eq!(
            (title.source, title.text.as_str()),
            (TitleSource::Prompt, "Add idempotency keys")
        );
        assert_eq!(batch.diagnostics().count(), 0);
    }

    /// The log of the subagent `agent` under a projects root, holding `lines`
    /// as the subagent's, with its description `meta` beside it.
    fn fork_file(root: &Path, agent: &str, meta: &Value, lines: &[Value]) -> PathBuf {
        let path = root
            .join("-work-ledger")
            .join(SESSION)
            .join("subagents")
            .join(format!("agent-{agent}.jsonl"));
        let lines: Vec<Value> = lines
            .iter()
            .map(|line| {
                let mut line = line.clone();
                line["agentId"] = json!(agent);
                line
            })
            .collect();
        write(&path, &lines);
        std::fs::write(
            path.with_file_name(format!("agent-{agent}.meta.json")),
            meta.to_string(),
        )
        .unwrap();
        path
    }

    /// A fork's description, as Claude Code writes it: forked from
    /// `parent`, a subagent, or from the session itself.
    fn fork_meta(parent: Option<&str>) -> Value {
        let mut meta = json!({"agentType": "fork", "description": "Write watch.rs file watcher",
                              "isFork": true, "model": "claude-opus-5", "requestNonInteractive": true,
                              "requestShape": "tool", "spawnDepth": 1, "toolUseId": "toolu_fork"});
        if let Some(parent) = parent {
            meta["parentAgentId"] = json!(parent);
        }
        meta
    }

    /// Response `id`, which calls the Agent tool to start the fork.
    fn spawning(time: &str, id: &str, usage: Value) -> Value {
        let mut line = assistant(SESSION, time, id, usage);
        line["message"]["content"] = json!([{"type": "tool_use", "id": "toolu_fork", "name": "Agent",
                                             "input": {"description": "Write watch.rs file watcher"}}]);
        line
    }

    /// A fork's first line of its own: the result of the call that started
    /// it, and what it was asked to do.
    fn forked(time: &str, text: &str) -> Value {
        json!({"type": "user", "sessionId": SESSION, "timestamp": time, "cwd": "/work/ledger",
               "message": {"role": "user", "content": [
                   {"type": "tool_result", "tool_use_id": "toolu_fork", "content": [{"type": "text", "text": "Forked."}]},
                   {"type": "text", "text": text}]}})
    }

    /// A fork of the subagent `apar3nt`: its copy of the subagent's history,
    /// two prompts and two responses, through the response that started the
    /// fork, then the fork's own prompt and response.
    fn subagent_fork_lines() -> Vec<Value> {
        vec![
            user(SESSION, "2026-09-14T12:50:00.000Z", "Survey the scanner"),
            assistant(
                SESSION,
                "2026-09-14T12:50:05.000Z",
                "1",
                usage(10, 20_000, 0, 1_000, 40),
            ),
            json!({"type": "attachment", "sessionId": SESSION, "timestamp": "2026-09-14T12:50:06.000Z",
                   "attachment": {"type": "todo_reminder"}}),
            user(SESSION, "2026-09-14T12:50:10.000Z", "Now the watcher"),
            spawning(
                "2026-09-14T12:50:15.000Z",
                "2",
                usage(3, 21_000, 0, 500, 60),
            ),
            forked("2026-09-14T13:00:05.000Z", "Write the file watcher"),
            assistant(
                SESSION,
                "2026-09-14T13:00:09.000Z",
                "3",
                usage(3, 1_000, 0, 50, 5),
            ),
        ]
    }

    #[test]
    fn a_fork_of_the_session_leaves_its_copy_of_the_last_response_to_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = fork_file(
            dir.path(),
            "af0rk",
            &fork_meta(None),
            &[
                json!({"type": "fork-context-ref", "parentSessionId": SESSION,
                       "parentLastUuid": "3d3e9837", "contextLength": 316}),
                spawning(
                    "2026-09-14T13:00:00.000Z",
                    "1",
                    usage(32, 254_907, 0, 9_217, 16),
                ),
                forked("2026-09-14T13:00:05.000Z", "Write the file watcher"),
                assistant(
                    SESSION,
                    "2026-09-14T13:00:09.000Z",
                    "2",
                    usage(3, 1_000, 0, 50, 5),
                ),
            ],
        );
        let batch = read(&path);

        let seen = observations(&batch);
        assert_eq!(seen.len(), 2);
        let copied = seen
            .iter()
            .find(|seen| seen.response == "msg_1:req_1")
            .unwrap();
        assert_eq!(
            (copied.session.clone(), copied.copy, copied.tokens.output),
            (key(SESSION), true, 16)
        );
        let own = seen
            .iter()
            .find(|seen| seen.response == "msg_2:req_2")
            .unwrap();
        assert_eq!((own.session.clone(), own.copy), (key("af0rk"), false));

        let link = batch.links().next().unwrap();
        assert_eq!(
            (link.child.clone(), link.parent.clone(), link.kind),
            (key("af0rk"), key(SESSION), LinkKind::Subagent)
        );
        // The fork began with its own prompt, not with the parent's response.
        let (_, facts) = batch
            .sessions()
            .find(|(session, _)| **session == key("af0rk"))
            .unwrap();
        assert_eq!(facts.started, Some(at("2026-09-14T13:00:05.000Z")));
        let title = facts.title.as_ref().unwrap();
        assert_eq!(
            (title.source, title.text.as_str()),
            (TitleSource::Description, "Write watch.rs file watcher")
        );
        // Its copy of the session's response holds none of the session's
        // conversation, which is gone without the session's own file.
        assert!(matches!(
            ClaudeCode::default().conversation(&key(SESSION), std::slice::from_ref(&path)),
            Err(crate::Error::Gone(_))
        ));
    }

    #[test]
    fn a_fork_of_a_subagent_leaves_the_history_it_copied_to_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let path = fork_file(
            dir.path(),
            "af0rk",
            &fork_meta(Some("apar3nt")),
            &subagent_fork_lines(),
        );
        let batch = read(&path);

        let mut seen: Vec<(&str, SessionKey, bool)> = observations(&batch)
            .iter()
            .map(|seen| (seen.response.as_str(), seen.session.clone(), seen.copy))
            .collect();
        seen.sort();
        assert_eq!(
            seen,
            [
                ("msg_1:req_1", key("apar3nt"), true),
                ("msg_2:req_2", key("apar3nt"), true),
                ("msg_3:req_3", key("af0rk"), false),
            ]
        );
        // Nothing the subagent said is said again as the fork's.
        let said: Vec<(&SessionKey, &str)> = batch
            .said()
            .map(|said| (&said.session, said.text.as_str()))
            .collect();
        assert_eq!(
            said,
            [
                (&key("af0rk"), "Write the file watcher"),
                (&key("af0rk"), "On it.")
            ]
        );
        let link = batch.links().next().unwrap();
        assert_eq!(
            (link.child.clone(), link.parent.clone(), link.kind),
            (key("af0rk"), key("apar3nt"), LinkKind::Subagent)
        );
        let (_, facts) = batch
            .sessions()
            .find(|(session, _)| **session == key("af0rk"))
            .unwrap();
        assert_eq!(facts.started, Some(at("2026-09-14T13:00:05.000Z")));

        let entries = ClaudeCode::default()
            .conversation(&key("af0rk"), std::slice::from_ref(&path))
            .unwrap()
            .entries;
        let shown: Vec<(Speaker, &str)> = entries
            .iter()
            .map(|entry| (entry.speaker, entry.text.as_str()))
            .collect();
        // The result of the call that started it answers no call of its own.
        assert_eq!(
            shown,
            [
                (Speaker::System, "Forked."),
                (Speaker::User, "Write the file watcher"),
                (Speaker::Assistant, "On it.")
            ]
        );
        assert_said_as_shown(
            &ClaudeCode::default(),
            &key("af0rk"),
            std::slice::from_ref(&path),
            std::slice::from_ref(&path),
        );
    }

    #[test]
    fn a_fork_read_in_two_parts_still_knows_what_it_copied() {
        let dir = tempfile::tempdir().unwrap();
        let lines = subagent_fork_lines();
        // Read first as far as the copy's second prompt.
        let path = fork_file(
            dir.path(),
            "af0rk",
            &fork_meta(Some("apar3nt")),
            &lines[..4],
        );
        let mut first = Batch::default();
        let checkpoint = ClaudeCode::default()
            .read(&path, &Checkpoint::default(), &mut first)
            .unwrap();
        fork_file(dir.path(), "af0rk", &fork_meta(Some("apar3nt")), &lines);
        let mut second = Batch::default();
        ClaudeCode::default()
            .read(&path, &checkpoint, &mut second)
            .unwrap();

        let copied = |batch: &Batch| -> Vec<(String, bool)> {
            observations(batch)
                .iter()
                .map(|seen| (seen.response.clone(), seen.copy))
                .collect()
        };
        assert_eq!(copied(&first), [("msg_1:req_1".to_owned(), true)]);
        assert_eq!(first.said().count(), 0);
        assert_eq!(
            copied(&second),
            [
                ("msg_2:req_2".to_owned(), true),
                ("msg_3:req_3".to_owned(), false)
            ]
        );
    }

    #[test]
    fn a_subagents_log_waits_for_its_description_while_it_is_new() {
        let dir = tempfile::tempdir().unwrap();
        let path = fork_file(
            dir.path(),
            "af0rk",
            &fork_meta(Some("apar3nt")),
            &subagent_fork_lines(),
        );
        let meta = path.with_file_name("agent-af0rk.meta.json");
        std::fs::remove_file(&meta).unwrap();
        let mut batch = Batch::default();
        let checkpoint = ClaudeCode::default()
            .read(&path, &Checkpoint::default(), &mut batch)
            .unwrap();
        assert_eq!(checkpoint, Checkpoint::default());
        assert_eq!(batch.records(), 0);

        // Begun long enough ago, it is read without one, as the session's.
        let begun = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_created(begun))
            .unwrap();
        let batch = read(&path);
        let link = batch.links().next().unwrap();
        assert_eq!(
            (link.child.clone(), link.parent.clone()),
            (key("af0rk"), key(SESSION))
        );
        assert_eq!(observations(&batch).len(), 3);
    }

    #[test]
    fn a_response_counted_only_in_its_calls_is_their_sum_priced_by_the_largest() {
        let dir = tempfile::tempdir().unwrap();
        // As Claude Code writes some lines: two model calls, zeros for the
        // response's own counts, and its thinking beside them. Claude Code
        // names no model for some calls, writing null.
        let calls = json!({
            "input_tokens": 0, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0,
            "output_tokens": 0, "output_tokens_details": {"thinking_tokens": 870},
            "server_tool_use": {"web_search_requests": 0, "web_fetch_requests": 0},
            "service_tier": "standard", "speed": "standard", "inference_geo": "not_available",
            "cache_creation": {"ephemeral_1h_input_tokens": 0, "ephemeral_5m_input_tokens": 0},
            "iterations": [
                {"input_tokens": 10, "output_tokens": 50, "cache_read_input_tokens": 150_000,
                 "cache_creation_input_tokens": 1_000, "type": "message",
                 "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 1_000}},
                {"input_tokens": 5, "output_tokens": 900, "cache_read_input_tokens": 180_000,
                 "cache_creation_input_tokens": 2_000, "type": "message", "model": null,
                 "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 2_000}}]
        });
        let path = session_file(
            dir.path(),
            &[assistant(SESSION, "2026-09-14T13:10:00.000Z", "1", calls)],
        );
        let batch = read(&path);

        let seen = observations(&batch);
        // Input 10 + 5, cache reads 150,000 + 180,000, 1-hour writes
        // 1,000 + 2,000 and output 50 + 900, 870 of it thinking.
        assert_eq!(
            seen[0].tokens,
            Tokens {
                input: 15,
                cache_read: 330_000,
                cache_write_5m: 0,
                cache_write_1h: 3_000,
                output: 950,
                reasoning: 870
            }
        );
        // The calls' prompts are 10 + 150,000 + 1,000 = 151,010 and
        // 5 + 180,000 + 2,000 = 182,005; not their sum, 333,015.
        assert_eq!(seen[0].prompt, 182_005);
        assert_eq!(batch.diagnostics().count(), 0);
    }

    #[test]
    fn a_fallback_credit_of_null_is_passed_over_and_any_other_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        // As Claude Code 2.1.285 writes every response, and one with a
        // credit whose meaning isn't known.
        let with = |credit: Value| {
            json!({"input_tokens": 2, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 22_964,
                   "output_tokens": 406, "output_tokens_details": {"thinking_tokens": 218},
                   "server_tool_use": {"web_search_requests": 0, "web_fetch_requests": 0},
                   "service_tier": "standard", "speed": "standard", "inference_geo": "not_available",
                   "cache_creation": {"ephemeral_1h_input_tokens": 0, "ephemeral_5m_input_tokens": 0},
                   "fallback_credit": credit})
        };
        let path = session_file(
            dir.path(),
            &[
                assistant(SESSION, "2026-09-29T20:37:59.434Z", "1", with(Value::Null)),
                assistant(
                    SESSION,
                    "2026-09-29T20:38:59.434Z",
                    "2",
                    with(json!({"amount": 1})),
                ),
            ],
        );
        let batch = read(&path);
        // Both are counted, as the credit changes none of their tokens.
        assert_eq!(observations(&batch).len(), 2);
        let noted = noted(&batch);
        assert_eq!(
            noted,
            [(
                DiagnosticKind::UnknownField,
                "usage.fallback_credit {\"amount\":1}"
            )]
        );
    }

    /// Usage of two model calls of 150,000 input and 10 output tokens each,
    /// with the response's own counts `input` and `output`.
    fn two_calls(input: u64, output: u64) -> Value {
        let call = json!({"input_tokens": 150_000, "output_tokens": 10, "cache_read_input_tokens": 0,
                          "cache_creation_input_tokens": 0, "type": "message",
                          "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 0}});
        json!({"input_tokens": input, "output_tokens": output, "cache_creation_input_tokens": 0,
               "cache_read_input_tokens": 0, "service_tier": "standard",
               "cache_creation": {"ephemeral_1h_input_tokens": 0, "ephemeral_5m_input_tokens": 0},
               "iterations": [call.clone(), call]})
    }

    #[test]
    fn a_responses_own_counts_are_its_calls_sum_or_zero_and_otherwise_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        // The response's own counts as their sum, 150,000 × 2 = 300,000
        // input and 10 × 2 = 20 output, and left at zero; then a line
        // written while the response streamed, with only its first call's
        // counts and 5 of its output, before the line that lists both; and
        // 300,001 input, which is neither the sum nor zero.
        let streamed = assistant(
            SESSION,
            "2026-09-14T13:10:00.000Z",
            "3",
            usage(150_000, 0, 0, 0, 5),
        );
        let path = session_file(
            dir.path(),
            &[
                assistant(
                    SESSION,
                    "2026-09-14T13:10:00.000Z",
                    "1",
                    two_calls(300_000, 20),
                ),
                assistant(SESSION, "2026-09-14T13:10:01.000Z", "2", two_calls(0, 0)),
                streamed,
                assistant(
                    SESSION,
                    "2026-09-14T13:10:02.000Z",
                    "3",
                    two_calls(300_000, 20),
                ),
                assistant(
                    SESSION,
                    "2026-09-14T13:10:03.000Z",
                    "4",
                    two_calls(300_001, 20),
                ),
            ],
        );
        let batch = read(&path);

        let seen: Vec<(&str, u64, u64, u64)> = observations(&batch)
            .iter()
            .map(|seen| {
                (
                    seen.response.as_str(),
                    seen.tokens.input,
                    seen.tokens.output,
                    seen.prompt,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("msg_1:req_1", 300_000, 20, 150_000),
                ("msg_2:req_2", 300_000, 20, 150_000),
                ("msg_3:req_3", 300_000, 20, 150_000),
            ]
        );
        let noted = noted(&batch);
        assert_eq!(
            noted,
            [(
                DiagnosticKind::Invalid,
                "usage whose counts are not its iterations' sum"
            )]
        );
    }

    #[test]
    fn claude_codes_own_totals_are_reported_for_the_session_and_its_subagents() {
        let dir = tempfile::tempdir().unwrap();
        let path = session_file(
            dir.path(),
            &[
                user(SESSION, "2026-09-14T12:58:00.000Z", "Go"),
                json!({"type": "cost-state", "sessionId": SESSION, "totalCostUSD": 1.0, "modelUsage": {
                "claude-opus-5[1m]": {"inputTokens": 944, "outputTokens": 278_078, "thinkingTokens": 54_063,
                    "cacheReadInputTokens": 59_465_289, "cacheCreationInputTokens": 546_579,
                    "webSearchRequests": 0, "costUSD": 42.274_133_749_999_99},
                "claude-haiku-4-5-20251001": {"inputTokens": 3, "outputTokens": 285, "cacheReadInputTokens": 0,
                    "cacheCreationInputTokens": 94_081, "webSearchRequests": 0, "costUSD": 0.119_029_25}}}),
            ],
        );
        let batch = read(&path);

        let report = batch.reports().next().unwrap();
        assert_eq!(
            (report.session.clone(), report.scope),
            (key(SESSION), ReportScope::Tree)
        );
        assert_eq!(report.at, Some(at("2026-09-14T12:58:00.000Z")));
        let opus = report
            .models
            .iter()
            .find(|total| total.model.as_deref() == Some("claude-opus-5[1m]"))
            .unwrap();
        assert_eq!(
            (opus.input, opus.cache_read, opus.cache_write, opus.output),
            (944, 59_465_289, 546_579, 278_078)
        );
        assert_eq!(opus.cost, Usd::from_nanos(42_274_133_750));
        let haiku = report
            .models
            .iter()
            .find(|total| total.model.as_deref() == Some("claude-haiku-4-5-20251001"))
            .unwrap();
        assert_eq!(haiku.cost, Usd::from_nanos(119_029_250));
    }

    #[test]
    fn titles_rank_a_users_name_over_a_generated_title_over_the_first_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = session_file(
            dir.path(),
            &[
                user(SESSION, "2026-09-14T12:58:00.000Z", "First thing asked"),
                json!({"type": "ai-title", "sessionId": SESSION, "aiTitle": "Early title"}),
                user(SESSION, "2026-09-14T12:59:00.000Z", "Second thing asked"),
                json!({"type": "ai-title", "sessionId": SESSION, "aiTitle": "Payment idempotency"}),
            ],
        );
        let generated = read(&path);
        let (_, facts) = generated.sessions().next().unwrap();
        assert_eq!(facts.title.as_ref().unwrap().text, "Payment idempotency");

        let named = session_file(
            dir.path(),
            &[
                json!({"type": "agent-name", "sessionId": SESSION, "agentName": "ledger work"}),
                json!({"type": "ai-title", "sessionId": SESSION, "aiTitle": "Payment idempotency"}),
            ],
        );
        let named = read(&named);
        let (_, facts) = named.sessions().next().unwrap();
        assert_eq!(facts.title.as_ref().unwrap().text, "ledger work");
    }

    #[test]
    fn records_and_fields_not_understood_are_noted_and_errors_are_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        let mut surprising = usage(1, 2, 3, 0, 4);
        surprising["audio_tokens"] = json!(7);
        // A tool run and a kind of output not known, beside those known.
        surprising["server_tool_use"] = json!({"web_search_requests": 0, "web_fetch_requests": 1,
                                               "code_execution_requests": 2});
        surprising["output_tokens_details"] = json!({"thinking_tokens": 0, "image_tokens": 5});
        let mut synthetic = assistant(
            SESSION,
            "2026-09-14T12:58:02.000Z",
            "2",
            usage(0, 0, 0, 0, 0),
        );
        synthetic["message"]["model"] = json!("<synthetic>");
        // A call of a field not known, by a model other than the response's.
        let mut called = usage(1, 2, 3, 0, 4);
        called["iterations"] = json!([{"input_tokens": 1, "output_tokens": 4,
            "cache_read_input_tokens": 2, "cache_creation_input_tokens": 3, "type": "message",
            "model": "claude-haiku-4-5", "speculative_tokens": 1,
            "cache_creation": {"ephemeral_5m_input_tokens": 3, "ephemeral_1h_input_tokens": 0}}]);
        // Writes of 3 split as 3 kept five minutes and 2 an hour.
        let mut split = usage(1, 2, 3, 0, 4);
        split["cache_creation"]["ephemeral_1h_input_tokens"] = json!(2);
        // Thinking of 10 in an output of 4.
        let mut thinking = usage(1, 2, 3, 0, 4);
        thinking["output_tokens_details"] = json!({"thinking_tokens": 10});
        let path = session_file(
            dir.path(),
            &[
                json!({"type": "hologram", "sessionId": SESSION}),
                assistant(SESSION, "2026-09-14T12:58:01.000Z", "1", surprising),
                synthetic,
                json!("not an object"),
                assistant(SESSION, "2026-09-14T12:58:03.000Z", "3", called),
                assistant(SESSION, "2026-09-14T12:58:04.000Z", "4", split),
                assistant(SESSION, "2026-09-14T12:58:05.000Z", "5", thinking),
                json!({"type": "cost-state", "sessionId": SESSION, "modelUsage": {"claude-opus-5": {
                    "inputTokens": 3, "outputTokens": 12, "thinkingTokens": 0, "cacheReadInputTokens": 6,
                    "cacheCreationInputTokens": 9, "webSearchRequests": 0, "audioTokens": 2, "costUSD": 0.01}}}),
            ],
        );
        let batch = read(&path);

        let mut counted: Vec<&str> = observations(&batch)
            .iter()
            .map(|seen| seen.response.as_str())
            .collect();
        counted.sort_unstable();
        assert_eq!(counted, ["msg_1:req_1", "msg_3:req_3"]);
        assert_eq!(batch.reports().count(), 1);
        let noted = noted(&batch);
        assert_eq!(
            noted,
            [
                (
                    DiagnosticKind::Unreadable,
                    "a line that is not a JSON object"
                ),
                (DiagnosticKind::UnknownRecord, "record type hologram"),
                (DiagnosticKind::UnknownField, "modelUsage.audioTokens"),
                (DiagnosticKind::UnknownField, "usage.audio_tokens"),
                (
                    DiagnosticKind::UnknownField,
                    "usage.iterations model other than the message's"
                ),
                (
                    DiagnosticKind::UnknownField,
                    "usage.iterations.speculative_tokens"
                ),
                (
                    DiagnosticKind::UnknownField,
                    "usage.output_tokens_details.image_tokens"
                ),
                (
                    DiagnosticKind::UnknownField,
                    "usage.server_tool_use.code_execution_requests"
                ),
                (
                    DiagnosticKind::Invalid,
                    "usage whose thinking exceeds its output"
                ),
                (
                    DiagnosticKind::Invalid,
                    "usage.cache_creation does not add up to cache_creation_input_tokens"
                ),
            ]
        );
    }

    #[test]
    fn system_records_and_content_blocks_of_kinds_not_known_are_noted_and_the_rest_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut prompt = user(SESSION, "2026-09-14T12:58:00.000Z", "");
        prompt["message"]["content"] = json!([{"type": "text", "text": "Review the scan"},
                                              {"type": "hologram", "data": "?"}]);
        let mut reply = assistant(
            SESSION,
            "2026-09-14T12:58:01.000Z",
            "1",
            usage(1, 2, 3, 0, 4),
        );
        reply["message"]["content"] = json!([{"type": "text", "text": "On it."},
                                             {"type": "hologram", "data": "?"}]);
        let path = session_file(
            dir.path(),
            &[
                prompt,
                json!({"type": "system", "subtype": "turn_duration", "sessionId": SESSION,
                       "timestamp": "2026-09-14T12:58:02.000Z", "durationMs": 1_200}),
                json!({"type": "system", "subtype": "hologram_summary", "sessionId": SESSION,
                       "timestamp": "2026-09-14T12:58:03.000Z"}),
                // What Claude Code attaches for the model is passed over,
                // whatever its kind.
                json!({"type": "attachment", "sessionId": SESSION, "timestamp": "2026-09-14T12:58:04.000Z",
                       "attachment": {"type": "a_reminder_never_seen"}}),
                reply,
            ],
        );
        let batch = read(&path);

        // The response is counted, 1 + 2 + 3 + 4 = 10 tokens, and what was
        // said around the blocks not known is said.
        let seen = observations(&batch);
        assert_eq!((seen.len(), seen[0].tokens.total()), (1, 1 + 2 + 3 + 4));
        let said = said(&batch);
        assert_eq!(said, ["Review the scan", "On it."]);
        let noted = noted(&batch);
        assert_eq!(
            noted,
            [
                (
                    DiagnosticKind::UnknownRecord,
                    "assistant content block hologram"
                ),
                (
                    DiagnosticKind::UnknownRecord,
                    "system record hologram_summary"
                ),
                (DiagnosticKind::UnknownRecord, "user content block hologram"),
            ]
        );
    }

    #[test]
    fn a_count_missing_or_out_of_range_invalidates_its_record_and_no_other() {
        let dir = tempfile::tempdir().unwrap();
        // One more than 2^50 = 1,125,899,906,842,624, a negative count, and
        // no output, which Anthropic's API always gives.
        let mut huge = usage(1, 2, 3, 0, 4);
        huge["output_tokens"] = json!(1_125_899_906_842_625_u64);
        let mut negative = usage(1, 2, 3, 0, 4);
        negative["input_tokens"] = json!(-1);
        let mut missing = usage(1, 2, 3, 0, 4);
        missing.as_object_mut().unwrap().remove("output_tokens");
        let path = session_file(
            dir.path(),
            &[
                assistant(
                    SESSION,
                    "2026-09-14T12:58:01.000Z",
                    "1",
                    usage(1, 2, 3, 0, 4),
                ),
                assistant(SESSION, "2026-09-14T12:58:02.000Z", "2", huge),
                assistant(SESSION, "2026-09-14T12:58:03.000Z", "3", negative),
                assistant(SESSION, "2026-09-14T12:58:04.000Z", "4", missing),
                json!({"type": "cost-state", "sessionId": SESSION, "modelUsage": {"claude-opus-5": {
                    "inputTokens": -3, "outputTokens": 12, "thinkingTokens": 0, "cacheReadInputTokens": 6,
                    "cacheCreationInputTokens": 9, "webSearchRequests": 0, "costUSD": 0.01}}}),
            ],
        );
        let batch = read(&path);

        let counted: Vec<&str> = observations(&batch)
            .iter()
            .map(|seen| seen.response.as_str())
            .collect();
        assert_eq!(counted, ["msg_1:req_1"]);
        assert_eq!(batch.reports().count(), 0);
        let noted = noted(&batch);
        assert_eq!(
            noted,
            [
                (
                    DiagnosticKind::Invalid,
                    "a cost-state count missing or out of range"
                ),
                (DiagnosticKind::Invalid, "usage.input_tokens"),
                (DiagnosticKind::Invalid, "usage.output_tokens"),
            ]
        );
    }

    #[test]
    fn only_session_and_subagent_logs_are_artifacts() {
        let root = Path::new("/home/.claude/projects");
        let classify = |path: &str| {
            ClaudeCode::default()
                .classify(root, &root.join(path))
                .is_some()
        };
        assert!(classify(&format!("-work/{SESSION}.jsonl")));
        assert!(classify(&format!(
            "-work/{SESSION}/subagents/agent-a5e550564f24608e2.jsonl"
        )));
        assert!(!classify(&format!(
            "-work/{SESSION}/subagents/agent-a5e550564f24608e2.meta.json"
        )));
        assert!(!classify(&format!(
            "-work/{SESSION}/tool-results/b8j4mpdg2.txt"
        )));
        assert!(!classify("-work/memory/notes.md"));
    }

    #[test]
    fn a_conversation_pairs_each_result_with_its_call_and_leaves_out_a_forks_copy() {
        let dir = tempfile::tempdir().unwrap();
        let mut call = assistant(
            SESSION,
            "2026-09-14T13:00:09.000Z",
            "2",
            usage(1, 1, 0, 0, 1),
        );
        call["message"]["content"] = json!([
            {"type": "thinking", "thinking": "Reading the watcher."},
            {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "watch.rs"}}]);
        let result = json!({"type": "user", "sessionId": SESSION, "timestamp": "2026-09-14T13:00:10.000Z",
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "fn watch() {}"}]}});
        let reminder = json!({"type": "user", "sessionId": SESSION, "isMeta": true,
            "timestamp": "2026-09-14T13:00:11.000Z", "message": {"role": "user", "content": "<system-reminder>Be brief</system-reminder>"}});
        let path = fork_file(
            dir.path(),
            "af0rk",
            &fork_meta(None),
            &[
                json!({"type": "fork-context-ref", "parentSessionId": SESSION}),
                spawning("2026-09-14T13:00:00.000Z", "1", usage(1, 1, 0, 0, 1)),
                forked("2026-09-14T13:00:05.000Z", "Write the file watcher"),
                call,
                result,
                reminder,
            ],
        );
        let entries = ClaudeCode::default()
            .conversation(&key("af0rk"), std::slice::from_ref(&path))
            .unwrap()
            .entries;
        let speakers: Vec<Speaker> = entries.iter().map(|entry| entry.speaker).collect();
        assert_eq!(
            speakers,
            [
                Speaker::System,
                Speaker::User,
                Speaker::Reasoning,
                Speaker::Tool,
                Speaker::System
            ]
        );
        let tool = entries[3].tool.as_ref().unwrap();
        assert_eq!(
            (tool.name.as_str(), tool.output.as_deref()),
            ("Read", Some("fn watch() {}"))
        );
        assert_eq!(entries[1].text, "Write the file watcher");
        assert_said_as_shown(
            &ClaudeCode::default(),
            &key("af0rk"),
            std::slice::from_ref(&path),
            std::slice::from_ref(&path),
        );
    }

    #[test]
    fn a_compacted_conversations_summary_is_not_said_and_does_not_title_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut summary = user(
            SESSION,
            "2026-09-14T13:00:00.000Z",
            "This session is being continued from a previous conversation.",
        );
        summary["isCompactSummary"] = json!(true);
        let path = session_file(
            dir.path(),
            &[
                summary,
                user(SESSION, "2026-09-14T13:00:05.000Z", "Keep going"),
                assistant(
                    SESSION,
                    "2026-09-14T13:00:09.000Z",
                    "1",
                    usage(1, 1, 0, 0, 1),
                ),
            ],
        );
        let batch = read(&path);
        let said = said(&batch);
        assert_eq!(said, ["Keep going", "On it."]);
        let (_, facts) = batch.sessions().next().unwrap();
        assert_eq!(
            facts.title.as_ref().map(|title| title.text.as_str()),
            Some("Keep going")
        );
        let entries = ClaudeCode::default()
            .conversation(&key(SESSION), std::slice::from_ref(&path))
            .unwrap()
            .entries;
        let speakers: Vec<Speaker> = entries.iter().map(|entry| entry.speaker).collect();
        assert_eq!(
            speakers,
            [Speaker::System, Speaker::User, Speaker::Assistant]
        );
        assert_said_as_shown(
            &ClaudeCode::default(),
            &key(SESSION),
            std::slice::from_ref(&path),
            std::slice::from_ref(&path),
        );
    }

    #[test]
    fn what_claude_code_wrote_itself_is_neither_counted_nor_said() {
        let dir = tempfile::tempdir().unwrap();
        // Claude Code's own message in place of a response, as it writes one
        // when a limit is reached.
        let limit = json!({"type": "assistant", "sessionId": SESSION, "timestamp": "2026-09-14T13:00:06.000Z",
            "requestId": "req_011Limit", "error": "rate_limit", "isApiErrorMessage": true, "apiErrorStatus": 429,
            "message": {"id": "46983ad6-61c9-484f-b6c8-725f84621304", "model": "<synthetic>", "role": "assistant",
                "type": "message", "stop_reason": "stop_sequence",
                "content": [{"type": "text", "text": "You've hit your limit · resets 3pm"}],
                "usage": {"input_tokens": 0, "output_tokens": 0, "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0, "output_tokens_details": null, "iterations": null,
                    "server_tool_use": {"web_search_requests": 0, "web_fetch_requests": 0},
                    "cache_creation": {"ephemeral_1h_input_tokens": 0, "ephemeral_5m_input_tokens": 0},
                    "service_tier": null, "speed": null, "inference_geo": null}}});
        let path = session_file(
            dir.path(),
            &[
                user(SESSION, "2026-09-14T13:00:05.000Z", "Keep going"),
                limit,
            ],
        );
        let batch = read(&path);
        assert_eq!(observations(&batch).len(), 0);
        let said = said(&batch);
        assert_eq!(said, ["Keep going"]);
        let entries = ClaudeCode::default()
            .conversation(&key(SESSION), std::slice::from_ref(&path))
            .unwrap()
            .entries;
        let shown: Vec<(Speaker, &str, Option<&str>)> = entries
            .iter()
            .map(|entry| (entry.speaker, entry.text.as_str(), entry.model.as_deref()))
            .collect();
        assert_eq!(
            shown,
            [
                (Speaker::User, "Keep going", None),
                (Speaker::System, "You've hit your limit · resets 3pm", None)
            ]
        );
        assert_said_as_shown(
            &ClaudeCode::default(),
            &key(SESSION),
            std::slice::from_ref(&path),
            std::slice::from_ref(&path),
        );
    }

    #[test]
    fn an_image_and_replies_that_cant_be_counted_are_said_as_shown() {
        let dir = tempfile::tempdir().unwrap();
        // An image pasted with the prompt, which has no words.
        let mut pasted = user(SESSION, "2026-09-14T13:00:05.000Z", "");
        pasted["message"]["content"] = json!([
            {"type": "text", "text": "What does this show?"},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}}
        ]);
        // A reply whose id is not a string, and one with no usage: neither
        // can be counted, and both are shown.
        let mut misshapen = assistant(
            SESSION,
            "2026-09-14T13:00:09.000Z",
            "1",
            usage(1, 1, 0, 0, 1),
        );
        misshapen["message"]["id"] = json!(7);
        misshapen["message"]["content"] =
            json!([{"type": "text", "text": "A chart of scan times."}]);
        let mut unused = assistant(SESSION, "2026-09-14T13:00:12.000Z", "2", Value::Null);
        unused["message"]["content"] =
            json!([{"type": "text", "text": "The last bar is today's."}]);
        let path = session_file(dir.path(), &[pasted, misshapen, unused]);
        let batch = read(&path);
        assert_eq!(observations(&batch).len(), 0);
        let noted = noted(&batch);
        assert_eq!(
            noted,
            [
                (
                    DiagnosticKind::Unreadable,
                    "an assistant message of the wrong shape"
                ),
                (DiagnosticKind::Invalid, "a response with no usage")
            ]
        );
        let entries = ClaudeCode::default()
            .conversation(&key(SESSION), std::slice::from_ref(&path))
            .unwrap()
            .entries;
        let shown: Vec<(Speaker, &str)> = entries
            .iter()
            .map(|entry| (entry.speaker, entry.text.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                (Speaker::User, "What does this show?"),
                (Speaker::Assistant, "A chart of scan times."),
                (Speaker::Assistant, "The last bar is today's.")
            ]
        );
        assert_said_as_shown(
            &ClaudeCode::default(),
            &key(SESSION),
            std::slice::from_ref(&path),
            std::slice::from_ref(&path),
        );
    }

    #[test]
    fn a_conversation_read_again_reads_on_and_comes_to_the_same() {
        let dir = tempfile::tempdir().unwrap();
        // A call, whose result arrives in the lines appended after the first
        // reading, and a line still being written when it was read.
        let call = json!({"type": "assistant", "sessionId": SESSION, "timestamp": "2026-09-14T12:58:10.000Z",
            "requestId": "req_1", "message": {"id": "msg_1", "model": "claude-opus-5", "role": "assistant",
            "content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "ls"}}],
            "usage": usage(2, 600, 100, 200, 10)}});
        let result = json!({"type": "user", "sessionId": SESSION, "timestamp": "2026-09-14T12:58:11.000Z",
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1",
            "content": "Cargo.toml"}]}});
        let first = [
            user(SESSION, "2026-09-14T12:58:00.000Z", "List the files"),
            call,
        ];
        let later = [
            result,
            assistant(
                SESSION,
                "2026-09-14T12:58:12.000Z",
                "2",
                usage(2, 600, 100, 200, 5),
            ),
        ];
        let path = session_file(dir.path(), &first);
        let reader = ClaudeCode::default();
        let key = key(SESSION);
        let conversation = |reader: &ClaudeCode| {
            reader
                .conversation(&key, std::slice::from_ref(&path))
                .unwrap()
                .entries
        };
        // Half of the result's line is written when it is read first.
        let partial = format!("{}", later[0]);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, &partial.as_bytes()[..20]).unwrap();
        assert_eq!(conversation(&reader).len(), 2);
        let rest: String = std::iter::once(partial[20..].to_owned() + "\n")
            .chain(later[1..].iter().map(|line| format!("{line}\n")))
            .collect();
        std::io::Write::write_all(&mut file, rest.as_bytes()).unwrap();
        drop(file);

        // Read on, the call answered, it is what reading it whole gives.
        let read_on = conversation(&reader);
        assert_eq!(read_on, conversation(&ClaudeCode::default()));
        assert_eq!(
            read_on[1]
                .tool
                .as_ref()
                .and_then(|call| call.output.as_deref()),
            Some("Cargo.toml")
        );
        assert_eq!(read_on.len(), 3);

        // A file written again from the start is read again from the start.
        write(&path, &first[..1]);
        assert_eq!(conversation(&reader), conversation(&ClaudeCode::default()));
        assert_eq!(conversation(&reader).len(), 1);
    }
}
