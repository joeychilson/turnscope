//! Claude Code.
//!
//! Each session is a JSON Lines log, `~/.claude/projects/<project>/<session>.jsonl`,
//! and each subagent another, `<project>/<session>/subagents/agent-<id>.jsonl`,
//! with `agent-<id>.meta.json` beside it: its task, whether it is a fork, the
//! call that started it, and the subagent that did when not the session.
//! A subagent's lines carry its session's `sessionId` and its own `agentId`.
//! A second account's folder is `~/.claude-<name>`, used through
//! `CLAUDE_CONFIG_DIR`, once Claude Code has written `.claude.json` there.
//!
//! What follows was measured on this Mac's history, September and October 2026.
//!
//! **Usage.** A response is written one line per content block, its usage as
//! it stood then, so its output count grows from line to line: 119,715 lines
//! held 53,928 responses. A response is `message.id` with `requestId`, and its
//! reports merge to the largest. `input_tokens` excludes what was read from
//! and written to the cache. A response made of several model calls lists
//! them in `usage.iterations`, and its own counts are their sum or zero; for
//! some responses the calls are the only record. A reply naming the model
//! `<synthetic>` is Claude Code's own, such as an error, and costs nothing.
//!
//! **Forks.** A fork's log opens with a copy of its parent's history, through
//! the line making the call that started it (`toolUseId` in its description),
//! under the fork's `agentId`. A fork of the session itself opens with
//! `fork-context-ref` and a copy of the parent's last response, ended by the
//! fork's first prompt. The copy is the parent's: its responses are marked
//! copies of the parent's, and nothing else in it is the fork's. Only the
//! description tells a fork from another subagent, and Claude Code may write
//! it up to 1.5 s after the log, so a new log waits for it.
//!
//! **Continuations and `/clear`.** A line can name, in `session_id`, another
//! session than its `sessionId`. A session carried on in a new log (the old
//! log ends with `continued-in`) opens with what the old one held since it
//! last compacted, under the old session's name; but after `/clear`, every
//! line goes on naming the session the process began as: 120 of the 123 logs
//! naming another session held only responses of their own. So such a line's
//! response is its log's, as a copy: where the session it names holds it
//! too, that session's own report wins. Resuming shows a continuation's copy,
//! so what it says stays the new session's conversation.
//!
//! **What the person typed.** A prompt typed while the model worked is an
//! `attachment` of type `queued_command` with `commandMode` `prompt` (178
//! of them, beside task notifications queued the same way). A subagent's
//! hand-back or another session's message comes as one too, marked `isMeta`
//! (323), and isn't the person's. Pastes are
//! marked `<pasted_content …>`, and a command of Claude Code's own, such as
//! `/clear`, asks for no work, so it titles nothing: 30 sessions had been
//! titled `/clear`. A log can hold a line twice, rewritten as it was.
//!
//! **Not read.** `cost-state` records count calls the transcript doesn't
//! hold, about 1–2% of cost; usage is counted from the transcript alone.
//! Claude Code deletes logs older than `cleanupPeriodDays`; what was read of
//! them stays.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use super::{
    Agent, Conversation, Credential, Entry, Info, Kind, Link, Login, Parent, Read, Response, Role,
    Secret, Title, Tokens,
};
use crate::{Error, Result};

pub struct ClaudeCode;

static INFO: Info = Info {
    id: "claude-code",
    name: "Claude Code",
    command: "claude",
    resume: "claude --resume {id}",
    mcp_add: &["mcp", "add", "--scope", "user", "turnscope", "--"],
    mcp_remove: Some(&["mcp", "remove", "--scope", "user", "turnscope"]),
    folder_var: Some("CLAUDE_CONFIG_DIR"),
    charges: false,
    session_var: Some("CLAUDE_CODE_SESSION_ID"),
    cwd_var: Some("CLAUDE_PROJECT_DIR"),
};

/// What Claude Code adds to a message sent as the person's.
const INJECTED: &[&str] = &[
    "bash-input",
    "bash-stderr",
    "bash-stdout",
    "fork-boilerplate",
    "local-command-caveat",
    "local-command-stdout",
    "system-reminder",
    "task-notification",
];

/// The tools that change the file their input names.
const EDITS: &[&str] = &["Edit", "MultiEdit", "NotebookEdit", "Write"];

/// Claude Code's own commands, which ask for no work: a session they open
/// isn't titled by them.
const COMMANDS: &[&str] = &[
    "/add-dir",
    "/agents",
    "/clear",
    "/compact",
    "/config",
    "/context",
    "/cost",
    "/doctor",
    "/effort",
    "/exit",
    "/fast",
    "/feedback",
    "/help",
    "/hooks",
    "/login",
    "/logout",
    "/mcp",
    "/memory",
    "/model",
    "/permissions",
    "/plugin",
    "/recap",
    "/reload-plugins",
    "/rename",
    "/resume",
    "/status",
    "/usage",
];

/// What the person typed: a slash command as they typed it, or the text
/// without what Claude Code added, and what they pasted without its marks.
fn typed(text: &str) -> String {
    // Claude Code's note that the person stopped the model, not their words.
    if text.starts_with("[Request interrupted") {
        return String::new();
    }
    match super::inner(text, "command-name") {
        Some(name) => format!(
            "{name} {}",
            super::inner(text, "command-args").unwrap_or_default()
        )
        .trim()
        .to_owned(),
        None => {
            let mut text = super::without_tags(text, INJECTED);
            for mark in ["<pasted_content", "</pasted_content"] {
                while let Some(start) = text.find(mark) {
                    let end = text[start..]
                        .find('>')
                        .map_or(text.len(), |end| start + end + 1);
                    text.replace_range(start..end, "");
                }
            }
            text.trim().to_owned()
        }
    }
}

/// How Claude Code opens a summary of a compacted conversation, after any
/// notice about what it restates.
const CONTINUED: &str =
    "This session is being continued from a previous conversation that ran out of context.";

