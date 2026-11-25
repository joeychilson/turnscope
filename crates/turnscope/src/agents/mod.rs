//! The coding agents Turnscope reads. Each is one module implementing
//! [`Agent`], listed in [`ALL`]; nothing outside this folder knows any
//! agent's files.

mod antigravity;
mod claude_code;
mod codex;
mod grok_build;
mod opencode;
mod pi;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Display;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use serde_json::Value;
use serde_json::value::RawValue;

use crate::{Error, Result};

/// Every agent, in the order they're listed.
pub static ALL: &[&dyn Agent] = &[
    &claude_code::ClaudeCode,
    &codex::Codex,
    &opencode::OpenCode,
    &pi::Pi,
    &grok_build::GrokBuild,
    &antigravity::Antigravity,
];

pub fn by_id(id: &str) -> Option<&'static dyn Agent> {
    ALL.iter().copied().find(|agent| agent.info().id == id)
}

pub fn find(id: &str) -> crate::Result<&'static dyn Agent> {
    by_id(id).ok_or_else(|| {
        let ids: Vec<&str> = ALL.iter().map(|agent| agent.info().id).collect();
        crate::Error::Usage(format!("no agent is {id}; agents are {}", ids.join(", ")))
    })
}

pub fn name(id: &str) -> &str {
    by_id(id).map_or(id, |agent| agent.info().name)
}

/// Whether `path` is a file an agent keeps a login in (`Info::login_file`).
pub fn login_file(path: &Path) -> bool {
    let name = path.file_name();
    ALL.iter().any(|agent| {
        agent
            .info()
            .login_file
            .is_some_and(|file| name == Some(file.as_ref()))
    })
}

pub trait Agent: Sync {
    fn info(&self) -> &'static Info;

    /// The folders it keeps history and logins in under `home`: its own
    /// first, then any it was pointed at for a second account. A folder it
    /// held a login in and no longer lists is taken as signed out of, so one
    /// is listed for as long as the agent may be signed in there.
    fn folders(&self, home: &Path) -> Vec<PathBuf>;

    /// Whether the agent is on this Mac, by `folder`, its own: the folder is
    /// there, unless another program keeps a folder of that name too.
    fn installed(&self, folder: &Path) -> bool {
        folder.exists()
    }

    /// Like `folders`, leaving out the agent's own folder when the agent isn't
    /// installed: it may not exist or may belong to another program, as
    /// `~/.gemini` is the Gemini command line's too.
    fn installed_folders(&self, home: &Path) -> Vec<PathBuf> {
        let mut folders = self.folders(home);
        if folders
            .first()
            .is_some_and(|folder| !self.installed(folder))
        {
            folders.remove(0);
        }
        folders
    }

    /// Its history files in `folder`, in the order they're read. One that
    /// doesn't exist is passed over, so a file it may keep can be listed
    /// without looking.
    fn files(&self, folder: &Path) -> Vec<PathBuf>;

    /// What `file` holds past `cursor`, which a read returned before; an
    /// empty cursor reads from the start.
    fn read(&self, file: &Path, cursor: &str) -> Result<Read>;

    /// What `folder` holds for each provider it signs in to. A provider it
    /// held a login for and no longer lists is taken as signed out of.
    ///
    /// Fails when a place that keeps logins can't be read, as a file being
    /// written: taking that for no login would sign its accounts out.
    fn logins(&self, folder: &Path, home: &Path) -> Result<Vec<Login>>;

    /// The command that `folder`'s configuration runs for Turnscope's MCP
    /// server, if any. Each of its folders keeps its own.
    fn mcp_command(&self, folder: &Path, home: &Path) -> Option<String>;
}

