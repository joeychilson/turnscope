//! The agents Turnscope reads, and what reading one produces.
//!
//! Adding an agent means one new module here implementing [`AgentReader`], a
//! line in [`readers`], an [`Agent`] with its key, name, resume command and
//! folder variable, and its own folder in `folders::own`. Nothing else in
//! the engine knows any agent's format.
//! Each reader's module documentation records what was measured of its
//! agent's files, with the numbers and the date, since those findings are
//! what its rules rest on.
//!
//! A reader records what an artifact holds through a [`Batch`]: reports of
//! responses' usage, what it says of sessions, links between them, the
//! agent's own totals, what was said, which sessions' transcripts it holds,
//! and what didn't read cleanly. It brings every figure to the one form of
//! [`Tokens`], in which no count includes another, so Codex's input, which
//! includes cached input, has the cached input taken out, and Anthropic's,
//! which doesn't, is taken as it is. Whatever `read` records as said has to
//! be what `conversation` shows as the person's or a model's entries.
//!
//! **Counts a format always gives are required.** A record without one, such
//! as a response with no output count, is invalid and noted, since an agent
//! that stopped writing it would otherwise lose usage without a word. A count
//! a format may leave out is zero when absent: how much of the output was
//! reasoning, how many calls a turn made, what was read from or written to a
//! cache where the provider gives that only when it applies, and what only
//! newer versions count, such as Claude Code's thinking and web searches. A
//! cost left out is unknown.
//!
//! **What a reader notes, and what it passes over.** Every record kind a
//! reader meets is one it reads, or one on its list of kinds it knows hold
//! nothing it counts or says, measured on real history; any other is
//! noted as an unknown record. So is a kind within a record where the kind
//! decides what is counted, said or shown: a subtype of an event stream
//! (Codex's `event_msg`, Claude Code's `system` records, Grok Build's
//! updates), a kind of item or of message part, a field of a usage object or
//! of the record that carries one, which may be a token that is charged or
//! change what the counts mean, and a value that decides a price, such as a
//! service tier. Each is noted by name, cut to [`KIND_NAME`] characters, for
//! the first [`UNKNOWN_KINDS`] names in one read, and the rest together as
//! `… of other kinds`, so no artifact can make diagnostics without bound.
//! Left unlisted on purpose is what a record holds whose kind is, as a whole,
//! neither counted, said nor shown: Claude Code's `attachment` records, a
//! Codex `turn_context`'s settings besides the model and service tier,
//! OpenCode's session and message metadata, and the body of a tool's call or
//! result, which is shown as it is. A new kind there changes nothing
//! Turnscope reports.
//!
//! **One pass over a line.** Readers read each line once, borrowing from it:
//! the fields they use, and those of the objects within it they look into,
//! such as a Codex line's payload or a Claude Code line's message
//! ([`Object`]), each held unparsed until needed, and the rest only skimmed
//! over. Most of a line is what usage never needs: half of the bytes of
//! Codex's 25 largest rollouts were events of items completed (2026-09-29).
//! Holding an object unparsed and parsing it again read it twice; reading
//! it in the line's one pass took the CPU time of reading this Mac's 3.6 GB
//! of Codex history from 3.4 s to 2.2 s, and of its 3.9 GB of Claude Code's
//! from 3.4 s to 3.0 s (2026-09-29).
//!
//! **What every reader's tests show**, on small files shaped like the agent's
//! own records, with totals worked out by hand: what it records as said is
//! what its conversation shows (`agent::tests::assert_said_as_shown`); a count
//! missing, out of range or at odds with another invalidates its record and
//! no other; and a record, field or value it doesn't know is a diagnostic,
//! not silence. Beside them, `tests/reading_history.rs` reads every agent's
//! logs cut at every line and read on, which comes to what reading them
//! whole does; reads history in another order with the same answers; and
//! reads a file rewritten, one changed without growing, one cut short and
//! written past its old end, one that comes back, and a database changed
//! only in its write-ahead log, each correctly.

mod claude;
mod codex;
mod grok;
pub(crate) mod opencode;
mod pi;

use std::borrow::Cow;
use std::collections::btree_map::Entry as Slot;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use serde::de::value::MapAccessDeserializer;
use serde::de::{DeserializeOwned, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use crate::error::{Error, Result};
use crate::jsonl;
use crate::session::{SessionFacts, SessionKey, SessionLink, shell_word};
use crate::time::Instant;
use crate::transcript::{Speaker, Transcript, prompt_parts};
use crate::usage::{LARGEST_COUNT, Tokens, Usd};

/// A coding agent whose history Turnscope reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Agent {
    /// Anthropic's Claude Code.
    ClaudeCode,
    /// OpenAI's Codex.
    Codex,
    /// OpenCode.
    OpenCode,
    /// Pi.
    Pi,
    /// xAI's Grok Build.
    Grok,
}

impl Agent {
    /// Every agent, in the order they are listed.
    pub const ALL: [Agent; 5] = [
        Agent::ClaudeCode,
        Agent::Codex,
        Agent::OpenCode,
        Agent::Pi,
        Agent::Grok,
    ];

