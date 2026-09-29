//! The Turnscope engine: coding agents' history, read once and kept.
//!
//! Agents write their history to the user's home directory, each in folders
//! of its own ([`Folder`]). The engine reads it with one reader per agent,
//! and writes what each artifact said to a ledger that keeps it even after
//! the agent deletes the artifact.
//! Everything shown — merged usage, costs, totals — is derived from the
//! ledger and can always be derived again.
//!
//! Reading is incremental: a log that has only grown is read from where the
//! last read stopped, so a scan of unchanged history costs a directory walk.
//!
//! One API serves the menu bar app's feed (`turnscope watch`), the MCP
//! server and the command line: [`Engine`] and what this crate exports.
//! Answers come in shapes ready to use, and formatting is the app's.
//! Several processes can open one data directory at once; one of them, the
//! app's while it runs, keeps it current ([`Engine::run`]), and the rest
//! read what it keeps or, with none keeping it, scan before they answer
//! ([`Engine::kept`]).
//! [`Engine::health`] says how complete an answer is: when history was last
//! looked through in full, and what that look couldn't read.
//!
//! The engine has no async runtime and no TLS: its work runs on threads of
//! its own and rayon's pool, and its few requests go through the system's
//! curl.

mod agent;
mod breakdown;
mod cache;
mod catalog;
mod context;
mod error;
mod folders;
mod handoff;
mod health;
mod ingest;
mod jsonl;
mod ledger;
mod limits;
mod model;
mod net;
mod outside;
mod price;
mod project;
mod query;
mod runtime;
mod search;
mod session;
mod sharing;
mod time;
mod transcript;
mod usage;

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, Instant as Clock};

use rusqlite::Connection;

pub use crate::agent::{Agent, DiagnosticKind};
pub use crate::breakdown::{LimitShare, PromptUsage, SessionUsage, SubagentUsage};
pub use crate::catalog::{Catalog, CatalogError};
pub use crate::error::{Error, Result};
pub use crate::folders::{Folder, FolderOrigin};
pub use crate::handoff::{Command, FileChange, Goal, Handoff, Quote, Step, StepStatus};
pub use crate::health::{
    AgentHealth, Check, Comparison, Diagnostic, Difference, Doctor, Health, Prices, Pricing,
    Unpriced,
};
pub use crate::ingest::ScanReport;
pub use crate::limits::{
    AccountLimits, Alert, AlertKind, LimitProblem, LimitState, LimitTrack, LimitWindow, Outlook,
    PastWindow, Standing, Subscription, WeekEnded, plan_name,
};
pub use crate::model::{ModelInfo, ModelKey};
pub use crate::outside::Counts;
pub use crate::query::{
    Dimension, Filter, Page, RUNNING, SessionOrder, SessionQuery, SessionRow, Totals, UsageQuery,
    UsageRow, UsageTable,
};
pub use crate::runtime::{CatalogCheck, CatalogOutcome, Change, Options, Running};
pub use crate::search::{Excerpt, SearchHit, SearchQuery};
pub use crate::session::{LinkKind, SessionKey, shell_word};
pub use crate::time::{Bucket, Instant, Span, Zone};
pub use crate::transcript::{Entry, Speaker, ToolCall};
pub use crate::usage::{Tokens, Usd};

use crate::agent::AgentReader;
use crate::cache::Cache;
use crate::catalog::Names;
use crate::ledger::Ledger;
use crate::price::PriceBook;
use crate::sharing::{Held, Lock};
use crate::transcript::Transcript;

/// How many read-only connections are kept between questions: as many as
/// are asked at once, as [`Engine::excerpts`] asks.
const IDLE_READERS: usize = 4;

/// How many conversations [`Engine::excerpts`] reads side by side: one after
/// another, fifty took 6 to 8 s on this Mac's history.
const EXCERPTS_AT_ONCE: usize = 4;

/// How soon a long read of history first shows what it has read so far.
const SHOW_PROGRESS: Duration = Duration::from_secs(1);

/// The engine: every agent's reader, the ledger they write to, and the prices
/// usage is charged at.
///
/// Several processes can open the same data directory, as the app, MCP
/// servers and the command line do. Each reads freely; writes take turns, and
/// only one running engine keeps the directory current at a time.
pub struct Engine {
    home: PathBuf,
    data: PathBuf,
    readers: Vec<Box<dyn AgentReader>>,
    // Locked in this order, ledger before cache, whenever both are held.
    // Writes go through these; questions are answered from `idle`.
    ledger: Mutex<Ledger>,
    cache: Mutex<Cache>,
    /// Connections that only read, kept for the next question.
    idle: Mutex<Vec<Readers>>,
    book: RwLock<Book>,
    /// Held, with the ledger, by every write, so that processes sharing the
    /// data directory write one at a time.
    writer: Lock,
    /// Whether this process's running engine keeps the data directory
    /// current.
    keeping: AtomicBool,
    subscribers: Mutex<Vec<async_channel::Sender<Change>>>,
    /// What models and providers are called, and the catalog it was read
    /// from, by when that was published: read at the first question that
    /// needs it, and again once a newer catalog is taken in.
    names: Mutex<Option<(Option<i64>, Arc<Names>)>>,
}

/// Prices, as of the catalog they were read with.
///
/// Another process can take in a newer catalog, so a writer checks the
/// ledger's before charging anything.
struct Book {
    /// When the catalog was published, as the ledger keeps it.
    as_of: Option<i64>,
    prices: PriceBook,
}

impl Book {
    fn read(ledger: &Ledger) -> Result<Book> {
        Ok(Book {
            as_of: ledger.catalog_as_of()?,
            prices: ledger.price_book()?,
        })
    }
}