/// What Turnscope knows of an agent as a program.
pub struct Info {
    /// As stored and written in session ids: `claude-code`.
    pub id: &'static str,
    pub name: &'static str,
    /// Its command: run to connect it, and how its process is recognised
    /// among an MCP server's ancestors.
    pub command: &'static str,
    /// The program in its app that starts an MCP server, where the app
    /// doesn't run `command`: the end of its path, as
    /// `Antigravity.app/Contents/Resources/bin/language_server`.
    pub app: Option<&'static str>,
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
    /// What it sets in an MCP server it starts: its session's id.
    pub session_var: Option<&'static str>,
    /// The name of a file it keeps a login in, so a write to it asks for
    /// limits (`limits::refresh`), as once it signs in again. Not one
    /// it keeps other things in too, as Claude Code's `.claude.json`.
    pub login_file: Option<&'static str>,
}

impl Info {
    pub fn session(&self, native: &str) -> String {
        format!("{}:{native}", self.id)
    }
}

/// What one read of a file found.
#[derive(Default)]
pub struct Read {
    /// Where the next read starts, in the reader's own terms.
    pub cursor: String,
    pub sessions: BTreeMap<String, Session>,
    /// By id, each response's reports merged.
    pub responses: HashMap<String, Response>,
    /// Limits the agent wrote, the latest of each provider and scope
    /// (`Read::in_band`).
    pub limits: Vec<InBand>,
    /// Lines or rows it couldn't read.
    pub skipped: u64,
}

/// Limits an agent wrote in its history, as Codex does with each response
/// (`limits::record_in_band`), keyed as its provider's are.
pub struct InBand {
    /// The provider as the agent's responses name it.
    pub provider: String,
    /// When the agent wrote them.
    pub at: i64,
    /// Of one scope: the plan's own limits, or one on some models.
    pub readings: Vec<crate::providers::Reading>,
}

impl InBand {
    fn scope(&self) -> Option<&str> {
        self.readings.first().and_then(|r| r.scope.as_deref())
    }
}

impl Read {
    pub fn session(&mut self, id: &str) -> &mut Session {
        if !self.sessions.contains_key(id) {
            self.sessions.insert(id.to_owned(), Session::default());
        }
        self.sessions.get_mut(id).expect("session just inserted")
    }

    /// Take in the folder `session` works in: the first given, of those not
    /// empty.
    pub fn cwd(&mut self, session: &str, cwd: Option<&str>) {
        if let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) {
            self.session(session)
                .cwd
                .get_or_insert_with(|| cwd.to_owned());
        }
    }

    /// Take in limits the agent wrote, over any it wrote before of the same
    /// provider and scope: a long file's first read keeps its latest alone.
    pub fn in_band(&mut self, snapshot: InBand) {
        if snapshot.readings.is_empty() {
            return;
        }
        let scope = snapshot.scope();
        if let Some(existing) = self
            .limits
            .iter_mut()
            .find(|known| known.provider == snapshot.provider && known.scope() == scope)
        {
            if snapshot.at >= existing.at {
                *existing = snapshot;
            }
        } else {
            self.limits.push(snapshot);
        }
    }

    /// Take in a response, and its session as one this file tells of. One
    /// with no tokens and no charge, as one stopped before it wrote
    /// anything, is none.
    pub fn respond(&mut self, response: Response) {
        if response.tokens == Tokens::default() && response.cost.is_none_or(|cost| cost == 0.0) {
            return;
        }
        self.session(&response.session);
        match self.responses.get_mut(&response.id) {
            Some(known) => known.merge(response),
            None => {
                self.responses.insert(response.id.clone(), response);
            }
        }
    }
}

/// What a file says of a session. Several files and reads add up to the
/// best title.
#[derive(Default)]
pub struct Session {
    /// The folder it works in, which its project is found by.
    pub cwd: Option<String>,
    pub title: Option<(Title, String)>,
    /// The session it's a subagent of, which its usage counts toward. A
    /// fork or a continuation has none: its usage is its own.
    pub parent: Option<String>,
}

/// How much of a title is kept, in characters. A title is often the first
/// thing the person asked, whole: of 13,652 titles in one database, 4,206
/// were longer, the longest 121,215 characters, and they were 42.8 MB of
/// the 43.4 MB all titles took (November 2026). Nothing shows more than 80.
const TITLE: usize = 200;

