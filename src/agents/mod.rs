//! The coding agents Turnscope reads. Each is one module implementing
//! [`Agent`], listed in [`ALL`]; nothing outside this folder knows any
//! agent's files.

mod claude_code;
mod codex;
mod grok_build;
mod opencode;
mod pi;

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde_json::Value;

use crate::{Error, Result};

/// Every agent, in the order they're listed.
pub static ALL: &[&dyn Agent] = &[
    &claude_code::ClaudeCode,
    &codex::Codex,
    &opencode::OpenCode,
    &pi::Pi,
    &grok_build::GrokBuild,
];

pub fn by_id(id: &str) -> Option<&'static dyn Agent> {
    ALL.iter().copied().find(|agent| agent.info().id == id)
}

/// The agent with id `id`, or an error naming the agents there are.
pub fn find(id: &str) -> crate::Result<&'static dyn Agent> {
    by_id(id).ok_or_else(|| {
        let ids: Vec<&str> = ALL.iter().map(|agent| agent.info().id).collect();
        crate::Error::Usage(format!("no agent is {id}; agents are {}", ids.join(", ")))
    })
}

/// The agent's name, from its id; the id for one Turnscope doesn't read.
pub fn name(id: &str) -> &str {
    by_id(id).map_or(id, |agent| agent.info().name)
}

/// A coding agent whose history and logins Turnscope reads.
pub trait Agent: Sync {
    fn info(&self) -> &'static Info;

    /// The folders it keeps history and logins in under `home`: its own
    /// first, then any it was pointed at for a second account.
    fn folders(&self, home: &Path) -> Vec<PathBuf>;

    /// Its history files in `folder`. One that doesn't exist is passed over,
    /// so a file it may keep can be listed without looking.
    fn files(&self, folder: &Path) -> Vec<PathBuf>;

    /// What `file` holds past `cursor`, which a read returned before; an
    /// empty cursor reads from the start.
    fn read(&self, file: &Path, cursor: &str) -> Result<Read>;

    /// The conversation of the session `id` (without the agent's prefix), from
    /// the files that hold it.
    fn transcript(&self, id: &str, files: &[PathBuf]) -> Result<Vec<Entry>>;

    /// What `folder` holds for each provider it signs in to. A provider it
    /// held a login for and no longer lists is taken as signed out of.
    ///
    /// Fails when a place that keeps logins can't be read, as a file being
    /// written: taking that for no login would sign its accounts out.
    fn logins(&self, folder: &Path, home: &Path) -> Result<Vec<Login>>;

    /// The command that `folder`'s configuration runs for Turnscope's MCP
    /// server, if any. Each of its folders keeps its own.
    fn mcp_command(&self, folder: &Path, home: &Path) -> Option<String>;

    /// What a call of its tool `name` is to a handoff: work, unless the
    /// agent's tools read, ask the person or run a subagent by that name.
    fn role(&self, _name: &str) -> Role {
        Role::Work
    }
}

/// What Turnscope knows of an agent as a program.
pub struct Info {
    /// As stored and written in session ids: `claude-code`.
    pub id: &'static str,
    pub name: &'static str,
    /// Its command: run to connect it, and how its process is recognised
    /// among an MCP server's ancestors.
    pub command: &'static str,
    /// Resumes a session, `{id}` being its id.
    pub resume: &'static str,
    /// The arguments that add Turnscope's MCP server, before its command.
    pub mcp_add: &'static [&'static str],
    /// The arguments that remove it, where the agent has a command for that.
    /// `connect` runs them before adding, so a refused add can put back the
    /// server that was there.
    pub mcp_remove: Option<&'static [&'static str]>,
    /// The variable that points it at a folder other than its own.
    pub folder_var: Option<&'static str>,
    /// The cost it records is what the provider charged, not its estimate.
    pub charges: bool,
    /// What it sets in an MCP server it starts: its session's id, and the
    /// folder it works in.
    pub session_var: Option<&'static str>,
    pub cwd_var: Option<&'static str>,
}

/// What one read of a file found.
#[derive(Default)]
pub struct Read {
    /// Where the next read starts, in the reader's own terms.
    pub cursor: String,
    /// The sessions whose conversation the file holds, with what it says of
    /// them.
    pub sessions: BTreeMap<String, Session>,
    /// By id, each response's reports merged.
    pub responses: HashMap<String, Response>,
    /// What the person and the models said, by session, for search.
    pub said: Vec<(String, String)>,
    /// Lines or rows it couldn't read.
    pub skipped: u64,
}