/// The ledger and the cache, for reading only. SQLite's write-ahead log gives
/// each connection a consistent view of what was last written, so a question
/// is answered at once, never waiting for a write under way.
struct Readers {
    ledger: Ledger,
    cache: Cache,
}

impl Readers {
    /// Answer `question` from one view of the ledger and one of the cache,
    /// whatever is written meanwhile, so its figures agree with each other.
    fn answer<T>(&self, question: impl FnOnce(&Ledger, &Connection) -> Result<T>) -> Result<T> {
        let _ledger = self.ledger.connection().unchecked_transaction()?;
        let _cache = self.cache.connection().unchecked_transaction()?;
        question(&self.ledger, self.cache.connection())
    }
}

/// Read-only connections for one question, kept for the next when dropped.
struct Reading<'a> {
    idle: &'a Mutex<Vec<Readers>>,
    readers: Option<Readers>,
}

impl Deref for Reading<'_> {
    type Target = Readers;

    fn deref(&self) -> &Readers {
        // Taken only by `drop`.
        self.readers
            .as_ref()
            .expect("readers are held until dropped")
    }
}

impl Drop for Reading<'_> {
    fn drop(&mut self) {
        let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
        if idle.len() < IDLE_READERS
            && let Some(readers) = self.readers.take()
        {
            idle.push(readers);
        }
    }
}

/// The ledger, held for writing: no other thread of this process, and no
/// other process sharing the data directory, writes until it is dropped.
struct Writing<'a> {
    // Released before the ledger, so that another process can start writing
    // while this one's threads take their turns.
    _held: Held<'a>,
    ledger: MutexGuard<'a, Ledger>,
}

impl Deref for Writing<'_> {
    type Target = Ledger;

    fn deref(&self) -> &Ledger {
        &self.ledger
    }
}

impl DerefMut for Writing<'_> {
    fn deref_mut(&mut self) -> &mut Ledger {
        &mut self.ledger
    }
}

impl Engine {
    /// Open the engine with its ledger in `data`, reading agents' history
    /// under `home`. Nothing is read until [`Engine::scan`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when `data` or its lock can't be made or held,
    /// [`Error::Ledger`] when the ledger or the cache can't be opened or
    /// brought up to date, [`Error::NewerLedger`] or [`Error::NewerCache`]
    /// when a newer build made them, and [`Error::Corrupt`] when a value
    /// stored in them is out of range.
    pub fn open(data: &Path, home: &Path) -> Result<Engine> {
        // Made for the owner alone: it holds titles, paths and the words of
        // what was said, as the index finds them. One there already, as a
        // folder given with `--data`, is left as it is.
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(data)
            .map_err(|error| Error::io(data, error))?;
        let writer = Lock::open(&data.join(sharing::WRITER))?;
        let (ledger, cache, book) = {
            // Opening migrates the ledger and can rebuild the cache.
            let _held = writer.hold()?;
            let mut ledger = Ledger::open(&data.join(ledger::FILE))?;
            // The catalog that ships with the app stands until a newer one is
            // fetched, and replaces an older one kept from a previous version.
            if ledger
                .catalog_as_of()?
                .is_none_or(|kept| kept < Catalog::bundled_as_of())
            {
                ledger.take_catalog(&Catalog::bundled(), "bundled", None)?;
            }
            let book = Book::read(&ledger)?;
            let mut cache = Cache::open(&data.join(cache::FILE))?;
            cache.catch_up(&mut ledger, &book.prices, home)?;
            (ledger, cache, book)
        };
        Ok(Engine {
            home: home.to_path_buf(),
            data: data.to_path_buf(),
            readers: agent::readers(),
            ledger: Mutex::new(ledger),
            cache: Mutex::new(cache),
            idle: Mutex::new(Vec::new()),
            book: RwLock::new(book),
            writer,
            keeping: AtomicBool::new(false),
            subscribers: Mutex::new(Vec::new()),
            names: Mutex::new(None),
        })
    }

    /// Read whatever is new or changed in every agent's history, waiting for
    /// any other process writing to the data directory to finish first.
    ///
    /// # Errors
    ///
    /// Returns an error when the ledger cannot be read or written. An artifact
    /// that cannot be read is listed in the report and does not stop the scan.
    pub fn scan(&self) -> Result<ScanReport> {
        let mut ledger = self.writing()?;
        // A long read, as the first is, shows what it has read so far, the
        // newest history first: after a second, and then after four times as
        // long each time, since each showing works the cache out again.
        let mut caught = Clock::now();
        let mut wait = SHOW_PROGRESS;
        let roots = self.roots();
        let report = ingest::scan(&self.readers, &roots, &mut ledger, |ledger| {
            if caught.elapsed() >= wait {
                self.catch_up(ledger)?;
                caught = Clock::now();
                wait = wait.saturating_mul(4);
            }
            Ok(())
        })?;
        // Recorded before the last catch-up tells subscribers, so what they
        // ask on hearing of it finds the look finished.
        ledger.record_look(Instant::now(), &report.failed)?;
        self.catch_up(&mut ledger)?;
        self.let_go(&ledger)?;
        Ok(report)
    }

    /// How complete what the engine answers from is: when every agent's
    /// history was last looked through in full, as any process sharing the
    /// data directory last did, and what that look could not read. Whether
    /// an engine is keeping it current now is [`Engine::kept`].
    ///
    /// # Errors
    ///
    /// Returns an error when the ledger cannot be read.
    pub fn health(&self) -> Result<Health> {
        let (looked, failed) = match self.ask(|ledger, _| ledger.last_look())? {
            Some(look) => (Some(look.at), look.failed),
            None => (None, Vec::new()),
        };
        Ok(Health { looked, failed })
    }

