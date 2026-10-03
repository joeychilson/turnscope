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
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, mpsc};
use std::time::{Duration, Instant as Clock};

use rusqlite::Connection;

use crate::limits::recording::LimitsRead;

pub use crate::agent::{Agent, DiagnosticKind};
pub use crate::breakdown::{LimitShare, PromptUsage, SessionUsage, SubagentUsage};
pub use crate::catalog::{Catalog, CatalogError};
pub use crate::error::{Error, Result};
pub use crate::folders::{Folder, FolderOrigin};
pub use crate::handoff::{Command, FileChange, Goal, Handoff, Quote, Step, StepStatus};
pub use crate::health::{
    AgentHealth, Check, Comparison, Diagnostic, Difference, Doctor, Health, PriceSource, Pricing,
    Unpriced,
};
pub use crate::ingest::ScanReport;
pub use crate::limits::{
    AccountLimits, Alert, AlertKind, LimitProblem, LimitState, LimitTrack, LimitWindow, Outlook,
    PastWindow, Standing, Subscription, WeekEnded,
};
pub use crate::model::{ModelInfo, ModelKey};
pub use crate::outside::Counts;
pub use crate::query::{
    Dimension, Filter, Page, SessionOrder, SessionQuery, SessionRow, Totals, UsageQuery, UsageRow,
    UsageTable, running_since,
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
    /// While this process reads history in full ([`Engine::scan`]), the
    /// limits read meanwhile, which the scan records between its groups:
    /// it holds the ledger throughout, as reading history must, and a first
    /// read took 10 s (2026-09-30), during which limits read in under one
    /// waited to be recorded and the app showed no account. `None` while no
    /// scan runs, when limits are recorded as they are read.
    waiting: Mutex<Option<Vec<LimitsRead>>>,
    subscribers: Mutex<Vec<mpsc::Sender<Change>>>,
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
            waiting: Mutex::new(None),
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
        // Limits read while it holds the ledger wait for it to record them.
        let queueing = self.queue_limits();
        // A long read, as the first is, shows what it has read so far, the
        // newest history first: after a second, and then after four times as
        // long each time, since each showing works the cache out again.
        let mut caught = Clock::now();
        let mut wait = SHOW_PROGRESS;
        let roots = self.roots();
        let scanned = ingest::scan(&self.readers, &roots, &mut ledger, |ledger| {
            self.record_waiting(ledger, false);
            if caught.elapsed() >= wait {
                self.catch_up(ledger)?;
                caught = Clock::now();
                wait = wait.saturating_mul(4);
            }
            Ok(())
        });
        // Whatever became of the read, the last limits that waited are
        // recorded and no more wait.
        self.record_waiting(&mut ledger, true);
        drop(queueing);
        let report = scanned?;
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
    pub fn subscribe(&self) -> mpsc::Receiver<Change> {
        let (sender, receiver) = mpsc::channel();
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
        subscribers.retain(|subscriber| subscriber.send(change.clone()).is_ok());
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

    /// Every reader's roots in the folders of its agent read now, by the
    /// reader's place among them.
    pub(crate) fn roots(&self) -> Vec<(usize, PathBuf)> {
        self.folders()
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
        // Files the agent deleted since history was last read are gone, as
        // they would be had it been read since. A file there but out of
        // reach, as behind a folder that can't be searched, is no file gone
        // but one that can't be read, and says so.
        let mut present = Vec::with_capacity(artifacts.len());
        for path in artifacts {
            if path.try_exists().map_err(|error| Error::io(&path, error))? {
                present.push(path);
            }
        }
        let artifacts = present;
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

    /// The sessions `keys` name, subagents among them, as a list of all
    /// time shows them, by key: none for none, and none for a key no session
    /// has.
    ///
    /// # Errors
    ///
    /// Returns an error when the cache cannot be read.
    pub fn session_rows(&self, keys: &[SessionKey]) -> Result<HashMap<SessionKey, SessionRow>> {
        self.ask(|_, cache| query::rows(cache, keys))
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use crate::Engine;

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
    fn a_data_directory_it_makes_is_the_owners_alone() {
        let home = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let data = parent.path().join("com.example.turnscope");
        Engine::open(&data, home.path()).unwrap();
        let mode = std::fs::metadata(&data).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}
