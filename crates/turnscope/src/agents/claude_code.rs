//! Claude Code.
//!
//! A session is a JSON Lines log, `~/.claude/projects/<project>/<session>.jsonl`,
//! and each of its subagents another,
//! `<project>/<session>/subagents/agent-<id>.jsonl`, described in
//! `agent-<id>.meta.json` beside it; a subagent's lines carry its session's
//! `sessionId` and its own `agentId`. A second account's folder is
//! `~/.claude-<name>`, used through `CLAUDE_CONFIG_DIR`, once it holds
//! `.claude.json`.
//!
//! Measured on this Mac's history, September and October 2026:
//!
//! - **Usage.** A response is written a line per content block, each with its
//!   usage as it stood then (119,715 lines held 53,928 responses), so its
//!   reports, `message.id` with `requestId`, merge to the largest.
//!   `input_tokens` leaves out the cache. A response of several model calls
//!   lists them in `usage.iterations`, and its own counts are their sum or
//!   zero.
//! - **Forks.** A fork's log opens with a copy of its parent's history, under
//!   the fork's `agentId`, through the call that started it (`toolUseId` in
//!   its description). A fork of the session itself opens with
//!   `fork-context-ref`, naming the parent in `parentSessionId`, and a copy of
//!   the parent's last response, ended by the fork's first prompt. A copy's
//!   responses are the parent's, and nothing else in it is the fork's.
//! - **Continuations.** A line can name another session in `session_id` than
//!   in `sessionId`. A session carried on in a new log opens with what the old
//!   one held since it last compacted, under the old one's name; but after
//!   `/clear`, every line names the session the process began as (120 of 123
//!   such logs held only responses of their own). So such a line's response
//!   is its log's, as a copy: the named session's own report wins.
//! - **Not read.** `cost-state` counts calls the log doesn't hold, about 1–2%
//!   of cost. Logs older than `cleanupPeriodDays` are deleted; what was read
//!   of them stays.

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
/// isn't titled by them. 30 sessions measured had been titled `/clear`.
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
    if let Some(name) = super::inner(text, "command-name") {
        let name = name.trim();
        let args = super::inner(text, "command-args")
            .map(str::trim)
            .unwrap_or_default();
        return if args.is_empty() {
            name.to_owned()
        } else {
            format!("{name} {args}")
        };
    }
    let mut text = super::without_tags(text, INJECTED);
    let paste = |text: &str| {
        super::mark(text, "<", "pasted_content")
            .or_else(|| super::mark(text, "</", "pasted_content"))
    };
    while let Some((start, end)) = paste(&text) {
        text.replace_range(start..end, "");
    }
    text.trim().to_owned()
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
    #[serde(borrow)]
    usage: Option<Usage<'a>>,
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

/// The input and output counts are required, so a new format shows in
/// `doctor` as lines skipped. A gateway may leave out the cache counts or
/// their split (not measured), which are then 0.
#[derive(Clone, Copy, Deserialize)]
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

#[derive(Clone, Copy, Deserialize, Default)]
#[serde(default)]
struct CacheCreation {
    ephemeral_5m_input_tokens: u64,
    ephemeral_1h_input_tokens: u64,
}

impl Counts {
    /// Its tokens, but for reasoning. Cache writes without a split that
    /// splits any are taken as kept for 5 minutes, the cache's default.
    fn tokens(&self) -> Tokens {
        let split = self
            .cache_creation
            .as_ref()
            .filter(|split| split.ephemeral_5m_input_tokens + split.ephemeral_1h_input_tokens > 0);
        Tokens {
            input: self.input_tokens,
            cache_read: self.cache_read_input_tokens,
            cache_write_5m: split.map_or(self.cache_creation_input_tokens, |split| {
                split.ephemeral_5m_input_tokens
            }),
            cache_write_1h: split.map_or(0, |split| split.ephemeral_1h_input_tokens),
            output: self.output_tokens,
            reasoning: 0,
        }
    }
}