    /// Keep the engine current: watch every agent's history for changes and
    /// read them as they come, look through everything now and then, and, as
    /// `options` allow, check models.dev for new prices and read
    /// subscriptions' limits from their providers. What changes goes to every
    /// subscriber. It runs until the returned handle is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] when a thread cannot be started.
    pub fn run(self: &Arc<Self>, options: Options) -> std::io::Result<Running> {
        runtime::run(self, options)
    }

    /// Whether a running engine, in this process or another sharing the data
    /// directory, keeps what the engine answers current. When none does, a
    /// [`Engine::scan`] before answering reads what changed since.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the lock that says so cannot be tried.
    pub fn kept(&self) -> Result<bool> {
        if self.keeping.load(Ordering::Acquire) {
            return Ok(true);
        }
        sharing::held(&self.data.join(sharing::KEEPER))
    }

    /// Hear of every change from now on. A subscriber that stops listening is
    /// forgotten.
    pub fn subscribe(&self) -> async_channel::Receiver<Change> {
        let (sender, receiver) = async_channel::unbounded();
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(sender);
        receiver
    }

    /// Tell every subscriber of `change`.
    pub(crate) fn publish(&self, change: Change) {
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        subscribers.retain(|subscriber| subscriber.try_send(change.clone()).is_ok());
    }

    /// The home directory agents' history is read under, as
    /// [`Engine::open`] was given it.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Every folder of the agents' read now: each agent's own, and those
    /// found beside it holding a second account's sign-in, by agent and then
    /// path. What each holds, history and sign-ins, is read, as [`Folder`]
    /// says.
    pub fn folders(&self) -> Vec<Folder> {
        folders::read(&self.home)
    }

    /// Each of `readers`' roots in `folders`, the folders of its agent, by
    /// the reader's place among them.
    fn roots_in(&self, folders: &[Folder]) -> Vec<(usize, PathBuf)> {
        folders
            .iter()
            .flat_map(|folder| {
                self.readers
                    .iter()
                    .enumerate()
                    .filter(move |(_, reader)| reader.agent() == folder.agent)
                    .flat_map(move |(index, reader)| {
                        reader
                            .roots(&folder.path)
                            .into_iter()
                            .map(move |root| (index, root))
                    })
            })
            .collect()
    }

    /// Every reader's roots in the folders read now, by the reader's place.
    pub(crate) fn roots(&self) -> Vec<(usize, PathBuf)> {
        self.roots_in(&self.folders())
    }

    /// The root of the project `directory` belongs to: its repository's main
    /// working tree, or the directory itself outside any repository, as
    /// [`Filter::projects`] takes it.
    pub fn project_root(&self, directory: &str) -> String {
        project::resolve(directory, &self.home).0
    }

    /// A session's conversation, read from the agent's files now. Each call
    /// in it that started a subagent names that subagent
    /// ([`ToolCall::subagent`]), where the agent records which it was.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Gone`] when no file holding it is left, and an error
    /// when one cannot be read.
    pub fn conversation(&self, session: &SessionKey) -> Result<Vec<Entry>> {
        Ok(self.transcript(session)?.entries)
    }

    /// Where `session` stopped, for whoever picks it up: its first and last
    /// requests, its last reply, its plan, the files it changed and the
    /// commands it ran, as [`Handoff`] says, read from the agent's files now
    /// as its conversation is.
    ///
    /// # Errors
    ///
    /// As [`Engine::conversation`].
    pub fn handoff(&self, session: &SessionKey) -> Result<Handoff> {
        let transcript = self.transcript(session)?;
        Ok(handoff::handoff(&transcript.entries, &transcript.work))
    }

    /// `session`'s transcript as its reader reads it now, each call that
    /// started a subagent naming it.
    fn transcript(&self, session: &SessionKey) -> Result<Transcript> {
        let (artifacts, children) = self.ask(|ledger, _| {
            Ok((
                ledger.artifacts_of(session)?,
                ledger.launched_from(session)?,
            ))
        })?;
        let reader = self
            .readers
            .iter()
            .find(|reader| reader.agent() == session.agent())
            .filter(|_| !artifacts.is_empty())
            .ok_or_else(|| Error::Gone(session.to_string()))?;
        let mut transcript = reader.conversation(session, &artifacts)?;
        transcript.resolve(&children);
        Ok(transcript)
    }

    /// Where the session `key`'s usage went: prompt by prompt, subagent by
    /// subagent, and its share of each limit it drew on, as
    /// [`SessionUsage`] says; `None` when there is no such session. Its
    /// prompts come from its conversation, read from the agent's files now:
    /// a session whose files are gone has its usage and no prompts.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger or the cache cannot be read,
    /// and an error when the session's files are there but cannot be read.
    pub fn session_usage(&self, key: &SessionKey) -> Result<Option<SessionUsage>> {
        let entries = match self.conversation(key) {
            Ok(entries) => Some(entries),
            Err(Error::Gone(_)) => None,
            Err(error) => return Err(error),
        };
        self.ask(|ledger, cache| breakdown::session_usage(cache, ledger, key, entries.as_deref()))
    }

    /// Where the session `key`'s usage went, as [`Engine::session_usage`]
    /// says, without reading its conversation: quicker, for a question that
    /// needs its subagents and its shares of limits and not its prompts. It
    /// has no prompts, and all its usage is no prompt's.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger or the cache cannot be read.
    pub fn session_usage_without_prompts(&self, key: &SessionKey) -> Result<Option<SessionUsage>> {
        self.ask(|ledger, cache| breakdown::session_usage(cache, ledger, key, None))
    }

