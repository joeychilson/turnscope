//! Keeping the engine current while the app runs.
//!
//! Only one running engine keeps a data directory current at a time. One
//! started while another runs waits, answering from what the other keeps,
//! and takes over when it stops.
//!
//! Keeping it, three threads do the work. One reads history: it watches every
//! agent's roots through the file system's events (FSEvents on macOS),
//! gathers the paths that change until writing pauses, and reads only those.
//! Every ten minutes, and whenever the file system says it dropped events, it
//! looks through everything, which finds what an event was missed for and
//! roots that have appeared since. Another checks models.dev for new prices
//! every six hours, and the third reads subscriptions' limits, so a slow
//! network never holds up reading history. The history thread starts the
//! other two once it holds the keeper lock, and stops them before it lets go.
//!
//! Waking the Mac needs no look of its own: nothing writes while it sleeps,
//! and FSEvents' journal outlasts sleep. Waits that must follow the clock
//! rather than time awake, the six hours between price checks and the five
//! minutes between reads of limits, look at the system clock, so what fell
//! due in the night is done soon after waking. A price check sends the last
//! catalog's entity tag as `If-None-Match`, so when nothing has changed it
//! gets a 304 and no body.
//!
//! What changed goes to every subscriber as a [`Change`]. A line an agent
//! writes reached subscribers in about 190 ms on 2026-09-23.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant as Clock};

use notify::{EventKind, RecursiveMode, Watcher as _};

use crate::cache::Caught;
use crate::catalog::Catalog;
use crate::error::Result;
use crate::limits::{self, Subscription};
use crate::net::{self, Destination};
use crate::session::SessionKey;
use crate::sharing::{self, Held, Lock};
use crate::time::Instant;
use crate::{Engine, ScanReport, ingest};

/// How long writing must pause before what changed is read.
const SETTLE: Duration = Duration::from_millis(150);

/// The longest what changed waits to be read while writing goes on.
const LONGEST_WAIT: Duration = Duration::from_secs(1);

/// How often everything is looked through, in case an event was missed.
const LOOK_THROUGH: Duration = Duration::from_secs(10 * 60);

/// How often a root that isn't there is looked for when there is nothing
/// to watch for it, as for an agent not installed: a look at a few paths.
const LOOK_FOR_ROOTS: Duration = Duration::from_secs(30);

/// How often models.dev is asked for new prices.
const PRICES: Duration = Duration::from_secs(6 * 60 * 60);

/// How soon models.dev is asked again after it could not be reached.
const RETRY: Duration = Duration::from_secs(30 * 60);

/// How often the places sign-ins are kept are looked at: a look at a few
/// files, cheap enough that one renewed or added in an agent shows within
/// this long.
const LOOK_FOR_SIGN_INS: Duration = Duration::from_secs(30);

/// The longest a wait for prices lasts before the clock is looked at again.
/// A wait stands still while the Mac sleeps, so a check due after a night
/// asleep would otherwise wait up to six more hours.
const LOOK_AT_THE_CLOCK: Duration = Duration::from_secs(5 * 60);

/// How often an engine waiting for another to stop keeping the data directory
/// tries again.
const WAIT_TO_KEEP: Duration = Duration::from_secs(2);

/// How often an account's limits are read while its sign-ins are unchanged.
/// Every read is a request to the provider, some of which turn away frequent
/// polling.
const READ_LIMITS: Duration = Duration::from_secs(5 * 60);

/// Where the catalog is fetched from.
const MODELS_DEV: &str = "https://models.dev/api.json";

/// The share of the models the last catalog priced that a new catalog must
/// still price, or it is taken to be broken: nine in ten.
const KEPT: (usize, usize) = (9, 10);

/// A check of models.dev for new prices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogCheck {
    /// When it was made.
    pub at: Instant,
    /// What it found.
    pub outcome: CatalogOutcome,
    /// For a catalog taken in, how many models it added, changed and
    /// corrected; for one refused or not reached, why. Empty when unchanged.
    pub detail: String,
}

/// What a check of models.dev for new prices found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogOutcome {
    /// The catalog has not changed since it was last taken in.
    Unchanged,
    /// A new catalog was taken in.
    Updated,
    /// A catalog arrived that can't be taken in, and the last one stands:
    /// one that looks broken, or one fetched after the clock went back,
    /// which is dated no later than the last.
    Refused,
    /// models.dev could not be reached; the last catalog stands.
    Unreachable,
}

impl CatalogOutcome {
    /// The outcome as stored.
    pub(crate) fn key(self) -> &'static str {
        match self {
            CatalogOutcome::Unchanged => "unchanged",
            CatalogOutcome::Updated => "updated",
            CatalogOutcome::Refused => "refused",
            CatalogOutcome::Unreachable => "unreachable",
        }
    }

    /// The outcome with `key`.
    pub(crate) fn from_key(key: &str) -> Option<CatalogOutcome> {
        [
            CatalogOutcome::Unchanged,
            CatalogOutcome::Updated,
            CatalogOutcome::Refused,
            CatalogOutcome::Unreachable,
        ]
        .into_iter()
        .find(|outcome| outcome.key() == key)
    }
}