    /// The agent's stable key, as stored and as written in session keys.
    pub fn key(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Codex => "codex",
            Agent::OpenCode => "opencode",
            Agent::Pi => "pi",
            Agent::Grok => "grok",
        }
    }

    /// The agent with `key`.
    pub fn from_key(key: &str) -> Option<Agent> {
        Agent::ALL.into_iter().find(|agent| agent.key() == key)
    }

    /// The shell command that picks up the agent's session `native` again.
    pub(crate) fn resume(self, native: &str) -> String {
        let id = shell_word(native);
        match self {
            Agent::ClaudeCode => format!("claude --resume {id}"),
            Agent::Codex => format!("codex resume {id}"),
            Agent::OpenCode => format!("opencode --session {id}"),
            Agent::Pi => format!("pi --session {id}"),
            Agent::Grok => format!("grok --resume {id}"),
        }
    }

    /// The environment variable that points the agent at a folder other
    /// than its own ([`crate::Folder`]), as a second account of one
    /// subscription is kept in; `None` for an agent that keeps everything in
    /// one place.
    pub fn folder_variable(self) -> Option<&'static str> {
        match self {
            Agent::ClaudeCode => Some("CLAUDE_CONFIG_DIR"),
            Agent::Codex => Some("CODEX_HOME"),
            Agent::Pi => Some("PI_CODING_AGENT_DIR"),
            Agent::OpenCode | Agent::Grok => None,
        }
    }

    /// The agent's name, as people know it.
    pub fn name(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "Claude Code",
            Agent::Codex => "Codex",
            Agent::OpenCode => "OpenCode",
            Agent::Pi => "Pi",
            Agent::Grok => "Grok Build",
        }
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// How an artifact changes, which decides how it is read again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtifactKind {
    /// Only ever appended to, a line at a time. Reading resumes where the last
    /// read stopped.
    Log,
    /// Rewritten whole. Every change is read from the start.
    Document,
    /// A SQLite database another program keeps. Every change is read on from
    /// the reader's own marks, such as the latest update time it has seen.
    Database,
}

impl ArtifactKind {
    /// The kind as the ledger stores it.
    pub(crate) fn key(self) -> &'static str {
        match self {
            ArtifactKind::Log => "log",
            ArtifactKind::Document => "document",
            ArtifactKind::Database => "database",
        }
    }
}

/// Where a reader stopped in an artifact, and what it needs to carry on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Checkpoint {
    /// The byte offset just past the last complete line read; zero before any.
    pub offset: u64,
    /// The reader's own state, in a form only that reader and version read.
    pub state: Vec<u8>,
}

impl Checkpoint {
    /// Where a reader stopped, at `offset`, with its own `state`, which it
    /// calls `what`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Corrupt`] when the state cannot be written.
    fn at(offset: u64, state: &impl Serialize, what: &'static str) -> Result<Checkpoint> {
        let state =
            serde_json::to_vec(state).map_err(|error| Error::corrupt(what, error.to_string()))?;
        Ok(Checkpoint { offset, state })
    }

    /// The reader's own state kept here, which it calls `what`, or its first
    /// state before any is.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Corrupt`] when the state kept does not read.
    fn state<S: DeserializeOwned + Default>(&self, what: &'static str) -> Result<S> {
        if self.state.is_empty() {
            return Ok(S::default());
        }
        serde_json::from_slice(&self.state).map_err(|error| Error::corrupt(what, error.to_string()))
    }
}

/// Reads one agent's history.
pub(crate) trait AgentReader: Send + Sync {
    /// The agent this reads.
    fn agent(&self) -> Agent;

    /// The version of this reader's interpretation.
    ///
    /// Increase it whenever a change would read an existing artifact
    /// differently. Every artifact the new version can reach is then read
    /// again from the start; what was read from artifacts that have since
    /// disappeared is kept as it was read.
    fn version(&self) -> u32;

    /// Where this agent keeps history in `folder`, one of its folders
    /// ([`crate::Folder`]): directories, searched throughout, or single
    /// files. One that does not exist is passed over.
    fn roots(&self, folder: &Path) -> Vec<PathBuf>;

    /// What `path`, found under `root` or being `root` itself, is to this
    /// reader, or `None` when it is not one of the agent's artifacts.
    fn classify(&self, root: &Path, path: &Path) -> Option<ArtifactKind>;

    /// Read `path` from `from`, putting what it holds into `batch`, and say
    /// where to start next time.
    ///
    /// A log's reader reads only complete lines, so a line still being
    /// written is read in full next time. Anything the reader cannot make
    /// sense of goes into the batch as a diagnostic; only a failure to read the
    /// artifact at all is an error.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Io`] when the artifact cannot be read.
    fn read(&self, path: &Path, from: &Checkpoint, batch: &mut Batch) -> Result<Checkpoint>;

    /// Read `session`'s conversation from `artifacts`: every artifact that
    /// speaks of the session, is still there, and holds its transcript as far
    /// as its last read said, in path order. A reader uses those that hold
    /// the session's own conversation. A conversation with nothing in it is
    /// empty; one no artifact holds any more is gone.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Gone`] when none of `artifacts` holds the
    /// session's conversation now, as when its agent deleted it, and
    /// [`crate::Error::Io`] or [`crate::Error::Database`] when an artifact
    /// cannot be read.
    fn conversation(&self, session: &SessionKey, artifacts: &[PathBuf]) -> Result<Transcript>;
}

/// The folder of `reader`'s agent that the artifact at `path` is kept in:
/// the nearest folder above it among whose roots it is one of the agent's
/// artifacts. `None` for a path no folder's roots hold.
pub(crate) fn folder_of(reader: &dyn AgentReader, path: &Path) -> Option<PathBuf> {
    path.ancestors().skip(1).find_map(|folder| {
        reader
            .roots(folder)
            .iter()
            .any(|root| path.starts_with(root) && reader.classify(root, path).is_some())
            .then(|| folder.to_path_buf())
    })
}

/// Every agent's reader.
pub(crate) fn readers() -> Vec<Box<dyn AgentReader>> {
    vec![
        Box::new(claude::ClaudeCode::default()),
        Box::new(codex::Codex::default()),
        Box::new(opencode::OpenCode),
        Box::new(pi::Pi),
        Box::new(grok::Grok),
    ]
}

