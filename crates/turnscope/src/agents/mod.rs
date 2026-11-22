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
use std::path::{Path, PathBuf};

use base64::Engine as _;
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize as _;
use serde_json::Value;

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

    /// Whether `path` is a login file for this agent: changes to it wake the
    /// engine to refresh stale or failing accounts right away.
    fn login_file(&self, _path: &Path) -> bool {
        false
    }

    /// In-band limit readings from JSON a harness pipes to `turnscope statusline`
    /// on standard input, such as Claude Code's `rate_limits` field.
    fn statusline_limits(&self, _input: &Value, _now: i64) -> Option<InBand> {
        None
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
    /// Live limit snapshots emitted in-band by the agent (such as Codex's
    /// `token_count` events), newest kept per `(provider, scope)`.
    pub limits: Vec<InBand>,
    /// Lines or rows it couldn't read.
    pub skipped: u64,
}

/// Live limit readings emitted directly by an agent, avoiding a separate
/// provider API poll.
pub struct InBand {
    /// The provider as the agent's responses name it (matching `sign_in.provider`).
    pub provider: String,
    /// When the agent recorded this snapshot, in Unix milliseconds.
    pub at: i64,
    /// Optional plan name reported alongside the limits (such as `"plus"`).
    pub plan: Option<String>,
    pub readings: Vec<crate::providers::Reading>,
}

impl Read {
    pub fn session(&mut self, id: &str) -> &mut Session {
        self.sessions.entry(id.to_owned()).or_default()
    }