impl Engine {
    /// Look through everything, telling subscribers what changed or what went
    /// wrong, and of each artifact that fails to be read and isn't among
    /// those `failing` has told of.
    fn look_through(&self, failing: &mut Failing) {
        match self.scan() {
            Ok(report) => self.tell(failing.untold(report.failed, true)),
            Err(error) => self.publish(Change::Trouble(format!("reading history: {error}"))),
        }
    }

    /// Read what changed at `paths`, telling subscribers what changed or what
    /// went wrong, and of each artifact that fails to be read and isn't among
    /// those `failing` has told of.
    fn read_changed(&self, paths: &[PathBuf], failing: &mut Failing) {
        let read = (|| -> Result<ScanReport> {
            let roots = self.roots();
            let mut ledger = self.writing()?;
            let report = ingest::scan_paths(&self.readers, &roots, &mut ledger, paths)?;
            self.catch_up(&mut ledger)?;
            self.let_go(&ledger)?;
            Ok(report)
        })();
        match read {
            Ok(report) => self.tell(failing.untold(report.failed, false)),
            Err(error) => self.publish(Change::Trouble(format!("reading history: {error}"))),
        }
    }

    /// Tell subscribers of each artifact in `failed` and why it couldn't be
    /// read.
    fn tell(&self, failed: Vec<(PathBuf, String)>) {
        for (path, reason) in failed {
            self.publish(Change::Trouble(format!(
                "could not read {}: {reason}",
                path.display()
            )));
        }
    }

    /// When models.dev was last asked for prices, and whether it answered.
    ///
    /// # Errors
    ///
    /// Returns an error when the ledger cannot be read.
    fn last_price_check(&self) -> Result<Option<(Instant, bool)>> {
        let checked = self.ask(|ledger, _| ledger.last_check())?;
        Ok(checked.map(|check| (check.at, check.outcome != CatalogOutcome::Unreachable)))
    }

    /// Ask models.dev whether its catalog has changed, and take in a new one
    /// that looks whole: one that parses, lists at least a thousand priced
    /// models, and still prices nine in ten of those the last catalog did.
    /// Usage made after a price changed is charged at the new price from
    /// then on.
    ///
    /// Every check is recorded in the ledger, whatever it found.
    ///
    /// # Errors
    ///
    /// Returns an error when the ledger cannot be read or written. A failure
    /// to reach models.dev, or a broken catalog, is what the check found.
    fn check_prices(&self) -> Result<CatalogCheck> {
        let stored = self.ask(|ledger, _| ledger.catalog())?;
        let asking: Vec<String> = stored
            .iter()
            .filter_map(|stored| stored.etag.as_ref())
            .map(|etag| format!("If-None-Match: {etag}"))
            .collect();
        let at = Instant::now();
        let (outcome, detail) = match net::get(Destination::Catalog, MODELS_DEV, &asking) {
            Err(error) => (CatalogOutcome::Unreachable, error.to_string()),
            Ok(answer) if answer.status == 304 => (CatalogOutcome::Unchanged, String::new()),
            Ok(answer) if answer.status != 200 => (
                CatalogOutcome::Unreachable,
                format!("models.dev answered {}", answer.status),
            ),
            Ok(answer) => match Catalog::from_models_dev(&answer.body, at) {
                Err(error) => (CatalogOutcome::Refused, error.to_string()),
                Ok(catalog) => {
                    let last = stored.as_ref().map(|stored| &stored.catalog);
                    if let Some(reason) = last.and_then(|last| dropped(last, &catalog)) {
                        (CatalogOutcome::Refused, reason)
                    } else {
                        let mut ledger = self.writing()?;
                        match ledger.take_catalog(&catalog, "models.dev", answer.etag.as_deref())? {
                            Some(taken) => {
                                self.catch_up(&mut ledger)?;
                                self.let_go(&ledger)?;
                                (
                                    CatalogOutcome::Updated,
                                    format!(
                                        "{} added, {} changed, {} corrected",
                                        taken.added, taken.changed, taken.corrected
                                    ),
                                )
                            }
                            // Dated when it was fetched, it is dated before the
                            // catalog kept only when the clock has gone back.
                            None => (
                                CatalogOutcome::Refused,
                                format!(
                                    "it was fetched at {at}, no later than the catalog kept: the \
                                     clock went back"
                                ),
                            ),
                        }
                    }
                }
            },
        };
        let check = CatalogCheck {
            at,
            outcome,
            detail,
        };
        self.writing()?.note_check(&check)?;
        Ok(check)
    }
}

/// Why `catalog` looks broken beside `last`, the catalog taken in before, if
/// it does: it no longer prices nine in ten of the models `last` priced, by
/// provider and model. A listing cut short keeps some providers whole and
/// drops others, which a count of models alone can miss.
fn dropped(last: &Catalog, catalog: &Catalog) -> Option<String> {
    fn priced(catalog: &Catalog) -> HashSet<(&str, &str)> {
        catalog
            .models()
            .filter(|(_, _, model)| model.prices.is_some())
            .map(|(provider, id, _)| (provider, id))
            .collect()
    }
    let before = priced(last);
    let kept = before.intersection(&priced(catalog)).count();
    (kept.saturating_mul(KEPT.1) < before.len().saturating_mul(KEPT.0)).then(|| {
        format!(
            "it prices {kept} of the {} models the last catalog priced",
            before.len()
        )
    })
}