    /// How large each of `sessions`' context grew: its largest response's
    /// prompt, its input however it was cached, in tokens, as pricing
    /// measures it for a long-context tier (one call's, for usage an agent
    /// counts over several). Its subagents' responses are theirs. A session
    /// with no response is left out, its context unknown.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the cache cannot be read.
    pub fn largest_contexts(&self, sessions: &[SessionKey]) -> Result<HashMap<SessionKey, u64>> {
        self.ask(|_, cache| context::largest(cache, sessions))
    }

    /// A page of the sessions in which the person or a model said something
    /// that matches `question.words`, among those the rest of the question
    /// admits, most recent mention first: every such session, a page at a
    /// time, whatever the filter leaves out. A session with no mention that
    /// has a time is placed by its latest activity.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger or the cache cannot be read,
    /// and [`Error::Cursor`] when `after` is not where a page ended.
    pub fn search(&self, question: &SearchQuery) -> Result<Page<SearchHit>> {
        self.ask(|ledger, cache| search::sessions(ledger, cache, question))
    }

    /// For each of `sessions`, in order, the first thing the person or a
    /// model said in it that matches `words`, with the words around the
    /// match, read from its conversation now: `None` where nothing does,
    /// and an error where the conversation can't be read, as
    /// [`Engine::conversation`] fails. Each conversation is read whole from
    /// the agent's files, so a few are read side by side.
    pub fn excerpts(&self, sessions: &[SessionKey], words: &str) -> Vec<Result<Option<Excerpt>>> {
        if !search::has_words(words) {
            return sessions.iter().map(|_| Ok(None)).collect();
        }
        let next = AtomicUsize::new(0);
        let mut read: Vec<(usize, Result<Option<Excerpt>>)> = std::thread::scope(|scope| {
            let readers: Vec<_> = (0..EXCERPTS_AT_ONCE.min(sessions.len()))
                .map(|_| {
                    scope.spawn(|| {
                        let mut read = Vec::new();
                        loop {
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            let Some(session) = sessions.get(index) else {
                                return read;
                            };
                            let excerpt = self
                                .conversation(session)
                                .and_then(|entries| search::excerpt(&entries, words));
                            read.push((index, excerpt));
                        }
                    })
                })
                .collect();
            readers
                .into_iter()
                .flat_map(|reader| {
                    reader
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                })
                .collect()
        });
        read.sort_unstable_by_key(|(index, _)| *index);
        read.into_iter().map(|(_, excerpt)| excerpt).collect()
    }

    /// Every account's limits, with how fast each is rising and whether the
    /// account is in use: each subscription's accounts, and each API-key
    /// account ([`Subscription::ApiKey`]); each with whether the person hid
    /// it, hidden ones among them.
    ///
    /// # Errors
    ///
    /// Returns an error when the ledger or the cache cannot be read.
    pub fn limits(&self) -> Result<Vec<AccountLimits>> {
        self.ask(|ledger, cache| limits::current(ledger, cache, Instant::now()))
    }

    /// Hide `account`, or show it again, and tell subscribers
    /// ([`Change::Limits`]). A hidden account is still among
    /// [`Engine::limits`], marked so, and gives rise to no alerts.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] when `account` is no account's id, and
    /// [`Error::Ledger`] when the ledger cannot be written.
    pub fn set_account_hidden(&self, account: &str, hidden: bool) -> Result<()> {
        limits::checked_account(account)?;
        self.writing()?.set_hidden(account, hidden)?;
        self.publish(Change::Limits);
        Ok(())
    }

    /// The current window of `account`'s limit `key` as its readings draw
    /// it, and what each session, project and model took of it; `None`
    /// when no reading of it is kept. What each took is approximate: each
    /// rise between two readings is shared among the responses of its
    /// stretch that drew on the account, made where and when it was signed
    /// in, by what each cost at list prices, and a rise while nothing here
    /// spent is use elsewhere.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger or the cache cannot be read.
    pub fn limit_window(&self, account: &str, key: &str) -> Result<Option<LimitWindow>> {
        self.ask(|ledger, cache| limits::share::window(ledger, cache, account, key, Instant::now()))
    }

    /// Read every subscription's limits from its provider now, with the
    /// sign-ins on this Mac, and each OpenRouter key's own from OpenRouter,
    /// and tell subscribers what was read. Only while
    /// this process keeps the data directory ([`Engine::run`]) are the alerts
    /// the limits give rise to worked out and sent.
    ///
    /// # Errors
    ///
    /// Returns the first of any subscription's failures, having read every
    /// other: a place that keeps sign-ins to it that is there but can't be
    /// read, as a file is while an agent writes it, which leaves its accounts
    /// as they were until the next read; or a ledger or cache that can't be
    /// read or written. A provider that cannot be reached is no failure but
    /// the account's problem, recorded with it.
    pub fn read_all_limits(&self) -> Result<()> {
        let mut first = Ok(());
        for subscription in Subscription::ALL {
            // Read whatever became of the one before.
            let read = self.read_limits(subscription);
            first = first.and(read);
        }
        first
    }