impl Read {
    pub fn session(&mut self, id: &str) -> &mut Session {
        self.sessions.entry(id.to_owned()).or_default()
    }

    pub fn respond(&mut self, response: Response) {
        match self.responses.get_mut(&response.id) {
            Some(known) => known.merge(response),
            None => {
                self.responses.insert(response.id.clone(), response);
            }
        }
    }

    /// Note `text` as said in `session`, for search.
    pub fn say(&mut self, session: &str, text: &str) {
        let text = text.trim();
        if !text.is_empty() {
            self.said
                .push((session.to_owned(), crate::redact::redact(text)));
        }
    }
}

/// What a file says of a session. Several files and reads add up: the
/// earliest start, the latest activity, the best title.
#[derive(Default)]
pub struct Session {
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub title: Option<(Title, String)>,
    pub parent: Option<Parent>,
    pub started: Option<i64>,
    pub last: Option<i64>,
}

impl Session {
    pub fn saw(&mut self, at: i64) {
        self.started = Some(self.started.map_or(at, |started| started.min(at)));
        self.last = Some(self.last.map_or(at, |last| last.max(at)));
    }

    /// Offer a title; a better kind replaces a worse, and a later one of the
    /// same kind the earlier. Its secrets are redacted, as it's shown
    /// wherever the session is.
    pub fn title(&mut self, kind: Title, text: &str) {
        let text = crate::redact::redact(text)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !text.is_empty() && self.title.as_ref().is_none_or(|(had, _)| kind >= *had) {
            self.title = Some((kind, text));
        }
    }
}

/// Where a title came from, worst first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Title {
    /// The first thing the person asked.
    Prompt,
    /// A subagent's task, as its parent described it.
    Task,
    /// One the agent generated.
    Generated,
    /// One the person gave it.
    Named,
}

pub struct Parent {
    pub id: String,
    pub link: Link,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Subagent,
    Fork,
    /// The session carried on in a new file.
    Continuation,
}

/// One response's usage, as a file reports it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Response {
    /// Unique within the agent.
    pub id: String,
    pub session: String,
    /// A report that is, or may be, of another session's response, as a
    /// fork's copy of its parent's history: it adds to what is known of the
    /// response, and its session gives way to a report that isn't a copy.
    pub copy: bool,
    pub at: i64,
    /// As the agent names it: `anthropic`, `openrouter`, Pi's `openai-codex`.
    pub provider: String,
    pub model: String,
    pub tokens: Tokens,
    /// The largest prompt of the model calls it covers, which sets the price
    /// tier where long contexts cost more.
    pub prompt: u64,
    pub web_searches: u64,
    /// Served ahead of standard traffic at a higher rate, as fast mode is.
    pub priority: bool,
    /// What the agent says it cost.
    pub cost: Option<f64>,
}

impl Response {
    /// Fold in another report of the same response, as a line written as it
    /// streamed, or a copy in another file: the most of each count, the
    /// latest time, the greater model and provider by name, and as its
    /// session, its own report's over a copy's, then the lesser id. Every
    /// choice is the same in any order, so files read in any order agree.
    pub fn merge(&mut self, other: Response) {
        if (other.copy, &other.session) < (self.copy, &self.session) {
            self.copy = other.copy;
            self.session = other.session;
        }
        self.at = self.at.max(other.at);
        self.provider = self.provider.clone().max(other.provider);
        self.model = self.model.clone().max(other.model);
        let (tokens, more) = (&mut self.tokens, other.tokens);
        tokens.input = tokens.input.max(more.input);
        tokens.cache_read = tokens.cache_read.max(more.cache_read);
        tokens.cache_write_5m = tokens.cache_write_5m.max(more.cache_write_5m);
        tokens.cache_write_1h = tokens.cache_write_1h.max(more.cache_write_1h);
        tokens.output = tokens.output.max(more.output);
        tokens.reasoning = tokens.reasoning.max(more.reasoning);
        self.prompt = self.prompt.max(other.prompt);
        self.web_searches = self.web_searches.max(other.web_searches);
        self.priority |= other.priority;
        self.cost = match (self.cost, other.cost) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
}

/// Tokens. No input count includes another; `reasoning` is part of `output`.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tokens {
    /// Input neither read from the cache nor written to it.
    pub input: u64,
    pub cache_read: u64,
    /// Written to a cache kept five minutes, or as long as a provider keeps
    /// its only kind.
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    /// Output, reasoning included.
    pub output: u64,
    /// How much of the output was reasoning.
    pub reasoning: u64,
}