/// What follows a summary: where to read more, and to carry on.
const AFTER_SUMMARY: &[&str] = &[
    "\nIf you need specific details from before compaction",
    "\nContinue the conversation from where it left off",
];

/// A compaction summary without what Claude Code wraps it in: the lines
/// before it, and those after it telling the model to carry on.
fn summarized(text: &str) -> &str {
    let text = text
        .find(CONTINUED)
        .map_or(text, |at| &text[at + CONTINUED.len()..])
        .trim_start();
    let text = text
        .strip_prefix("The summary below covers the earlier portion of the conversation.")
        .unwrap_or(text)
        .trim_start();
    let text = text.strip_prefix("Summary:").unwrap_or(text);
    let end = AFTER_SUMMARY
        .iter()
        .filter_map(|after| text.find(after))
        .min()
        .unwrap_or(text.len());
    text[..end].trim()
}

/// Whether `typed` asks for work, so it can title a session.
fn asks(typed: &str) -> bool {
    let command = typed.split_whitespace().next().unwrap_or_default();
    !typed.is_empty() && !COMMANDS.contains(&command)
}

#[derive(Default, Serialize, Deserialize)]
struct Cursor {
    offset: u64,
    /// While a fork's copy of its parent's history is being read.
    copy: Option<Copy>,
    /// Whether the person has said anything yet.
    prompted: bool,
    /// Whether the first prompt has titled the session.
    titled: bool,
    /// Whether a subagent's description has been read.
    described: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct Copy {
    parent: String,
    /// The call the copy runs through; without one, the first prompt ends it.
    through: Option<String>,
}

#[derive(Deserialize)]
struct Line<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Cow<'a, str>,
    #[serde(rename = "sessionId", borrow)]
    session_id: Option<Cow<'a, str>>,
    /// The session a continuation's copied line was the old one's, or after
    /// `/clear`, the session the process began as.
    #[serde(rename = "session_id", borrow)]
    named: Option<Cow<'a, str>>,
    #[serde(rename = "agentId", borrow)]
    agent_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(borrow)]
    cwd: Option<Cow<'a, str>>,
    #[serde(rename = "gitBranch", borrow)]
    git_branch: Option<Cow<'a, str>>,
    #[serde(rename = "requestId", borrow)]
    request_id: Option<Cow<'a, str>>,
    #[serde(rename = "isMeta", default)]
    is_meta: bool,
    #[serde(rename = "isCompactSummary", default)]
    is_compact_summary: bool,
    #[serde(rename = "promptSource", borrow)]
    prompt_source: Option<Cow<'a, str>>,
    #[serde(borrow)]
    message: Option<Message<'a>>,
    #[serde(rename = "aiTitle", borrow)]
    ai_title: Option<Cow<'a, str>>,
    #[serde(rename = "agentName", borrow)]
    agent_name: Option<Cow<'a, str>>,
    #[serde(rename = "customTitle", borrow)]
    custom_title: Option<Cow<'a, str>>,
    #[serde(rename = "continuedInSessionId", borrow)]
    continued_in: Option<Cow<'a, str>>,
    #[serde(rename = "parentSessionId", borrow)]
    parent_session_id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    uuid: Option<Cow<'a, str>>,
    #[serde(borrow)]
    attachment: Option<Attachment<'a>>,
}

/// An attachment: of those, a `queued_command` holds what the person typed
/// while the model worked, beside notifications Claude Code queued.
#[derive(Deserialize)]
struct Attachment<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(rename = "commandMode", borrow)]
    mode: Option<Cow<'a, str>>,
    prompt: Option<Value>,
    /// Queued by Claude Code: a subagent's hand-back, or another session's
    /// message.
    #[serde(rename = "isMeta", default)]
    meta: bool,
}

impl Attachment<'_> {
    /// A background subagent's report, if this is its hand-back: the
    /// subagent's id and the report, as Claude Code queues it: framed by a
    /// paragraph of its own, every line indented two spaces.
    fn hand_back(&self) -> Option<(&str, String)> {
        let text = self.prompt.as_ref()?.as_str()?;
        let rest = text.strip_prefix("<agent-message from=\"")?;
        let (id, rest) = rest.split_once("\">\n")?;
        let (frame, report) = rest.split_once('\n')?;
        frame.starts_with("[Subagent hand-back]").then_some(())?;
        let report = report.trim_end().strip_suffix("</agent-message>")?;
        let report = report
            .lines()
            .map(|line| line.strip_prefix("  ").unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        Some((id, report.trim().to_owned()))
    }

    /// What was queued as a prompt while the model worked, if this holds
    /// it, and whether the person typed it.
    fn queued(&self) -> Option<(String, bool)> {
        if (self.kind.as_deref(), self.mode.as_deref()) != (Some("queued_command"), Some("prompt"))
        {
            return None;
        }
        let text = match self.prompt.as_ref()? {
            Value::String(text) => text.clone(),
            blocks => blocks
                .as_array()?
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        };
        Some((typed(&text), !self.meta)).filter(|(text, _)| !text.is_empty())
    }
}

impl Line<'_> {
    /// A line with the person's role that Claude Code sent itself: a skill's
    /// instructions, a compaction's summary, a task's notification.
    fn claude_codes(&self) -> bool {
        self.is_meta || self.is_compact_summary || self.prompt_source.as_deref() == Some("system")
    }

    fn at(&self) -> Option<i64> {
        self.timestamp.as_deref().and_then(crate::time::millis)
    }
}

#[derive(Deserialize)]
struct Message<'a> {
    #[serde(borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(borrow)]
    model: Option<Cow<'a, str>>,
    usage: Option<Usage>,
    /// A string, or a list of blocks; held unparsed so blocks are read only
    /// as far as needed.
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}

