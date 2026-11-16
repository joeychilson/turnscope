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
//! The usage is what the API answered; a backend other than Anthropic's, as
//! a gateway, can leave out the cache counts or their `cache_creation` split
//! (not measured here). A cache count left out is 0, and cache writes
//! without the split are taken as kept for 5 minutes, the cache's default.
//! The input and output counts stay required: a line without them is
//! skipped, and `doctor` says so, which is how a new format shows.
//!
//! **Forks.** A fork's log opens with a copy of its parent's history, through
//! the line making the call that started it (`toolUseId` in its description),
//! under the fork's `agentId`. A fork of the session itself opens with
//! `fork-context-ref`, naming the parent in `parentSessionId`, and a copy of
//! the parent's last response, ended by the fork's first prompt. The copy is
//! the parent's: its responses are marked copies of the parent's, and
//! nothing else in it is the fork's. Only the description tells a fork from
//! another subagent, and Claude Code may write it up to 1.5 s after the log,
//! so a new log waits for it, and for it to parse, as one read while it's
//! written won't.
//!
//! **Continuations and `/clear`.** A line can name, in `session_id`, another
//! session than its `sessionId`. A session carried on in a new log (the old
//! log ends with `continued-in`) opens with what the old one held since it
//! last compacted, under the old session's name; but after `/clear`, every
//! line goes on naming the session the process began as: 120 of the 123 logs
//! naming another session held only responses of their own. So such a line's
//! response is its log's, as a copy: where the session it names holds it
//! too, that session's own report wins.
//!
//! **Titles.** A session neither Claude Code nor the person named is titled
//! by the first thing the person asked, without what Claude Code added to
//! it. Pastes are marked `<pasted_content …>`, and a command of Claude Code's
//! own, such as `/clear`, asks for no work, so it titles nothing: 30 sessions
//! had been titled `/clear`.
//!
//! **Not read.** `cost-state` records count calls the log doesn't hold,
//! about 1–2% of cost; usage is counted from the log alone.
//! Claude Code deletes logs older than `cleanupPeriodDays`; what was read of
//! them stays.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use super::{Agent, Credential, Info, Login, Read, Response, Secret, Title, Tokens};
use crate::{Error, Result};

pub struct ClaudeCode;

static INFO: Info = Info {
    id: "claude-code",
    name: "Claude Code",
    command: "claude",
    app: None,
    mcp_add: &["mcp", "add", "--scope", "user", "turnscope", "--"],
    mcp_remove: Some(&["mcp", "remove", "--scope", "user", "turnscope"]),
    folder_var: Some("CLAUDE_CONFIG_DIR"),
    charges: false,
    session_var: Some("CLAUDE_CODE_SESSION_ID"),
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
}

#[derive(Serialize, Deserialize)]
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
    #[serde(rename = "parentSessionId", borrow)]
    parent_session_id: Option<Cow<'a, str>>,
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
    #[serde(default)]
    cache_read_input_tokens: u64,
    /// Every cache write, which `cache_creation` splits by how long the
    /// cache keeps it.
    #[serde(default)]
    cache_creation_input_tokens: u64,
    cache_creation: Option<CacheCreation>,
    output_tokens: u64,
}

#[derive(Deserialize, Clone, Copy, Default)]
#[serde(default)]
struct CacheCreation {
    ephemeral_5m_input_tokens: u64,
    ephemeral_1h_input_tokens: u64,
}

impl Counts {
    fn prompt(&self) -> u64 {
        let writes = self.writes();
        self.input_tokens
            + self.cache_read_input_tokens
            + writes.ephemeral_5m_input_tokens
            + writes.ephemeral_1h_input_tokens
    }

    /// The cache writes, by how long they're kept. Without a split that
    /// splits any, every write is taken as kept for 5 minutes, the cache's
    /// default.
    fn writes(&self) -> CacheCreation {
        self.cache_creation
            .filter(|split| split.ephemeral_5m_input_tokens + split.ephemeral_1h_input_tokens > 0)
            .unwrap_or(CacheCreation {
                ephemeral_5m_input_tokens: self.cache_creation_input_tokens,
                ephemeral_1h_input_tokens: 0,
            })
    }
}

#[derive(Deserialize)]
struct Usage {
    #[serde(flatten)]
    counts: Counts,
    iterations: Option<Vec<Counts>>,
    output_tokens_details: Option<OutputDetails>,
    server_tool_use: Option<ServerTools>,
    speed: Option<String>,
}