/// One entry of a conversation.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Entry {
    pub kind: Kind,
    pub at: Option<i64>,
    /// What was said; empty for a tool call.
    pub text: String,
    pub tool: Option<Tool>,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    User,
    Assistant,
    Reasoning,
    Tool,
    /// What the agent added itself: instructions, reminders, notices.
    System,
    /// The summary an agent wrote when it compacted the conversation.
    Summary,
    /// What a subagent was asked to do, which arrives as the person's.
    Task,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Tool {
    pub name: String,
    pub input: String,
    pub output: Option<String>,
    pub failed: bool,
    /// The files it changed, as the agent names them: absolute, or within
    /// the session's folder.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// What it is to a handoff, as its agent says (`Agent::role`).
    #[serde(skip)]
    pub role: Role,
}

/// What a tool call is to a handoff.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Role {
    /// It read: a file, a search, a page. The next agent reads it again as
    /// it needs, so it's named in a line.
    Read,
    /// It asked the person, whose answer is a request of theirs.
    Ask,
    /// It ran a subagent, whose output is its report.
    Subagent,
    /// Anything else, as a command or an edit: the work itself.
    #[default]
    Work,
}

/// A conversation as it's read: tool results are folded into their calls.
#[derive(Default)]
pub struct Conversation {
    pub entries: Vec<Entry>,
    calls: BTreeMap<String, usize>,
}

impl Conversation {
    pub fn say(&mut self, kind: Kind, at: Option<i64>, text: &str) {
        if !text.trim().is_empty() {
            self.entries.push(Entry {
                kind,
                at,
                text: text.to_owned(),
                tool: None,
            });
        }
    }

    pub fn call(&mut self, id: Option<&str>, at: Option<i64>, name: &str, input: String) {
        if let Some(id) = id {
            self.calls.insert(id.to_owned(), self.entries.len());
        }
        self.entries.push(Entry {
            kind: Kind::Tool,
            at,
            text: String::new(),
            tool: Some(Tool {
                name: name.to_owned(),
                input,
                output: None,
                failed: false,
                files: Vec::new(),
                role: Role::Work,
            }),
        });
    }

    /// Note `files` as changed by the call made last.
    pub fn changed(&mut self, files: impl IntoIterator<Item = String>) {
        let last = self
            .entries
            .last_mut()
            .and_then(|entry| entry.tool.as_mut());
        if let Some(call) = last {
            call.files
                .extend(files.into_iter().filter(|file| !file.is_empty()));
        }
    }

    pub fn answer(&mut self, id: &str, output: String, failed: bool) {
        let call = self
            .calls
            .get(id)
            .and_then(|&at| self.entries[at].tool.as_mut());
        if let Some(call) = call {
            call.output = Some(output);
            call.failed = failed;
        }
    }
}

/// What an agent folder holds for one provider.
pub struct Login {
    /// The provider as the agent's responses name it, which says whose usage
    /// it is.
    pub provider: String,
    /// `None`: signed out, or signed in to something Turnscope doesn't read,
    /// whose use is then no account's rather than a guess.
    pub credential: Option<Credential>,
}

pub struct Credential {
    /// The provider that reads it, from `providers::ALL`.
    pub provider: &'static str,
    pub secret: Secret,
    /// An API key, rather than a sign-in's token.
    pub key: bool,
    /// When a sign-in's token expires, in Unix milliseconds.
    pub expires: Option<i64>,
    /// The account's id and label, where the agent's files say.
    pub identity: Option<(String, Option<String>)>,
    /// The plan, where the agent's files say, such as Claude Code's `max`.
    pub plan: Option<String>,
}

/// A token or key. It can't be printed, serialized or cloned, so it can't
/// reach a log, the database or an answer by accident.
pub struct Secret(String);