impl Message<'_> {
    /// Claude Code wrote it itself, such as to say a limit was reached.
    fn synthetic(&self) -> bool {
        self.model.as_deref() == Some("<synthetic>")
    }

    /// Its text: the string, or its text blocks.
    fn texts(&self) -> Vec<String> {
        #[derive(Deserialize)]
        struct Block<'a> {
            #[serde(rename = "type", borrow)]
            kind: Option<Cow<'a, str>>,
            #[serde(borrow)]
            text: Option<Cow<'a, str>>,
        }
        let Some(content) = self.content else {
            return Vec::new();
        };
        if let Ok(text) = serde_json::from_str::<String>(content.get()) {
            return vec![text];
        }
        serde_json::from_str::<Vec<Block>>(content.get())
            .unwrap_or_default()
            .into_iter()
            .filter(|block| block.kind.as_deref() == Some("text"))
            .filter_map(|block| block.text.map(Cow::into_owned))
            .collect()
    }

    /// Whether it makes the tool call `id`.
    fn calls(&self, id: &str) -> bool {
        #[derive(Deserialize)]
        struct Block<'a> {
            #[serde(borrow)]
            id: Option<Cow<'a, str>>,
        }
        self.content
            .and_then(|content| serde_json::from_str::<Vec<Block>>(content.get()).ok())
            .is_some_and(|blocks| blocks.iter().any(|block| block.id.as_deref() == Some(id)))
    }
}

#[derive(Deserialize, Clone, Copy, Default)]
struct Counts {
    input_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation: CacheCreation,
    output_tokens: u64,
}

#[derive(Deserialize, Clone, Copy, Default)]
struct CacheCreation {
    ephemeral_5m_input_tokens: u64,
    ephemeral_1h_input_tokens: u64,
}

impl Counts {
    fn prompt(&self) -> u64 {
        self.input_tokens
            + self.cache_read_input_tokens
            + self.cache_creation.ephemeral_5m_input_tokens
            + self.cache_creation.ephemeral_1h_input_tokens
    }
}

#[derive(Deserialize)]
struct Usage {
    #[serde(flatten)]
    counts: Counts,
    #[serde(default)]
    iterations: Option<Vec<Counts>>,
    #[serde(default)]
    output_tokens_details: Option<OutputDetails>,
    #[serde(default)]
    server_tool_use: Option<ServerTools>,
    #[serde(default)]
    speed: Option<String>,
}

#[derive(Deserialize)]
struct OutputDetails {
    #[serde(default)]
    thinking_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct ServerTools {
    #[serde(default)]
    web_search_requests: Option<u64>,
}

/// A subagent's description, `agent-<id>.meta.json`.
#[derive(Deserialize)]
struct Meta {
    description: Option<String>,
    #[serde(rename = "parentAgentId")]
    parent_agent_id: Option<String>,
    #[serde(rename = "isFork", default)]
    is_fork: bool,
    #[serde(rename = "toolUseId")]
    tool_use_id: Option<String>,
}

/// The session and subagent a log is named for: `(session, None)` for a
/// session's own, `(session, Some(agent))` for a subagent's.
fn named(file: &Path) -> Option<(String, Option<String>)> {
    let name = file.file_name()?.to_str()?.strip_suffix(".jsonl")?;
    let folder = file.parent()?;
    if folder.file_name()? == "subagents" {
        let session = folder.parent()?.file_name()?.to_str()?;
        let agent = name.strip_prefix("agent-")?;
        Some((session.to_owned(), Some(agent.to_owned())))
    } else {
        Some((name.to_owned(), None))
    }
}

fn id(native: &str) -> String {
    format!("claude-code:{native}")
}

impl Agent for ClaudeCode {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn folders(&self, home: &Path) -> Vec<PathBuf> {
        super::own_and_found(home, ".claude", ".claude-", ".claude.json")
    }

    fn files(&self, folder: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let projects = std::fs::read_dir(folder.join("projects"))
            .into_iter()
            .flatten();
        for project in projects.flatten() {
            for entry in std::fs::read_dir(project.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                let path = entry.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
                {
                    files.push(path);
                } else if path.is_dir() {
                    let subagents = std::fs::read_dir(path.join("subagents"))
                        .into_iter()
                        .flatten();
                    files.extend(
                        subagents
                            .flatten()
                            .map(|entry| entry.path())
                            .filter(|path| {
                                path.extension()
                                    .is_some_and(|extension| extension == "jsonl")
                            }),
                    );
                }
            }
        }
        files
    }

    fn read(&self, file: &Path, cursor: &str) -> Result<Read> {
        let (session, agent) = named(file)
            .ok_or_else(|| Error::Failed(format!("{} isn't a Claude Code log", file.display())))?;
        let own = id(agent.as_deref().unwrap_or(&session));
        let mut cursor: Cursor = serde_json::from_str(cursor).unwrap_or_default();
        let mut read = Read::default();

        if let Some(agent) = &agent
            && !cursor.described
        {
            let meta = file.with_file_name(format!("agent-{agent}.meta.json"));
            match std::fs::read(&meta) {
                Ok(bytes) => {
                    let meta: Option<Meta> = serde_json::from_slice(&bytes).ok();
                    let parent = meta
                        .as_ref()
                        .and_then(|meta| meta.parent_agent_id.clone())
                        .filter(|parent| !parent.is_empty())
                        .unwrap_or(session.clone());
                    let fork = meta.as_ref().is_some_and(|meta| meta.is_fork);
                    if fork && cursor.offset == 0 {
                        cursor.copy =
                            meta.as_ref()
                                .and_then(|meta| meta.tool_use_id.clone())
                                .map(|call| Copy {
                                    parent: id(&parent),
                                    through: Some(call),
                                });
                    }
                    // A fork here is a subagent begun with a copy of its
                    // parent's context: it's still the parent's subagent.
                    let described = read.session(&own);
                    described.parent = Some(Parent {
                        id: id(&parent),
                        link: Link::Subagent,
                    });
                    if let Some(task) = meta.and_then(|meta| meta.description) {
                        described.title(Title::Task, &task);
                    }
                    cursor.described = true;
                }
                // Read without its description, a fork's copy would read as
                // the fork's own, so a new log waits for it.
                // An error, so it's tried again at the next read.
                Err(_) if cursor.offset == 0 && begun_lately(file) => {
                    return Err(Error::Failed(format!(
                        "{} waits for its description",
                        file.display()
                    )));
                }
                Err(_) => {
                    read.session(&own).parent = Some(Parent {
                        id: id(&session),
                        link: Link::Subagent,
                    });
                    cursor.described = true;
                }
            }
        }

        cursor.offset = super::lines(file, cursor.offset, |bytes| {
            let Ok(line) = serde_json::from_slice::<Line>(bytes) else {
                read.skipped += 1;
                return;
            };
            read_line(&line, &own, &mut cursor, &mut read);
        })?;
        read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
        Ok(read)
    }