impl Session {
    /// Kept as written, a pasted key included: the agent's own files hold it
    /// too, and at most 159 of 13,652 titles had one (November 2026).
    pub fn title(&mut self, kind: Title, text: &str) {
        if self.title.as_ref().is_some_and(|(had, _)| kind < *had) {
            return;
        }
        let mut out = String::new();
        let mut left = TITLE;
        for word in text.split_whitespace() {
            if !out.is_empty() {
                if left <= 1 {
                    break;
                }
                out.push(' ');
                left -= 1;
            }
            for ch in word.chars().take(left) {
                out.push(ch);
                left -= 1;
            }
        }
        if !out.is_empty() {
            self.title = Some((kind, out));
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
    /// As the agent names it, which may not be a provider's id: one agent
    /// names a ChatGPT sign-in `openai-codex`.
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
    /// streamed, or a copy in another file. Every choice is the same in any
    /// order, so files read in any order agree.
    pub fn merge(&mut self, other: Response) {
        if (other.copy, &other.session) < (self.copy, &self.session) {
            self.copy = other.copy;
            self.session = other.session;
        }
        self.at = self.at.max(other.at);
        if other.provider > self.provider {
            self.provider = other.provider;
        }
        if other.model > self.model {
            self.model = other.model;
        }
        self.tokens = self.tokens.zip(other.tokens, u64::max);
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
    pub output: u64,
    pub reasoning: u64,
}

impl Tokens {
    /// Every input count, which together are the prompt.
    pub fn prompt(&self) -> u64 {
        self.input + self.cache_read + self.cache_write_5m + self.cache_write_1h
    }

    pub fn zip(self, other: Tokens, f: fn(u64, u64) -> u64) -> Tokens {
        Tokens {
            input: f(self.input, other.input),
            cache_read: f(self.cache_read, other.cache_read),
            cache_write_5m: f(self.cache_write_5m, other.cache_write_5m),
            cache_write_1h: f(self.cache_write_1h, other.cache_write_1h),
            output: f(self.output, other.output),
            reasoning: f(self.reasoning, other.reasoning),
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
    /// The plan, where the agent's files say, such as `max`.
    pub plan: Option<String>,
    /// What renews the login in memory once its token expires, where its
    /// provider renews logins at all: only one whose refresh token isn't
    /// replaced when used, so renewing takes nothing from the agent.
    pub refresh: Option<Refresh>,
}

impl Credential {
    /// Whether it's good for a minute more: one about to expire would be
    /// refused on the way, which reads as a login to sign in to again,
    /// where it only needs renewing.
    pub fn fresh(&self, now: i64) -> bool {
        self.expires.is_none_or(|at| at > now + crate::time::MINUTE)
    }
}

/// What renews a login: its refresh token, and the agent's programs, which
/// hold the OAuth client to renew it with.
pub struct Refresh {
    pub token: Secret,
    pub programs: Vec<PathBuf>,
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
        crate::sha256_hex(self.0.as_bytes(), 8)
    }
}

fn reading(path: &Path, error: impl Display) -> Error {
    Error::Failed(format!("reading {}: {error}", path.display()))
}

fn non_empty(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| !text.is_empty())
}

fn str_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(non_empty)
}

/// Read an agent's SQLite database: with no `-wal` beside it, immutable, which
/// leaves no files behind (a plain open left 14 by 7 databases, 2026-11-10),
/// failing if the database or its log changed meanwhile.
fn opened<T>(file: &Path, read: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
    let mut log = file.as_os_str().to_owned();
    log.push("-wal");
    let alone = || {
        let file = std::fs::metadata(file).ok()?;
        (!Path::new(&log).exists()).then_some((file.len(), file.modified().ok()))
    };
    let before = file.to_str().zip(alone());
    let db = match before {
        Some((path, _)) => {
            let path = path
                .replace('%', "%25")
                .replace('?', "%3f")
                .replace('#', "%23");
            Connection::open_with_flags(
                format!("file:{path}?immutable=1"),
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )
        }
        None => Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_ONLY),
    }
    .map_err(|error| {
        let error = error.to_string();
        reading(file, error.split(": file:").next().unwrap_or_default())
    })?;
    let read = db
        .busy_timeout(std::time::Duration::from_secs(2))
        .and_then(|()| read(&db))
        .map_err(|error| reading(file, error))?;
    match before {
        Some((_, before)) if alone() != Some(before) => {
            Err(reading(file, "it changed while it was read"))
        }
        _ => Ok(read),
    }
}

/// Visit each line of `path` from `from`, and return where the next read
/// starts: before a last line with no `\n`, as one still being written.
fn try_lines(
    path: &Path,
    from: u64,
    mut visit: impl FnMut(&[u8]) -> Result<ControlFlow<()>>,
) -> Result<u64> {
    let failed = |error| reading(path, error);
    let mut file = File::open(path).map_err(failed)?;
    let capacity = file.metadata().ok().map_or(64 << 10, |meta| {
        (meta.len().saturating_sub(from) as usize).clamp(4096, 1 << 20)
    });
    if from > 0 {
        file.seek(SeekFrom::Start(from)).map_err(failed)?;
    }
    let mut reader = BufReader::with_capacity(capacity, file);
    let mut line = Vec::new();
    let mut offset = from;
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line).map_err(failed)?;
        if read == 0 || line.last() != Some(&b'\n') {
            return Ok(offset);
        }
        offset += read as u64;
        if !line.trim_ascii().is_empty() && visit(&line)?.is_break() {
            return Ok(offset);
        }
    }
}