/// Something that changed in what the engine answers.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// History was read, and `sessions` changed.
    History {
        /// Sessions whose facts or totals changed, with every session above
        /// each, whose usage with its subagents' changed with it.
        sessions: Vec<SessionKey>,
        /// Whether everything was worked out again, so every answer may have
        /// changed.
        everything: bool,
    },
    /// Prices changed, so every cost was worked out again. History read in
    /// the same catch-up comes as a change of its own beside it.
    Prices,
    /// Subscription limits were read, or the person hid an account or
    /// showed it again.
    Limits,
    /// A limit gave rise to an alert, sent once per window.
    Alert(crate::limits::Alert),
    /// The weekly recap is due: how each account's week that ended went,
    /// sent once a week, from Monday 9 AM where the clock is.
    Recap(Vec<crate::limits::WeekEnded>),
    /// Reading history, an artifact of it, prices or limits failed; what the
    /// engine had stands, and it tries again.
    Trouble(String),
}

impl Change {
    /// The changes a catch-up of the cache made: new prices, history, both
    /// or neither, so a subscriber that heeds only one of them hears of it.
    pub(crate) fn of(caught: Caught) -> Vec<Change> {
        let mut changes = Vec::new();
        if caught.prices {
            changes.push(Change::Prices);
        }
        if caught.rebuilt || !caught.sessions.is_empty() {
            changes.push(Change::History {
                sessions: caught.sessions.into_iter().collect(),
                everything: caught.rebuilt,
            });
        }
        changes
    }
}

/// How the engine runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Whether to ask models.dev for new prices. Without, the prices the app
    /// shipped with, or last fetched, stand.
    pub check_prices: bool,
    /// Whether to ask providers for subscription limits, with the sign-ins
    /// the agents keep.
    pub read_limits: bool,
}

/// What the history thread is asked to do.
enum Signal {
    /// Read what changed at these paths.
    Changed(Vec<PathBuf>),
    /// Look through everything.
    LookThrough,
    /// The engine keeping the data directory may have let go of it.
    Released,
    /// Stop.
    Stop,
}

/// The engine running. Dropping it stops the engine's threads, and waits for
/// each to finish what it is doing: a read under way, or a request, which
/// curl ends within its limit of a minute.
///
/// A process that ends needn't drop it: its threads end with it, and every
/// lock with them, and a write cut short was a transaction, which the next
/// open of the ledger undoes. The app ends so when it quits, and never waits
/// on a request.
pub struct Running {
    signals: Sender<Signal>,
    history: Option<JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.signals.send(Signal::Stop);
        // The history thread stops the threads it started before it lets go
        // of the data directory.
        if let Some(thread) = self.history.take() {
            let _ = thread.join();
        }
    }
}

/// Start keeping `engine` current.
///
/// # Errors
///
/// Returns [`std::io::Error`] when the history thread cannot be started.
pub(crate) fn run(engine: &Arc<Engine>, options: Options) -> std::io::Result<Running> {
    let (signals, received) = mpsc::channel::<Signal>();
    let events = signals.clone();
    let engine = Arc::clone(engine);
    let history = std::thread::Builder::new()
        .name("turnscope-history".into())
        .spawn(move || keep(&engine, options, &events, &received))?;
    Ok(Running {
        signals,
        history: Some(history),
    })
}

/// Keep the data directory current until told to stop, once no other engine
/// does: read history as it changes, and, as `options` allow, check prices
/// and read limits on threads of their own.
///
/// Those threads start once this one holds the keeper lock, and are stopped
/// before it lets go, so only the keeper checks prices and reads limits.
fn keep(
    engine: &Arc<Engine>,
    options: Options,
    events: &Sender<Signal>,
    signals: &Receiver<Signal>,
) {
    let keeper = match Lock::open(&engine.data.join(sharing::KEEPER)) {
        Ok(keeper) => keeper,
        Err(error) => {
            engine.publish(Change::Trouble(format!("keeping history: {error}")));
            return;
        }
    };
    let Some(held) = wait_to_keep(engine, &keeper, events, signals) else {
        return;
    };
    let keeping = Keeping::start(&engine.keeping);
    let helpers = Helpers::start(engine, options);
    keep_history(engine, events, signals);
    drop(helpers);
    drop(keeping);
    drop(held);
    // An engine waiting to take over watches for this; one that doesn't see
    // it tries again soon anyway.
    let _ = keeper.touch();
}

/// The threads that check prices and read limits for the keeper. Dropping
/// them stops them, every one told before any is waited for.
struct Helpers(Vec<(Sender<()>, JoinHandle<()>)>);