    fn transcript(&self, native: &str, files: &[PathBuf]) -> Result<Vec<Entry>> {
        let (file, (session, agent)) = files
            .iter()
            .find_map(|file| {
                let named = named(file)?;
                (named.1.as_deref().unwrap_or(&named.0) == native).then_some((file, named))
            })
            .ok_or_else(|| Error::NotFound(format!("the log of claude-code:{native} is gone")))?;
        let meta: Option<Meta> = agent.as_ref().and_then(|agent| {
            let meta = std::fs::read(file.with_file_name(format!("agent-{agent}.meta.json")));
            serde_json::from_slice(&meta.ok()?).ok()
        });
        let mut copy = meta.filter(|meta| meta.is_fork).and_then(|meta| {
            Some(Copy {
                parent: meta.parent_agent_id.unwrap_or(session),
                through: Some(meta.tool_use_id?),
            })
        });
        let mut prompted = false;
        let mut conversation = Conversation::default();
        // A log can hold a line twice, rewritten as it was.
        let mut seen = std::collections::HashSet::new();
        super::lines(file, 0, |bytes| {
            let Ok(line) = serde_json::from_slice::<Line>(bytes) else {
                return;
            };
            if in_copy(&line, &mut copy, &mut prompted)
                || line
                    .uuid
                    .as_ref()
                    .is_some_and(|uuid| !seen.insert(uuid.to_string()))
            {
                return;
            }
            match &*line.kind {
                "user" | "assistant" => converse(&line, bytes, agent.is_some(), &mut conversation),
                "attachment" => {
                    let Some(attachment) = &line.attachment else {
                        return;
                    };
                    // A subagent run in the background answers its call
                    // with word that it started; its report comes later,
                    // and is that call's output.
                    if let Some((id, report)) = attachment.hand_back() {
                        let started = format!("agentId: {id}");
                        let call = conversation
                            .entries
                            .iter_mut()
                            .rev()
                            .filter_map(|entry| entry.tool.as_mut())
                            .find(|tool| {
                                tool.output
                                    .as_deref()
                                    .is_some_and(|output| output.contains(&started))
                            });
                        if let Some(call) = call {
                            call.output = Some(report);
                            return;
                        }
                    }
                    if let Some((text, typed)) = attachment.queued() {
                        let kind = match (typed, agent.is_some()) {
                            (false, _) => Kind::System,
                            (true, true) => Kind::Task,
                            (true, false) => Kind::User,
                        };
                        conversation.say(kind, line.at(), &text);
                    }
                }
                _ => {}
            }
        })?;
        Ok(conversation.entries)
    }