fn lines(path: &Path, from: u64, mut visit: impl FnMut(&[u8])) -> Result<u64> {
    try_lines(path, from, |line| {
        visit(line);
        Ok(ControlFlow::Continue(()))
    })
}

fn entries(dir: &Path) -> impl Iterator<Item = PathBuf> + use<> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
}

/// Content with `text`, as a message's part.
#[derive(Deserialize)]
struct Text<'a> {
    #[serde(borrow)]
    text: Option<Cow<'a, str>>,
}

/// The texts of a message's `content`: a string, or parts with `text`.
fn texts(content: &RawValue) -> Vec<String> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Content<'a> {
        Text(#[serde(borrow)] Cow<'a, str>),
        Parts(#[serde(borrow)] Vec<Text<'a>>),
    }
    match serde_json::from_str(content.get()) {
        Ok(Content::Text(text)) => vec![text.into_owned()],
        Ok(Content::Parts(parts)) => parts
            .into_iter()
            .filter_map(|part| Some(part.text?.into_owned()))
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// For a field a reader can do without: one null or of another type reads as
/// its default, so it doesn't lose the line it's on.
fn lenient<'de, D, T>(field: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    let raw = <&RawValue>::deserialize(field)?;
    Ok(serde_json::from_str(raw.get()).unwrap_or_default())
}

/// The newer of the sign-in in the Keychain and the one in the file: the one
/// whose token expires later. One that reads wins over one that doesn't, so a
/// file half written doesn't sign the account out, nor does a Keychain that
/// can't be read when the file can.
fn fresher(
    kept: Result<Option<Credential>>,
    file: Result<Option<Credential>>,
) -> Result<Option<Credential>> {
    match (kept, file) {
        (Ok(Some(kept)), Ok(Some(file))) => Ok(Some(if expiry(&file) > expiry(&kept) {
            file
        } else {
            kept
        })),
        (Ok(Some(one)), _) | (_, Ok(Some(one))) => Ok(Some(one)),
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(None), Ok(None)) => Ok(None),
    }
}

/// When a sign-in expires, one with no expiry last.
fn expiry(credential: &Credential) -> i64 {
    credential.expires.unwrap_or(i64::MAX)
}

pub(crate) fn json_file(path: &Path) -> Result<Option<Value>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| reading(path, error)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(reading(path, error)),
    }
}