    /// Read every account of `subscription` from its provider, record what was
    /// read and what each place a sign-in to it is kept held, and tell
    /// subscribers of the limits, of any sessions whose account that changed,
    /// and, in the process that keeps the data directory, of any alerts they
    /// give rise to and of the weekly recap when it is due.
    ///
    /// # Errors
    ///
    /// Returns an error, recording nothing, when a place that keeps sign-ins
    /// to `subscription` can't be read, and when the ledger or the cache
    /// can't be read or written.
    pub(crate) fn read_limits(&self, subscription: Subscription) -> Result<()> {
        let now = Instant::now();
        // The requests are made before the ledger is held, so a slow provider
        // holds up nothing else.
        let folders = self.folders();
        let (reads, mut seen) = limits::read_subscription(subscription, &folders, &self.home, now)?;
        let (alerts, weeks) = {
            let mut ledger = self.writing()?;
            ledger.record_limits(subscription, &reads, now)?;
            // A key or unread sign-in found before and not now is gone.
            if subscription == Subscription::ApiKey {
                let gone = limits::api::vanished(&seen, &ledger.sign_ins()?, &folders);
                seen.extend(gone);
            }
            // Which account usage drew on rests on what was signed in where.
            if ledger.record_sign_ins(&seen, now)? {
                self.catch_up(&mut ledger)?;
            }
            // Alerts are the keeper's to send, since it is the app that shows
            // them: another process, such as an MCP server reading limits
            // while no app runs, would record as sent alerts no one sees.
            if self.keeping.load(Ordering::Acquire) {
                let zone = Zone::system();
                let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
                let state = limits::current(&ledger, cache.connection(), now)?;
                let alerts = limits::alerts(&mut ledger, &state, now)?;
                let weeks =
                    limits::recap::recap(&mut ledger, cache.connection(), &state, now, &zone)?;
                (alerts, weeks)
            } else {
                (Vec::new(), Vec::new())
            }
        };
        self.publish(Change::Limits);
        for alert in alerts {
            self.publish(Change::Alert(alert));
        }
        if !weeks.is_empty() {
            self.publish(Change::Recap(weeks));
        }
        Ok(())
    }

    /// Bring the cache up to `ledger`, which this process holds for writing,
    /// and tell subscribers what changed.
    fn catch_up(&self, ledger: &mut Ledger) -> Result<()> {
        self.refresh_prices(ledger)?;
        let caught = {
            let book = self.book.read().unwrap_or_else(PoisonError::into_inner);
            let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            cache.catch_up(ledger, &book.prices, &self.home)?
        };
        for change in Change::of(caught) {
            self.publish(change);
        }
        Ok(())
    }

    /// Answer a question about usage, with local times in `zone`.
    ///
    /// # Errors
    ///
    /// Returns an error when the cache cannot be read.
    pub fn usage(&self, question: &UsageQuery, zone: &Zone) -> Result<UsageTable> {
        self.ask(|_, cache| query::usage(cache, question, zone))
    }

    /// Every model with usage, in order of their keys, with what the catalog
    /// calls it, for the interface to show a name in place of an id.
    ///
    /// # Errors
    ///
    /// Returns an error when the cache or the ledger cannot be read, or the
    /// stored catalog is not the catalog's JSON.
    pub fn models(&self) -> Result<Vec<ModelInfo>> {
        let (models, names) =
            self.ask(|ledger, cache| Ok((query::models(cache)?, self.names(ledger)?)))?;
        Ok(models
            .into_iter()
            .map(|key| ModelInfo {
                name: names.model(key.as_str()).map(str::to_owned),
                key,
            })
            .collect())
    }

    /// What models are called, from the catalog the ledger
    /// keeps, read again only once a newer one is taken in.
    fn names(&self, ledger: &Ledger) -> Result<Arc<Names>> {
        let as_of = ledger.catalog_as_of()?;
        let mut kept = self.names.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((read, names)) = kept.as_ref()
            && *read == as_of
        {
            return Ok(names.clone());
        }
        let names = Arc::new(match ledger.catalog()? {
            Some(stored) => stored.catalog.names(),
            None => Catalog::bundled().names(),
        });
        *kept = Some((as_of, names.clone()));
        Ok(names)
    }

    /// List sessions.
    ///
    /// # Errors
    ///
    /// Returns an error when the cache cannot be read, and [`Error::Cursor`]
    /// when `after` is not where a previous page ended.
    pub fn sessions(&self, question: &SessionQuery) -> Result<Page<SessionRow>> {
        self.ask(|_, cache| query::sessions(cache, question))
    }

    /// Report on what the ledger holds and how it compares with the agents'
    /// own totals.
    ///
    /// # Errors
    ///
    /// Returns an error when the ledger cannot be read.
    pub fn doctor(&self) -> Result<Doctor> {
        // From connections of its own, so this process's writes don't wait
        // on the seconds a report takes.
        self.ask(|ledger, _| {
            self.refresh_prices(ledger)?;
            let book = self.book.read().unwrap_or_else(PoisonError::into_inner);
            health::doctor(ledger, &book.prices)
        })
    }

    /// Read prices again when another process has taken in a newer catalog
    /// than the one they were read with.
    fn refresh_prices(&self, ledger: &Ledger) -> Result<()> {
        let as_of = ledger.catalog_as_of()?;
        let current = self
            .book
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_of
            == as_of;
        if !current {
            *self.book.write().unwrap_or_else(PoisonError::into_inner) = Book::read(ledger)?;
        }
        Ok(())
    }

    /// Answer `question` from connections that only read, as
    /// [`Readers::answer`] does.
    fn ask<T>(&self, question: impl FnOnce(&Ledger, &Connection) -> Result<T>) -> Result<T> {
        self.reading()?.answer(question)
    }