/// Something the person or a model said, kept for search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Said {
    /// The session it was said in.
    pub session: SessionKey,
    /// When, where the agent recorded it.
    pub at: Option<Instant>,
    /// What was said.
    pub text: String,
    /// The message it is part of, for a message that can be read again as
    /// it changes: reading it again replaces what it said before.
    pub message: Option<String>,
    /// Where it sits in its session's numbered history, for an agent that
    /// numbers it and can take it back from a point, as Codex does: see
    /// [`Resumed`].
    pub ordinal: Option<i64>,
}

/// Where an artifact takes up its session's numbered history, as a Codex
/// rollout that resumes a thread does.
///
/// With a `base`, it carries on from there: whatever the session said before
/// `at`, numbered `base` or later, was taken back, as by a rewind, and is
/// neither shown nor found. Without one, it numbers the history afresh from
/// `at`, so no later resumption takes back anything said before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Resumed {
    /// When the artifact took the history up.
    pub at: Instant,
    /// The ordinal the history before it is kept up to, exclusive.
    pub base: Option<i64>,
}

impl Resumed {
    /// Whether what a session said at `at`, numbered `ordinal`, was taken
    /// back by one of `resumed`, every resumption of the session: one from
    /// an ordinal at or before it, made after it was said, with no fresh
    /// numbering between the two. What has no ordinal or no time is never
    /// taken back.
    ///
    /// The ledger's search applies the same rule in SQL, to what was said
    /// and the resumptions every artifact recorded, so that search finds
    /// only what the conversation shows, whatever order the artifacts were
    /// read in.
    pub(crate) fn takes_back(
        resumed: &[Resumed],
        ordinal: Option<i64>,
        at: Option<Instant>,
    ) -> bool {
        let (Some(ordinal), Some(at)) = (ordinal, at) else {
            return false;
        };
        resumed.iter().any(|cut| {
            cut.base.is_some_and(|base| ordinal >= base)
                && at < cut.at
                && !resumed
                    .iter()
                    .any(|fresh| fresh.base.is_none() && at < fresh.at && fresh.at < cut.at)
        })
    }
}

/// One report of usage for one response, as one artifact gave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Observation {
    /// The response, identified so that every artifact reporting it gives the
    /// same key. Unique within the agent.
    pub response: String,
    /// The session the response belongs to.
    pub session: SessionKey,
    /// Whether this report is a copy of a response that belongs to another
    /// artifact, as a forked subagent copies its parent's last response. A copy
    /// adds to what is known of the response but never decides whose it is.
    pub copy: bool,
    /// When the response was reported.
    pub at: Instant,
    /// The provider that served it, such as `anthropic` or `openrouter`.
    pub provider: String,
    /// The model, as the agent recorded it; empty when it names none yet,
    /// which is unknown, and gives way to any model a report names.
    pub model: String,
    /// What it used.
    pub tokens: Tokens,
    /// The largest prompt among the requests it covers, which sets the price
    /// tier where a provider charges more for long contexts. For one response
    /// it is [`Tokens::prompt`]. Grok Build counts a turn's requests only
    /// together, so for it this is their average.
    pub prompt: u64,
    /// Web searches the provider ran for it, which are charged apart from tokens.
    pub web_searches: u64,
    /// What the agent says it cost, when it says.
    pub cost: Option<Usd>,
    /// Whether it was served ahead of standard traffic at a higher rate, as
    /// Claude Code's fast mode and OpenAI's priority processing are.
    pub priority: bool,
}

impl Observation {
    /// Fold in another report of the same response, from the same artifact.
    fn merge(&mut self, other: Observation) {
        // A report from the response's own session decides whose it is, over a
        // copy's.
        if self.copy && !other.copy {
            self.session = other.session;
            self.copy = false;
        }
        // A model named stands over none, as the ledger combines them.
        if self.model.is_empty() {
            self.model = other.model;
        }
        self.tokens.merge(&other.tokens);
        self.prompt = self.prompt.max(other.prompt);
        self.web_searches = self.web_searches.max(other.web_searches);
        self.cost = self.cost.max(other.cost);
        self.at = self.at.max(other.at);
        self.priority |= other.priority;
    }
}

/// Which sessions an agent's own totals cover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReportScope {
    /// The session alone.
    Session,
    /// The session and every subagent under it.
    Tree,
}

impl ReportScope {
    /// The scope as the ledger stores it.
    pub(crate) fn key(self) -> &'static str {
        match self {
            ReportScope::Session => "session",
            ReportScope::Tree => "tree",
        }
    }

    /// The scope with `key`.
    pub(crate) fn from_key(key: &str) -> Option<ReportScope> {
        [ReportScope::Session, ReportScope::Tree]
            .into_iter()
            .find(|scope| scope.key() == key)
    }
}

/// An agent's own total for one model within a session, or for all of its
/// models together.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModelTotal {
    /// The model, as the agent recorded it; `None` when the total covers every
    /// model the session used.
    pub model: Option<String>,
    /// Input neither read from nor written to the cache.
    pub input: u64,
    /// Input read from the cache.
    pub cache_read: u64,
    /// Input written to the cache, for however long.
    pub cache_write: u64,
    /// Output, reasoning included.
    pub output: u64,
    /// Web searches.
    pub web_searches: u64,
    /// What the agent says it cost, when it says.
    pub cost: Option<Usd>,
}