/// The login to `provider` that `kept` holds, as an agent keeps one in JSON:
/// `{type: "oauth", access, expires}` for a sign-in, or `{type, key}` for an
/// API key, its type `key_type`. `reads` is the id of the provider that
/// reads it; a login no provider reads has no credential.
fn kept_login(provider: &str, reads: &str, kept: &Value, key_type: &str) -> Login {
    let reads = crate::providers::by_id(reads);
    // A sign-in to a provider that sells no plan gives the agent a key.
    let sells_none = reads.is_some_and(|reads| reads.info().subscription.is_none());
    let (secret, key) = match kept["type"].as_str() {
        Some(kind) if kind == key_type => (kept["key"].as_str(), true),
        Some("oauth") => (kept["access"].as_str(), sells_none),
        _ => (None, false),
    };
    let secret = secret.filter(|secret| !secret.is_empty());
    Login {
        credential: reads.zip(secret).map(|(reads, secret)| Credential {
            provider: reads.info().id,
            secret: Secret::new(secret),
            key,
            expires: kept["expires"].as_i64(),
            identity: None,
            plan: None,
            refresh: None,
        }),
        provider: provider.to_owned(),
    }
}

/// The Keychain item `service`, read with `/usr/bin/security` as the agents
/// write it, so macOS asks nothing; under `home`, so a test's sees no item.
fn keychain<T>(
    home: &Path,
    service: &str,
    account: Option<&str>,
    parse: impl FnOnce(&[u8]) -> std::result::Result<T, String>,
) -> Result<Option<T>> {
    let failed = |why: String| Error::Failed(format!("reading the Keychain item {service}: {why}"));
    let mut security = std::process::Command::new("/usr/bin/security");
    security.args(["find-generic-password", "-s", service]);
    if let Some(account) = account {
        security.args(["-a", account]);
    }
    security.arg("-w").env("HOME", home);
    let output = crate::process::output_within(&mut security, std::time::Duration::from_secs(20))
        .map_err(|error| failed(error.to_string()))?;
    let kept = found(output).map_err(failed)?;
    kept.map(|kept| parse(&kept)).transpose().map_err(failed)
}

fn found(output: std::process::Output) -> std::result::Result<Option<Vec<u8>>, String> {
    match output.status.code() {
        Some(0) => Ok(Some(output.stdout)),
        Some(44) => Ok(None),
        _ => {
            let why = String::from_utf8_lossy(&output.stderr);
            let why = match why.trim() {
                "" => "security gave no reason",
                why => why,
            };
            Err(format!("{why} ({})", output.status))
        }
    }
}

/// A JWT's claims, unverified: read only for its account and expiry.
pub(crate) fn claims(token: &str) -> Value {
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

pub(crate) fn own_and_found(home: &Path, own: &str, signed: &str) -> Vec<PathBuf> {
    let prefix = format!("{own}-");
    let mut folders: Vec<PathBuf> = entries(home)
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.len() > prefix.len() && name.starts_with(&prefix))
                && path.join(signed).is_file()
        })
        .collect();
    folders.sort();
    folders.insert(0, home.join(own));
    let mut seen = std::collections::HashSet::new();
    folders.retain(|folder| {
        seen.insert(std::fs::canonicalize(folder).unwrap_or_else(|_| folder.clone()))
    });
    folders
}

/// What the person typed: what `tag` holds, or else `text` without the
/// agent's `added` blocks.
fn typed_in(text: &str, tag: &str, added: &[&str]) -> String {
    match inner(text, tag) {
        Some(typed) => typed.trim().to_owned(),
        None => without_tags(text, added),
    }
}

/// Where in `text` the first mark of `tag` is that begins with `opens`, `<`
/// or `</`: as `<tag>`, or `<tag …>` with attributes.
fn mark(text: &str, opens: &str, tag: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(rel) = text[from..].find(opens) {
        let at = from + rel;
        let named = at + opens.len() + tag.len();
        if text[at + opens.len()..].starts_with(tag) {
            let rest = &text[named..];
            match rest.chars().next() {
                Some('>') => return Some((at, named + 1)),
                Some(space) if space.is_whitespace() => {
                    return Some((at, named + rest.find('>')? + 1));
                }
                _ => {}
            }
        }
        from = at + opens.len();
    }
    None
}