    /// Connections that only read, for a question: one kept from an earlier
    /// question, or new ones.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when new ones cannot be opened.
    fn reading(&self) -> Result<Reading<'_>> {
        let readers = loop {
            let kept = self
                .idle
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop();
            match kept {
                Some(readers) if readers.cache.current()? => break readers,
                // Another build sharing the data directory put a cache of
                // its own in place of the one these read, which would go on
                // answering as it last stood: they go, and new ones read the
                // new cache, or refuse it when it is a newer build's.
                Some(_) => {}
                None => {
                    break Readers {
                        ledger: Ledger::reader(&self.data.join(ledger::FILE))?,
                        cache: Cache::reader(&self.data.join(cache::FILE))?,
                    };
                }
            }
        };
        Ok(Reading {
            idle: &self.idle,
            readers: Some(readers),
        })
    }

    /// Give back the pages SQLite keeps for the writing connections, `ledger`
    /// and the cache's, once a piece of work that wrote is done. Each keeps
    /// up to 64 MB, which a first read or a rebuild works in and an engine
    /// waiting for the next line doesn't need: SQLite holds every page it
    /// has read until told otherwise, which kept an app waiting in the menu
    /// bar at 42 MB of pages a few minutes after it opened (2026-09-27). The
    /// next piece of work reads what it needs again, from the system's file
    /// cache.
    ///
    /// # Errors
    ///
    /// Returns an error when a database refuses.
    fn let_go(&self, ledger: &Ledger) -> Result<()> {
        ledger.connection().execute_batch("PRAGMA shrink_memory")?;
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache.connection().execute_batch("PRAGMA shrink_memory")?;
        Ok(())
    }

    /// The ledger, for a write, once no other process is writing.
    fn writing(&self) -> Result<Writing<'_>> {
        // Every write to the ledger is a transaction, so a thread that
        // panicked while holding it left the ledger as its last commit did,
        // and it is safe to go on using.
        let ledger = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        let held = self.writer.hold()?;
        Ok(Writing {
            _held: held,
            ledger,
        })
    }
}