/// An agent's own totals for a session, which the transcripts are checked
/// against. A later report of a session replaces an earlier one whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SessionReport {
    /// The session reported on.
    pub session: SessionKey,
    /// Which sessions the totals cover.
    pub scope: ReportScope,
    /// When the totals were written, as near as the artifact tells.
    pub at: Option<Instant>,
    /// The totals, one per model.
    pub models: Vec<ModelTotal>,
}

/// What kind of trouble a diagnostic records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DiagnosticKind {
    /// A line that is not JSON, or not the shape its kind should have.
    Unreadable,
    /// A record of a kind the reader does not know.
    UnknownRecord,
    /// A field or value the reader does not know, where knowing matters.
    UnknownField,
    /// A value out of range or inconsistent with another; the record it was in
    /// is not counted.
    Invalid,
    /// Usage an agent spent and never recorded, in a record the reader
    /// knows, as a turn cancelled before its usage was written: known to be
    /// there, and unknown in amount. It is understood, unlike the others.
    Unrecorded,
}

impl DiagnosticKind {
    /// The kind as the ledger stores it.
    pub fn key(self) -> &'static str {
        match self {
            DiagnosticKind::Unreadable => "unreadable",
            DiagnosticKind::UnknownRecord => "unknown-record",
            DiagnosticKind::UnknownField => "unknown-field",
            DiagnosticKind::Invalid => "invalid",
            DiagnosticKind::Unrecorded => "unrecorded",
        }
    }

    /// The kind with `key`.
    pub(crate) fn from_key(key: &str) -> Option<DiagnosticKind> {
        [
            DiagnosticKind::Unreadable,
            DiagnosticKind::UnknownRecord,
            DiagnosticKind::UnknownField,
            DiagnosticKind::Invalid,
            DiagnosticKind::Unrecorded,
        ]
        .into_iter()
        .find(|kind| kind.key() == key)
    }

    /// Whether it records something the reader didn't understand, as every
    /// kind but [`DiagnosticKind::Unrecorded`] does.
    pub fn misunderstood(self) -> bool {
        self != DiagnosticKind::Unrecorded
    }
}

/// How many kinds of record or field one read notes by name as unknown; any
/// more are noted together, so an artifact of many made-up kinds can't make
/// as many diagnostics.
const UNKNOWN_KINDS: usize = 32;

/// How many characters of an unknown kind's name a diagnostic keeps.
const KIND_NAME: usize = 64;

/// How often one kind of trouble came up, and where first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Occurrences {
    /// How many times.
    pub count: u64,
    /// The byte offset of the first line it came up on.
    pub first: u64,
}

/// Everything one read of an artifact found, combined as it was found.
///
/// Repeated reports combine here by the same rules the ledger uses to combine
/// them with what earlier reads found, so reading an artifact in one pass or in
/// many gives the same result.
#[derive(Debug, Default)]
pub(crate) struct Batch {
    said: Vec<Said>,
    /// The messages given again as they changed, whose earlier words are
    /// forgotten whatever they say now.
    given: BTreeSet<String>,
    /// Each session's messages, all of them, for the sessions whose every
    /// message this read gave: what was said in any other is forgotten.
    messages: BTreeMap<SessionKey, BTreeSet<String>>,
    /// Where the artifact took up each session's numbered history.
    resumed: BTreeMap<SessionKey, Resumed>,
    observations: BTreeMap<String, Observation>,
    sessions: BTreeMap<SessionKey, SessionFacts>,
    links: BTreeMap<SessionKey, SessionLink>,
    reports: BTreeMap<SessionKey, SessionReport>,
    diagnostics: BTreeMap<(DiagnosticKind, String), Occurrences>,
    /// The sessions whose transcripts the artifact holds, when it says so;
    /// `None` when it holds that of every session it speaks of.
    holding: Option<BTreeSet<SessionKey>>,
    /// How many kinds this read has noted by name as unknown, up to
    /// [`UNKNOWN_KINDS`].
    unknown_kinds: usize,
}

impl Batch {
    /// Record something the person or a model said in a session, for search.
    /// What an agent injected and what tools returned are not recorded.
    pub(crate) fn say(&mut self, session: &SessionKey, at: Option<Instant>, text: &str) {
        self.push_said(session, at, text, None, None);
    }

    /// Record what a model said, as [`Batch::say`] does, at `ordinal` in its
    /// session's numbered history, which a later artifact can take back
    /// ([`Batch::resume`]).
    pub(crate) fn say_numbered(
        &mut self,
        session: &SessionKey,
        at: Option<Instant>,
        ordinal: Option<i64>,
        text: &str,
    ) {
        self.push_said(session, at, text, None, ordinal);
    }

    /// Record part of what `message` says, for a message that can be read
    /// again as it changes. What a read gives of the message replaces
    /// everything it said before in this artifact.
    pub(crate) fn say_in(
        &mut self,
        message: &str,
        session: &SessionKey,
        at: Option<Instant>,
        text: &str,
    ) {
        self.give(message);
        self.push_said(session, at, text, Some(message), None);
    }

    /// Record that this read gave `message`, one that can be read again as
    /// it changes, so everything it said before in this artifact is
    /// forgotten, even when it now says nothing.
    pub(crate) fn give(&mut self, message: &str) {
        if !self.given.contains(message) {
            self.given.insert(message.to_owned());
        }
    }

    /// Record what the person said in a message sent as theirs, divided as
    /// [`prompt_parts`] divides it: the search side of
    /// [`crate::transcript::Builder::prompt`].
    pub(crate) fn prompt(&mut self, session: &SessionKey, at: Option<Instant>, text: &str) {
        self.push_prompt(session, at, text, None, None);
    }