fn inner<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let after = &text[mark(text, "<", tag)?.1..];
    Some(match mark(after, "</", tag) {
        Some((end, _)) => &after[..end],
        None => after,
    })
}

/// `text` without the agent's `<tag>…</tag>` blocks. A tag never closed is
/// the agent's only at the start, so "Add a <skill> element" is kept.
fn without_tags(text: &str, tags: &[&str]) -> String {
    if !text.contains('<') {
        return text.trim().to_owned();
    }
    let mut text = text.to_owned();
    for tag in tags {
        while let Some((start, inside)) = mark(&text, "<", tag)
            && let Some((_, end)) = mark(&text[inside..], "</", tag)
        {
            text.replace_range(start..inside + end, "");
        }
        while let Some((start, end)) = mark(&text, "</", tag) {
            text.replace_range(start..end, "");
        }
    }
    let trimmed = text.trim();
    let opens = trimmed.starts_with('<')
        && tags
            .iter()
            .any(|tag| mark(trimmed, "<", tag).is_some_and(|(start, _)| start == 0));
    if opens {
        String::new()
    } else if trimmed.len() == text.len() {
        text
    } else {
        trimmed.to_owned()
    }
}

fn json_mcp_command(path: &Path, pointer: &str) -> Option<String> {
    let config = json_file(path).ok()??;
    Some(config.pointer(pointer)?.as_str()?.to_owned())
}

fn toml_mcp_command(path: &Path) -> Option<String> {
    let config = std::fs::read_to_string(path).ok()?;
    config
        .lines()
        .skip_while(|line| line.trim() != "[mcp_servers.turnscope]")
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with('['))
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "command")
                .then(|| toml_string(value.trim()))
                .flatten()
        })
}

fn toml_string(value: &str) -> Option<String> {
    if let Some(rest) = value.strip_prefix('\'') {
        let (text, _) = rest.split_once('\'')?;
        return Some(text.to_owned());
    }
    if let Some(rest) = value.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(ch) = chars.next() {
            match ch {
                '"' => return Some(out),
                '\\' => match chars.next()? {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                },
                other => out.push(other),
            }
        }
        return None;
    }
    let unquoted = value.split('#').next()?.trim();
    (!unquoted.is_empty()).then(|| unquoted.to_owned())
}

#[cfg(test)]
fn jsonl(file: PathBuf, lines: &[Value]) -> PathBuf {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    std::fs::write(&file, text).unwrap();
    file
}