/// `Counts`' fields again, over `#[serde(flatten)]`, which would buffer the
/// whole object.
#[derive(Deserialize)]
struct Usage<'a> {
    input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    cache_creation: Option<CacheCreation>,
    output_tokens: u64,
    iterations: Option<Vec<Counts>>,
    output_tokens_details: Option<OutputDetails>,
    server_tool_use: Option<ServerTools>,
    #[serde(borrow)]
    speed: Option<Cow<'a, str>>,
}

impl Usage<'_> {
    fn counts(&self) -> Counts {
        Counts {
            input_tokens: self.input_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
            cache_creation: self.cache_creation,
            output_tokens: self.output_tokens,
        }
    }
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
        let through = self.tool_use_id.clone().filter(|_| self.is_fork)?;
        Some(Copy {
            parent: parent.to_owned(),
            through: Some(through),
        })
    }
}

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

fn is_own_folder(folder: &Path, home: &Path) -> bool {
    folder == home.join(".claude")
}

fn settings(folder: &Path, home: &Path) -> PathBuf {
    // Its own folder's settings are beside it; another's are inside it.
    if is_own_folder(folder, home) {
        home
    } else {
        folder
    }
    .join(".claude.json")
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
        for project in super::entries(&folder.join("projects")) {
            for entry in std::fs::read_dir(&project).into_iter().flatten().flatten() {
                let path = entry.path();
                if jsonl(&path) {
                    files.push(path);
                } else if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    files.extend(super::entries(&path.join("subagents")).filter(jsonl));
                }
            }
        }
        files
    }

    fn read(&self, file: &Path, cursor: &str) -> Result<Read> {
        let (session, agent) = named(file)
            .ok_or_else(|| Error::Failed(format!("{} isn't a Claude Code log", file.display())))?;
        let own_native = agent.as_deref().unwrap_or(&session);
        let own = INFO.session(own_native);
        let saved = serde_json::from_str::<Cursor>(cursor);
        let (start, mut cursor) = (saved.is_err(), saved.unwrap_or_default());
        let mut read = Read::default();

        if start && let Some(agent) = &agent {
            let meta = meta(file, agent);
            // Only the description tells a fork from another subagent, and
            // Claude Code may write it up to 1.5 s after the log: a new log
            // fails its read until it's there whole, to be read again.
            if meta.is_none() && begun_lately(file) {
                return Err(Error::Failed(format!(
                    "{} waits for its description",
                    file.display()
                )));
            }
            let meta = meta.unwrap_or_default();
            let parent = meta.parent(&session);
            cursor.copy = meta.copy(&parent);
            // A subagent forked with its parent's context is still a subagent.
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
            read_line(&line, own_native, &own, &mut cursor, &mut read);
        })?;
        read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
        Ok(read)
    }

    fn logins(&self, folder: &Path, home: &Path) -> Result<Vec<Login>> {
        // With no account in its settings, there's nothing to read the
        // login for, so a signed-out folder asks the Keychain nothing.
        let Some(identity) = identity(&settings(folder, home))? else {
            return Ok(vec![Login {
                provider: "anthropic".to_owned(),
                credential: None,
            }]);
        };
        // Its own folder's login is the item `Claude Code-credentials`;
        // another folder's adds the first 8 hex digits of the SHA-256 of its
        // path (Claude Code 2.1.284's code, read 2026-09-29).
        let item = if is_own_folder(folder, home) {
            "Claude Code-credentials".to_owned()
        } else {
            let hex = crate::sha256_hex(folder.as_os_str().as_encoded_bytes(), 4);
            format!("Claude Code-credentials-{hex}")
        };
        let keychain = super::keychain(home, &item, None, |kept| {
            serde_json::from_slice::<Value>(kept).map_err(|error| error.to_string())
        })
        .map(|kept| kept.and_then(|kept| credential(&kept, identity.clone())));
        let file = super::json_file(&folder.join(".credentials.json"))
            .map(|kept| kept.and_then(|kept| credential(&kept, identity.clone())));
        Ok(vec![Login {
            provider: "anthropic".to_owned(),
            credential: super::fresher(keychain, file)?,
        }])
    }

    fn mcp_command(&self, folder: &Path, home: &Path) -> Option<String> {
        super::json_mcp_command(&settings(folder, home), "/mcpServers/turnscope/command")
    }

    fn login_file(&self, path: &Path) -> bool {
        path.file_name() == Some(".credentials.json".as_ref())
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

fn read_line(line: &Line, own_native: &str, own: &str, cursor: &mut Cursor, read: &mut Read) {
    let copied = (*line.kind == *"assistant")
        .then(|| cursor.copy.as_ref().map(|copy| copy.parent.clone()))
        .flatten();
    if in_copy(line, cursor) {
        if let Some(parent) = copied {
            respond(line, &parent, true, read);
        }
        return;
    }
    // A subagent's lines name the session it ran in; its agent id says whose
    // they are.
    let owner = || -> Cow<'_, str> {
        match (line.agent_id.as_deref(), line.session_id.as_deref()) {
            (Some(agent), _) | (None, Some(agent)) if !agent.is_empty() && agent != own_native => {
                Cow::Owned(INFO.session(agent))
            }
            _ => Cow::Borrowed(own),
        }
    };
    match &*line.kind {
        "user" => {
            // The first thing the person asked titles the session. Once it
            // has, what they say isn't read.
            if !cursor.titled && !line.claude_codes() {
                let content = line.message.as_ref().and_then(|message| message.content);
                let texts = content.map(super::texts).unwrap_or_default();
                let asked = texts.iter().map(|text| typed(text)).find(|text| asks(text));
                if let Some(asked) = asked {
                    read.session(&owner()).title(Title::Prompt, &asked);
                    cursor.titled = true;
                }
            }
        }
        "assistant" => {
            let named = line.named.as_deref();
            let copy = named.is_some_and(|named| Some(named) != line.session_id.as_deref());
            respond(line, &owner(), copy, read);
        }
        "attachment" | "system" => {}
        "ai-title" => {
            if let Some(title) = &line.ai_title {
                read.session(&owner()).title(Title::Generated, title);
            }
            return;
        }
        "agent-name" | "custom-title" => {
            if let Some(name) = line.agent_name.as_ref().or(line.custom_title.as_ref()) {
                read.session(&owner()).title(Title::Named, name);
            }
            return;
        }
        _ => return,
    }
    if let Some(cwd) = line.cwd.as_deref().filter(|cwd| !cwd.is_empty()) {
        let session = read.session(&owner());
        if session.cwd.is_none() {
            session.cwd = Some(cwd.to_owned());
        }
    }
}

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
    let single = [usage.counts()];
    let calls = usage
        .iterations
        .as_deref()
        .filter(|calls| !calls.is_empty())
        .unwrap_or(&single);
    let (mut tokens, mut prompt) = (Tokens::default(), 0);
    for call in calls.iter().map(Counts::tokens) {
        prompt =
            prompt.max(call.input + call.cache_read + call.cache_write_5m + call.cache_write_1h);
        tokens = tokens.zip(call, |a, b| a + b);
    }
    tokens.reasoning = usage
        .output_tokens_details
        .as_ref()
        .and_then(|details| details.thinking_tokens)
        .unwrap_or_default();
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
        tokens,
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