#[derive(Deserialize)]
struct OutputDetails {
    thinking_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct ServerTools {
    web_search_requests: Option<u64>,
}

/// A subagent's description, `agent-<id>.meta.json`.
#[derive(Deserialize, Default)]
struct Meta {
    description: Option<String>,
    #[serde(rename = "parentAgentId")]
    parent_agent_id: Option<String>,
    #[serde(rename = "isFork", default)]
    is_fork: bool,
    #[serde(rename = "toolUseId")]
    tool_use_id: Option<String>,
}

impl Meta {
    /// The subagent that started this one, or when none is named, its
    /// session, `session`.
    fn parent(&self, session: &str) -> String {
        INFO.session(
            self.parent_agent_id
                .as_deref()
                .filter(|parent| !parent.is_empty())
                .unwrap_or(session),
        )
    }

    /// A fork's copy of `parent`'s history, through the call that started it.
    fn copy(&self, parent: &str) -> Option<Copy> {
        self.is_fork.then_some(())?;
        Some(Copy {
            parent: parent.to_owned(),
            through: Some(self.tool_use_id.clone()?),
        })
    }
}

/// The description of the subagent `agent` whose log is `file`, if it's
/// there and parses.
fn meta(file: &Path, agent: &str) -> Option<Meta> {
    let bytes = std::fs::read(file.with_file_name(format!("agent-{agent}.meta.json"))).ok()?;
    serde_json::from_slice(&bytes).ok()
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

/// The settings file, `.claude.json`, of the folder `folder` in `home`.
fn settings(folder: &Path, home: &Path) -> PathBuf {
    // Its own folder's settings are beside it; another's are inside it.
    let own = folder == home.join(".claude");
    if own { home } else { folder }.join(".claude.json")
}

impl Agent for ClaudeCode {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn folders(&self, home: &Path) -> Vec<PathBuf> {
        super::own_and_found(home, ".claude", ".claude-", ".claude.json")
    }

    fn files(&self, folder: &Path) -> Vec<PathBuf> {
        let jsonl = |path: &PathBuf| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        };
        let mut files = Vec::new();
        let projects = super::entries(&folder.join("projects"));
        for path in projects.flat_map(|project| super::entries(&project)) {
            if jsonl(&path) {
                files.push(path);
            } else if path.is_dir() {
                files.extend(super::entries(&path.join("subagents")).filter(jsonl));
            }
        }
        files
    }

    fn read(&self, file: &Path, cursor: &str) -> Result<Read> {
        let (session, agent) = named(file)
            .ok_or_else(|| Error::Failed(format!("{} isn't a Claude Code log", file.display())))?;
        let own = INFO.session(agent.as_deref().unwrap_or(&session));
        // Without a cursor a read saved, a log is read from its start.
        let saved = serde_json::from_str::<Cursor>(cursor);
        let (start, mut cursor) = (saved.is_err(), saved.unwrap_or_default());
        let mut read = Read::default();

        if start && let Some(agent) = &agent {
            let meta = meta(file, agent);
            // Read without its description, a fork's copy would read as the
            // fork's own, so a new log waits for it, and for one read while
            // Claude Code was still writing it, which won't parse. An error,
            // so it's tried again at the next read.
            if meta.is_none() && begun_lately(file) {
                return Err(Error::Failed(format!(
                    "{} waits for its description",
                    file.display()
                )));
            }
            let meta = meta.unwrap_or_default();
            let parent = meta.parent(&session);
            cursor.copy = meta.copy(&parent);
            // A fork here is a subagent begun with a copy of its parent's
            // context: it's still the parent's subagent.
            let described = read.session(&own);
            described.parent = Some(parent);
            if let Some(task) = meta.description {
                described.title(Title::Task, &task);
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

    fn logins(&self, folder: &Path, home: &Path) -> Result<Vec<Login>> {
        // Its own folder's login is the item `Claude Code-credentials`;
        // another folder's adds the first 8 hex digits of the SHA-256 of its
        // path (Claude Code 2.1.284's code, read 2026-09-29).
        let own = folder == home.join(".claude");
        let item = if own {
            "Claude Code-credentials".to_owned()
        } else {
            let hex = crate::sha256_hex(folder.as_os_str().as_encoded_bytes(), 4);
            format!("Claude Code-credentials-{hex}")
        };
        let kept: Value = match super::keychain(home, &item, None)? {
            Some(kept) => serde_json::from_slice(&kept).map_err(|error| {
                Error::Failed(format!("reading the Keychain item {item}: {error}"))
            })?,
            None => Value::Null,
        };
        let token = kept.pointer("/claudeAiOauth/accessToken");
        let credential = match token.and_then(Value::as_str) {
            Some(token) => {
                // Whose it is: a plan belongs to an organization, and one
                // account can be in several.
                let config = super::json_file(&settings(folder, home))?.unwrap_or_default();
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
            None => None,
        };
        Ok(vec![Login {
            provider: "anthropic".to_owned(),
            credential,
        }])
    }

    fn mcp_command(&self, folder: &Path, home: &Path) -> Option<String> {
        super::json_mcp_command(&settings(folder, home), "/mcpServers/turnscope/command")
    }
}

/// Whether `line` is part of a fork's copy of its parent's history, keeping
/// the cursor's `copy` and `prompted` up to date.
fn in_copy(line: &Line, cursor: &mut Cursor) -> bool {
    if let Some(copying) = cursor.copy.as_ref() {
        match (&*line.kind, &copying.through) {
            ("user", None) => cursor.copy = None,
            ("assistant", Some(call)) => {
                if line
                    .message
                    .as_ref()
                    .is_some_and(|message| message.calls(call))
                {
                    cursor.copy = None;
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
            if !cursor.prompted && cursor.copy.is_none() {
                cursor.copy = line.parent_session_id.as_deref().map(|parent| Copy {
                    parent: INFO.session(parent),
                    through: None,
                });
            }
            true
        }
        "user" => {
            cursor.prompted = true;
            false
        }
        _ => false,
    }
}

fn read_line(line: &Line, own: &str, cursor: &mut Cursor, read: &mut Read) {
    let copied = cursor.copy.as_ref().map(|copy| copy.parent.clone());
    if in_copy(line, cursor) {
        if let (Some(parent), "assistant") = (copied, &*line.kind) {
            respond(line, &parent, true, read);
        }
        return;
    }
    // A subagent's lines name the session it ran in; its agent id says whose
    // they are.
    let owner = match (&line.agent_id, &line.session_id) {
        (Some(agent), _) | (None, Some(agent)) if !agent.is_empty() => INFO.session(agent),
        _ => own.to_owned(),
    };
    match &*line.kind {
        "user" => {
            // The first thing the person asked titles the session. Once it
            // has, what they say isn't read.
            if !cursor.titled && !line.claude_codes() {
                let texts = line
                    .message
                    .as_ref()
                    .map(Message::texts)
                    .unwrap_or_default();
                let asked = texts.iter().map(|text| typed(text)).find(|text| asks(text));
                if let Some(asked) = asked {
                    read.session(&owner).title(Title::Prompt, &asked);
                    cursor.titled = true;
                }
            }
        }
        "assistant" => {
            let named = line.named.as_deref();
            let copy = named.is_some_and(|named| Some(named) != line.session_id.as_deref());
            respond(line, &owner, copy, read);
        }
        "attachment" | "system" => {}
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
        _ => return,
    }
    let session = read.session(&owner);
    if let Some(at) = line.at() {
        session.saw(at);
    }
    if session.cwd.is_none() {
        session.cwd = line.cwd.as_deref().map(str::to_owned);
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
        let sum = calls.iter().fold(Counts::default(), |sum, call| {
            let (writes, more) = (sum.writes(), call.writes());
            Counts {
                input_tokens: sum.input_tokens + call.input_tokens,
                cache_read_input_tokens: sum.cache_read_input_tokens + call.cache_read_input_tokens,
                cache_creation_input_tokens: 0,
                cache_creation: Some(CacheCreation {
                    ephemeral_5m_input_tokens: writes.ephemeral_5m_input_tokens
                        + more.ephemeral_5m_input_tokens,
                    ephemeral_1h_input_tokens: writes.ephemeral_1h_input_tokens
                        + more.ephemeral_1h_input_tokens,
                }),
                output_tokens: sum.output_tokens + call.output_tokens,
            }
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
            cache_write_5m: counts.writes().ephemeral_5m_input_tokens,
            cache_write_1h: counts.writes().ephemeral_1h_input_tokens,
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
    use serde_json::json;

    use super::*;

    /// A session's log of `lines` in a project folder, and its file.
    fn log(dir: &Path, name: &str, lines: &[Value]) -> PathBuf {
        crate::agents::jsonl(dir.join("projects/-work-app").join(name), lines)
    }

    /// The `n`th response, `msg_<n>` of the request `req_<n>`.
    fn assistant(session: &str, n: u32, at: &str, usage: Value, content: Value) -> Value {
        json!({"type": "assistant", "sessionId": session, "requestId": format!("req_{n}"), "timestamp": at,
            "message": {"id": format!("msg_{n}"), "model": "claude-opus-5", "role": "assistant", "usage": usage, "content": content}})
    }

    fn usage(input: u64, output: u64) -> Value {
        json!({"input_tokens": input, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 30,
            "cache_creation": {"ephemeral_5m_input_tokens": 10, "ephemeral_1h_input_tokens": 20}, "output_tokens": output})
    }

    #[test]
    fn a_response_written_as_it_streams_is_one_response_at_its_most() {
        let dir = tempfile::tempdir().unwrap();
        let file = log(
            dir.path(),
            "s1.jsonl",
            &[
                json!({"type": "user", "sessionId": "s1", "timestamp": "2026-09-30T09:00:00Z", "cwd": "/work/app",
                "message": {"role": "user", "content": "Review src."}}),
                assistant("s1", 1, "2026-09-30T09:00:01Z", usage(5, 16), json!([])),
                assistant("s1", 1, "2026-09-30T09:00:02Z", usage(5, 900), json!([])),
                // Claude Code's own reply costs nothing.
                json!({"type": "assistant", "sessionId": "s1", "timestamp": "2026-09-30T09:00:03Z",
                "message": {"id": "msg_2", "model": "<synthetic>", "role": "assistant", "usage": usage(0, 0), "content": []}}),
                json!({"type": "ai-title", "sessionId": "s1", "aiTitle": "Review src"}),
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
        // Read on from where it stopped, nothing is read twice.
        let again = ClaudeCode.read(&file, &read.cursor).unwrap();
        assert!(again.responses.is_empty() && again.sessions.is_empty());
    }

    #[test]
    fn a_response_of_several_calls_is_their_sum_at_the_largest_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let mut counted = usage(0, 0);
        counted["iterations"] = json!([usage(1000, 10), usage(3000, 20)]);
        let file = log(
            dir.path(),
            "s1.jsonl",
            &[assistant(
                "s1",
                1,
                "2026-09-30T09:00:01Z",
                counted,
                json!([]),
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
        let call = json!([{"type": "tool_use", "id": "call_1", "name": "Agent", "input": {}}]);
        let mut own = assistant("s1", 2, "2026-09-30T09:05:00Z", usage(7, 70), json!([]));
        own["agentId"] = json!("f1");
        let mut copied = assistant("s1", 1, "2026-09-30T09:00:00Z", usage(5, 50), call);
        copied["agentId"] = json!("f1");
        let file = log(dir.path(), "s1/subagents/agent-f1.jsonl", &[copied, own]);
        std::fs::write(
            file.with_file_name("agent-f1.meta.json"),
            r#"{"description": "Try the other parser", "isFork": true, "toolUseId": "call_1"}"#,
        )
        .unwrap();
        let read = ClaudeCode.read(&file, "").unwrap();
        let copy = &read.responses["msg_1:req_1"];
        assert_eq!((copy.session.as_str(), copy.copy), ("claude-code:s1", true));
        let own = &read.responses["msg_2:req_2"];
        assert_eq!((own.session.as_str(), own.copy), ("claude-code:f1", false));
        let fork = &read.sessions["claude-code:f1"];
        assert_eq!(fork.parent.as_deref(), Some("claude-code:s1"));
    }

    #[test]
    fn a_fork_waits_for_a_description_half_written() {
        let dir = tempfile::tempdir().unwrap();
        let file = log(dir.path(), "s1/subagents/agent-f1.jsonl", &[json!({})]);
        // As read while Claude Code is still writing it.
        std::fs::write(
            file.with_file_name("agent-f1.meta.json"),
            r#"{"description": "Try the oth"#,
        )
        .unwrap();
        assert!(ClaudeCode.read(&file, "").is_err());
    }

    #[test]
    fn usage_without_a_cache_split_counts_its_writes_as_kept_5_minutes() {
        let dir = tempfile::tempdir().unwrap();
        // No split, and one that splits none of the writes.
        for (name, usage) in [
            (
                "s1.jsonl",
                json!({"input_tokens": 5, "cache_creation_input_tokens": 30, "output_tokens": 16}),
            ),
            (
                "s2.jsonl",
                json!({"input_tokens": 5, "cache_creation_input_tokens": 30, "cache_creation": {},
                    "output_tokens": 16}),
            ),
        ] {
            let file = log(
                dir.path(),
                name,
                &[assistant("s1", 1, "2026-09-30T09:00:00Z", usage, json!([]))],
            );
            let read = ClaudeCode.read(&file, "").unwrap();
            let tokens = &read.responses["msg_1:req_1"].tokens;
            assert_eq!(
                (
                    tokens.input,
                    tokens.cache_read,
                    tokens.cache_write_5m,
                    tokens.cache_write_1h,
                    tokens.output
                ),
                (5, 0, 30, 0, 16),
                "{name}"
            );
            assert_eq!(read.skipped, 0);
        }
    }

    #[test]
    fn a_line_without_its_output_count_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let file = log(
            dir.path(),
            "s1.jsonl",
            &[assistant(
                "s1",
                1,
                "2026-09-30T09:00:00Z",
                json!({"input_tokens": 5, "cache_read_input_tokens": 100}),
                json!([]),
            )],
        );
        let read = ClaudeCode.read(&file, "").unwrap();
        assert!(read.responses.is_empty());
        assert_eq!(read.skipped, 1);
    }

    #[test]
    fn a_line_naming_another_session_yields_to_its_own_report_where_there_is_one() {
        let dir = tempfile::tempdir().unwrap();
        // A continuation's copy of s1's response, and after `/clear`, a
        // response of its own, both naming s1.
        let mut copied = assistant("s2", 1, "2026-09-30T09:00:00Z", usage(5, 50), json!([]));
        copied["session_id"] = json!("s1");
        let mut own = assistant("s2", 2, "2026-09-30T09:10:00Z", usage(5, 50), json!([]));
        own["session_id"] = json!("s1");
        let file = log(dir.path(), "s2.jsonl", &[copied.clone(), own]);
        let new = ClaudeCode.read(&file, "").unwrap();
        for response in new.responses.values() {
            assert_eq!(
                (response.session.as_str(), response.copy),
                ("claude-code:s2", true)
            );
        }
        let mut original = copied;
        original["sessionId"] = json!("s1");
        original.as_object_mut().unwrap().remove("session_id");
        let old = log(
            dir.path(),
            "s1.jsonl",
            &[
                original,
                json!({"type": "continued-in", "sessionId": "s1", "continuedInSessionId": "s2"}),
            ],
        );
        let old = ClaudeCode.read(&old, "").unwrap();
        let mut response = new.responses["msg_1:req_1"].clone();
        response.merge(old.responses["msg_1:req_1"].clone());
        assert_eq!(response.session, "claude-code:s1", "s1's own report wins");
        assert_eq!(new.responses["msg_2:req_2"].session, "claude-code:s2");
        // A continuation is a session of its own.
        assert!(!old.sessions.contains_key("claude-code:s2"));
    }

    #[test]
    fn a_command_of_claude_codes_own_titles_nothing_and_a_paste_loses_its_marks() {
        let dir = tempfile::tempdir().unwrap();
        let file = log(
            dir.path(),
            "s1.jsonl",
            &[
                json!({"type": "user", "sessionId": "s1", "timestamp": "2026-09-30T09:00:00Z",
                    "message": {"role": "user", "content": "<command-name>/clear</command-name><command-args></command-args>"}}),
                json!({"type": "user", "sessionId": "s1", "timestamp": "2026-09-30T09:00:02Z",
                    "message": {"role": "user", "content": "Fix <pasted_content id=\"a1\">the parser</pasted_content id=\"a1\">."}}),
                // The first request titles it, not one after.
                json!({"type": "user", "sessionId": "s1", "timestamp": "2026-09-30T09:00:09Z",
                    "message": {"role": "user", "content": "Also the tests."}}),
            ],
        );
        let read = ClaudeCode.read(&file, "").unwrap();
        assert_eq!(
            read.sessions["claude-code:s1"].title.as_ref().unwrap().1,
            "Fix the parser."
        );
        // A command that isn't Claude Code's own asks for work, and reads as
        // the person typed it.
        let review = typed("<command-name>/review</command-name><command-args>src</command-args>");
        assert_eq!(review, "/review src");
        assert!(asks(&review));
    }

    #[test]
    fn a_tag_the_person_typed_is_theirs_and_claude_codes_own_are_taken_out() {
        let asked =
            "Wrap the note in a <system-reminder> tag, then say how a <task-notification> differs.";
        assert_eq!(
            typed(&format!(
                "<system-reminder>be brief</system-reminder>\n{asked}"
            )),
            asked
        );
    }
}