#[cfg(test)]
fn credential(login: &Login) -> Option<(&'static str, bool, Option<i64>, &str)> {
    let credential = login.credential.as_ref()?;
    Some((
        credential.provider,
        credential.key,
        credential.expires,
        credential.secret.expose(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fingerprint names a key's account and limits where they're kept.
    #[test]
    fn a_fingerprint_is_the_first_16_hex_digits_of_its_sha256() {
        assert_eq!(Secret::new("work").fingerprint(), "00e13ed7af55b276");
    }

    #[test]
    fn a_title_is_kept_on_one_line_and_cut_at_200_characters() {
        let mut session = Session::default();
        session.title(Title::Prompt, &format!("Fix\n  the {}", "é".repeat(300)));
        let (_, title) = session.title.as_ref().unwrap();
        assert!(title.starts_with("Fix the éé"), "{title}");
        assert_eq!(title.chars().count(), 200);
        session.title(Title::Named, " \n ");
        assert_eq!(session.title.as_ref().unwrap().1.chars().count(), 200);
        let full = "a".repeat(199);
        session.title(Title::Named, &format!("{full} b"));
        assert_eq!(session.title.as_ref().unwrap().1, full);
        // Renamed: of one rank, the latest.
        session.title(Title::Named, "Parser");
        assert_eq!(session.title.unwrap().1, "Parser");
    }

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
        // A read takes each report in, and each session a report names.
        let mut read = Read::default();
        for report in reports {
            read.respond(report);
        }
        assert_eq!(read.responses["r"], merged[0]);
        assert!(
            read.sessions
                .keys()
                .eq(["claude-code:fork", "claude-code:own"])
        );
    }

    #[test]
    fn a_database_is_read_with_nothing_left_beside_it_or_with_its_log() {
        // In a folder whose name would end an address.
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("a #1?");
        std::fs::create_dir(&folder).unwrap();
        let database = folder.join("history.db");
        let beside = |ending: &str| {
            let mut path = database.clone().into_os_string();
            path.push(ending);
            PathBuf::from(path)
        };
        let rows = |db: &Connection| {
            db.query_row("SELECT count(*) FROM call", [], |row| row.get::<_, i64>(0))
        };
        // As an agent keeps one, and leaves it once closed.
        let db = Connection::open(&database).unwrap();
        db.pragma_update(None, "journal_mode", "WAL").unwrap();
        db.execute_batch("CREATE TABLE call (n INTEGER); INSERT INTO call VALUES (1);")
            .unwrap();
        drop(db);
        assert!(!beside("-wal").exists());
        assert_eq!(opened(&database, rows).unwrap(), 1);
        assert!(!beside("-wal").exists() && !beside("-shm").exists());

        // While its agent has it open, what it wrote last is in its log.
        let db = Connection::open(&database).unwrap();
        db.execute("INSERT INTO call VALUES (2)", []).unwrap();
        assert!(beside("-wal").exists());
        assert_eq!(opened(&database, rows).unwrap(), 2);

        // Read as one that can't change, one its agent opens or writes
        // meanwhile isn't taken for read.
        drop(db);
        assert!(!beside("-wal").exists());
        let opened_meanwhile = opened(&database, |_| {
            std::fs::write(beside("-wal"), "").unwrap();
            Ok(())
        });
        assert!(opened_meanwhile.is_err());
        std::fs::remove_file(beside("-wal")).unwrap();
        let written_meanwhile = opened(&database, |_| {
            let file = File::options().write(true).open(&database).unwrap();
            let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
            file.set_modified(later).unwrap();
            Ok(())
        });
        assert!(written_meanwhile.is_err());
        assert!(opened(&database, |_| Ok(())).is_ok());
    }

    #[test]
    fn a_report_with_no_tokens_and_no_charge_is_no_response() {
        let report = |id: &str, tokens: Tokens, cost: Option<f64>| Response {
            id: id.to_owned(),
            session: format!("claude-code:{id}"),
            copy: false,
            at: 0,
            provider: "anthropic".to_owned(),
            model: "claude-opus-5".to_owned(),
            tokens,
            prompt: 0,
            web_searches: 0,
            priority: false,
            cost,
        };
        let one = Tokens {
            output: 1,
            ..Tokens::default()
        };
        let mut read = Read::default();
        for (id, tokens, cost) in [
            ("none", Tokens::default(), None),
            ("free", Tokens::default(), Some(0.0)),
            ("charged", Tokens::default(), Some(0.01)),
            ("used", one, None),
        ] {
            read.respond(report(id, tokens, cost));
        }
        let mut kept: Vec<&str> = read.responses.keys().map(String::as_str).collect();
        kept.sort();
        assert_eq!(kept, ["charged", "used"]);
        assert!(
            read.sessions
                .keys()
                .eq(["claude-code:charged", "claude-code:used"])
        );
    }

    #[test]
    fn what_agents_add_is_taken_out_of_what_the_person_typed() {
        let tags = ["system-reminder", "goal", "image", "skill"];
        for (text, kept) in [
            (
                "<system-reminder>hi</system-reminder>Fix it.<x>y",
                "Fix it.<x>y",
            ),
            (
                "<goal source=\"thread\">Go on.</goal> Add tests. <goalpost>kept</goalpost>",
                "Add tests. <goalpost>kept</goalpost>",
            ),
            // A tag with no closing one, as an agent writes an image's two, each
            // in a part of its own.
            ("<image name=[Image #1]>", ""),
            ("</image>", ""),
            // After a block of the agent's, whichever tag is listed first.
            ("<skill>Write plainly.</skill>\n<image name=[Image #1]>", ""),
            // Further in, it's a word the person typed, and so is what follows.
            (
                "Add a <skill> element to the manifest schema and document it.",
                "Add a <skill> element to the manifest schema and document it.",
            ),
            (
                "Replace the <image href=\"a.png\"/> in logo.svg with a path, then run the tests.",
                "Replace the <image href=\"a.png\"/> in logo.svg with a path, then run the tests.",
            ),
            // A block of the agent's after one is still taken out.
            (
                "Document <skill> in the schema.<image name=x>y</image>",
                "Document <skill> in the schema.",
            ),
        ] {
            assert_eq!(without_tags(text, &tags), kept, "{text}");
        }
        assert_eq!(
            inner("a <user_query>Do it.</user_query> b", "user_query"),
            Some("Do it.")
        );
    }

    #[test]
    fn a_failed_keychain_read_says_what_security_said_and_how_it_ended() {
        use std::os::unix::process::ExitStatusExt as _;
        let ended = |code: i32, stdout: &str, stderr: &str| std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.into(),
            stderr: stderr.into(),
        };
        assert_eq!(found(ended(0, "kept\n", "")), Ok(Some(b"kept\n".to_vec())));
        let none = "security: SecKeychainSearchCopyNext: The specified item could not be found in the keychain.\n";
        assert_eq!(found(ended(44, "", none)), Ok(None));
        // A locked Keychain, with no one to ask for its password.
        let locked = "security: SecKeychainSearchCopyNext: User interaction is not allowed.\n";
        assert_eq!(
            found(ended(36, "", locked)),
            Err("security: SecKeychainSearchCopyNext: User interaction is not allowed. (exit status: 36)".to_owned())
        );
        assert_eq!(
            found(ended(1, "", "")),
            Err("security gave no reason (exit status: 1)".to_owned())
        );
    }

    #[test]
    fn toml_mcp_command_ignores_inline_comments_and_keeps_quoted_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(
            &file,
            "[mcp_servers.turnscope]\ncommand = \"/Applications/Turnscope.app/Contents/Helpers/turnscope\" # managed\n",
        )
        .unwrap();
        assert_eq!(
            toml_mcp_command(&file).as_deref(),
            Some("/Applications/Turnscope.app/Contents/Helpers/turnscope")
        );
        std::fs::write(
            &file,
            "[mcp_servers.turnscope]\ncommand = '/Users/me/My Apps/turnscope' # literal\n",
        )
        .unwrap();
        assert_eq!(
            toml_mcp_command(&file).as_deref(),
            Some("/Users/me/My Apps/turnscope")
        );
    }

    #[test]
    fn of_the_keychain_and_the_file_the_newer_sign_in_is_read() {
        let signed = |token: &str, expires: i64| {
            Ok(Some(Credential {
                provider: "google",
                secret: Secret::new(token),
                key: false,
                expires: Some(expires),
                identity: None,
                plan: None,
                refresh: None,
            }))
        };
        let failed = || Err(crate::Error::Failed("made-up failure".to_owned()));
        let read = |kept, file| {
            fresher(kept, file)
                .unwrap()
                .map(|one| one.secret.expose().to_owned())
        };
        assert_eq!(
            read(signed("kept", 1), signed("file", 2)).as_deref(),
            Some("file")
        );
        assert_eq!(
            read(signed("kept", 2), signed("file", 1)).as_deref(),
            Some("kept")
        );
        assert_eq!(read(failed(), signed("file", 1)).as_deref(), Some("file"));
        assert_eq!(read(signed("kept", 1), failed()).as_deref(), Some("kept"));
        let lasting = signed("kept", 0).map(|one| {
            one.map(|kept| Credential {
                expires: None,
                ..kept
            })
        });
        assert_eq!(read(lasting, signed("file", 2)).as_deref(), Some("kept"));
        assert!(fresher(Ok(None), failed()).is_err());
    }
}