    /// Record what the person said, as [`Batch::prompt`] does, at `ordinal`
    /// in its session's numbered history, which a later artifact can take
    /// back ([`Batch::resume`]).
    pub(crate) fn prompt_numbered(
        &mut self,
        session: &SessionKey,
        at: Option<Instant>,
        ordinal: Option<i64>,
        text: &str,
    ) {
        self.push_prompt(session, at, text, None, ordinal);
    }

    /// Record what the person said in part of `message`, as [`Batch::prompt`]
    /// does, for a message [`Batch::say_in`] records.
    pub(crate) fn prompt_in(
        &mut self,
        message: &str,
        session: &SessionKey,
        at: Option<Instant>,
        text: &str,
    ) {
        self.give(message);
        self.push_prompt(session, at, text, Some(message), None);
    }

    /// Record that `messages` are every message `session` holds in the
    /// artifact now: what the artifact said before in any other message of
    /// the session is forgotten, as for one its agent deleted when the
    /// person took it back, since the conversation no longer shows it.
    pub(crate) fn hold_messages(
        &mut self,
        session: &SessionKey,
        messages: impl IntoIterator<Item = String>,
    ) {
        self.messages
            .entry(session.clone())
            .or_default()
            .extend(messages);
    }

    /// Record where the artifact took up `session`'s numbered history. An
    /// artifact takes up each session's once, where it opens.
    pub(crate) fn resume(&mut self, session: &SessionKey, resumed: Resumed) {
        self.resumed.insert(session.clone(), resumed);
    }

    fn push_prompt(
        &mut self,
        session: &SessionKey,
        at: Option<Instant>,
        text: &str,
        message: Option<&str>,
        ordinal: Option<i64>,
    ) {
        for (speaker, part) in prompt_parts(text) {
            if speaker == Speaker::User {
                self.push_said(session, at, &part, message, ordinal);
            }
        }
    }

    fn push_said(
        &mut self,
        session: &SessionKey,
        at: Option<Instant>,
        text: &str,
        message: Option<&str>,
        ordinal: Option<i64>,
    ) {
        let text = text.trim();
        if !text.is_empty() {
            self.said.push(Said {
                session: session.clone(),
                at,
                text: text.to_owned(),
                message: message.map(str::to_owned),
                ordinal,
            });
        }
    }

    /// Roughly how many bytes it holds: what was said, and a record's worth
    /// for everything else. Enough to bound what waits to be written, not an
    /// account of every byte.
    pub(crate) fn weight(&self) -> usize {
        const RECORD: usize = 256;
        let said: usize = self
            .said
            .iter()
            .map(|said| said.text.len() + said.message.as_ref().map_or(0, String::len) + RECORD)
            .sum();
        let others = self.records().saturating_sub(self.said.len());
        said.saturating_add(others.saturating_mul(RECORD))
    }

    /// What was said, in the order read.
    pub(crate) fn said(&self) -> impl Iterator<Item = &Said> {
        self.said.iter()
    }

    /// The messages this read gave, whose earlier words it replaces.
    pub(crate) fn given(&self) -> impl Iterator<Item = &str> {
        self.given.iter().map(String::as_str)
    }

    /// The sessions whose every message this read gave, with those messages
    /// ([`Batch::hold_messages`]).
    pub(crate) fn held_messages(&self) -> impl Iterator<Item = (&SessionKey, &BTreeSet<String>)> {
        self.messages.iter()
    }

    /// Where the artifact took up sessions' numbered history
    /// ([`Batch::resume`]).
    pub(crate) fn resumed(&self) -> impl Iterator<Item = (&SessionKey, &Resumed)> {
        self.resumed.iter()
    }

    /// Record a report of a response's usage.
    pub(crate) fn observe(&mut self, observation: Observation) {
        match self.observations.entry(observation.response.clone()) {
            Slot::Vacant(entry) => {
                entry.insert(observation);
            }
            Slot::Occupied(mut entry) => entry.get_mut().merge(observation),
        }
    }

    /// Facts about a session, to add to in place.
    pub(crate) fn session_mut(&mut self, key: &SessionKey) -> &mut SessionFacts {
        self.sessions.entry(key.clone()).or_default()
    }

    /// Record where a session came from. A later link for the same session
    /// replaces an earlier one.
    pub(crate) fn link(&mut self, link: SessionLink) {
        self.links.insert(link.child.clone(), link);
    }

    /// Record an agent's own totals for a session, replacing any earlier ones.
    pub(crate) fn report(&mut self, report: SessionReport) {
        self.reports.insert(report.session.clone(), report);
    }

    /// Record that the artifact holds the transcripts of `sessions`, and of
    /// no other session it speaks of in this read or spoke of before: one
    /// its agent deleted from a database that holds many, or one whose
    /// conversation is kept in another of the agent's files. A session whose
    /// transcript no artifact holds any more can't be opened, and what was
    /// read of it is kept.
    pub(crate) fn hold_only(&mut self, sessions: impl IntoIterator<Item = SessionKey>) {
        self.holding
            .get_or_insert_with(BTreeSet::new)
            .extend(sessions);
    }

    /// Note each field of `object` not among `known`, as `within.field`: a
    /// field an agent adds may be a kind of token it is charged for.
    pub(crate) fn note_unknown_fields(
        &mut self,
        object: &Map<String, Value>,
        known: &[&str],
        within: &str,
        offset: u64,
    ) {
        for field in object.keys() {
            if !known.contains(&field.as_str()) {
                self.note_named(DiagnosticKind::UnknownField, within, ".", field, offset);
            }
        }
    }