    fn logins(&self, folder: &Path, home: &Path) -> Result<Vec<Login>> {
        // Its own folder's login is the item `Claude Code-credentials`;
        // another folder's adds the first 8 hex digits of the SHA-256 of its
        // path (Claude Code 2.1.284's code, read 2026-09-29).
        let own = folder == home.join(".claude");
        let item = if own {
            "Claude Code-credentials".to_owned()
        } else {
            use sha2::Digest as _;
            let digest = sha2::Sha256::digest(folder.as_os_str().as_encoded_bytes());
            let hex: String = digest[..4]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            format!("Claude Code-credentials-{hex}")
        };
        // Read with `/usr/bin/security`, the tool Claude Code writes it with,
        // so no permission prompt appears; it finds the Keychain under `HOME`.
        let failed =
            |why: String| Error::Failed(format!("reading the Keychain item {item}: {why}"));
        let mut security = std::process::Command::new("/usr/bin/security");
        security
            .args(["find-generic-password", "-s", &item, "-w"])
            .env("HOME", home);
        let output = crate::output_within(&mut security, Duration::from_secs(20))
            .map_err(|error| failed(error.to_string()))?;
        // 44: no such item.
        let kept: Option<Value> = match output.status.code() {
            Some(0) => Some(
                serde_json::from_slice(&output.stdout)
                    .map_err(|error| failed(error.to_string()))?,
            ),
            Some(44) => None,
            code => return Err(failed(format!("security exited with {code:?}"))),
        };
        let token = kept
            .as_ref()
            .and_then(|kept| kept.pointer("/claudeAiOauth/accessToken")?.as_str());
        let credential = match (kept.as_ref(), token) {
            (Some(kept), Some(token)) => {
                // Whose it is: a plan belongs to an organization, and one
                // account can be in several.
                let config = if own {
                    home.join(".claude.json")
                } else {
                    folder.join(".claude.json")
                };
                let config = super::json_file(&config)?.unwrap_or_default();
                let part = |pointer: &str| {
                    config
                        .pointer(pointer)
                        .and_then(Value::as_str)
                        .filter(|part| !part.is_empty())
                };
                let identity = match (
                    part("/oauthAccount/accountUuid"),
                    part("/oauthAccount/organizationUuid"),
                ) {
                    (Some(account), Some(organization)) => Some((
                        format!("{account}:{organization}"),
                        part("/oauthAccount/emailAddress").map(str::to_owned),
                    )),
                    _ => None,
                };
                Some(Credential {
                    provider: "anthropic",
                    secret: Secret::new(token),
                    key: false,
                    expires: kept
                        .pointer("/claudeAiOauth/expiresAt")
                        .and_then(Value::as_i64),
                    identity,
                    plan: kept
                        .pointer("/claudeAiOauth/subscriptionType")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
            }
            _ => None,
        };
        Ok(vec![Login {
            provider: "anthropic".to_owned(),
            credential,
        }])
    }

    fn mcp_command(&self, folder: &Path, home: &Path) -> Option<String> {
        // Its own folder's settings are beside it; another's are inside it.
        let settings = match folder == home.join(".claude") {
            true => home.join(".claude.json"),
            false => folder.join(".claude.json"),
        };
        super::json_mcp_command(&settings, "/mcpServers/turnscope/command")
    }

    fn role(&self, name: &str) -> Role {
        match name {
            "Read" | "Grep" | "Glob" | "LS" | "WebFetch" | "WebSearch" | "NotebookRead" => {
                Role::Read
            }
            "AskUserQuestion" => Role::Ask,
            "Agent" | "Task" => Role::Subagent,
            _ => Role::Work,
        }
    }
}

/// Whether `line` is part of a fork's copy of its parent's history, keeping
/// `copy` and `prompted` up to date.
fn in_copy(line: &Line, copy: &mut Option<Copy>, prompted: &mut bool) -> bool {
    if let Some(copying) = copy.as_ref() {
        match (&*line.kind, &copying.through) {
            ("user", None) => *copy = None,
            ("assistant", Some(call)) => {
                if line
                    .message
                    .as_ref()
                    .is_some_and(|message| message.calls(call))
                {
                    *copy = None;
                }
                return true;
            }
            ("user" | "assistant" | "system" | "attachment", _) => return true,
            _ => return false,
        }
    }
    match &*line.kind {
        // A fork of the session itself, before its first prompt.
        "fork-context-ref" => {
            if !*prompted && copy.is_none() {
                *copy = line.parent_session_id.as_deref().map(|parent| Copy {
                    parent: id(parent),
                    through: None,
                });
            }
            true
        }
        "user" => {
            *prompted = true;
            false
        }
        _ => false,
    }
}

fn read_line(line: &Line, own: &str, cursor: &mut Cursor, read: &mut Read) {
    let copied = cursor.copy.as_ref().map(|copy| copy.parent.clone());
    if in_copy(line, &mut cursor.copy, &mut cursor.prompted) {
        if let (Some(parent), "assistant") = (copied, &*line.kind) {
            respond(line, &parent, true, read);
        }
        return;
    }
    // A subagent's lines name the session it ran in; its agent id says whose
    // they are.
    let owner = match (&line.agent_id, &line.session_id) {
        (Some(agent), _) | (None, Some(agent)) if !agent.is_empty() => id(agent),
        _ => own.to_owned(),
    };
    match &*line.kind {
        "user" => {
            if !line.claude_codes() {
                for text in line
                    .message
                    .as_ref()
                    .map(Message::texts)
                    .unwrap_or_default()
                {
                    let text = typed(&text);
                    read.say(&owner, &text);
                    if !cursor.titled && asks(&text) {
                        read.session(&owner).title(Title::Prompt, &text);
                        cursor.titled = true;
                    }
                }
            }
        }
        "assistant" => {
            let named = line.named.as_deref();
            let copy = named.is_some_and(|named| Some(named) != line.session_id.as_deref());
            respond(line, &owner, copy, read);
            if let Some(message) = line.message.as_ref().filter(|message| !message.synthetic()) {
                read.say(&owner, &message.texts().join("\n"));
            }
        }
        "attachment" => {
            if let Some((text, true)) = line.attachment.as_ref().and_then(Attachment::queued) {
                read.say(&owner, &text);
            }
        }
        "system" => {}
        "ai-title" => {
            if let Some(title) = &line.ai_title {
                read.session(&owner).title(Title::Generated, title);
            }
            return;
        }
        "agent-name" | "custom-title" => {
            if let Some(name) = line.agent_name.as_ref().or(line.custom_title.as_ref()) {
                read.session(&owner).title(Title::Named, name);
            }
            return;
        }
        "continued-in" => {
            if let Some(next) = line.continued_in.as_deref() {
                read.session(&id(next)).parent = Some(Parent {
                    id: owner,
                    link: Link::Continuation,
                });
            }
            return;
        }
        _ => return,
    }
    let session = read.session(&owner);
    if let Some(at) = line.at() {
        session.saw(at);
    }
    if session.cwd.is_none() {
        session.cwd = line.cwd.as_deref().map(str::to_owned);
    }
    if let Some(branch) = &line.git_branch {
        session.branch = Some(branch.to_string());
    }
}

/// Record the usage on an assistant line as `session`'s.
fn respond(line: &Line, session: &str, copy: bool, read: &mut Read) {
    let Some(message) = line.message.as_ref().filter(|message| !message.synthetic()) else {
        return;
    };
    let (Some(usage), Some(message_id), Some(model), Some(at)) =
        (&message.usage, &message.id, &message.model, line.at())
    else {
        read.skipped += 1;
        return;
    };
    // The calls' sum, where the response lists them.
    let calls = usage.iterations.as_deref().unwrap_or_default();
    let (counts, prompt) = if calls.is_empty() {
        (usage.counts, usage.counts.prompt())
    } else {
        let sum = calls.iter().fold(Counts::default(), |sum, call| Counts {
            input_tokens: sum.input_tokens + call.input_tokens,
            cache_read_input_tokens: sum.cache_read_input_tokens + call.cache_read_input_tokens,
            cache_creation: CacheCreation {
                ephemeral_5m_input_tokens: sum.cache_creation.ephemeral_5m_input_tokens
                    + call.cache_creation.ephemeral_5m_input_tokens,
                ephemeral_1h_input_tokens: sum.cache_creation.ephemeral_1h_input_tokens
                    + call.cache_creation.ephemeral_1h_input_tokens,
            },
            output_tokens: sum.output_tokens + call.output_tokens,
        });
        (
            sum,
            calls.iter().map(Counts::prompt).max().unwrap_or_default(),
        )
    };
    read.respond(Response {
        id: match &line.request_id {
            Some(request) if !request.is_empty() => format!("{message_id}:{request}"),
            _ => message_id.to_string(),
        },
        session: session.to_owned(),
        copy,
        at,
        provider: "anthropic".to_owned(),
        model: model.to_string(),
        tokens: Tokens {
            input: counts.input_tokens,
            cache_read: counts.cache_read_input_tokens,
            cache_write_5m: counts.cache_creation.ephemeral_5m_input_tokens,
            cache_write_1h: counts.cache_creation.ephemeral_1h_input_tokens,
            output: counts.output_tokens,
            reasoning: usage
                .output_tokens_details
                .as_ref()
                .and_then(|details| details.thinking_tokens)
                .unwrap_or_default(),
        },
        prompt,
        web_searches: usage
            .server_tool_use
            .as_ref()
            .and_then(|tools| tools.web_search_requests)
            .unwrap_or_default(),
        priority: usage.speed.as_deref() == Some("fast"),
        cost: None,
    });
}

/// Add a user or assistant line to a conversation. In a subagent's, what
/// arrives as the person's is its task.
fn converse(line: &Line, bytes: &[u8], subagent: bool, conversation: &mut Conversation) {
    #[derive(Deserialize)]
    struct Full {
        message: Option<FullMessage>,
    }
    #[derive(Deserialize)]
    struct FullMessage {
        content: Option<Value>,
    }
    let at = line.at();
    let Some(message) = &line.message else {
        return;
    };
    let content = serde_json::from_slice::<Full>(bytes)
        .ok()
        .and_then(|full| full.message?.content)
        .unwrap_or_default();
    let blocks = content.as_array().map(Vec::as_slice).unwrap_or_default();
    if &*line.kind == "assistant" {
        let kind = if message.synthetic() {
            Kind::System
        } else {
            Kind::Assistant
        };
        conversation.say(kind, at, &message.texts().join("\n"));
        for block in blocks {
            match block["type"].as_str() {
                Some("thinking") => conversation.say(
                    Kind::Reasoning,
                    at,
                    block["thinking"].as_str().unwrap_or_default(),
                ),
                Some("tool_use") => {
                    let name = block["name"].as_str().unwrap_or("tool");
                    conversation.call(block["id"].as_str(), at, name, block["input"].to_string());
                    if EDITS.contains(&name) {
                        let input = &block["input"];
                        let file = input["file_path"]
                            .as_str()
                            .or(input["notebook_path"].as_str());
                        conversation.changed(file.map(str::to_owned));
                    }
                }
                _ => {}
            }
        }
        return;
    }
    for block in blocks.iter().filter(|block| block["type"] == "tool_result") {
        let output = match &block["content"] {
            Value::String(text) => text.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        // Claude Code's own note closing a command's output, not the
        // command's.
        let output = match output.rfind("Shell cwd was reset to ") {
            Some(at) if !output[at..].trim_end().contains('\n') => {
                output[..at].trim_end().to_owned()
            }
            _ => output,
        };
        if let Some(call) = block["tool_use_id"].as_str() {
            conversation.answer(call, output, block["is_error"].as_bool().unwrap_or(false));
        }
    }
    for text in message.texts() {
        let kind = if line.is_compact_summary {
            conversation.say(Kind::Summary, at, summarized(&text));
            continue;
        } else if line.claude_codes() {
            Kind::System
        } else {
            let said = typed(&text);
            if said.is_empty() {
                Kind::System
            } else if !asks(&said) {
                // A command of Claude Code's own, as `/clear`, asks for
                // nothing.
                conversation.say(Kind::System, at, &said);
                continue;
            } else {
                conversation.say(if subagent { Kind::Task } else { Kind::User }, at, &said);
                continue;
            }
        };
        conversation.say(kind, at, &text);
    }
}

/// Whether the log at `file` was begun in the last 10 seconds.
fn begun_lately(file: &Path) -> bool {
    std::fs::metadata(file)
        .and_then(|meta| meta.created())
        .ok()
        .and_then(|begun| begun.elapsed().ok())
        .is_some_and(|age| age < Duration::from_secs(10))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session's log of `lines` in a project folder, and its file.
    fn log(dir: &Path, name: &str, lines: &[serde_json::Value]) -> PathBuf {
        let file = dir.join("projects/-work-app").join(name);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(&file, text).unwrap();
        file
    }

    fn assistant(
        session: &str,
        message: &str,
        request: &str,
        at: &str,
        usage: serde_json::Value,
        content: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({"type": "assistant", "sessionId": session, "requestId": request, "timestamp": at,
            "message": {"id": message, "model": "claude-opus-5", "role": "assistant", "usage": usage, "content": content}})
    }

    fn usage(input: u64, output: u64) -> serde_json::Value {
        serde_json::json!({"input_tokens": input, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 30,
            "cache_creation": {"ephemeral_5m_input_tokens": 10, "ephemeral_1h_input_tokens": 20}, "output_tokens": output})
    }

    #[test]
    fn a_response_written_as_it_streams_is_one_response_at_its_most() {
        let dir = tempfile::tempdir().unwrap();
        let text = serde_json::json!([{"type": "text", "text": "Done."}]);
        let file = log(
            dir.path(),
            "s1.jsonl",
            &[
                serde_json::json!({"type": "user", "sessionId": "s1", "timestamp": "2026-09-30T09:00:00Z", "cwd": "/work/app",
                "message": {"role": "user", "content": "<command-name>/review</command-name><command-args>src</command-args>"}}),
                assistant(
                    "s1",
                    "msg_1",
                    "req_1",
                    "2026-09-30T09:00:01Z",
                    usage(5, 16),
                    text.clone(),
                ),
                assistant(
                    "s1",
                    "msg_1",
                    "req_1",
                    "2026-09-30T09:00:02Z",
                    usage(5, 900),
                    text,
                ),
                // Claude Code's own reply costs nothing.
                serde_json::json!({"type": "assistant", "sessionId": "s1", "timestamp": "2026-09-30T09:00:03Z",
                "message": {"id": "msg_2", "model": "<synthetic>", "role": "assistant", "usage": usage(0, 0), "content": []}}),
                serde_json::json!({"type": "ai-title", "sessionId": "s1", "aiTitle": "Review src"}),
            ],
        );
        let read = ClaudeCode.read(&file, "").unwrap();
        assert_eq!(read.responses.len(), 1);
        let response = &read.responses["msg_1:req_1"];
        assert_eq!(
            (response.session.as_str(), response.copy),
            ("claude-code:s1", false)
        );
        assert_eq!(
            response.tokens,
            Tokens {
                input: 5,
                cache_read: 100,
                cache_write_5m: 10,
                cache_write_1h: 20,
                output: 900,
                reasoning: 0
            }
        );
        assert_eq!(response.prompt, 135);
        let session = &read.sessions["claude-code:s1"];
        assert_eq!(session.title.as_ref().unwrap().1, "Review src");
        assert_eq!(session.cwd.as_deref(), Some("/work/app"));
        assert_eq!(
            read.said[0],
            ("claude-code:s1".to_owned(), "/review src".to_owned())
        );
        // Read on from where it stopped, nothing is read twice.
        let again = ClaudeCode.read(&file, &read.cursor).unwrap();
        assert!(again.responses.is_empty() && again.said.is_empty());
    }

    #[test]
    fn a_response_of_several_calls_is_their_sum_at_the_largest_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let mut counted = usage(0, 0);
        counted["iterations"] = serde_json::json!([usage(1000, 10), usage(3000, 20)]);
        let file = log(
            dir.path(),
            "s1.jsonl",
            &[assistant(
                "s1",
                "msg_1",
                "req_1",
                "2026-09-30T09:00:01Z",
                counted,
                serde_json::json!([]),
            )],
        );
        let response = &ClaudeCode.read(&file, "").unwrap().responses["msg_1:req_1"];
        assert_eq!(
            (
                response.tokens.input,
                response.tokens.cache_read,
                response.tokens.output
            ),
            (4000, 200, 30)
        );
        assert_eq!(response.prompt, 3130);
    }

    #[test]
    fn a_forks_copy_of_its_parents_history_is_the_parents() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("projects/-work-app/s1/subagents");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("agent-f1.meta.json"),
            r#"{"description": "Try the other parser", "isFork": true, "toolUseId": "call_1"}"#,
        )
        .unwrap();
        let line = |value: serde_json::Value| format!("{value}\n");
        let call =
            serde_json::json!([{"type": "tool_use", "id": "call_1", "name": "Agent", "input": {}}]);
        let mut own = assistant(
            "s1",
            "msg_2",
            "req_2",
            "2026-09-30T09:05:00Z",
            usage(7, 70),
            serde_json::json!([]),
        );
        own["agentId"] = serde_json::json!("f1");
        let mut copied = assistant(
            "s1",
            "msg_1",
            "req_1",
            "2026-09-30T09:00:00Z",
            usage(5, 50),
            call,
        );
        copied["agentId"] = serde_json::json!("f1");
        let file = folder.join("agent-f1.jsonl");
        std::fs::write(&file, line(copied) + &line(own)).unwrap();
        let read = ClaudeCode.read(&file, "").unwrap();
        let copy = &read.responses["msg_1:req_1"];
        assert_eq!((copy.session.as_str(), copy.copy), ("claude-code:s1", true));
        let own = &read.responses["msg_2:req_2"];
        assert_eq!((own.session.as_str(), own.copy), ("claude-code:f1", false));
        let fork = &read.sessions["claude-code:f1"];
        assert!(
            fork.parent.as_ref().is_some_and(
                |parent| parent.id == "claude-code:s1" && parent.link == Link::Subagent
            )
        );
        assert_eq!(
            fork.started,
            crate::time::millis("2026-09-30T09:05:00Z"),
            "the copy's times are the parent's"
        );
    }