#[cfg(feature = "fixture")]
impl Engine {
    /// Record `accounts`, each of `subscription`, as read at `at`: their
    /// labels, plans and agents, and each limit's use and window, as a read
    /// of their provider would, and each signed in at `at` where its
    /// [`AccountLimits::folders`] say, or else in its agents' own folders,
    /// as a look for sign-ins would find it. For synthetic
    /// history, whose subscriptions no provider could be asked about. What
    /// is worked out from readings and sign-ins, whether an account is in
    /// use, its pace and when it runs out, and which account each response
    /// drew on, is worked out from these as from any others, so an account
    /// read twice, its use risen between, is in use.
    ///
    /// # Errors
    ///
    /// Returns an error when the ledger cannot be written.
    pub fn record_fixture_limits(
        &self,
        subscription: Subscription,
        accounts: &[AccountLimits],
        at: Instant,
    ) -> Result<()> {
        let reads: Vec<limits::Read> = accounts
            .iter()
            .map(|account| limits::Read {
                id: account.id.clone(),
                label: account.label.clone(),
                plan: account.plan.clone(),
                agents: account.agents.clone(),
                // A limit whose share is unknown has no reading to record.
                limits: Ok(account
                    .limits
                    .iter()
                    .filter_map(|limit| {
                        Some(limits::Reported {
                            key: limit.key.clone(),
                            name: limit.name.clone(),
                            scope: limit.scope.clone(),
                            used: limit.used?,
                            starts: limit.starts,
                            resets: limit.resets,
                        })
                    })
                    .collect()),
            })
            .collect();
        let seen: Vec<limits::Seen> = accounts
            .iter()
            .flat_map(|account| {
                // Where it says it is signed in, or else in each of its
                // agents' own folders.
                let signed: Vec<(Agent, PathBuf)> = if account.folders.is_empty() {
                    account
                        .agents
                        .iter()
                        .map(|agent| (*agent, folders::own(*agent, &self.home)))
                        .collect()
                } else {
                    account.folders.clone()
                };
                signed.into_iter().flat_map(move |(agent, folder)| {
                    limits::attribution::places(subscription, agent, &folder)
                        .into_iter()
                        .map(|place| limits::Seen {
                            place,
                            held: limits::Held::Account(account.id.clone()),
                        })
                })
            })
            .collect();
        let mut ledger = self.writing()?;
        ledger.record_limits(subscription, &reads, at)?;
        if ledger.record_sign_ins(&seen, at)? {
            self.catch_up(&mut ledger)?;
        }
        drop(ledger);
        self.publish(Change::Limits);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use crate::limits::{Held, Place, Read, Reported, Seen};
    use crate::{Agent, AlertKind, Change, Engine, Instant, Subscription};

    #[test]
    fn a_subscription_whose_sign_ins_cant_be_read_is_left_as_it_was() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let engine = Engine::open(data.path(), home.path()).unwrap();
        let read_at = Instant::from_millis(1_789_000_000_000).unwrap();
        let read = Read {
            id: "chatgpt:acct-1".into(),
            label: None,
            plan: None,
            agents: vec![Agent::Codex],
            limits: Ok(vec![Reported {
                key: "primary_window".into(),
                name: "5 hours".into(),
                scope: None,
                used: 40.0,
                starts: None,
                resets: None,
            }]),
        };
        engine
            .writing()
            .unwrap()
            .record_limits(Subscription::ChatGpt, &[read], read_at)
            .unwrap();
        // Codex part way through writing its sign-in.
        let auth = home.path().join(".codex/auth.json");
        std::fs::create_dir_all(auth.parent().unwrap()).unwrap();
        std::fs::write(&auth, r#"{"tokens": {"access_to"#).unwrap();

        assert!(engine.read_limits(Subscription::ChatGpt).is_err());
        let account = &engine.limits().unwrap()[0];
        assert!(account.signed_in, "not signed out");
        assert_eq!(account.checked_at, Some(read_at), "not read at all");
    }

    #[test]
    fn doctor_reports_while_this_process_writes() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::open(data.path(), home.path()).unwrap());
        let writing = engine.writing().unwrap();
        let (reported, report) = mpsc::channel();
        let asking = Arc::clone(&engine);
        std::thread::spawn(move || reported.send(asking.doctor().is_ok()));
        assert_eq!(report.recv_timeout(Duration::from_secs(10)), Ok(true));
        drop(writing);
    }

    #[test]
    fn only_the_process_keeping_the_data_directory_sends_alerts() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let engine = Engine::open(data.path(), home.path()).unwrap();
        let changes = engine.subscribe();
        // A limit used up in a window that ended ten minutes ago, said to be
        // reached then. Its account is signed in nowhere here, so reading
        // limits finds it signed out and back.
        let now = Instant::now().millis();
        let minutes = |minutes: i64| Instant::from_millis(now + minutes * 60_000).unwrap();
        let read = Read {
            id: "chatgpt:acct-1".into(),
            label: None,
            plan: None,
            agents: vec![Agent::Codex],
            limits: Ok(vec![Reported {
                key: "primary_window".into(),
                name: "5 hours".into(),
                scope: None,
                used: 100.0,
                starts: None,
                resets: Some(minutes(-10)),
            }]),
        };
        {
            let mut ledger = engine.writing().unwrap();
            ledger
                .record_limits(Subscription::ChatGpt, &[read], minutes(-60))
                .unwrap();
            let window = minutes(-10).millis();
            let reached = AlertKind::Reached;
            ledger
                .send_alert(
                    "chatgpt:acct-1",
                    "primary_window",
                    reached,
                    window,
                    minutes(-60),
                )
                .unwrap();
        }
        let alerts = || -> Vec<AlertKind> {
            std::iter::from_fn(|| changes.try_recv().ok())
                .filter_map(|change| match change {
                    Change::Alert(alert) => Some(alert.kind),
                    _ => None,
                })
                .collect()
        };

        engine.read_limits(Subscription::ChatGpt).unwrap();
        assert!(alerts().is_empty(), "nothing is said, or recorded as said");
        engine.keeping.store(true, Ordering::Release);
        engine.read_limits(Subscription::ChatGpt).unwrap();
        assert_eq!(alerts(), [AlertKind::Available]);
    }

    #[test]
    fn a_data_directory_it_makes_is_the_owners_alone() {
        let home = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let data = parent.path().join("com.example.turnscope");
        Engine::open(&data, home.path()).unwrap();
        let mode = std::fs::metadata(&data).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    /// 2026-09-27 at `hour`:`minute` UTC.
    fn at(hour: i64, minute: i64) -> Instant {
        Instant::from_millis(1_790_467_200_000 + hour * 3_600_000 + minute * 60_000).unwrap()
    }

    /// Write a Claude Code session `session`, in the agent's `folder`, of
    /// one response of Claude Opus 5 at each of `times`, each 1,000 tokens
    /// in and 100 out.
    fn claude_session(folder: &Path, session: &str, times: &[Instant]) {
        claude_session_of(folder, session, "claude-opus-5", times);
    }

    /// As [`claude_session`], of `model`.
    fn claude_session_of(folder: &Path, session: &str, model: &str, times: &[Instant]) {
        let log = folder
            .join("projects/-work-ledger")
            .join(format!("{session}.jsonl"));
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        let lines: String = times
            .iter()
            .enumerate()
            .map(|(index, time)| {
                let id = format!("{session}-{index}");
                format!(
                    "{}\n",
                    serde_json::json!({"type": "assistant", "sessionId": session,
                        "timestamp": time.to_string(), "requestId": format!("req_{id}"),
                        "cwd": "/work/ledger",
                        "message": {"id": format!("msg_{id}"), "model": model,
                                    "usage": {"input_tokens": 1_000, "output_tokens": 100}}})
                )
            })
            .collect();
        std::fs::write(log, lines).unwrap();
    }

    /// What Claude Code's `folder` held for Anthropic's usage.
    fn claude_signed(folder: &Path, account: Option<&str>) -> Seen {
        Seen {
            place: Place {
                agent: Agent::ClaudeCode,
                folder: folder.to_path_buf(),
                provider: "anthropic".to_owned(),
            },
            held: account.map_or(Held::Nothing, |account| Held::Account(account.to_owned())),
        }
    }

    /// Each response in the cache, by its key, and the account it drew on.
    fn drew_on(engine: &Engine) -> Vec<(String, Option<String>)> {
        let readers = engine.reading().unwrap();
        let mut statement = readers
            .cache
            .connection()
            .prepare("SELECT response, account FROM usage ORDER BY response")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn usage_draws_on_the_account_signed_in_where_and_when_it_was_made() {
        let home = tempfile::tempdir().unwrap();
        let own = home.path().join(".claude");
        let work = home.path().join(".claude-work");
        // Claude Code in its own folder answered at 10:00 and 14:00; pointed
        // at ~/.claude-work, where it keeps whose sign-in it holds, at 11:00.
        claude_session(&own, "own", &[at(10, 0), at(14, 0)]);
        claude_session(&work, "work", &[at(11, 0)]);
        std::fs::write(work.join(".claude.json"), "{}").unwrap();
        let spans = [
            // Its own folder held personal from 9:00 to 10:30, and, after
            // the person signed into another account, work from 13:00.
            (at(9, 0), claude_signed(&own, Some("claude:personal"))),
            (at(10, 30), claude_signed(&own, Some("claude:personal"))),
            (at(13, 0), claude_signed(&own, Some("claude:work"))),
            (at(15, 0), claude_signed(&own, Some("claude:work"))),
            // ~/.claude-work held a third account throughout.
            (at(9, 0), claude_signed(&work, Some("claude:other"))),
        ];
        let expected = [
            (
                "msg_own-0:req_own-0".to_owned(),
                Some("claude:personal".to_owned()),
            ),
            (
                "msg_own-1:req_own-1".to_owned(),
                Some("claude:work".to_owned()),
            ),
            (
                "msg_work-0:req_work-0".to_owned(),
                Some("claude:other".to_owned()),
            ),
        ];

        // Recorded before history is read, and after, in either order: the
        // same accounts.
        for (history_first, reversed) in [(true, false), (false, true)] {
            let data = tempfile::tempdir().unwrap();
            let engine = Engine::open(data.path(), home.path()).unwrap();
            if history_first {
                engine.scan().unwrap();
            }
            let mut ordered = spans.to_vec();
            if reversed {
                ordered.reverse();
            }
            for (when, seen) in ordered {
                let mut ledger = engine.writing().unwrap();
                if ledger.record_sign_ins(&[seen], when).unwrap() {
                    engine.catch_up(&mut ledger).unwrap();
                }
            }
            if !history_first {
                engine.scan().unwrap();
            }
            assert_eq!(drew_on(&engine), expected, "history first: {history_first}");
            // Of a session that drew on two accounts as much, the first by id.
            let sessions: Vec<(String, Option<String>)> = engine
                .sessions(&crate::SessionQuery {
                    empty: true,
                    ..crate::SessionQuery::default()
                })
                .unwrap()
                .items
                .into_iter()
                .map(|row| (row.key.native().to_owned(), row.account))
                .collect();
            assert!(sessions.contains(&("own".to_owned(), Some("claude:personal".to_owned()))));
            assert!(sessions.contains(&("work".to_owned(), Some("claude:other".to_owned()))));
        }
    }

    #[test]
    fn two_accounts_of_one_subscription_rising_at_once_share_only_their_own_use() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let own = home.path().join(".claude");
        let work = home.path().join(".claude-work");
        // One response in each folder in the same stretch, each costing the
        // same.
        claude_session(&own, "own", &[at(10, 0)]);
        claude_session(&work, "work", &[at(10, 10)]);
        std::fs::write(work.join(".claude.json"), "{}").unwrap();
        let engine = Engine::open(data.path(), home.path()).unwrap();
        engine.scan().unwrap();
        // Each account's five hours began at 9:00 and was read at 11:00:
        // personal's had risen 10 points, work's 20.
        let read = |id: &str, used: f64| Read {
            id: id.to_owned(),
            label: None,
            plan: None,
            agents: vec![Agent::ClaudeCode],
            limits: Ok(vec![Reported {
                key: "five_hour".to_owned(),
                name: "5 hours".to_owned(),
                scope: None,
                used,
                starts: Some(at(9, 0)),
                resets: Some(at(14, 0)),
            }]),
        };
        {
            let mut ledger = engine.writing().unwrap();
            ledger
                .record_limits(
                    Subscription::Claude,
                    &[read("claude:personal", 10.0), read("claude:work", 20.0)],
                    at(11, 0),
                )
                .unwrap();
            ledger
                .record_sign_ins(
                    &[
                        claude_signed(&own, Some("claude:personal")),
                        claude_signed(&work, Some("claude:work")),
                    ],
                    at(11, 0),
                )
                .unwrap();
            engine.catch_up(&mut ledger).unwrap();
        }
        let readers = engine.reading().unwrap();
        let taken = |account: &str| -> Vec<(String, f64)> {
            crate::limits::share::window(
                &readers.ledger,
                readers.cache.connection(),
                account,
                "five_hour",
                at(11, 30),
            )
            .unwrap()
            .unwrap()
            .sessions
            .into_iter()
            .map(|(key, share)| (key.native().to_owned(), share))
            .collect()
        };
        // Each rise is all its own account's one response's: 10 points of
        // personal's, 20 of work's, neither shared with the other.
        assert_eq!(taken("claude:personal"), [("own".to_owned(), 10.0)]);
        assert_eq!(taken("claude:work"), [("work".to_owned(), 20.0)]);
    }

    #[test]
    fn a_rise_only_unpriced_responses_spent_in_is_neither_shared_nor_elsewhere() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let own = home.path().join(".claude");
        // A priced response at 10:00, and one of a model no catalog prices
        // at 12:00, both of the account signed in from 9:00.
        claude_session(&own, "priced", &[at(10, 0)]);
        claude_session_of(&own, "unpriced", "claude-nobody-knows-1", &[at(12, 0)]);
        let engine = Engine::open(data.path(), home.path()).unwrap();
        engine.scan().unwrap();
        let read = |used: f64| Read {
            id: "claude:personal".to_owned(),
            label: None,
            plan: None,
            agents: vec![Agent::ClaudeCode],
            limits: Ok(vec![Reported {
                key: "five_hour".to_owned(),
                name: "5 hours".to_owned(),
                scope: None,
                used,
                starts: Some(at(9, 0)),
                resets: Some(at(14, 0)),
            }]),
        };
        {
            let mut ledger = engine.writing().unwrap();
            ledger
                .record_sign_ins(&[claude_signed(&own, Some("claude:personal"))], at(9, 0))
                .unwrap();
            for (when, used) in [(at(11, 0), 10.0), (at(13, 0), 25.0), (at(13, 30), 30.0)] {
                ledger
                    .record_limits(Subscription::Claude, &[read(used)], when)
                    .unwrap();
            }
            engine.catch_up(&mut ledger).unwrap();
        }
        let readers = engine.reading().unwrap();
        let window = crate::limits::share::window(
            &readers.ledger,
            readers.cache.connection(),
            "claude:personal",
            "five_hour",
            at(13, 45),
        )
        .unwrap()
        .unwrap();
        // 9:00 to 11:00 rose 10, all the priced response's; 11:00 to 13:00
        // rose 15 while only the unpriced one was made here; 13:00 to 13:30
        // rose 5 while nothing was.
        let sessions: Vec<(String, f64)> = window
            .sessions
            .iter()
            .map(|(key, share)| (key.native().to_owned(), *share))
            .collect();
        assert_eq!(sessions, [("priced".to_owned(), 10.0)]);
        assert_eq!(window.unpriced, 15.0);
        assert_eq!(window.elsewhere, 5.0);
    }
}