    /// Note a kind of `what`, such as `record type`, named `name`, that the
    /// reader doesn't know, as `what name`.
    pub(crate) fn note_unknown(&mut self, what: &str, name: &str, offset: u64) {
        self.note_named(DiagnosticKind::UnknownRecord, what, " ", name, offset);
    }

    /// Note a `what`, such as `content block`, of `kind` when it is not among
    /// `known`, as [`Batch::note_unknown`] does, or as unreadable when it has
    /// no kind.
    pub(crate) fn note_kind(
        &mut self,
        kind: Option<&str>,
        known: &[&str],
        what: &str,
        offset: u64,
    ) {
        match kind {
            Some(kind) if known.contains(&kind) => {}
            Some(kind) => self.note_unknown(what, kind, offset),
            None => self.note(
                DiagnosticKind::Unreadable,
                format!("a {what} with no type"),
                offset,
            ),
        }
    }

    /// Note each part of `content`, a list of parts, of a kind not among
    /// `known`, as [`Batch::note_kind`] does for a `what`. Text, or nothing,
    /// has no parts; content of any other shape is unreadable, as `misshapen`
    /// says.
    pub(crate) fn note_parts(
        &mut self,
        content: Option<&Value>,
        known: &[&str],
        what: &str,
        misshapen: &str,
        offset: u64,
    ) {
        let parts = match content {
            Some(Value::Array(parts)) => parts,
            None | Some(Value::Null | Value::String(_)) => return,
            Some(_) => return self.note(DiagnosticKind::Unreadable, misshapen, offset),
        };
        for part in parts {
            let kind = part.get("type").and_then(Value::as_str);
            self.note_kind(kind, known, what, offset);
        }
    }

    /// Note a value of the field `what`, such as `usage.speed`, that the
    /// reader doesn't know, as `what value`.
    pub(crate) fn note_unknown_value(&mut self, what: &str, value: &str, offset: u64) {
        self.note_named(DiagnosticKind::UnknownField, what, " ", value, offset);
    }

    /// Note a record not counted because of `what`, such as `a finished turn
    /// with no usage, stop_reason`, with the value the agent gave for it, as
    /// `what value`, bounded as [`Batch::note_unknown`] bounds a name.
    pub(crate) fn note_invalid_value(&mut self, what: &str, value: &str, offset: u64) {
        self.note_named(DiagnosticKind::Invalid, what, " ", value, offset);
    }

    /// Note trouble of `kind` with the thing of `what` called `name`, as
    /// `what`, `between` and `name`: a name cut to [`KIND_NAME`] characters,
    /// and, past [`UNKNOWN_KINDS`] names in one read, no name, as `what`
    /// `of other kinds`, so what an agent writes can't make diagnostics
    /// without bound.
    fn note_named(
        &mut self,
        kind: DiagnosticKind,
        what: &str,
        between: &str,
        name: &str,
        offset: u64,
    ) {
        let name: String = name
            .chars()
            .take(KIND_NAME)
            .map(|character| {
                if character.is_control() {
                    '\u{fffd}'
                } else {
                    character
                }
            })
            .collect();
        let detail = format!("{what}{between}{name}");
        let seen = self.diagnostics.contains_key(&(kind, detail.clone()));
        if seen || self.unknown_kinds < UNKNOWN_KINDS {
            self.unknown_kinds += usize::from(!seen);
            self.note(kind, detail, offset);
        } else {
            self.note(kind, format!("{what} of other kinds"), offset);
        }
    }

    /// Record trouble found at byte `offset`.
    pub(crate) fn note(&mut self, kind: DiagnosticKind, detail: impl Into<String>, offset: u64) {
        self.diagnostics
            .entry((kind, detail.into()))
            .and_modify(|seen| {
                seen.count = seen.count.saturating_add(1);
                seen.first = seen.first.min(offset);
            })
            .or_insert(Occurrences {
                count: 1,
                first: offset,
            });
    }

    /// The reports of usage, one per response.
    pub(crate) fn observations(&self) -> impl Iterator<Item = &Observation> {
        self.observations.values()
    }

    /// The sessions spoken of, with what was said of each.
    pub(crate) fn sessions(&self) -> impl Iterator<Item = (&SessionKey, &SessionFacts)> {
        self.sessions.iter()
    }

    /// Where sessions came from.
    pub(crate) fn links(&self) -> impl Iterator<Item = &SessionLink> {
        self.links.values()
    }

    /// The agents' own totals.
    pub(crate) fn reports(&self) -> impl Iterator<Item = &SessionReport> {
        self.reports.values()
    }

    /// The trouble found, by kind and detail.
    pub(crate) fn diagnostics(
        &self,
    ) -> impl Iterator<Item = (&(DiagnosticKind, String), &Occurrences)> {
        self.diagnostics.iter()
    }

    /// The sessions whose transcripts the artifact holds, when it said which
    /// with [`Batch::hold_only`].
    pub(crate) fn holding(&self) -> Option<&BTreeSet<SessionKey>> {
        self.holding.as_ref()
    }

    /// How many records writing it takes, near enough to weigh one batch
    /// against another.
    pub(crate) fn records(&self) -> usize {
        self.said.len()
            + self.given.len()
            + self.messages.values().map(BTreeSet::len).sum::<usize>()
            + self.resumed.len()
            + self.observations.len()
            + self.sessions.len()
            + self.links.len()
            + self.reports.len()
            + self.diagnostics.len()
            + self.holding.as_ref().map_or(0, BTreeSet::len)
    }
}

/// A count an agent's format always writes: `None` when it is absent, null,
/// or not a whole number from zero to [`LARGEST_COUNT`]. An agent that
/// stopped writing it would otherwise lose usage without a word.
fn count(value: Option<&Value>) -> Option<u64> {
    value?.as_u64().filter(|count| *count <= LARGEST_COUNT)
}