impl Secret {
    pub fn new(text: &str) -> Secret {
        Secret(text.to_owned())
    }

    /// The text, for the one request it authorizes.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The first 16 hex digits of its SHA-256: tells keys apart without
    /// saying either.
    pub fn fingerprint(&self) -> String {
        use sha2::Digest as _;
        sha2::Sha256::digest(self.0.as_bytes())[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// Pass each complete line of `path` from byte `from` to `visit`, and say
/// where the next read starts: past the last line that ended with a newline,
/// since one without is still being written.
pub fn lines(path: &Path, from: u64, mut visit: impl FnMut(&[u8])) -> Result<u64> {
    let failed = |error| Error::Failed(format!("reading {}: {error}", path.display()));
    let mut file = File::open(path).map_err(failed)?;
    file.seek(SeekFrom::Start(from)).map_err(failed)?;
    let mut reader = BufReader::with_capacity(1 << 20, file);
    let mut line = Vec::new();
    let mut offset = from;
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line).map_err(failed)?;
        if read == 0 || line.last() != Some(&b'\n') {
            return Ok(offset);
        }
        offset += read as u64;
        if line.len() > 1 {
            visit(&line);
        }
    }
}

/// The JSON file at `path`, or `None` when there is none.
pub fn json_file(path: &Path) -> Result<Option<Value>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| Error::Failed(format!("reading {}: {error}", path.display()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::Failed(format!(
            "reading {}: {error}",
            path.display()
        ))),
    }
}

/// The claims of a JWT, read without checking its signature: they only tell
/// accounts apart and when a token expires. Null when it isn't one.
pub fn claims(token: &str) -> Value {
    token
        .split('.')
        .nth(1)
        .and_then(|payload| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(payload)
                .ok()
        })
        .and_then(|json| serde_json::from_slice(&json).ok())
        .unwrap_or(Value::Null)
}

/// `home/own`, then the folders beside it named `prefix` and more that hold
/// `signed`, the file the agent writes once signed in there: how a second
/// account's folder is kept. Each once, a link counting as what it links to.
pub fn own_and_found(home: &Path, own: &str, prefix: &str, signed: &str) -> Vec<PathBuf> {
    let mut folders = vec![home.join(own)];
    if let Ok(entries) = std::fs::read_dir(home) {
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.len() > prefix.len() && name.starts_with(prefix))
                    && entry.path().join(signed).is_file()
            })
            .map(|entry| entry.path())
            .collect();
        found.sort();
        folders.extend(found);
    }
    let mut seen = std::collections::HashSet::new();
    folders.retain(|folder| seen.insert(std::fs::canonicalize(folder).unwrap_or(folder.clone())));
    folders
}

/// Where `<tag>`, or `<tag` with attributes, opens in `text`: its start, and
/// where what it holds starts.
fn opening(text: &str, tag: &str) -> Option<(usize, usize)> {
    let open = format!("<{tag}");
    let mut from = 0;
    while let Some(at) = text[from..].find(&open).map(|at| from + at) {
        let after = at + open.len();
        match text[after..].chars().next() {
            Some('>') => return Some((at, after + 1)),
            Some(space) if space.is_whitespace() => {
                let end = text[after..].find('>')?;
                return Some((at, after + end + 1));
            }
            _ => from = after,
        }
    }
    None
}

/// What `<tag>…</tag>` holds in `text`.
pub fn inner<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let after = &text[opening(text, tag)?.1..];
    Some(
        after
            .split_once(&format!("</{tag}>"))
            .map_or(after, |(inner, _)| inner),
    )
}

/// `text` without the blocks `<tag>…</tag>` of `tags`, attributes or not:
/// what an agent adds to a message sent as the person's.
pub fn without_tags(text: &str, tags: &[&str]) -> String {
    let mut text = text.to_owned();
    for tag in tags {
        let close = format!("</{tag}>");
        while let Some((start, inside)) = opening(&text, tag) {
            let end = text[inside..]
                .find(&close)
                .map_or(text.len(), |end| inside + end + close.len());
            text.replace_range(start..end, "");
        }
        // One left closing a block an agent split across parts.
        text = text.replace(&close, "");
    }
    text.trim().to_owned()
}