    #[test]
    fn a_line_naming_another_session_yields_to_its_own_report_where_there_is_one() {
        let dir = tempfile::tempdir().unwrap();
        // A continuation's copy of s1's response, and after `/clear`, a
        // response of its own, both naming s1.
        let mut copied = assistant(
            "s2",
            "msg_1",
            "req_1",
            "2026-09-30T09:00:00Z",
            usage(5, 50),
            serde_json::json!([]),
        );
        copied["session_id"] = serde_json::json!("s1");
        let mut own = assistant(
            "s2",
            "msg_2",
            "req_2",
            "2026-09-30T09:10:00Z",
            usage(5, 50),
            serde_json::json!([]),
        );
        own["session_id"] = serde_json::json!("s1");
        let file = log(dir.path(), "s2.jsonl", &[copied.clone(), own]);
        let new = ClaudeCode.read(&file, "").unwrap();
        for response in new.responses.values() {
            assert_eq!(
                (response.session.as_str(), response.copy),
                ("claude-code:s2", true)
            );
        }
        let mut original = copied;
        original["sessionId"] = serde_json::json!("s1");
        original.as_object_mut().unwrap().remove("session_id");
        let old = log(
            dir.path(),
            "s1.jsonl",
            &[
                original,
                serde_json::json!({"type": "continued-in", "sessionId": "s1", "continuedInSessionId": "s2"}),
            ],
        );
        let old = ClaudeCode.read(&old, "").unwrap();
        let mut response = new.responses["msg_1:req_1"].clone();
        response.merge(old.responses["msg_1:req_1"].clone());
        assert_eq!(response.session, "claude-code:s1", "s1's own report wins");
        assert_eq!(new.responses["msg_2:req_2"].session, "claude-code:s2");
        assert!(
            old.sessions["claude-code:s2"]
                .parent
                .as_ref()
                .is_some_and(|parent| parent.link == Link::Continuation)
        );
    }