    /// Record an in-band limit snapshot, keeping the newest timestamp for each
    /// `(provider, scope)` bucket group so re-reading a long file keeps only
    /// its latest snapshot.
    pub fn in_band(&mut self, snapshot: InBand) {
        if snapshot.readings.is_empty() {
            return;
        }
        let scope = snapshot.readings[0].scope.as_deref();
        if let Some(existing) = self.limits.iter_mut().find(|known| {
            known.provider == snapshot.provider
                && known
                    .readings
                    .first()
                    .and_then(|reading| reading.scope.as_deref())
                    == scope
        }) {
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
        if !self.sessions.contains_key(&response.session) {
            self.sessions
                .insert(response.session.clone(), Session::default());
        }
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
    /// Offer a title; a better kind replaces a worse, and a later one of the
    /// same kind the earlier.
    ///
    /// It's kept as written, a key pasted into it too, over taking keys out:
    /// the person gave that text to an agent already, and the agent's own
    /// files hold it beside this database. Of 13,652 titles in one database,
    /// at most 159 had held a key (November 2026).
    pub fn title(&mut self, kind: Title, text: &str) {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !text.is_empty() && self.title.as_ref().is_none_or(|(had, _)| kind >= *had) {
            self.title = Some((kind, text.chars().take(TITLE).collect()));
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
    /// What a provider with non-rotating refresh tokens uses to renew an
    /// expired access token in memory, without writing to the agent's files or
    /// Keychain.
    pub refresh: Option<Refresh>,
}

/// What a provider with non-rotating refresh tokens uses to renew an expired
/// access token in memory.
pub struct Refresh {
    pub token: Secret,
    /// Installed binaries holding the agent's OAuth client, in preference
    /// order.
    pub binaries: Vec<PathBuf>,
}

/// The fresher of two reads of the same login (such as Keychain and a fallback
/// file). A readable login wins over a missing or unreadable second copy; a
/// read error beside no login is returned so a half-written file never signs
/// the account out.
pub fn fresher(
    first: Result<Option<Credential>>,
    second: Result<Option<Credential>>,
) -> Result<Option<Credential>> {
    match (first, second) {
        (Ok(Some(mut a)), Ok(Some(mut b))) => {
            if b.expires > a.expires {
                if b.refresh.is_none() {
                    b.refresh = a.refresh;
                }
                Ok(Some(b))
            } else {
                if a.refresh.is_none() {
                    a.refresh = b.refresh;
                }
                Ok(Some(a))
            }
        }
        (Ok(Some(kept)), Ok(None) | Err(_)) | (Ok(None) | Err(_), Ok(Some(kept))) => Ok(Some(kept)),
        (Ok(None), Ok(None)) => Ok(None),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
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

pub fn reading(path: &Path, error: impl Display) -> Error {
    Error::Failed(format!("reading {}: {error}", path.display()))
}

/// What `read` makes of the SQLite database `file`, an agent's, which is
/// never written: not even the files SQLite keeps beside one.
///
/// A database keeps a write-ahead log, `<file>-wal`, while its agent has it
/// open. With no log beside it, all it holds is in the file, which is then
/// read as one that can't change: read as any other, SQLite makes a log and
/// its index beside it and leaves them, 14 files after reading 7 databases
/// (measured 2026-11-10), and the log's new time makes the next catch-up
/// read the database again. With a log, it's read as any other.
///
/// SQLite takes no lock on a file it's told can't change. So one that
/// changed while it was read, or that its agent opened meanwhile, fails,
/// and is read again at the next catch-up.
pub fn opened<T>(file: &Path, read: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
    let mut log = file.as_os_str().to_owned();
    log.push("-wal");
    let alone = || {
        let file = std::fs::metadata(file).ok()?;
        (!Path::new(&log).exists()).then_some((file.len(), file.modified().ok()))
    };
    let before = file.to_str().zip(alone());
    let db = match before {
        Some((path, _)) => {
            // What would end the path in an address.
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
    // SQLite ends its message with the address it was given, the path again.
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

/// Pass each complete line of `path` from byte `from` to `visit`, and say
/// where the next read starts: past the last line that ended with a newline,
/// since one without is still being written.
pub fn lines(path: &Path, from: u64, mut visit: impl FnMut(&[u8])) -> Result<u64> {
    let failed = |error| reading(path, error);
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

/// What the folder `dir` holds; nothing where it can't be read.
pub fn entries(dir: &Path) -> impl Iterator<Item = PathBuf> + use<> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
}

/// The text of a message's `content`: the string, or its parts' `text`.
pub fn texts<'a>(content: impl serde::Deserializer<'a> + Copy) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Part<'a> {
        #[serde(borrow)]
        text: Option<Cow<'a, str>>,
    }
    if let Ok(text) = String::deserialize(content) {
        return vec![text];
    }
    Vec::<Part>::deserialize(content)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|part| part.text.map(Cow::into_owned))
        .collect()
}

pub fn json_file(path: &Path) -> Result<Option<Value>> {
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
pub fn kept_login(provider: &str, reads: &str, kept: &Value, key_type: &str) -> Login {
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

/// What the Keychain item with `service`, and with `account` where its agent
/// keeps several under one service, holds, as `parse` reads it. `None` when
/// there's no such item.
///
/// Read with `/usr/bin/security`, the tool an agent writes its item with,
/// so no permission prompt appears; it finds the Keychain under `home`.
/// Under a home with none, as a test's, it searches the System keychain
/// alone (`HOME=<empty folder> security list-keychains`, November 2026), so
/// a test never reads a real login.
pub fn keychain<T>(
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

/// What `security` found, by how it ended: the item's value, or `None` when
/// there's no such item (44). For any other end, why: what it wrote on
/// standard error, as "User interaction is not allowed." for a locked
/// Keychain, and how it ended.
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
    let mut folders: Vec<PathBuf> = entries(home)
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.len() > prefix.len() && name.starts_with(prefix))
                && path.join(signed).is_file()
        })
        .collect();
    folders.sort();
    folders.insert(0, home.join(own));
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
///
/// An opening tag with no closing one is the agent's only where the text,
/// its blocks taken out, opens with it, as an agent may write an image's
/// `<image name=[Image #1]>` in a part of its own. Further in, it's a word
/// the person typed, as in `Add a <skill> element to the schema`, and what
/// follows it is theirs too: chosen over taking it out to the end of the
/// text, which cut such a message at the tag. A mention before a block of
/// the same tag still loses the words between them: tags alone don't tell
/// the two apart. (Not measured: no real message was read for it.)
pub fn without_tags(text: &str, tags: &[&str]) -> String {
    let mut text = text.to_owned();
    for tag in tags {
        let close = format!("</{tag}>");
        while let Some((start, inside)) = opening(&text, tag)
            && let Some(end) = text[inside..].find(&close)
        {
            text.replace_range(start..inside + end + close.len(), "");
        }
        // One left closing a block an agent split across parts.
        text = text.replace(&close, "");
    }
    let text = text.trim();
    // One left opening such a block, which the text then opens with.
    let opens = tags
        .iter()
        .any(|tag| opening(text, tag).is_some_and(|(start, _)| start == 0));
    if opens {
        String::new()
    } else {
        text.to_owned()
    }
}

/// The command at `pointer` in the JSON file at `path`, where an agent
/// keeps Turnscope's MCP server.
pub fn json_mcp_command(path: &Path, pointer: &str) -> Option<String> {
    let config = json_file(path).ok()??;
    Some(config.pointer(pointer)?.as_str()?.to_owned())
}

/// The `command` of `[mcp_servers.turnscope]` in the TOML file at `path`, as
/// an agent's own command writes it: one key to a line.
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

/// `lines` written to `file` as JSON Lines, in a folder made for it.
#[cfg(test)]
fn jsonl(file: PathBuf, lines: &[Value]) -> PathBuf {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    std::fs::write(&file, text).unwrap();
    file
}

/// `login`'s credential as its provider, kind, expiry and secret, to compare
/// in a test.
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

    #[test]
    fn a_title_is_kept_on_one_line_and_cut_at_200_characters() {
        let mut session = Session::default();
        session.title(Title::Prompt, &format!("Fix\n  the {}", "é".repeat(300)));
        let (_, title) = session.title.as_ref().unwrap();
        assert!(title.starts_with("Fix the éé"), "{title}");
        assert_eq!(title.chars().count(), 200);
        session.title(Title::Named, " \n ");
        assert_eq!(session.title.as_ref().unwrap().1.chars().count(), 200);
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
    fn fresher_prefers_later_expiry_and_keeps_errors_from_signing_out() {
        let cred = |secret: &str, expires: Option<i64>| Credential {
            provider: "google",
            secret: Secret::new(secret),
            key: false,
            expires,
            identity: None,
            plan: None,
            refresh: None,
        };
        let picked = fresher(
            Ok(Some(cred("old", Some(100)))),
            Ok(Some(cred("new", Some(200)))),
        )
        .unwrap()
        .unwrap();
        assert_eq!(picked.secret.expose(), "new");
        let picked = fresher(
            Err(Error::Failed("locked".into())),
            Ok(Some(cred("file", Some(100)))),
        )
        .unwrap()
        .unwrap();
        assert_eq!(picked.secret.expose(), "file");
        assert!(fresher(Ok(None), Err(Error::Failed("corrupt".into()))).is_err());
        assert!(fresher(Ok(None), Ok(None)).unwrap().is_none());
    }
}