/// The files a patch in `text` adds, updates, deletes or moves to, as
/// `apply_patch` names them: in a patch as written, or quoted in a script that
/// passes one on.
pub fn patched(text: &str) -> Vec<String> {
    let mut files = Vec::new();
    for head in [
        "*** Add File: ",
        "*** Update File: ",
        "*** Delete File: ",
        "*** Move to: ",
    ] {
        for (at, _) in text.match_indices(head) {
            let path = &text[at + head.len()..];
            let end = path.find(['\n', '\\', '"', '`']).unwrap_or(path.len());
            files.push(path[..end].trim().to_owned());
        }
    }
    files
}

/// The command at `pointer` in the JSON file at `path`, where Claude Code,
/// OpenCode and Pi keep Turnscope's MCP server.
pub fn json_mcp_command(path: &Path, pointer: &str) -> Option<String> {
    let config = json_file(path).ok()??;
    Some(config.pointer(pointer)?.as_str()?.to_owned())
}

/// The `command` of `[mcp_servers.turnscope]` in the TOML file at `path`, as
/// Codex and Grok Build keep it: their own commands write the table one key
/// to a line.
pub fn toml_mcp_command(path: &Path) -> Option<String> {
    let config = std::fs::read_to_string(path).ok()?;
    config
        .lines()
        .skip_while(|line| line.trim() != "[mcp_servers.turnscope]")
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with('['))
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "command").then(|| value.trim().trim_matches('"').to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_of_a_response_merge_alike_in_any_order() {
        let report =
            |session: &str, copy: bool, output: u64, cache_read: u64, model: &str| Response {
                id: "r".to_owned(),
                session: session.to_owned(),
                copy,
                at: output as i64,
                provider: "anthropic".to_owned(),
                model: model.to_owned(),
                tokens: Tokens {
                    output,
                    cache_read,
                    ..Tokens::default()
                },
                prompt: cache_read,
                web_searches: 0,
                priority: false,
                cost: None,
            };
        let reports = [
            report("claude-code:fork", true, 900, 50, "claude-opus-5"),
            report("claude-code:own", false, 16, 70, ""),
            report("claude-code:own", false, 400, 70, "claude-opus-5"),
        ];
        let orders = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        let merged: Vec<Response> = orders
            .iter()
            .map(|order| {
                let mut merged = reports[order[0]].clone();
                for &next in &order[1..] {
                    merged.merge(reports[next].clone());
                }
                merged
            })
            .collect();
        assert!(merged.iter().all(|response| *response == merged[0]));
        // The own report's session, and the most of each count.
        assert_eq!(
            (merged[0].session.as_str(), merged[0].copy),
            ("claude-code:own", false)
        );
        assert_eq!(
            (
                merged[0].tokens.output,
                merged[0].tokens.cache_read,
                merged[0].model.as_str()
            ),
            (900, 70, "claude-opus-5")
        );
    }

    #[test]
    fn a_patch_names_its_files_written_out_or_quoted() {
        let written =
            "*** Begin Patch\n*** Update File: src/a.rs\n@@\n*** Add File: b.md\n+x\n*** End Patch";
        assert_eq!(patched(written), ["b.md", "src/a.rs"]);
        let quoted =
            r#"tools.apply_patch("*** Begin Patch\n*** Delete File: old.rs\n*** End Patch")"#;
        assert_eq!(patched(quoted), ["old.rs"]);
    }

    #[test]
    fn what_agents_add_is_taken_out_of_what_the_person_typed() {
        assert_eq!(
            without_tags(
                "<system-reminder>hi</system-reminder>Fix it.<x>y",
                &["system-reminder"]
            ),
            "Fix it.<x>y"
        );
        assert_eq!(
            without_tags(
                "<goal source=\"thread\">Go on.</goal> Add tests. <goalpost>kept</goalpost>",
                &["goal"]
            ),
            "Add tests. <goalpost>kept</goalpost>"
        );
        assert_eq!(
            inner("a <user_query>Do it.</user_query> b", "user_query"),
            Some("Do it.")
        );
    }

    #[test]
    fn a_title_is_redacted() {
        let mut session = Session::default();
        session.title(
            Title::Prompt,
            "Use ANTHROPIC_API_KEY=sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123 here",
        );
        let (_, title) = session.title.unwrap();
        assert!(!title.contains("abcdefghijklmnop"), "{title}");
    }
}