/// A count an agent may leave out, as some of its versions do or where it
/// does not apply: zero when absent or null, and otherwise as [`count`]
/// reads it.
fn optional_count(value: Option<&Value>) -> Option<u64> {
    match value {
        None | Some(Value::Null) => Some(0),
        value => count(value),
    }
}

/// A cost in dollars as an agent writes it: `Some(None)` when absent or
/// null, which is unknown, and `None` when it is not a number [`Usd`] can
/// hold.
fn cost(value: Option<&Value>) -> Option<Option<Usd>> {
    match value {
        None | Some(Value::Null) => Some(None),
        Some(value) => value.as_f64().and_then(Usd::from_dollars).map(Some),
    }
}

/// A field of a record read as `T` when it is an object, in the same pass
/// over the record, and as `None` when it is any other value, so a field of
/// an unexpected type costs that field, not the record. `T` holds its own
/// fields unparsed, so that no object fails to read as it: a failure within
/// the object would be the record's.
struct Object<T>(Option<T>);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Object<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Shape<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for Shape<T> {
            type Value = Object<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("any value")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Object<T>, A::Error> {
                T::deserialize(MapAccessDeserializer::new(map)).map(|object| Object(Some(object)))
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut items: A,
            ) -> std::result::Result<Object<T>, A::Error> {
                while items.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Object(None))
            }

            fn visit_bool<E>(self, _: bool) -> std::result::Result<Object<T>, E> {
                Ok(Object(None))
            }

            fn visit_i64<E>(self, _: i64) -> std::result::Result<Object<T>, E> {
                Ok(Object(None))
            }

            fn visit_u64<E>(self, _: u64) -> std::result::Result<Object<T>, E> {
                Ok(Object(None))
            }

            fn visit_f64<E>(self, _: f64) -> std::result::Result<Object<T>, E> {
                Ok(Object(None))
            }

            fn visit_str<E>(self, _: &str) -> std::result::Result<Object<T>, E> {
                Ok(Object(None))
            }

            fn visit_unit<E>(self) -> std::result::Result<Object<T>, E> {
                Ok(Object(None))
            }
        }
        deserializer.deserialize_any(Shape(PhantomData))
    }
}

/// A field held unparsed, when it is a string: borrowed from the line
/// unless it has escapes to undo.
fn string(field: Option<&RawValue>) -> Option<Cow<'_, str>> {
    let raw = field?.get();
    match serde_json::from_str::<&str>(raw) {
        Ok(text) => Some(Cow::Borrowed(text)),
        Err(_) => serde_json::from_str::<String>(raw).ok().map(Cow::Owned),
    }
}

/// A field held unparsed, as owned text, when it is a non-empty string.
fn owned(field: Option<&RawValue>) -> Option<String> {
    string(field)
        .filter(|value| !value.is_empty())
        .map(Cow::into_owned)
}

/// A field held unparsed, parsed: null where it is absent or isn't JSON.
fn value(field: Option<&RawValue>) -> Value {
    field
        .and_then(|raw| serde_json::from_str(raw.get()).ok())
        .unwrap_or(Value::Null)
}