/// Whose sign-in `settings` names: a plan belongs to an organization, and one
/// account can be in several.
fn identity(settings: &Path) -> Result<Option<(String, Option<String>)>> {
    let config = super::json_file(settings)?.unwrap_or_default();
    Ok(
        match (
            super::str_at(&config, "/oauthAccount/accountUuid"),
            super::str_at(&config, "/oauthAccount/organizationUuid"),
        ) {
            (Some(account), Some(organization)) => Some((
                format!("{account}:{organization}"),
                super::str_at(&config, "/oauthAccount/emailAddress").map(str::to_owned),
            )),
            _ => None,
        },
    )
}

/// The sign-in `kept` holds, the Keychain item's JSON or `.credentials.json`'s,
/// with its `identity`.
fn credential(kept: &Value, identity: (String, Option<String>)) -> Option<Credential> {
    let token = super::str_at(kept, "/claudeAiOauth/accessToken")?;
    Some(Credential {
        provider: "anthropic",
        secret: Secret::new(token),
        key: false,
        expires: kept
            .pointer("/claudeAiOauth/expiresAt")
            .and_then(Value::as_i64),
        identity: Some(identity),
        plan: super::str_at(kept, "/claudeAiOauth/subscriptionType").map(str::to_owned),
        refresh: None,
    })
}

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
    fn a_fork_of_the_session_opens_with_a_copy_of_its_parents_last_response() {
        let dir = tempfile::tempdir().unwrap();
        let fork = json!({"type": "fork-context-ref", "sessionId": "s2", "parentSessionId": "s1"});
        let prompt = |at: &str, text: &str| {
            json!({"type": "user", "sessionId": "s2", "timestamp": at,
                "message": {"role": "user", "content": text}})
        };
        let file = log(
            dir.path(),
            "s2.jsonl",
            &[
                fork.clone(),
                assistant("s2", 1, "2026-09-30T09:00:00Z", usage(5, 50), json!([])),
                prompt("2026-09-30T09:05:00Z", "Try the other parser."),
                assistant("s2", 2, "2026-09-30T09:05:09Z", usage(7, 70), json!([])),
                // Once the person has asked something, a fork's mark copies
                // nothing.
                fork,
                assistant("s2", 3, "2026-09-30T09:06:00Z", usage(9, 90), json!([])),
            ],
        );
        let read = ClaudeCode.read(&file, "").unwrap();
        let mut whose: Vec<(&str, &str, bool)> = read
            .responses
            .values()
            .map(|response| {
                (
                    response.id.as_str(),
                    response.session.as_str(),
                    response.copy,
                )
            })
            .collect();
        whose.sort();
        assert_eq!(
            whose,
            [
                ("msg_1:req_1", "claude-code:s1", true),
                ("msg_2:req_2", "claude-code:s2", false),
                ("msg_3:req_3", "claude-code:s2", false),
            ]
        );
        let session = &read.sessions["claude-code:s2"];
        assert_eq!(session.title.as_ref().unwrap().1, "Try the other parser.");
        // A fork is a session of its own.
        assert!(session.parent.is_none());
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
    fn cache_counts_may_be_left_out_and_the_output_count_may_not() {
        let dir = tempfile::tempdir().unwrap();
        // No split, and one that splits none of the writes: they're kept 5
        // minutes. Without its output count, the line is skipped.
        for (name, usage, tokens) in [
            (
                "s1.jsonl",
                json!({"input_tokens": 5, "cache_creation_input_tokens": 30, "output_tokens": 16}),
                Some((5, 0, 30, 0, 16)),
            ),
            (
                "s2.jsonl",
                json!({"input_tokens": 5, "cache_creation_input_tokens": 30, "cache_creation": {},
                    "output_tokens": 16}),
                Some((5, 0, 30, 0, 16)),
            ),
            (
                "s3.jsonl",
                json!({"input_tokens": 5, "cache_read_input_tokens": 100}),
                None,
            ),
        ] {
            let file = log(
                dir.path(),
                name,
                &[assistant("s1", 1, "2026-09-30T09:00:00Z", usage, json!([]))],
            );
            let read = ClaudeCode.read(&file, "").unwrap();
            let counted = read.responses.get("msg_1:req_1").map(|response| {
                let tokens = &response.tokens;
                (
                    tokens.input,
                    tokens.cache_read,
                    tokens.cache_write_5m,
                    tokens.cache_write_1h,
                    tokens.output,
                )
            });
            let skipped = u64::from(tokens.is_none());
            assert_eq!((counted, read.skipped), (tokens, skipped), "{name}");
        }
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
        let old = log(dir.path(), "s1.jsonl", &[original]);
        let old = ClaudeCode.read(&old, "").unwrap();
        let mut response = new.responses["msg_1:req_1"].clone();
        response.merge(old.responses["msg_1:req_1"].clone());
        assert_eq!(response.session, "claude-code:s1", "s1's own report wins");
        assert_eq!(new.responses["msg_2:req_2"].session, "claude-code:s2");
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
                // What Claude Code added is taken out, and the paste's marks.
                json!({"type": "user", "sessionId": "s1", "timestamp": "2026-09-30T09:00:02Z",
                    "message": {"role": "user", "content": "<system-reminder>be brief</system-reminder>\nFix <pasted_content id=\"a1\">the parser</pasted_content id=\"a1\">."}}),
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
        let review = typed("<command-name>/review</command-name><command-args>src</command-args>");
        assert_eq!(review, "/review src");
        assert!(asks(&review));
    }

    #[test]
    fn logins_reads_settings_and_credentials_file_for_main_and_extra_folders() {
        let home = tempfile::tempdir().unwrap();
        let folder = home.path().join(".claude");
        let work = home.path().join(".claude-work");
        std::fs::create_dir(&folder).unwrap();
        std::fs::create_dir(&work).unwrap();
        let config = home.path().join(".claude.json");

        // Half-written `.claude.json` fails before touching the Keychain.
        std::fs::write(&config, "{").unwrap();
        assert!(ClaudeCode.logins(&folder, home.path()).is_err());

        // Missing org or empty settings counts as signed out without Keychain.
        for incomplete in [
            json!({}),
            json!({"oauthAccount": {"accountUuid": "a1", "emailAddress": "me@example.com"}}),
        ] {
            std::fs::write(&config, incomplete.to_string()).unwrap();
            let logins = ClaudeCode.logins(&folder, home.path()).unwrap();
            assert_eq!(logins.len(), 1);
            assert_eq!(logins[0].provider, "anthropic");
            assert!(logins[0].credential.is_none());
        }

        // Valid settings + `.credentials.json` fallback (main folder beside,
        // extra folder inside).
        let account = json!({"oauthAccount": {"accountUuid": "a1", "organizationUuid": "o1", "emailAddress": "me@example.com"}});
        let creds = json!({"claudeAiOauth": {"accessToken": "file-tok", "expiresAt": 1_790_000_000_000_i64, "subscriptionType": "max"}});
        std::fs::write(&config, account.to_string()).unwrap();
        std::fs::write(folder.join(".credentials.json"), creds.to_string()).unwrap();
        std::fs::write(work.join(".claude.json"), account.to_string()).unwrap();
        std::fs::write(work.join(".credentials.json"), creds.to_string()).unwrap();
        for dir in [&folder, &work] {
            let logins = ClaudeCode.logins(dir, home.path()).unwrap();
            let cred = logins[0].credential.as_ref().unwrap();
            assert_eq!(cred.secret.expose(), "file-tok");
            assert_eq!(
                cred.identity.as_ref(),
                Some(&("a1:o1".to_owned(), Some("me@example.com".to_owned())))
            );
            assert_eq!(
                (cred.plan.as_deref(), cred.expires, cred.key),
                (Some("max"), Some(1_790_000_000_000), false)
            );
        }

        // Empty access token or empty JSON counts as signed out.
        for signed_out in [json!({}), json!({"claudeAiOauth": {"accessToken": ""}})] {
            std::fs::write(folder.join(".credentials.json"), signed_out.to_string()).unwrap();
            assert!(
                ClaudeCode.logins(&folder, home.path()).unwrap()[0]
                    .credential
                    .is_none()
            );
        }

        // A token with no expiry is still read.
        std::fs::write(
            folder.join(".credentials.json"),
            json!({"claudeAiOauth": {"accessToken": "tok-1"}}).to_string(),
        )
        .unwrap();
        let logins = ClaudeCode.logins(&folder, home.path()).unwrap();
        assert_eq!(logins[0].credential.as_ref().unwrap().expires, None);
    }
}