impl Helpers {
    /// Start those `options` ask for.
    fn start(engine: &Arc<Engine>, options: Options) -> Helpers {
        let mut threads = Vec::new();
        if options.check_prices {
            threads.extend(spawn(engine, "turnscope-prices", keep_prices));
        }
        if options.read_limits {
            threads.extend(spawn(engine, "turnscope-limits", keep_limits));
        }
        Helpers(threads)
    }
}

impl Drop for Helpers {
    fn drop(&mut self) {
        let (stops, threads): (Vec<_>, Vec<_>) = self.0.drain(..).unzip();
        // A thread stops when its channel closes.
        drop(stops);
        for thread in threads {
            let _ = thread.join();
        }
    }
}

/// Start the thread `name`, doing `keep` with `engine` until its channel
/// closes. `None`, told to subscribers, when it cannot be started.
fn spawn(
    engine: &Arc<Engine>,
    name: &str,
    keep: fn(&Engine, &Receiver<()>),
) -> Option<(Sender<()>, JoinHandle<()>)> {
    let (stop, stopped) = mpsc::channel::<()>();
    let kept = Arc::clone(engine);
    match std::thread::Builder::new()
        .name(name.into())
        .spawn(move || keep(&kept, &stopped))
    {
        Ok(thread) => Some((stop, thread)),
        Err(error) => {
            engine.publish(Change::Trouble(format!("could not start {name}: {error}")));
            None
        }
    }
}