/// Pass each complete line of the log at `path` from byte `from` to `visit`,
/// with its offset and `batch`, as [`jsonl::each_line`] does, and note each
/// line too long to read: what it held is unknown, not nothing. Returns where
/// the next read starts.
///
/// # Errors
///
/// Returns [`Error::Io`] when the log cannot be opened or read.
fn read_log(
    path: &Path,
    from: u64,
    batch: &mut Batch,
    mut visit: impl FnMut(&[u8], u64, &mut Batch),
) -> Result<u64> {
    let lines = jsonl::each_line(path, from, |line, offset| visit(line, offset, batch))?;
    for offset in lines.overlong {
        batch.note(
            DiagnosticKind::Invalid,
            format!(
                "a line longer than the longest read, {} MiB",
                jsonl::LONGEST_LINE >> 20
            ),
            offset,
        );
    }
    Ok(lines.end)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;

    use serde::Deserialize;
    use serde_json::value::RawValue;

    use super::{AgentReader, Batch, Checkpoint, DiagnosticKind, Object, Resumed};
    use crate::session::SessionKey;
    use crate::time::Instant;
    use crate::transcript::Speaker;

    #[test]
    fn kinds_not_known_are_noted_by_name_as_far_as_a_bound() {
        let mut batch = Batch::default();
        // 40 kinds, one of them twice, and one of a name of 100 characters
        // with a line break in it.
        for n in 0..40 {
            batch.note_unknown("record type", &format!("kind{n:02}"), n);
        }
        batch.note_unknown("record type", "kind00", 99);
        let long = format!("x\n{}", "y".repeat(98));
        batch.note_unknown("record type", &long, 100);
        let noted: Vec<(&str, u64)> = batch
            .diagnostics()
            .map(|((kind, detail), seen)| {
                assert_eq!(*kind, DiagnosticKind::UnknownRecord);
                (detail.as_str(), seen.count)
            })
            .collect();
        // The first 32 by name, the one seen twice counted twice; the other
        // 8, and the long one, together.
        assert_eq!(noted.len(), 33);
        assert_eq!(noted[0], ("record type kind00", 2));
        assert_eq!(noted[31], ("record type kind31", 1));
        assert_eq!(noted[32], ("record type of other kinds", 9));

        // A name is kept to 64 characters, a control character among them
        // replaced.
        let mut batch = Batch::default();
        batch.note_unknown("record type", &long, 0);
        let (_, detail) = batch.diagnostics().next().unwrap().0;
        assert_eq!(
            detail.as_str(),
            format!("record type x\u{fffd}{}", "y".repeat(62))
        );
    }

    #[test]
    fn an_object_of_another_shape_costs_its_field_and_not_the_record() {
        #[derive(Deserialize)]
        struct Record<'a> {
            #[serde(borrow, default)]
            field: Option<Object<Inner<'a>>>,
            after: u64,
        }
        #[derive(Deserialize)]
        struct Inner<'a> {
            #[serde(borrow, default)]
            kind: Option<&'a RawValue>,
        }
        let read = |field: &str| {
            let record = format!(r#"{{"field": {field}, "after": 7}}"#);
            let record: Record = serde_json::from_str(&record).unwrap();
            assert_eq!(record.after, 7, "{field}");
            record.field.map(|Object(inner)| {
                inner
                    .and_then(|inner| inner.kind)
                    .map(|kind| kind.get().to_owned())
            })
        };
        assert_eq!(
            read(r#"{"more": {}, "kind": [1, {"a": 2}]}"#),
            Some(Some(r#"[1, {"a": 2}]"#.to_owned()))
        );
        for other in [
            r#""text""#,
            "12",
            "-3",
            "1.5",
            "true",
            r#"[{"kind": 1}, 2]"#,
        ] {
            assert_eq!(read(other), Some(None), "{other}");
        }
        assert_eq!(read("null"), None);
    }

    #[test]
    fn a_resumption_takes_back_only_what_came_before_it_in_its_numbering() {
        let minute = |minute: i64| Instant::from_millis(minute * 60_000);
        // Opened at minute 0, numbered afresh at minute 10, and taken up
        // again at minute 20 from ordinal 5 and at minute 30 from ordinal 8.
        let resumed = [
            Resumed {
                at: minute(0).unwrap(),
                base: None,
            },
            Resumed {
                at: minute(10).unwrap(),
                base: None,
            },
            Resumed {
                at: minute(20).unwrap(),
                base: Some(5),
            },
            Resumed {
                at: minute(30).unwrap(),
                base: Some(8),
            },
        ];
        let taken =
            |ordinal: i64, at: i64| Resumed::takes_back(&resumed, Some(ordinal), minute(at));
        // Numbered 5 or later between minutes 10 and 20: taken back by the
        // first; before the fresh numbering at minute 10, by neither.
        assert!(taken(5, 15));
        assert!(!taken(4, 15));
        assert!(!taken(7, 5));
        // Numbered 8 or later between minutes 20 and 30: taken back by the
        // second; numbered 5 to 7 there, kept, since it came after the first.
        assert!(taken(9, 25));
        assert!(!taken(6, 25));
        // After every resumption, or at the moment of one, nothing is.
        assert!(!taken(9, 30));
        assert!(!taken(9, 40));
        // Without an ordinal or a time, nothing can be placed, so nothing is.
        assert!(!Resumed::takes_back(&resumed, None, minute(15)));
        assert!(!Resumed::takes_back(&resumed, Some(9), None));
    }

    /// What `batch` noted, as each diagnostic's kind and detail.
    pub(crate) fn noted(batch: &Batch) -> Vec<(DiagnosticKind, &str)> {
        batch
            .diagnostics()
            .map(|((kind, detail), _)| (*kind, detail.as_str()))
            .collect()
    }

    /// What `batch` noted, as each diagnostic's kind and detail and how many
    /// times it was seen.
    pub(crate) fn noted_times(batch: &Batch) -> Vec<(DiagnosticKind, &str, u64)> {
        batch
            .diagnostics()
            .map(|((kind, detail), seen)| (*kind, detail.as_str(), seen.count))
            .collect()
    }

    /// What `batch` records as said, in order.
    pub(crate) fn said(batch: &Batch) -> Vec<&str> {
        batch.said().map(|said| said.text.as_str()).collect()
    }

    /// Check that what `reader` records as said in `session`, reading the
    /// artifacts `read`, is what its conversation from `shown` gives as the
    /// person's and the models' entries: search finds nothing a conversation
    /// cannot show, and misses nothing it shows. Each artifact is read on its
    /// own, as the ledger keeps what each said, and what one took back of
    /// another's is left out, as search leaves it out ([`Resumed`]).
    pub(crate) fn assert_said_as_shown(
        reader: &dyn AgentReader,
        session: &SessionKey,
        read: &[PathBuf],
        shown: &[PathBuf],
    ) {
        let batches: Vec<Batch> = read
            .iter()
            .map(|path| {
                let mut batch = Batch::default();
                reader
                    .read(path, &Checkpoint::default(), &mut batch)
                    .unwrap();
                batch
            })
            .collect();
        let resumed: Vec<Resumed> = batches
            .iter()
            .flat_map(Batch::resumed)
            .filter(|(key, _)| *key == session)
            .map(|(_, resumed)| *resumed)
            .collect();
        // In any order, as search keeps none: artifacts read in another order
        // say the same things in another.
        let mut said: Vec<&str> = batches
            .iter()
            .flat_map(Batch::said)
            .filter(|said| {
                said.session == *session && !Resumed::takes_back(&resumed, said.ordinal, said.at)
            })
            .map(|said| said.text.as_str())
            .collect();
        said.sort_unstable();
        let entries = reader.conversation(session, shown).unwrap().entries;
        let mut entries: Vec<&str> = entries
            .iter()
            .filter(|entry| matches!(entry.speaker, Speaker::User | Speaker::Assistant))
            .map(|entry| entry.text.trim())
            .collect();
        entries.sort_unstable();
        assert_eq!(said, entries);
        assert!(!said.is_empty(), "the check needs something said");
    }
}