    #[test]
    fn a_background_subagents_report_is_its_calls_output() {
        let dir = tempfile::tempdir().unwrap();
        let hand_back = "<agent-message from=\"a7\">\n[Subagent hand-back] The text below is the final report. The report follows:\n  ## Done\n  \n  The parser is fixed.\n</agent-message>";
        let file = log(
            dir.path(),
            "s1.jsonl",
            &[
                assistant(
                    "s1",
                    "m1",
                    "r1",
                    "2026-09-30T09:00:00Z",
                    usage(10, 5),
                    serde_json::json!([{"type": "tool_use", "id": "t1", "name": "Agent", "input": {"description": "Fix the parser"}}]),
                ),
                serde_json::json!({"type": "user", "uuid": "u1", "sessionId": "s1", "timestamp": "2026-09-30T09:00:01Z",
                    "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1",
                        "content": [{"type": "text", "text": "Async agent launched successfully.\nagentId: a7 (internal ID)"}]}]}}),
                serde_json::json!({"type": "attachment", "uuid": "a1", "sessionId": "s1", "timestamp": "2026-09-30T09:05:00Z",
                    "attachment": {"type": "queued_command", "commandMode": "prompt", "isMeta": true, "prompt": hand_back}}),
            ],
        );
        let entries = ClaudeCode.transcript("s1", &[file]).unwrap();
        assert_eq!(entries.len(), 1);
        let call = entries[0].tool.as_ref().unwrap();
        assert_eq!(
            call.output.as_deref(),
            Some("## Done\n\nThe parser is fixed.")
        );
    }

    #[test]
    fn what_the_person_typed_mid_turn_is_read_and_a_command_titles_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let line = serde_json::json!({"type": "user", "uuid": "u2", "sessionId": "s1", "timestamp": "2026-09-30T09:00:02Z",
            "message": {"role": "user", "content": "Fix <pasted_content id=\"a1\">the parser</pasted_content id=\"a1\">."}});
        let file = log(
            dir.path(),
            "s1.jsonl",
            &[
                serde_json::json!({"type": "user", "uuid": "u1", "sessionId": "s1", "timestamp": "2026-09-30T09:00:00Z",
                    "message": {"role": "user", "content": "<command-name>/clear</command-name><command-args></command-args>"}}),
                line.clone(),
                line,
                serde_json::json!({"type": "attachment", "uuid": "a1", "sessionId": "s1", "timestamp": "2026-09-30T09:00:09Z",
                    "attachment": {"type": "queued_command", "commandMode": "prompt", "prompt": "Also the tests."}}),
                serde_json::json!({"type": "attachment", "uuid": "a2", "sessionId": "s1", "timestamp": "2026-09-30T09:00:10Z",
                    "attachment": {"type": "queued_command", "commandMode": "task-notification", "prompt": "<task-notification>done</task-notification>"}}),
                serde_json::json!({"type": "attachment", "uuid": "a3", "sessionId": "s1", "timestamp": "2026-09-30T09:00:11Z",
                    "attachment": {"type": "queued_command", "commandMode": "prompt", "isMeta": true,
                        "prompt": "<agent-message from=\"a1\">Done: the parser is fixed.</agent-message>"}}),
            ],
        );
        let read = ClaudeCode.read(&file, "").unwrap();
        assert_eq!(
            read.sessions["claude-code:s1"].title.as_ref().unwrap().1,
            "Fix the parser."
        );
        assert!(read.said.iter().any(|(_, text)| text == "Also the tests."));
        // A subagent's hand-back, queued as a prompt, isn't the person's.
        assert!(
            !read
                .said
                .iter()
                .any(|(_, text)| text.contains("agent-message"))
        );
        let entries = ClaudeCode.transcript("s1", &[file]).unwrap();
        let said: Vec<(Kind, &str)> = entries
            .iter()
            .map(|entry| (entry.kind, entry.text.get(..15).unwrap_or(&entry.text)))
            .collect();
        assert_eq!(
            said,
            [
                (Kind::System, "/clear"),
                (Kind::User, "Fix the parser."),
                (Kind::User, "Also the tests."),
                (Kind::System, "<agent-message ")
            ]
        );
    }

    #[test]
    fn a_conversation_folds_results_into_calls_and_marks_what_isnt_the_persons() {
        let dir = tempfile::tempdir().unwrap();
        let file = log(
            dir.path(),
            "s1.jsonl",
            &[
                serde_json::json!({"type": "user", "sessionId": "s1", "message": {"role": "user", "content": "Fix it. <system-reminder>be brief</system-reminder>"}}),
                assistant(
                    "s1",
                    "msg_1",
                    "req_1",
                    "2026-09-30T09:00:01Z",
                    usage(5, 9),
                    serde_json::json!([{"type": "thinking", "thinking": "Run the tests."}, {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "cargo test"}}]),
                ),
                serde_json::json!({"type": "user", "sessionId": "s1", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "1 failed", "is_error": true}]}}),
                serde_json::json!({"type": "user", "sessionId": "s1", "isCompactSummary": true, "message": {"role": "user", "content":
                    format!("<notice/>\nA notice.\n{CONTINUED}\n\nSummary:\nTests fail.\n\nIf you need specific details from before compaction, read it.\nContinue the conversation from where it left off.")}}),
            ],
        );
        let entries = ClaudeCode.transcript("s1", &[file]).unwrap();
        let kinds: Vec<Kind> = entries.iter().map(|entry| entry.kind).collect();
        assert_eq!(
            kinds,
            [Kind::User, Kind::Reasoning, Kind::Tool, Kind::Summary]
        );
        assert_eq!(entries[0].text, "Fix it.");
        assert_eq!(entries[3].text, "Tests fail.");
        let tool = entries[2].tool.as_ref().unwrap();
        assert_eq!(
            (tool.name.as_str(), tool.output.as_deref(), tool.failed),
            ("Bash", Some("1 failed"), true)
        );
    }
}