/// Read history as it changes until told to stop.
fn keep_history(engine: &Engine, events: &Sender<Signal>, signals: &Receiver<Signal>) {
    let sender = events.clone();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let signal = match event {
            // The file system dropped events, so what changed is unknown.
            Ok(event) if event.need_rescan() => Signal::LookThrough,
            Ok(event) if matches!(event.kind, EventKind::Access(_)) => return,
            Ok(event) => Signal::Changed(event.paths),
            Err(_) => Signal::LookThrough,
        };
        let _ = sender.send(signal);
    });
    let mut watching = Watching {
        watcher: match watcher {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                engine.publish(Change::Trouble(format!(
                    "could not watch for changes: {error}"
                )));
                None
            }
        },
        watched: BTreeMap::new(),
        failed: BTreeSet::new(),
        there: None,
    };

    let mut failing = Failing::default();
    let mut looking = watching.update(engine).looking;
    guarded(engine, "Reading history", || {
        engine.look_through(&mut failing)
    });
    let mut looked = Clock::now();
    let mut looked_for_roots = Clock::now();
    loop {
        let mut wait = LOOK_THROUGH.saturating_sub(looked.elapsed());
        if looking {
            wait = wait.min(LOOK_FOR_ROOTS.saturating_sub(looked_for_roots.elapsed()));
        }
        match signals.recv_timeout(wait) {
            Ok(Signal::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            // Left over from waiting to keep the data directory.
            Ok(Signal::Released) => {}
            Err(RecvTimeoutError::Timeout) if looked.elapsed() < LOOK_THROUGH => {
                // Only a root that wasn't there is due a look: one there now
                // is watched from here on, and read.
                let update = watching.update(engine);
                looking = update.looking;
                looked_for_roots = Clock::now();
                if !update.appeared.is_empty() {
                    guarded(engine, "Reading history", || {
                        engine.read_changed(&update.appeared, &mut failing);
                    });
                }
            }
            Ok(Signal::LookThrough) | Err(RecvTimeoutError::Timeout) => {
                looking = watching.update(engine).looking;
                looked_for_roots = Clock::now();
                guarded(engine, "Reading history", || {
                    engine.look_through(&mut failing)
                });
                looked = Clock::now();
            }
            Ok(Signal::Changed(paths)) => {
                let mut changed = paths;
                let mut look_through = false;
                let first = Clock::now();
                // Gather what else changes until writing pauses.
                while first.elapsed() < LONGEST_WAIT {
                    match signals.recv_timeout(SETTLE) {
                        Ok(Signal::Changed(more)) => changed.extend(more),
                        Ok(Signal::LookThrough) => look_through = true,
                        Ok(Signal::Released) => {}
                        Ok(Signal::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                        Err(RecvTimeoutError::Timeout) => break,
                    }
                }
                // A root that has just appeared, seen from the directory it
                // is in, is watched before it is read, so nothing written to
                // it in between goes unseen.
                let update = watching.update(engine);
                looking = update.looking;
                changed.extend(update.appeared);
                if look_through {
                    guarded(engine, "Reading history", || {
                        engine.look_through(&mut failing)
                    });
                    looked = Clock::now();
                } else {
                    guarded(engine, "Reading history", || {
                        engine.read_changed(&changed, &mut failing);
                    });
                }
            }
        }
    }
}

/// What the history thread watches: each root there, and, for a root not
/// there, the directory it would be in.
struct Watching {
    watcher: Option<notify::RecommendedWatcher>,
    /// Each directory watched, and whether with everything under it.
    watched: BTreeMap<PathBuf, bool>,
    /// Directories that could not be watched, told of once each.
    failed: BTreeSet<PathBuf>,
    /// The roots there at the last update; `None` before the first.
    there: Option<BTreeSet<PathBuf>>,
}

/// What [`Watching::update`] found.
struct Update {
    /// Roots watched now that weren't before, to be read at once.
    appeared: Vec<PathBuf>,
    /// Whether a root isn't there and nothing can be watched for it, so it
    /// is to be looked for every [`LOOK_FOR_ROOTS`].
    looking: bool,
}

impl Watching {
    /// Watch what the engine's roots call for now, as [`to_watch`] says, and
    /// stop watching what they no longer do. A watch that can't be set up is
    /// told of once, since that directory's changes are then read only when
    /// everything is looked through.
    fn update(&mut self, engine: &Engine) -> Update {
        let roots: Vec<PathBuf> = engine.roots().into_iter().map(|(_, root)| root).collect();
        let present = |roots: &[PathBuf]| -> BTreeSet<PathBuf> {
            roots.iter().filter(|root| root.exists()).cloned().collect()
        };
        // A root made while the watches are set up is seen by neither: the
        // watch on its folder starts after it was made, and it was looked
        // for before. So what is there is looked at again once they are,
        // until it holds still; a root made after that, a watch sees.
        let mut there = present(&roots);
        let mut looking = false;
        for _ in 0..4 {
            let (wanted, still_looking) =
                to_watch(&roots, &engine.home, Path::exists, Path::is_dir);
            looking = still_looking;
            self.watch(engine, wanted);
            let again = present(&roots);
            if again == there {
                break;
            }
            there = again;
        }
        // Roots there now that weren't at the last update, watched by now.
        // The first update has nothing to compare with, and a look through
        // everything follows it.
        let appeared = match &self.there {
            Some(before) => there.difference(before).cloned().collect(),
            None => Vec::new(),
        };
        self.there = Some(there);
        Update { appeared, looking }
    }

    /// Watch `wanted`, each directory with everything under it or not, and
    /// nothing else.
    ///
    /// Every change is made at once, as macOS's file system events start
    /// again with each: watching five agents' seven directories one at a
    /// time took 14 to 16 ms in a release build and all at once 6 to 8 ms,
    /// and in the debug build tests run, 2.4 s against 0.4 s (2026-09-27).
    fn watch(&mut self, engine: &Engine, wanted: BTreeMap<PathBuf, bool>) {
        let Some(watcher) = &mut self.watcher else {
            return;
        };
        let stale: Vec<PathBuf> = self
            .watched
            .iter()
            .filter(|(path, recursive)| wanted.get(*path) != Some(*recursive))
            .map(|(path, _)| path.clone())
            .collect();
        let new: Vec<(PathBuf, bool)> = wanted
            .into_iter()
            .filter(|(path, recursive)| self.watched.get(path) != Some(recursive))
            .collect();
        if stale.is_empty() && new.is_empty() {
            return;
        }
        let mut paths = watcher.paths_mut();
        for path in stale {
            // One gone with its directory needs no unwatching.
            let _ = paths.remove(&path);
            self.watched.remove(&path);
        }
        for (path, recursive) in new {
            let mode = if recursive {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            match paths.add(&path, mode) {
                Ok(()) => {
                    self.failed.remove(&path);
                    self.watched.insert(path, recursive);
                }
                Err(error) => {
                    if self.failed.insert(path.clone()) {
                        engine.publish(Change::Trouble(format!(
                            "could not watch {} for changes, so they are read every ten \
                             minutes: {error}",
                            path.display()
                        )));
                    }
                }
            }
        }
        if let Err(error) = paths.commit() {
            engine.publish(Change::Trouble(format!(
                "could not watch for changes, so they are read every ten minutes: {error}"
            )));
        }
    }
}

/// What to watch for `roots` under `home`, with `exists` and `is_dir` saying
/// what is there: each directory root there, with everything under it; the
/// directory a file root is in; and, for a root not there, the directory it
/// would be in when that is there and isn't `home` itself, to see the root
/// appear. And whether some root is neither there nor watched for, so to be
/// looked for now and then.
///
/// A root not there is watched for only from its own directory: one further
/// up is shared by other programs, as `~/.local/share` is, or is the home
/// directory, and watching it would wake the app for everything written
/// there, since macOS reports changes in all of a watched directory.
fn to_watch(
    roots: &[PathBuf],
    home: &Path,
    exists: impl Fn(&Path) -> bool,
    is_dir: impl Fn(&Path) -> bool,
) -> (BTreeMap<PathBuf, bool>, bool) {
    let mut wanted = BTreeMap::new();
    let mut looking = false;
    for root in roots {
        if exists(root) {
            if is_dir(root) {
                wanted.insert(root.clone(), true);
            } else if let Some(folder) = root.parent() {
                // A file is watched through its directory, which also sees
                // its database's side files.
                wanted.entry(folder.to_path_buf()).or_insert(false);
            }
            continue;
        }
        match root.parent() {
            Some(folder) if folder != home && folder.starts_with(home) && exists(folder) => {
                wanted.entry(folder.to_path_buf()).or_insert(false);
            }
            _ => looking = true,
        }
    }
    (wanted, looking)
}

/// The artifacts that fail to be read, as subscribers were told of them: an
/// artifact that goes on failing is tried at every read, and told of once.
#[derive(Debug, Default)]
struct Failing(BTreeSet<PathBuf>);

impl Failing {
    /// Of `failed`, those not told of yet, counted as told from now on.
    /// `everything` when `failed` came of a look through everything, and so
    /// holds every artifact that fails: one that no longer does is
    /// forgotten, to be told of again should it fail again.
    fn untold(
        &mut self,
        failed: Vec<(PathBuf, String)>,
        everything: bool,
    ) -> Vec<(PathBuf, String)> {
        if everything {
            self.0
                .retain(|path| failed.iter().any(|(failing, _)| failing == path));
        }
        failed
            .into_iter()
            .filter(|(path, _)| self.0.insert(path.clone()))
            .collect()
    }
}

/// Wait until no other engine keeps the data directory, and hold the lock
/// that says this one does. `None` when told to stop first.
///
/// An engine that stops keeping it touches the lock file once it lets go, and
/// the file is watched for that, so taking over is prompt. One that ends
/// without, as a crash does, is noticed within [`WAIT_TO_KEEP`].
fn wait_to_keep<'a>(
    engine: &Engine,
    keeper: &'a Lock,
    events: &Sender<Signal>,
    signals: &Receiver<Signal>,
) -> Option<Held<'a>> {
    let sender = events.clone();
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let released = event.map_or(true, |event| {
            event.need_rescan()
                || event
                    .paths
                    .iter()
                    .any(|path| path.file_name() == Some(OsStr::new(sharing::KEEPER)))
        });
        if released {
            let _ = sender.send(Signal::Released);
        }
    });
    // Without a watcher, waiting falls back on trying again now and then.
    let _watching = watcher.and_then(|mut watcher| {
        watcher.watch(&engine.data, RecursiveMode::NonRecursive)?;
        Ok(watcher)
    });
    let mut told = false;
    loop {
        match keeper.try_hold() {
            Ok(Some(held)) => return Some(held),
            Ok(None) if !told => engine.publish(Change::Trouble(
                "another Turnscope keeps this history current; waiting for it to stop".into(),
            )),
            Ok(None) => {}
            Err(error) if !told => {
                engine.publish(Change::Trouble(format!("keeping history: {error}")));
            }
            Err(_) => {}
        }
        told = true;
        match signals.recv_timeout(WAIT_TO_KEEP) {
            Ok(Signal::Stop) | Err(RecvTimeoutError::Disconnected) => return None,
            // What changes meanwhile is found by the look through that
            // keeping starts with.
            Ok(Signal::Changed(_) | Signal::LookThrough | Signal::Released)
            | Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

/// Marks this process as keeping the data directory until dropped.
struct Keeping<'a>(&'a AtomicBool);

impl<'a> Keeping<'a> {
    fn start(keeping: &'a AtomicBool) -> Keeping<'a> {
        keeping.store(true, Ordering::Release);
        Keeping(keeping)
    }
}

impl Drop for Keeping<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Check for new prices when they are due, until told to stop.
fn keep_prices(engine: &Engine, stop: &Receiver<()>) {
    // A check that failed, or stopped before it could record itself, waits
    // as one that could not reach models.dev does, rather than trying again
    // at once.
    let mut failed: Option<Instant> = None;
    loop {
        let now = Instant::now();
        let wait = match failed {
            Some(at) => remaining(at, RETRY, now),
            None => match engine.last_price_check() {
                Ok(None) => Duration::ZERO,
                // One that could not reach models.dev is tried again sooner.
                Ok(Some((at, answered))) => {
                    remaining(at, if answered { PRICES } else { RETRY }, now)
                }
                Err(error) => {
                    engine.publish(Change::Trouble(format!("prices: {error}")));
                    failed = Some(now);
                    RETRY
                }
            },
        };
        if wait.is_zero() {
            let recorded = guarded(engine, "Checking prices", || match engine.check_prices() {
                Ok(check) => {
                    if matches!(
                        check.outcome,
                        CatalogOutcome::Refused | CatalogOutcome::Unreachable
                    ) {
                        engine.publish(Change::Trouble(format!("prices: {}", check.detail)));
                    }
                    true
                }
                Err(error) => {
                    engine.publish(Change::Trouble(format!("prices: {error}")));
                    false
                }
            });
            failed = (recorded != Some(true)).then(Instant::now);
            continue;
        }
        // The wait stands still while the Mac sleeps, so it is cut short to
        // look at the clock again.
        match stop.recv_timeout(wait.min(LOOK_AT_THE_CLOCK)) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

/// How long from `now` until `interval` has passed since `at`, by the
/// system clock: none once it has, or when `at` is after `now`, as when the
/// clock was set back.
fn remaining(at: Instant, interval: Duration, now: Instant) -> Duration {
    // Any two instants can be subtracted without overflow.
    let since = now.millis() - at.millis();
    if since < 0 {
        return Duration::ZERO;
    }
    interval.saturating_sub(Duration::from_millis(since.unsigned_abs()))
}

/// Read every subscription's limits when its sign-ins change, and every five
/// minutes, until told to stop.
///
/// Where sign-ins are kept is looked at every half minute, in every folder
/// of the agents' found again then, which costs a look at the home
/// directory's entries and a few files; the sign-ins themselves are read
/// only when one of those places changed, since reading Claude Code's asks
/// the Keychain through a process of its own. A change is read once the
/// sign-ins have held still for a look: an agent signing in to another
/// account writes its token and whose it is apart, and a reading taken
/// between the two would be the new account's limits kept as the old one's.
fn keep_limits(engine: &Engine, stop: &Receiver<()>) {
    let mut seen: HashMap<Subscription, Seen> = HashMap::new();
    loop {
        // The agents' folders, looked for again each time, so one made since
        // is read.
        let folders = engine.folders();
        for subscription in Subscription::ALL {
            let stamp = limits::stamp(subscription, &folders, &engine.home);
            let last = seen.get(&subscription);
            let sign_ins = match last {
                Some(last) if last.stamp == stamp && last.sign_ins.is_some() => last.sign_ins,
                _ => match limits::fingerprint(subscription, &folders, &engine.home) {
                    Ok(sign_ins) => Some(sign_ins),
                    Err(error) => {
                        // Said once, not at every look it stays so.
                        if last.is_none_or(|last| last.sign_ins.is_some()) {
                            engine.publish(Change::Trouble(format!(
                                "{} limits: {error}",
                                subscription.name()
                            )));
                        }
                        None
                    }
                },
            };
            let mut read = last.and_then(|last| last.read);
            if let Some(sign_ins) = sign_ins
                && due(last, sign_ins, Instant::now())
            {
                guarded(engine, "Reading limits", || {
                    if let Err(error) = engine.read_limits(subscription) {
                        engine.publish(Change::Trouble(format!(
                            "{} limits: {error}",
                            subscription.name()
                        )));
                    }
                });
                read = Some((sign_ins, Instant::now()));
            }
            seen.insert(
                subscription,
                Seen {
                    stamp,
                    sign_ins,
                    read,
                },
            );
        }
        match stop.recv_timeout(LOOK_FOR_SIGN_INS) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

/// What a look saw of one subscription's sign-ins.
struct Seen {
    /// The stamp of the places they are kept.
    stamp: u64,
    /// The fingerprint of the sign-ins found then; `None` when a place they
    /// are kept couldn't be read.
    sign_ins: Option<u64>,
    /// The fingerprint read with last, and when, by the system clock, which
    /// goes on while the Mac sleeps.
    read: Option<(u64, Instant)>,
}

/// Whether to read a subscription's limits `now`, its sign-ins' fingerprint
/// `sign_ins`, when `last` is what the look before saw. The first look reads
/// at once. After that, sign-ins are read once they have held still for a
/// look, which one that couldn't be read hasn't, and every five minutes
/// while they hold still.
fn due(last: Option<&Seen>, sign_ins: u64, now: Instant) -> bool {
    let Some(last) = last else {
        return true;
    };
    last.sign_ins == Some(sign_ins)
        && last.read.is_none_or(|(read_with, at)| {
            read_with != sign_ins || remaining(at, READ_LIMITS, now).is_zero()
        })
}

/// Do `work`, and should it panic, as only a bug makes it, tell subscribers
/// rather than end the thread keeping the engine current: an app that went
/// on watching nothing until it was started again would say nothing of it.
/// `None` when it panicked.
fn guarded<T>(engine: &Engine, what: &str, work: impl FnOnce() -> T) -> Option<T> {
    let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).ok();
    if done.is_none() {
        engine.publish(Change::Trouble(format!(
            "{what} stopped unexpectedly; it is tried again later"
        )));
    }
    done
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use std::path::PathBuf;

    use super::{Failing, Seen, dropped, due, remaining, to_watch};
    use crate::catalog::Catalog;
    use crate::time::Instant;

    fn second(seconds: i64) -> Instant {
        Instant::from_millis(1_789_000_000_000 + seconds * 1_000).unwrap()
    }

    /// A catalog pricing each of `models`, by provider and model.
    fn pricing(models: impl IntoIterator<Item = (&'static str, String)>) -> Catalog {
        let mut listing = json!({});
        for (provider, model) in models {
            listing[provider]["models"][model] = json!({"cost": {"input": 1, "output": 2}});
        }
        Catalog::parse(listing.to_string().as_bytes()).unwrap()
    }

    #[test]
    fn a_catalog_must_still_price_nine_in_ten_of_the_models_priced_before() {
        let claude = |n: u8| ("anthropic", format!("claude-{n}"));
        let gpt = |name: &str| ("openai", name.to_owned());
        let last = pricing((0..10).map(claude));
        // Nine of the ten, and another.
        let nine = pricing((0..9).map(claude).chain([gpt("gpt-6")]));
        assert_eq!(dropped(&last, &nine), None);
        // As many models, but two of the ten gone for two others, which a
        // count of them would take.
        let eight = pricing((0..8).map(claude).chain([gpt("gpt-6"), gpt("gpt-6-mini")]));
        assert_eq!(
            dropped(&last, &eight).as_deref(),
            Some("it prices 8 of the 10 models the last catalog priced")
        );
        // The same models from another provider are other listings.
        let resold = pricing((0..10).map(|n| ("amazon-bedrock", format!("claude-{n}"))));
        assert!(dropped(&last, &resold).is_some());
    }

    /// What a look saw: sign-ins `sign_ins`, the last read with `1`, at
    /// `read`.
    fn seen(sign_ins: Option<u64>, read: Instant) -> Seen {
        Seen {
            stamp: 0,
            sign_ins,
            read: Some((1, read)),
        }
    }

    #[test]
    fn changed_sign_ins_are_read_once_they_have_held_still_for_a_look() {
        // The first look reads them at once.
        assert!(due(None, 1, second(0)));
        let read = second(0);
        let look = second(30);
        // Changed since the look before.
        assert!(!due(Some(&seen(Some(1), read)), 2, look));
        // As they were at the look before, but not as last read with.
        assert!(due(Some(&seen(Some(2), read)), 2, look));
        // A place that couldn't be read at the look before may have been
        // part way through a change.
        assert!(!due(Some(&seen(None, read)), 1, look));
    }

    #[test]
    fn unchanged_sign_ins_are_read_every_five_minutes() {
        let read = second(0);
        assert!(!due(Some(&seen(Some(1), read)), 1, second(299)));
        assert!(due(Some(&seen(Some(1), read)), 1, second(300)));
        // After a night asleep, at the first look.
        assert!(due(Some(&seen(Some(1), read)), 1, second(8 * 3_600)));
    }

    #[test]
    fn an_artifact_that_goes_on_failing_is_told_of_once() {
        let failed = |paths: &[&str]| -> Vec<(PathBuf, String)> {
            paths
                .iter()
                .map(|path| (PathBuf::from(path), "unreadable".to_owned()))
                .collect()
        };
        let told = |untold: Vec<(PathBuf, String)>| -> Vec<PathBuf> {
            untold.into_iter().map(|(path, _)| path).collect()
        };
        let mut failing = Failing::default();
        assert_eq!(
            told(failing.untold(failed(&["a", "b"]), true)),
            [PathBuf::from("a"), PathBuf::from("b")]
        );
        // Again at the next look through, and at a read of what changed.
        assert!(failing.untold(failed(&["a", "b"]), true).is_empty());
        assert!(failing.untold(failed(&["a"]), false).is_empty());
        // `a` reads again; `b` still fails.
        assert!(failing.untold(failed(&["b"]), true).is_empty());
        // `a` fails anew.
        assert_eq!(
            told(failing.untold(failed(&["a"]), false)),
            [PathBuf::from("a")]
        );
    }

    #[test]
    fn a_wait_is_over_once_the_clock_says_so_or_went_back() {
        let hours = |hours: u64| Duration::from_secs(hours * 3_600);
        let at = second(0);
        assert_eq!(remaining(at, hours(6), second(3_600)), hours(5));
        assert_eq!(remaining(at, hours(6), second(7 * 3_600)), Duration::ZERO);
        // Checked an hour from now: the clock was set back since.
        assert_eq!(remaining(at, hours(6), second(-3_600)), Duration::ZERO);
    }

    #[test]
    fn a_root_not_there_is_watched_for_from_its_own_folder_or_looked_for() {
        let home = PathBuf::from("/h");
        let there = [
            "/h/.claude",
            "/h/.claude/projects",
            "/h/.codex",
            "/h/.codex/session_index.jsonl",
            "/h/.local",
            "/h/.local/share",
        ];
        let exists = |path: &std::path::Path| {
            there
                .iter()
                .any(|there| path == std::path::Path::new(there))
        };
        let is_dir = |path: &std::path::Path| exists(path) && path.extension().is_none();
        let roots = |roots: &[&str]| roots.iter().map(PathBuf::from).collect::<Vec<_>>();
        let watched = |pairs: &[(&str, bool)]| {
            pairs
                .iter()
                .map(|(path, recursive)| (PathBuf::from(path), *recursive))
                .collect::<std::collections::BTreeMap<_, _>>()
        };

        // A root there is watched with all under it; a file, through its
        // folder; a root not there, from its folder, when that is there.
        assert_eq!(
            to_watch(
                &roots(&[
                    "/h/.claude/projects",
                    "/h/.codex/session_index.jsonl",
                    "/h/.codex/sessions"
                ]),
                &home,
                exists,
                is_dir
            ),
            (
                watched(&[("/h/.claude/projects", true), ("/h/.codex", false)]),
                false
            )
        );
        // Neither the home folder nor a folder other programs share, as
        // ~/.local/share is, is watched for a root: it is looked for.
        for missing in [
            "/h/.pi/agent/sessions",
            "/h/.grok",
            "/h/.local/share/opencode/opencode.db",
        ] {
            assert_eq!(
                to_watch(&roots(&[missing]), &home, exists, is_dir),
                (watched(&[]), true),
                "{missing}"
            );
        }
    }
}
