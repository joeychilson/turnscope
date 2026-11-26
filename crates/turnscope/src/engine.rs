//! What an app runs: the engine keeps the data current while the app is
//! open, and tells it what to show. Each platform's app only draws what
//! it's told and passes on what the person does.
//!
//! Chosen over a daemon the app talks to, as `turnscope serve` was until
//! 0.3: one process instead of two, no protocol to keep in step with the
//! app, and 18.8 MB less memory (measured 2026-11-17).

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use notify::Watcher as _;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::alerts::{self, Alert, Settings};
use crate::limits::Ask;
use crate::status::{self, Agent, Status};
use crate::{Error, Result, agents, usage};

/// What the app shows: the status, and every agent Turnscope reads,
/// installed or not.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shown {
    #[serde(flatten)]
    pub status: Status,
    pub agents: Vec<Agent>,
}

/// A session that used a limit, as the panel lists it under an open account.
#[derive(Clone, Debug, PartialEq)]
pub struct UsedMost {
    /// `claude-code:0f6e3f6a-713c`.
    pub session: String,
    pub title: Option<String>,
    /// The name of its project's folder.
    pub project: Option<String>,
    /// Full path to its project folder, when known.
    pub project_path: Option<String>,
    /// The agent's id.
    pub agent: String,
    /// About how many points of the limit it used: its share of the cost,
    /// times the points used. None when the window's start isn't known, the
    /// window reset since the limit was read, or nothing used had a price.
    pub share_percent: Option<f64>,
}

/// What the engine tells the app.
#[derive(Debug)]
pub enum Event {
    /// What to show, and the alerts it raised. The first status after the
    /// data opens comes with those raised before but never acknowledged too,
    /// as when the app quit before it could tell them. Told when anything it
    /// shows changed, but not when only its minute did (`Showing::next`), so
    /// `at` can be minutes old. Told too after each `hide`,
    /// `save_settings` and `connect`, changed or not, so a change the app
    /// showed at once goes back if the request failed or was refused.
    Shown(Box<Shown>, Vec<Alert>),
    /// Whether the data is kept current: None once a catch-up has read
    /// every agent, or why the last one failed. Told when it changes.
    Kept(Option<Stopped>),
    /// What used a limit most, as `used_most` asked.
    UsedMost {
        account: String,
        limit: String,
        sessions: Vec<UsedMost>,
    },
    /// A `connect` ended, leaving the agent's connection `connection`: a
    /// failed one can leave some of its folders changed.
    Connected {
        agent: String,
        connection: crate::status::Connection,
        error: Option<String>,
    },
    /// A refresh asked for (`Engine::refresh_now`) ended, told after its
    /// status.
    Refreshed,
}

/// Why the data isn't kept current.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stopped {
    /// Reading or writing the data failed, for this reason: it's tried
    /// again.
    Failing(String),
    /// A newer Turnscope has written to the data (`Error::Newer`). The
    /// engine then stops for good: this is the last `Event::Kept` it tells,
    /// it writes nothing more, raises no alert, and refuses every request
    /// but `used_most` (`Engine`), so the app shows what was read.
    Newer { replaced: bool },
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Stopped::Failing(why) => f.write_str(why),
            Stopped::Newer { replaced } => Error::Newer {
                replaced: *replaced,
            }
            .fmt(f),
        }
    }
}

const AGAIN: Duration = Duration::from_secs(5);

/// Keeps the data current and tells the app what to show, on threads of its
/// own. Requests run on a single thread in order (over a thread per request:
/// with two `Settings` saves in a row, the first won in 9 of 300 tries,
/// measured 2026-11-18). Connects run on a separate thread since agent
/// commands can take a minute.
///
/// Once a newer Turnscope has written (`Stopped::Newer`),
/// write requests are refused and `used_most` still answers.
pub struct Engine {
    inner: Arc<Inner>,
    requests: Sender<Work>,
    connects: Sender<Work>,
}

type Work = Box<dyn FnOnce(&Inner) + Send>;

/// What woke `keep`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wake {
    /// Something was written in an agent's folder.
    Written,
    /// Limits were asked for: the panel opened (`Engine::ask_limits`), or
    /// an agent wrote a login (`agents::login_file`).
    Asked,
    /// The person asked for the limits (`Engine::refresh_now`).
    Force,
    /// The engine is shutting down (`Drop for Engine`).
    Stop,
}

#[derive(PartialEq, Eq)]
enum Kept {
    Unset,
    Current,
    Stopped(Stopped),
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Stop {
    ended: Mutex<bool>,
    changed: Condvar,
}

impl Stop {
    fn new() -> Stop {
        Stop {
            ended: Mutex::new(false),
            changed: Condvar::new(),
        }
    }

    fn ended(&self) -> bool {
        *locked(&self.ended)
    }

    fn end(&self) {
        *locked(&self.ended) = true;
        self.changed.notify_all();
    }

    /// Wait up to `duration` for shutdown; returns whether the engine ended.
    fn wait(&self, duration: Duration) -> bool {
        let guard = locked(&self.ended);
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, duration, |ended| !*ended)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard
    }
}

struct Inner {
    data: PathBuf,
    home: PathBuf,
    binary: PathBuf,
    tell: Box<dyn Fn(Event) + Send + Sync>,
    showing: Mutex<Option<Showing>>,
    kept: Mutex<Kept>,
    wake: Mutex<Option<Sender<Wake>>>,
    /// Whether this engine currently holds `app.lock`: only the primary
    /// instance reads pending alerts and raises new ones.
    keeping: AtomicBool,
    /// A forced read sent to `keep` and not answered yet, which `run`
    /// answers if `keep` ends first.
    forcing: AtomicBool,
    stop: Stop,
}

impl Engine {
    /// Start keeping the data in `data` current from the agents' folders in
    /// `home`, telling `tell` what to show. `binary` is the `turnscope`
    /// binary that `connect` has agents run.
    ///
    /// `tell` runs on the engine's threads, at times with the status held
    /// (`Inner::give`): it hands each event on and returns, and never waits
    /// for another.
    pub fn start(
        data: PathBuf,
        home: PathBuf,
        binary: PathBuf,
        tell: impl Fn(Event) + Send + Sync + 'static,
    ) -> Engine {
        let inner = Arc::new(Inner {
            data,
            home,
            binary,
            tell: Box::new(tell),
            showing: Mutex::new(None),
            kept: Mutex::new(Kept::Unset),
            wake: Mutex::new(None),
            keeping: AtomicBool::new(false),
            forcing: AtomicBool::new(false),
            stop: Stop::new(),
        });
        let running = Arc::clone(&inner);
        std::thread::spawn(move || running.run());
        Engine {
            requests: queue(&inner),
            connects: queue(&inner),
            inner,
        }
    }

    /// Leave an account out of the panel, the menu bar and alerts, or show
    /// it again.
    pub fn hide(&self, account: String, hidden: bool) {
        self.change("hiding an account", Some(Tell::Always), move |showing| {
            status::hide(&showing.db, &account, hidden)
        });
    }

    /// Keep which notifications are sent.
    pub fn save_settings(&self, settings: Settings) {
        self.change("saving the settings", Some(Tell::Always), move |showing| {
            alerts::save_settings(&mut showing.db, &settings)
        });
    }

    /// Record alert `id` as told, so it isn't told again.
    pub fn acknowledge(&self, id: i64) {
        self.change("acknowledging alerts", None, move |showing| {
            alerts::acknowledge(&mut showing.db, id)
        });
    }

    /// Ask what used `account`'s limit `limit` most, told as
    /// `Event::UsedMost`.
    pub fn used_most(&self, account: String, limit: String) {
        Engine::ask(&self.requests, move |inner| {
            let sessions = match inner.showing().as_ref() {
                Some(showing) => showing.used_most(&account, &limit, crate::now()),
                None => return,
            };
            match sessions {
                Ok(sessions) => (inner.tell)(Event::UsedMost {
                    account,
                    limit,
                    sessions,
                }),
                Err(error) => log(&inner.data, &format!("what used a limit: {error}")),
            }
        });
    }

    /// Connect the agent `id` to Turnscope's MCP server, or disconnect it,
    /// with the agent's own command, which can take a minute. Told as
    /// `Event::Connected`, then the status again.
    pub fn connect(&self, agent: String, connecting: bool) {
        Engine::ask(&self.connects, move |inner| {
            let Some(found) = agents::by_id(&agent) else {
                return;
            };
            let done = match inner.newer() {
                Some(newer) => Err(newer),
                None if connecting => crate::connect::connect(found, &inner.home, &inner.binary),
                None => crate::connect::disconnect(found, &inner.home),
            };
            (inner.tell)(Event::Connected {
                agent,
                connection: crate::connect::connection(found, &inner.home, &inner.binary).0,
                error: done.err().map(|error| error.to_string()),
            });
            inner.give(Tell::WithAgents);
        });
    }

    /// Ask for limits read `limits::DUE` ago or more to be read again, and
    /// any whose read failed, as when the panel opens
    /// (`limits::refresh`). Nothing, while another app keeps the data
    /// current.
    pub fn ask_limits(&self) {
        if let Some(wake) = locked(&self.inner.wake).as_ref() {
            let _ = wake.send(Wake::Asked);
        }
    }

    /// Ask for a limit read skipping staleness (`DUE`/`IDLE`) and Turnscope's
    /// own backoff, but not a provider's `Retry-After` or `CLAIM`: told as the
    /// status, changed or not, then `Event::Refreshed`.
    pub fn refresh_now(&self) {
        let sent = locked(&self.inner.wake).as_ref().is_some_and(|wake| {
            self.inner.forcing.store(true, Ordering::Release);
            wake.send(Wake::Force).is_ok()
        });
        if !sent {
            Engine::ask(&self.requests, Inner::refreshed);
        }
    }

    /// Run `change` on the open data, unless a newer Turnscope wrote, then
    /// tell the status as `then` says.
    fn change(
        &self,
        name: &'static str,
        then: Option<Tell>,
        change: impl FnOnce(&mut Showing) -> Result<()> + Send + 'static,
    ) {
        Engine::ask(&self.requests, move |inner| {
            if inner.newer().is_none() {
                let done = match inner.showing().as_mut() {
                    Some(showing) => change(showing),
                    None => return,
                };
                if let Err(error) = done {
                    inner.failed(name, error);
                }
            }
            if let Some(tell) = then {
                inner.give(tell);
            }
        });
    }

    fn ask(queue: &Sender<Work>, work: impl FnOnce(&Inner) + Send + 'static) {
        let _ = queue.send(Box::new(work));
    }
}

/// A thread that runs what's sent to it, in the order sent, until the
/// `Engine` is dropped. A request that panics is logged and the next runs:
/// unguarded, every later request went unanswered until the app restarted.
fn queue(inner: &Arc<Inner>) -> Sender<Work> {
    let (send, received) = mpsc::channel::<Work>();
    let inner = Arc::clone(inner);
    std::thread::spawn(move || {
        for work in received {
            if inner.ended() {
                break;
            }
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&inner)));
            if ran.is_err() {
                log(&inner.data, "answering a request panicked");
            }
        }
    });
    send
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.inner.stop.end();
        if let Some(wake) = locked(&self.inner.wake).as_ref() {
            let _ = wake.send(Wake::Stop);
        }
    }
}

impl Inner {
    fn showing(&self) -> MutexGuard<'_, Option<Showing>> {
        locked(&self.showing)
    }

    fn ended(&self) -> bool {
        self.stop.ended()
    }

    fn run(&self) {
        while !self.ended() && self.newer().is_none() {
            let opened_now = if self.showing().is_none() {
                match Showing::open(&self.data, &self.home, &self.binary) {
                    Ok(opened) => {
                        *self.showing() = Some(opened);
                        true
                    }
                    Err(error) => {
                        if self.stopped(error) {
                            return;
                        }
                        if self.stop.wait(AGAIN) {
                            return;
                        }
                        continue;
                    }
                }
            } else {
                false
            };
            if self.ended() {
                return;
            }
            match lock(&self.data) {
                Ok(Some(lock)) => {
                    self.keeping.store(true, Ordering::Release);
                    if opened_now {
                        self.give(Tell::Always);
                    }
                    let kept = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        keep(&self.home, &self.data, &self.wake, &self.stop, |caught| {
                            self.caught(caught)
                        })
                    }));
                    self.keeping.store(false, Ordering::Release);
                    *locked(&self.wake) = None;
                    drop(lock);
                    if self.forcing.swap(false, Ordering::AcqRel) {
                        self.refreshed();
                    }
                    let why = match kept {
                        Ok(Ok(())) => continue,
                        Ok(Err(error)) => error,
                        Err(_) => Error::Failed("keeping the data current panicked".to_owned()),
                    };
                    if self.stopped(why) {
                        return;
                    }
                }
                // Another app keeps it current: this one shows what that
                // one writes. It writes nothing and raises no alerts, so it
                // looks for a newer Turnscope's writes itself.
                Ok(None) => {
                    self.keeping.store(false, Ordering::Release);
                    let newer = match self.showing().as_ref() {
                        Some(showing) => crate::db::writing(&showing.db),
                        None => Ok(()),
                    };
                    if let Err(error @ Error::Newer { .. }) = newer {
                        self.stopped(error);
                        return;
                    }
                    self.kept(None);
                    self.give(if opened_now {
                        Tell::Always
                    } else {
                        Tell::IfChanged
                    });
                }
                Err(error) => {
                    self.keeping.store(false, Ordering::Release);
                    if opened_now {
                        self.give(Tell::Always);
                    }
                    if self.stopped(error) {
                        return;
                    }
                }
            }
            if self.stop.wait(AGAIN) {
                return;
            }
        }
    }

    // Report current only once a catch-up reads every agent (not on lock or
    // limit read), so persistent failures like an unwritable lock stay visible.
    fn caught(&self, caught: Caught) -> bool {
        match caught {
            Caught::Limits { forced: true } => {
                self.forcing.store(false, Ordering::Release);
                self.refreshed();
            }
            Caught::Limits { forced: false } => self.give(Tell::IfChanged),
            Caught::Read { new } => {
                self.kept(None);
                if new {
                    self.give(Tell::IfChanged);
                }
            }
            Caught::Failed(error) => {
                self.stopped(error);
                self.give(Tell::IfChanged);
            }
        }
        !self.ended() && self.newer().is_none()
    }

    /// The status, changed or not, then that a refresh asked for is done.
    fn refreshed(&self) {
        self.give(Tell::Always);
        (self.tell)(Event::Refreshed);
    }

    fn stopped(&self, error: Error) -> bool {
        let stopped = match error {
            Error::Newer { replaced } => Stopped::Newer { replaced },
            error => Stopped::Failing(error.to_string()),
        };
        let newer = matches!(stopped, Stopped::Newer { .. });
        self.kept(Some(stopped));
        newer
    }

    fn failed(&self, doing: &str, error: Error) {
        match error {
            Error::Newer { .. } => {
                self.stopped(error);
            }
            error => log(&self.data, &format!("{doing}: {error}")),
        }
    }

    /// Tell whether the data is kept current, when that changed, and log
    /// why it isn't: logged each time, a database that couldn't be opened
    /// added a line every 5 s.
    fn kept(&self, now: Option<Stopped>) {
        let mut said = locked(&self.kept);
        let done = matches!(&*said, Kept::Stopped(Stopped::Newer { .. }));
        let next = match &now {
            Some(stopped) => Kept::Stopped(stopped.clone()),
            None => Kept::Current,
        };
        if !done && *said != next {
            *said = next;
            drop(said);
            if let Some(stopped) = &now {
                log(&self.data, &stopped.to_string());
            }
            (self.tell)(Event::Kept(now));
        }
    }

    fn newer(&self) -> Option<Error> {
        match &*locked(&self.kept) {
            Kept::Stopped(Stopped::Newer { replaced }) => Some(Error::Newer {
                replaced: *replaced,
            }),
            _ => None,
        }
    }

    // Deliver under the `showing` lock so an older status never overtakes a
    // newer one from another thread. Safe because `tell` never blocks.
    fn give(&self, tell: Tell) {
        let alerts = self.keeping.load(Ordering::Acquire);
        let mut showing = self.showing();
        let next = match showing.as_mut() {
            Some(showing) if self.newer().is_some() => Ok(showing
                .shown
                .clone()
                .filter(|_| tell != Tell::IfChanged)
                .map(|shown| (shown, Vec::new()))),
            Some(showing) => showing.next(crate::now(), tell, alerts),
            None => return,
        };
        match next {
            Ok(Some((shown, alerts))) => (self.tell)(Event::Shown(Box::new(shown), alerts)),
            Ok(None) => {}
            Err(error) => self.failed("working out the status", error),
        }
    }
}

const LOG_SIZE: u64 = 1 << 20;

/// Append `line` with a timestamp to `app.log` in `data`, rotating past 1 MiB to `app.log.1`.
pub fn log(data: &Path, line: &str) {
    static LOG: Mutex<()> = Mutex::new(());
    let _guard = locked(&LOG);
    let path = data.join("app.log");
    let open = || {
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
    };
    let Ok(mut file) = open() else {
        return;
    };
    if let Ok(meta) = file.metadata()
        && meta.len() > LOG_SIZE
        && file.lock().is_ok()
    {
        // Re-check under the file lock so another process rotating first
        // doesn't get its new `app.log` renamed over the 1 MiB `app.log.1`.
        let file_id = crate::platform::file_id(&meta);
        if std::fs::metadata(&path).is_ok_and(|current| {
            current.len() > LOG_SIZE && crate::platform::file_id(&current) == file_id
        }) {
            let _ = std::fs::rename(&path, data.join("app.log.1"));
        }
        drop(file);
        let Ok(reopened) = open() else {
            return;
        };
        file = reopened;
    }
    let line = line.replace('\n', " ");
    let _ = writeln!(file, "{:.0} {line}", jiff::Timestamp::now());
}

/// Whether an app process holds `app.lock` in `data`. Never creates the file.
/// Shared, so two processes asking at once don't take each other for the app.
pub fn running(data: &Path) -> bool {
    let Ok(file) = File::open(data.join("app.lock")) else {
        return false;
    };
    matches!(
        file.try_lock_shared(),
        Err(std::fs::TryLockError::WouldBlock)
    )
}

fn lock(data: &Path) -> Result<Option<File>> {
    crate::platform::create_private_dir(data)?;
    crate::db::lock_within(&data.join("app.lock"), Duration::ZERO)
}

// Minimum gap between file-triggered catch-ups: 5 s, over catching up as writes
// settle (9.8G vs 3.2G instructions over 5,000 sessions with heavy writes).
const GAP: Duration = Duration::from_secs(5);

/// How often the engine wakes while idle for clocks, due limits/prices, and
/// external database writes.
const TICK: Duration = Duration::from_secs(60);

/// How often an idle engine rescans history even with no watcher event, as a
/// fallback for symlinks or missed filesystem events.
const RESCAN: Duration = Duration::from_secs(5 * 60);

/// The least time between two reads of limits asked for
/// (`limits::refresh`): each gathers every agent's logins, the
/// Keychain's with `security`, and asks a failing provider again. A guess
/// at how soon someone opens the panel again, not measured.
const ASKED: Duration = Duration::from_secs(30);

enum Caught {
    Limits { forced: bool },
    Read { new: bool },
    Failed(Error),
}

struct Woke {
    written: bool,
    tick: bool,
}

/// Catch up when the agents' folders change, and each minute, as horizons
/// and outlooks move with the clock, reading the limits asked for first
/// (`Wake::Asked`). Tells `told` what each catch-up came to, and returns
/// once it says to stop.
fn keep(
    home: &Path,
    data: &Path,
    wake: &Mutex<Option<Sender<Wake>>>,
    stop: &Stop,
    mut told: impl FnMut(Caught) -> bool,
) -> Result<()> {
    let mut db = crate::db::open(data)?;
    let (changed, changes) = mpsc::channel::<Wake>();
    *locked(wake) = Some(changed.clone());
    if stop.ended() {
        return Ok(());
    }
    let mut watcher = watcher(changed)?;
    let mut watched: Vec<(PathBuf, u64)> = Vec::new();
    let mut watch = |watcher: &mut notify::RecommendedWatcher| -> bool {
        let mut added = false;
        let current: Vec<(PathBuf, u64)> = agents::ALL
            .iter()
            .flat_map(|agent| agent.installed_folders(home))
            .filter_map(|folder| {
                let meta = std::fs::metadata(&folder).ok()?;
                Some((folder, crate::platform::file_id(&meta)))
            })
            .collect();
        watched.retain(|entry| {
            let keep = current.contains(entry);
            if !keep {
                let _ = watcher.unwatch(&entry.0);
            }
            keep
        });
        for (folder, id) in current {
            if watched.iter().any(|(path, _)| path == &folder) {
                continue;
            }
            if watcher
                .watch(&folder, notify::RecursiveMode::Recursive)
                .is_ok()
            {
                watched.push((folder, id));
                added = true;
            }
        }
        added
    };
    // The minute and SQLite's `data_version`, which another connection's
    // write moves, at the last catch-up that worked: with neither moved and
    // nothing read, there's nothing new to show. Working out a status after
    // every catch-up ran 11.3G instructions, with a stand-in agent writing
    // three times a second for 30 s, against 0.4G with this.
    let mut pushed = None;
    // The limits before history: on a first launch that reads all of it (12 s
    // for a made-up 5,000 files), the app showed no account until it ended.
    let mut skip_scheduled_limits = crate::online();
    if skip_scheduled_limits
        && read_limits(&mut db, home, Ask::Scheduled)?
        && !told(Caught::Limits { forced: false })
    {
        return Ok(());
    }
    // Limits asked for and not read yet, and when they last were.
    let (mut asked, mut asked_at) = (None::<Ask>, None::<Instant>);
    let mut scanned: Option<Instant> = None;
    let mut next_tick = Instant::now() + TICK;
    let mut woke = Woke {
        written: true,
        tick: false,
    };
    loop {
        if stop.ended() {
            return Ok(());
        }
        let added = watch(&mut watcher);
        let reading = asked.filter(|_| due(asked, asked_at));
        let forced = reading == Some(Ask::Force);
        if let Some(ask) = reading {
            (asked, asked_at) = (None, Some(Instant::now()));
            let read_asked = crate::online() && read_limits(&mut db, home, ask)?;
            // Emit right after reading limits so panel opens and ⌘R aren't
            // blocked behind a history walk or ingest.
            if forced {
                if !told(Caught::Limits { forced: true }) {
                    return Ok(());
                }
            } else if read_asked && !told(Caught::Limits { forced: false }) {
                return Ok(());
            }
        }
        let need_history = scanned.is_none()
            || woke.written
            || added
            || forced
            || scanned.is_some_and(|at| at.elapsed() >= RESCAN);
        if need_history || woke.tick {
            let skip_limits = reading.is_some() || std::mem::take(&mut skip_scheduled_limits);
            let (priced, limits) = match crate::online() {
                true => (
                    crate::prices::refresh(&mut db).unwrap_or(false),
                    if skip_limits {
                        Ok(false)
                    } else {
                        crate::limits::refresh(&mut db, home, Ask::Scheduled)
                    },
                ),
                false => (false, Ok(false)),
            };
            let ingested = if need_history {
                match crate::ingest::ingest(&mut db, home, None) {
                    Ok(read) => {
                        scanned = Some(Instant::now());
                        Ok(read)
                    }
                    Err(error) => {
                        scanned = None;
                        Err(error)
                    }
                }
            } else {
                Ok(false)
            };
            next_tick = Instant::now() + TICK;
            let caught = match ingested.and_then(|read| Ok(limits? || priced || read)) {
                Ok(read) => {
                    let now = (
                        crate::time::minute(crate::now()),
                        db.pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))?,
                    );
                    let new = read || pushed != Some(now);
                    pushed = Some(now);
                    Caught::Read { new }
                }
                Err(error) => {
                    // Once a newer Turnscope has written here, this one writes
                    // nothing more, so it stops, and the app says why. Going on,
                    // it showed what it read before as current.
                    crate::db::writing(&db)?;
                    Caught::Failed(error)
                }
            };
            if !told(caught) {
                return Ok(());
            }
        } else if reading.is_some() {
            // Surface external DB writes (e.g. CLI `accounts hide`) and minute
            // rollovers when the panel opens without walking history.
            let now = (
                crate::time::minute(crate::now()),
                db.pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))?,
            );
            if pushed != Some(now) {
                pushed = Some(now);
                if !told(Caught::Read { new: true }) {
                    return Ok(());
                }
            }
        }
        match wait(&changes, &mut asked, asked_at, next_tick)? {
            Some(next) => woke = next,
            None => return Ok(()),
        }
    }
}

/// A watcher sending `changed` each write: `Wake::Asked` for a login,
/// `Wake::Written` for anything else. Any write, not only to history: macOS
/// reports a write when its file is closed, and Codex and OpenCode keep
/// theirs open while they work. Not a SQLite `-shm` file, which opening a
/// database writes: counted, a database that failed every read started a
/// catch-up every 5 s (21 in 100 s, measured 2026-11-11). An agent's writes
/// are in its `-wal`.
fn watcher(changed: Sender<Wake>) -> Result<notify::RecommendedWatcher> {
    notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let paths = event.as_ref().map_or(&[][..], |event| &event.paths[..]);
        let ours = !paths.is_empty()
            && paths
                .iter()
                .all(|path| path.to_string_lossy().ends_with("-shm"));
        let login = paths.iter().any(|path| agents::login_file(path));
        if !ours {
            let _ = changed.send(if login { Wake::Asked } else { Wake::Written });
        }
    })
    .map_err(|error| Error::Failed(error.to_string()))
}

/// Whether limits `asked` for, last read `at`, are read now (`wait`).
fn due(asked: Option<Ask>, at: Option<Instant>) -> bool {
    match asked {
        Some(Ask::Force) => true,
        Some(_) => at.is_none_or(|at| at.elapsed() >= ASKED),
        None => false,
    }
}

/// Wait for the next catch-up, noting in `asked` the limits asked for
/// meanwhile; `None` when stopping. A write waits for a burst of writes to
/// settle, a second at most, and out the gap: the next catch-up reads what's
/// written meanwhile. Limits asked for are read at once, or as soon as
/// `ASKED` elapses: waiting out a 60 s idle tick delayed a deferred login read.
fn wait(
    changes: &Receiver<Wake>,
    asked: &mut Option<Ask>,
    asked_at: Option<Instant>,
    next_tick: Instant,
) -> Result<Option<Woke>> {
    wait_within(changes, asked, asked_at, next_tick, ASKED)
}

fn wait_within(
    changes: &Receiver<Wake>,
    asked: &mut Option<Ask>,
    asked_at: Option<Instant>,
    next_tick: Instant,
    asked_gap: Duration,
) -> Result<Option<Woke>> {
    let waited = Instant::now();
    let (mut first, mut settled, mut written) = (None::<Instant>, false, false);
    let ready = |asked: Option<Ask>| match asked {
        Some(Ask::Force) => true,
        Some(_) => asked_at.is_none_or(|at| at.elapsed() >= asked_gap),
        None => false,
    };
    let done = |written| {
        Ok(Some(Woke {
            written,
            tick: Instant::now() >= next_tick,
        }))
    };
    loop {
        let asked_wait = match *asked {
            Some(Ask::Force) => Some(Duration::ZERO),
            Some(_) => {
                Some(asked_at.map_or(Duration::ZERO, |at| asked_gap.saturating_sub(at.elapsed())))
            }
            None => None,
        };
        let timeout = match first {
            None => {
                let left = next_tick.saturating_duration_since(Instant::now());
                asked_wait.map_or(left, |wait| left.min(wait))
            }
            Some(_) if !settled => Duration::from_millis(250),
            Some(_) => {
                let gap = GAP.saturating_sub(waited.elapsed());
                asked_wait.map_or(gap, |wait| gap.min(wait))
            }
        };
        match changes.recv_timeout(timeout) {
            Ok(Wake::Stop) => return Ok(None),
            Ok(Wake::Force) => *asked = Some(Ask::Force),
            Ok(Wake::Asked) => {
                asked.get_or_insert(Ask::Opened);
            }
            Ok(Wake::Written) => {
                written = true;
                let first = *first.get_or_insert_with(Instant::now);
                settled |= first.elapsed() >= Duration::from_secs(1);
            }
            Err(RecvTimeoutError::Timeout) if ready(*asked) => return done(written),
            Err(RecvTimeoutError::Timeout) if first.is_none() => {
                if Instant::now() >= next_tick {
                    return done(written);
                }
            }
            Err(RecvTimeoutError::Timeout) if settled => {
                if waited.elapsed() >= GAP {
                    return done(written);
                }
            }
            Err(RecvTimeoutError::Timeout) => settled = true,
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Error::Failed("the agents' folders can't be watched".into()));
            }
        }
        if ready(*asked) {
            return done(written);
        }
    }
}

/// Read the limits due (`limits::refresh`), or asked for; whether any were.
/// A failure is logged, and counts as a read so the status is told again,
/// unless a newer Turnscope wrote.
fn read_limits(db: &mut Connection, home: &Path, ask: Ask) -> Result<bool> {
    match crate::limits::refresh(db, home, ask) {
        Err(error @ Error::Newer { .. }) => Err(error),
        Err(error) => {
            log(crate::db::dir(db), &error.to_string());
            Ok(true)
        }
        read => read,
    }
}

type FileStamp = (PathBuf, u64, Option<SystemTime>, u64);

#[derive(PartialEq, Eq)]
struct AgentsStamp {
    installed: Vec<(Vec<PathBuf>, bool)>,
    files: Vec<FileStamp>,
}

fn file_stamp(path: PathBuf) -> Option<FileStamp> {
    let meta = std::fs::metadata(&path).ok()?;
    Some((
        path,
        meta.len(),
        meta.modified().ok(),
        crate::platform::file_id(&meta),
    ))
}

/// Fingerprint of installed agent folders and top-level JSON/TOML config files
/// so unchanged multi-MB MCP configs aren't re-parsed every minute.
fn agents_stamp(home: &Path, binary: &Path) -> AgentsStamp {
    let mut installed = Vec::new();
    let mut files = Vec::new();
    if let Some(stamp) = file_stamp(binary.to_path_buf()) {
        files.push(stamp);
    }
    let mut scan = |dir: &Path, dotfiles_only: bool| {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if dotfiles_only && !name.starts_with('.') {
                continue;
            }
            if (name.ends_with(".json") || name.ends_with(".toml"))
                && let Some(stamp) = file_stamp(entry.path())
            {
                files.push(stamp);
            }
        }
    };
    scan(home, true);
    let config_root = home.join(".config");
    for agent in agents::ALL {
        let folders = agent.folders(home);
        let is_installed = folders
            .first()
            .is_some_and(|folder| agent.installed(folder));
        for folder in &folders {
            scan(folder, false);
            scan(&folder.join("config"), false);
        }
        scan(&config_root.join(agent.info().id), false);
        installed.push((folders, is_installed));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files.dedup_by(|a, b| a.0 == b.0);
    AgentsStamp { installed, files }
}

struct AgentsCache {
    at: i64,
    stamp: AgentsStamp,
    agents: Vec<Agent>,
}

struct Showing {
    db: Connection,
    data: PathBuf,
    home: PathBuf,
    binary: PathBuf,
    shown: Option<Shown>,
    agents: Option<AgentsCache>,
    /// Whether the alerts pending when the data opened were told.
    pending_told: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tell {
    IfChanged,
    Always,
    WithAgents,
}

impl Showing {
    fn open(data: &Path, home: &Path, binary: &Path) -> Result<Showing> {
        let db = crate::db::open(data)?;
        crate::db::writing(&db)?;
        Ok(Showing {
            db,
            data: data.to_path_buf(),
            home: home.to_path_buf(),
            binary: binary.to_path_buf(),
            shown: None,
            agents: None,
            pending_told: false,
        })
    }

    // Skip telling statuses that differ only in `at` or unused `work` rates
    // (app ticks on its own clock): cut 60 -> 10 statuses/hr after last read,
    // 0/hr thereafter (measured 2026-11-18).
    fn next(&mut self, now: i64, tell: Tell, alerts: bool) -> Result<Option<(Shown, Vec<Alert>)>> {
        let status = status::status(&self.db, now)?;
        let in_use = |id: &str| status::in_use(&status.accounts, id);
        // Cache MCP config reads within the minute, and across minutes while
        // config files' `(size, mtime, file_id)` haven't changed (`~/.claude.json`
        // can be megabytes).
        let agents = match &mut self.agents {
            Some(cached) if tell != Tell::WithAgents && cached.at == status.at => cached
                .agents
                .iter()
                .map(|agent| Agent {
                    in_use: in_use(&agent.id),
                    ..agent.clone()
                })
                .collect(),
            cached => {
                let stamp = agents_stamp(&self.home, &self.binary);
                match cached {
                    Some(cached) if tell != Tell::WithAgents && cached.stamp == stamp => {
                        cached.at = status.at;
                        cached
                            .agents
                            .iter()
                            .map(|agent| Agent {
                                in_use: in_use(&agent.id),
                                ..agent.clone()
                            })
                            .collect()
                    }
                    _ => {
                        let agents = status::agents(&self.home, &self.binary, in_use);
                        self.agents = Some(AgentsCache {
                            at: status.at,
                            stamp,
                            agents: agents.clone(),
                        });
                        agents
                    }
                }
            }
        };
        let shown = Shown { status, agents };
        let unchanged = self.shown.as_ref().is_some_and(|told| same(told, &shown));
        if unchanged && (!alerts || self.pending_told) {
            return Ok((tell != Tell::IfChanged).then_some((shown, Vec::new())));
        }
        if !alerts {
            self.shown = Some(shown.clone());
            return Ok(Some((shown, Vec::new())));
        }
        // Read before `raise`, which adds alerts not yet acknowledged.
        let mut told = Vec::new();
        if !std::mem::replace(&mut self.pending_told, true) {
            match alerts::pending(&self.db, &shown.status, now) {
                Ok(pending) => told = pending,
                Err(error) => log(&self.data, &format!("reading pending alerts: {error}")),
            }
        }
        match alerts::raise(&mut self.db, &shown.status) {
            Ok(raised) => {
                self.shown = Some(shown.clone());
                told.extend(raised);
                let emit = !unchanged || !told.is_empty() || tell != Tell::IfChanged;
                Ok(emit.then_some((shown, told)))
            }
            Err(error @ Error::Newer { .. }) => Err(error),
            Err(error) => {
                log(&self.data, &format!("raising alerts: {error}"));
                let emit = !unchanged || !told.is_empty() || tell != Tell::IfChanged;
                Ok(emit.then_some((shown, told)))
            }
        }
    }

    fn used_most(&self, account: &str, limit: &str, now: i64) -> Result<Vec<UsedMost>> {
        let status = match &self.shown {
            Some(shown) => std::borrow::Cow::Borrowed(&shown.status),
            None => std::borrow::Cow::Owned(status::status(&self.db, now)?),
        };
        let (account, limit) = status
            .accounts
            .iter()
            .find(|found| found.id == account)
            .and_then(|account| {
                let limit = account.limits.iter().find(|found| found.key == limit)?;
                Some((account, limit))
            })
            .ok_or_else(|| Error::NotFound(format!("no limit {limit} of {account}")))?;
        let (used, shares, _) =
            usage::breakdown(&self.db, account, limit, None, usage::By::Session, now)?;
        let mut sessions = Vec::new();
        for (at, row) in used.rows.iter().enumerate().take(3) {
            let (project_path, agent) = usage::brief(&self.db, &row.key)?;
            let project = project_path
                .as_deref()
                .and_then(|path| Path::new(path).file_name())
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .map(str::to_owned);
            sessions.push(UsedMost {
                session: row.key.clone(),
                project,
                project_path,
                title: row.name.clone(),
                agent,
                share_percent: shares.as_ref().map(|shares| shares.rows[at]),
            });
        }
        Ok(sessions)
    }
}

fn same(a: &Shown, b: &Shown) -> bool {
    let Status {
        at: _,
        history_read,
        accounts,
        settings,
    } = &a.status;
    *history_read == b.status.history_read
        && *settings == b.status.settings
        && a.agents == b.agents
        && accounts.len() == b.status.accounts.len()
        && accounts
            .iter()
            .zip(&b.status.accounts)
            .all(|(a, b)| same_account(a, b))
}

fn same_account(a: &status::Account, b: &status::Account) -> bool {
    let status::Account {
        id,
        provider,
        kind,
        title,
        label,
        agents,
        used_by,
        in_use,
        recent,
        hidden,
        state,
        limits,
        deciding,
        spend,
    } = a;
    *id == b.id
        && *provider == b.provider
        && *kind == b.kind
        && *title == b.title
        && *label == b.label
        && *agents == b.agents
        && *used_by == b.used_by
        && *in_use == b.in_use
        && *recent == b.recent
        && *hidden == b.hidden
        && *state == b.state
        && *deciding == b.deciding
        && *spend == b.spend
        && limits.len() == b.limits.len()
        && limits.iter().zip(&b.limits).all(|(a, b)| same_limit(a, b))
}

fn same_limit(a: &status::Limit, b: &status::Limit) -> bool {
    let status::Limit {
        key,
        name,
        scope,
        window,
        read_at,
        left_percent,
        money,
        reset_since,
        held_by,
        outlook,
        // Work rates aren't shown by the app; ignoring them avoids spurious
        // `Event::Shown`, alert checks, and UI rebuilds.
        work: _,
    } = a;
    *key == b.key
        && *name == b.name
        && *scope == b.scope
        && *window == b.window
        && *read_at == b.read_at
        && *left_percent == b.left_percent
        && *money == b.money
        && *reset_since == b.reset_since
        && *held_by == b.held_by
        && *outlook == b.outlook
}

/// What the app shows of the data in `data` now, read once and only read:
/// no database is made or migrated, no alert is raised and nothing is
/// caught up. For a picture of the panel.
pub fn shown(data: &Path, home: &Path, binary: &Path) -> Result<Shown> {
    let db = crate::db::read(data)
        .ok_or_else(|| Error::NotFound("there's no database this Turnscope reads".to_owned()))?;
    let status = status::status(&db, crate::now())?;
    Ok(Shown {
        agents: status::agents(home, binary, |id| status::in_use(&status.accounts, id)),
        status,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver};
    use std::time::Duration;

    use super::*;

    /// How a newer Turnscope has written to the data: its schema, or
    /// anything else.
    const NEWER: [&str; 2] = [
        "PRAGMA user_version = 99",
        "UPDATE state SET version = 99000000",
    ];

    /// An engine on `data` and `home`, and what it tells.
    fn start(data: &Path, home: &Path) -> (Engine, Receiver<Event>) {
        let (sender, told) = mpsc::channel();
        let engine = Engine::start(data.into(), home.into(), home.into(), move |event| {
            let _ = sender.send(event);
        });
        (engine, told)
    }

    /// The next event `pick` takes, within `wait`.
    fn next<T>(told: &Receiver<Event>, wait: u64, pick: impl Fn(Event) -> Option<T>) -> T {
        let until = Instant::now() + Duration::from_secs(wait);
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let event = told.recv_timeout(left).expect("nothing was told in time");
            if let Some(picked) = pick(event) {
                return picked;
            }
        }
    }

    /// The next stop told, within `wait`.
    fn stopped(told: &Receiver<Event>, wait: u64) -> Stopped {
        next(told, wait, |event| match event {
            Event::Kept(Some(stopped)) => Some(stopped),
            _ => None,
        })
    }

    /// Prices and limits count as read just now, so nothing is asked over
    /// the network.
    fn read_just_now(data: &Path) -> Connection {
        let db = crate::db::open(data).unwrap();
        db.execute(
            "UPDATE state SET prices_at = ?1, limits_at = ?1",
            [crate::now()],
        )
        .unwrap();
        db
    }

    /// One the app quit before it could tell.
    #[test]
    fn an_alert_never_acknowledged_comes_with_the_first_status() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        read_just_now(data.path())
            .execute(
                "INSERT INTO alert (account, limit_key, kind, window, at, detail)
                 VALUES ('anthropic:a', 'five_hour', 'reset', 1, ?1, '{\"accountTitle\":\"Claude Max\"}')",
                [crate::now()],
            )
            .unwrap();
        let (_engine, told) = start(data.path(), home.path());
        let alerts = next(&told, 10, |event| match event {
            Event::Shown(_, alerts) => Some(alerts),
            _ => None,
        });
        let kinds: Vec<_> = alerts.iter().map(|alert| alert.kind).collect();
        assert_eq!(kinds, [crate::alerts::Kind::Reset]);
    }

    #[test]
    fn data_a_newer_turnscope_wrote_is_said() {
        for newer in NEWER {
            let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            crate::db::open(data.path())
                .unwrap()
                .execute_batch(newer)
                .unwrap();
            let (_engine, told) = start(data.path(), home.path());
            assert_eq!(
                stopped(&told, 10),
                Stopped::Newer { replaced: false },
                "{newer}"
            );
        }
    }

    /// Once a newer Turnscope has written to the data, the engine stops and
    /// writes nothing more: a hide changes nothing, and the status told after
    /// says so; a connect runs no command, and says why.
    #[test]
    fn once_a_newer_turnscope_writes_requests_are_refused() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let folder = home.path().join(".claude/projects/-work-a");
        std::fs::create_dir_all(&folder).unwrap();
        let db = read_just_now(data.path());
        db.execute(
            "INSERT INTO account (id, provider, kind, title)
             VALUES ('anthropic:a', 'anthropic', 'subscription', 'Claude')",
            [],
        )
        .unwrap();
        let (engine, told) = start(data.path(), home.path());
        next(&told, 10, |event| {
            matches!(event, Event::Kept(None)).then_some(())
        });
        db.execute_batch(NEWER[1]).unwrap();
        std::fs::write(folder.join("s1.jsonl"), "{}\n").unwrap();
        let stop = stopped(&told, 20);
        assert_eq!(stop, Stopped::Newer { replaced: false });
        engine.hide("anthropic:a".to_owned(), true);
        let hidden = next(&told, 10, |event| match event {
            Event::Shown(shown, _) => Some(shown.status.accounts[0].hidden),
            _ => None,
        });
        assert!(!hidden);
        engine.connect("opencode".to_owned(), false);
        let error = next(&told, 10, |event| match event {
            Event::Connected { error, .. } => Some(error),
            _ => None,
        });
        assert_eq!(error, Some(stop.to_string()));
    }

    /// A status worked out before a hide, and slow to be told, is told
    /// before the status after it: told after, the app showed the account
    /// again until the next change.
    #[test]
    fn a_status_is_never_told_after_a_newer_one() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let db = read_just_now(data.path());
        db.execute(
            "INSERT INTO account (id, provider, kind, title)
             VALUES ('anthropic:a', 'anthropic', 'subscription', 'Claude')",
            [],
        )
        .unwrap();
        // Keep `history_read` settled so `keep`'s initial pass doesn't emit an
        // unrelated `Event::Shown` while `hide` is racing on `showing`.
        db.execute("UPDATE state SET history_at = ?1", [crate::now()])
            .unwrap();
        let (sender, told) = mpsc::channel();
        let (holding, held) = mpsc::channel();
        let (release, released) = mpsc::channel::<()>();
        // Called from any thread, the closure must be `Sync`: a `Receiver`
        // isn't.
        let released = Mutex::new(released);
        let first = AtomicBool::new(true);
        // The first status is held until the test lets it go.
        let engine = Engine::start(
            data.path().into(),
            home.path().into(),
            home.path().into(),
            move |event| {
                if let Event::Shown(shown, _) = &event
                    && first.swap(false, Ordering::Relaxed)
                {
                    let _ = holding.send(shown.status.accounts[0].hidden);
                    let _ = released
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(10));
                }
                let _ = sender.send(event);
            },
        );
        let hidden = held.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(!hidden);
        engine.hide("anthropic:a".to_owned(), true);
        // Time for the hide's status to overtake the one held.
        std::thread::sleep(Duration::from_millis(500));
        release.send(()).unwrap();
        let shown = |wait| {
            next(&told, wait, |event| match event {
                Event::Shown(shown, _) => Some(shown.status.accounts[0].hidden),
                _ => None,
            })
        };
        assert_eq!((shown(10), shown(10)), (false, true));
    }

    /// A catch-up that fails is said, as one tried again, and the data is
    /// said to be current only once one works: said so before, a catch-up
    /// that failed each time never said why.
    #[test]
    fn a_catch_up_that_fails_is_said_until_one_works() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let folder = home.path().join(".claude/projects/-work-a");
        std::fs::create_dir_all(&folder).unwrap();
        drop(read_just_now(data.path()));
        // History can't be read: its lock can't be made.
        let blocked = data.path().join("ingest.lock");
        std::fs::create_dir(&blocked).unwrap();
        let (_engine, told) = start(data.path(), home.path());
        let kept = |wait| {
            next(&told, wait, |event| match event {
                Event::Kept(kept) => Some(kept),
                _ => None,
            })
        };
        let failed = kept(10).expect("said to be current");
        assert!(
            matches!(&failed, Stopped::Failing(why) if why.contains("ingest.lock")),
            "{failed:?}"
        );
        std::fs::remove_dir(&blocked).unwrap();
        std::fs::write(folder.join("s1.jsonl"), "{}\n").unwrap();
        assert_eq!(kept(20), None);
    }

    /// The same failure, tried again, is logged once.
    #[test]
    fn a_failure_tried_again_is_logged_once() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(data.path().join("turnscope.db"), "not a database").unwrap();
        let (_engine, told) = start(data.path(), home.path());
        assert!(matches!(stopped(&told, 10), Stopped::Failing(_)));
        // Past the second try.
        std::thread::sleep(AGAIN + Duration::from_secs(1));
        let logged = std::fs::read_to_string(data.path().join("app.log")).unwrap();
        assert_eq!(logged.lines().count(), 1, "{logged}");
    }

    #[test]
    fn a_full_log_moves_to_app_log_1_and_starts_over() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("app.log");
        let full = "x".repeat(LOG_SIZE as usize + 1);
        std::fs::write(&path, &full).unwrap();
        log(data.path(), "it failed\nat once");
        assert_eq!(
            std::fs::read_to_string(data.path().join("app.log.1")).unwrap(),
            full
        );
        let logged = std::fs::read_to_string(&path).unwrap();
        // One line, the time first.
        assert_eq!(logged.lines().count(), 1, "{logged}");
        assert!(
            logged.trim_end().ends_with("Z it failed at once"),
            "{logged}"
        );
    }

    /// An app that another keeps the data current for stops too, and says
    /// why, though it writes nothing itself.
    #[test]
    fn a_second_app_stops_once_a_newer_turnscope_writes() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let db = read_just_now(data.path());
        let _other = lock(data.path()).unwrap().expect("no app holds the lock");
        let (_engine, told) = start(data.path(), home.path());
        next(&told, 10, |event| {
            matches!(event, Event::Kept(None)).then_some(())
        });
        db.execute_batch(NEWER[1]).unwrap();
        assert_eq!(stopped(&told, 10), Stopped::Newer { replaced: false });
    }

    /// One app keeps a data directory current at a time, and the next can
    /// once it's gone; `running` says so without making `app.lock`.
    #[test]
    fn one_app_holds_the_lock_at_a_time() {
        let data = tempfile::tempdir().unwrap();
        assert!(!running(data.path()));
        assert!(!data.path().join("app.lock").exists());
        let first = lock(data.path()).unwrap();
        assert!(first.is_some());
        assert!(running(data.path()));
        assert!(lock(data.path()).unwrap().is_none());
        drop(first);
        assert!(!running(data.path()));
        // Nor is an app found while another process asks.
        let asking = File::open(data.path().join("app.lock")).unwrap();
        asking.lock_shared().unwrap();
        assert!(!running(data.path()));
        drop(asking);
        assert!(lock(data.path()).unwrap().is_some());
    }

    /// The app is told once the limits are read, before the first read of
    /// history. That read can't begin here: its lock is held, as another
    /// Turnscope reading history holds it.
    #[test]
    fn the_app_is_told_of_the_limits_before_history_is_read() {
        // Offline, no limits are read, so nothing is told before history.
        if !crate::online() {
            return;
        }
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let db = crate::db::open(data.path()).unwrap();
        // An Anthropic API key (`key_limits: false`) records an account with
        // no network request; prices count as read just now.
        let pi = home.path().join(".pi/agent");
        std::fs::create_dir_all(&pi).unwrap();
        std::fs::write(
            pi.join("auth.json"),
            r#"{"anthropic":{"type":"api_key","key":"sk-ant-made-up"}}"#,
        )
        .unwrap();
        db.execute("UPDATE state SET prices_at = ?1", [crate::now()])
            .unwrap();
        let reading = crate::db::lock(&data.path().join("ingest.lock")).unwrap();
        let (sender, messages) = mpsc::channel();
        let (home_kept, data_kept) = (home.path().to_path_buf(), data.path().to_path_buf());
        std::thread::spawn(move || {
            let wake = Mutex::new(None);
            let stop = Stop::new();
            keep(&home_kept, &data_kept, &wake, &stop, |_| {
                sender.send(()).is_ok()
            })
        });
        // Wait to be told, and say when the limits were read, and whether
        // the history was, by then.
        let told = || -> (Option<i64>, bool) {
            assert!(messages.recv_timeout(Duration::from_secs(10)).is_ok());
            db.query_row(
                "SELECT limits_at, history_at IS NOT NULL FROM state",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };
        let (limits, history) = told();
        assert!(limits.is_some() && !history);
        drop(reading);
        assert_eq!(told(), (limits, true));
    }

    /// Limits asked for, as when the panel opens, are read at once, not
    /// after the gap between catch-ups; Refresh Limits reads them again at
    /// once, though they were read under `ASKED` ago.
    #[test]
    fn limits_asked_for_are_read_at_once() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // Nothing is asked over the network: the home holds no login.
        read_just_now(data.path());
        let wake = Arc::new(Mutex::new(None));
        let (sender, told) = mpsc::channel();
        let (home_kept, data_kept, wake_kept) = (
            home.path().to_path_buf(),
            data.path().to_path_buf(),
            Arc::clone(&wake),
        );
        std::thread::spawn(move || {
            let stop = Stop::new();
            keep(&home_kept, &data_kept, &wake_kept, &stop, |_| {
                sender.send(Instant::now()).is_ok()
            })
        });
        told.recv_timeout(Duration::from_secs(10)).unwrap();
        // As when an agent at work writes just before the panel opens.
        locked(&wake).as_ref().unwrap().send(Wake::Written).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let asked = Instant::now();
        locked(&wake).as_ref().unwrap().send(Wake::Asked).unwrap();
        let answered = told.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            answered - asked < Duration::from_secs(1),
            "{:?}",
            answered - asked
        );
        let forced = Instant::now();
        locked(&wake).as_ref().unwrap().send(Wake::Force).unwrap();
        let answered = told.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            answered - forced < Duration::from_secs(1),
            "{:?}",
            answered - forced
        );
    }

    /// A status is told when what it shows changed, and only then: not
    /// after a catch-up that read history but changed nothing shown, nor a
    /// minute on, when only the minute it's as of changed. Asked to, it's
    /// told whatever changed.
    #[test]
    fn a_status_is_told_only_when_what_it_shows_changed() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // Claude Code is installed from the start, with no history yet.
        let folder = home.path().join(".claude/projects/-work-a");
        std::fs::create_dir_all(&folder).unwrap();
        let mut showing = Showing::open(data.path(), home.path(), home.path()).unwrap();
        let now = crate::now();
        // Whether the first read had finished, in the status told, if one was.
        let told = |showing: &mut Showing, at: i64, tell: Tell| {
            showing
                .next(at, tell, true)
                .unwrap()
                .map(|(shown, _)| shown.status.history_read)
        };

        assert_eq!(told(&mut showing, now, Tell::IfChanged), Some(false));
        // The first read finishes, though there's nothing to read.
        let mut db = crate::db::open(data.path()).unwrap();
        crate::ingest::ingest(&mut db, home.path(), None).unwrap();
        assert_eq!(told(&mut showing, now, Tell::IfChanged), Some(true));
        // A session that drew on no account: read, and nothing shown changed.
        let line = serde_json::json!({"type": "user", "sessionId": "s1",
            "timestamp": "2026-09-30T09:00:00Z", "message": {"role": "user", "content": "Hi."}});
        std::fs::write(folder.join("s1.jsonl"), format!("{line}\n")).unwrap();
        assert!(crate::ingest::ingest(&mut db, home.path(), None).unwrap());
        assert_eq!(told(&mut showing, now, Tell::IfChanged), None);
        let later = now + crate::time::MINUTE;
        assert_eq!(told(&mut showing, later, Tell::IfChanged), None);
        assert_eq!(told(&mut showing, later, Tell::Always), Some(true));
        let mut settings = Settings::default();
        settings.notify.reset = false;
        alerts::save_settings(&mut showing.db, &settings).unwrap();
        assert_eq!(told(&mut showing, later, Tell::IfChanged), Some(true));
    }

    /// An agent's MCP settings are read again after a connect, and a minute
    /// on, but not after any other request: Claude Code's `~/.claude.json`
    /// can be megabytes.
    #[test]
    fn an_agents_mcp_settings_are_read_again_after_a_connect_or_a_minute() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // `home` stands in for the binary agents run.
        let mut showing = Showing::open(data.path(), home.path(), home.path()).unwrap();
        let now = crate::now();
        let claude = |showing: &mut Showing, at: i64, tell: Tell| {
            let (shown, _) = showing.next(at, tell, true).unwrap()?;
            let agent = shown.agents.into_iter().find(|a| a.id == "claude-code");
            Some(agent.unwrap().connection)
        };
        let available = Some(status::Connection::Available);
        assert_eq!(claude(&mut showing, now, Tell::IfChanged), available);
        // Connected, as its own command does it.
        let settings = serde_json::json!({"mcpServers": {"turnscope": {"command": home.path()}}});
        let file = home.path().join(".claude.json");
        std::fs::write(&file, settings.to_string()).unwrap();
        assert_eq!(claude(&mut showing, now, Tell::Always), available);
        let connected = Some(status::Connection::Connected);
        assert_eq!(claude(&mut showing, now, Tell::WithAgents), connected);
        // Disconnected outside the app: seen a minute on.
        std::fs::remove_file(&file).unwrap();
        assert_eq!(claude(&mut showing, now, Tell::IfChanged), None);
        let later = now + crate::time::MINUTE;
        assert_eq!(claude(&mut showing, later, Tell::IfChanged), available);
    }

    /// Requests are applied in the order asked, each followed by the status:
    /// the app sends the whole settings each time, so the last saved is the
    /// one kept.
    #[test]
    fn requests_are_applied_in_the_order_asked() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        drop(read_just_now(data.path()));
        let (engine, told) = start(data.path(), home.path());
        // Once the first catch-up is told, the next is a minute away.
        next(&told, 10, |event| match event {
            Event::Shown(shown, _) if shown.status.history_read => Some(()),
            _ => None,
        });
        let settings = |used_up: bool| {
            let mut settings = Settings::default();
            settings.notify.used_up = used_up;
            settings
        };
        for round in 0..100 {
            let (first, second) = (settings(round % 2 == 1), settings(round % 2 == 0));
            engine.save_settings(first);
            engine.save_settings(second);
            let shown: Vec<Settings> = (0..2)
                .map(|_| {
                    next(&told, 10, |event| match event {
                        Event::Shown(shown, _) => Some(shown.status.settings),
                        _ => None,
                    })
                })
                .collect();
            assert_eq!(shown, [first, second], "round {round}");
        }
    }

    /// A request that panics takes only itself down: it's logged, and the
    /// next still runs.
    #[test]
    fn a_request_that_panics_leaves_the_next_to_run() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        drop(read_just_now(data.path()));
        let (engine, _told) = start(data.path(), home.path());
        Engine::ask(&engine.requests, |_| panic!("a made-up failure"));
        let (ran, answered) = mpsc::channel();
        Engine::ask(&engine.requests, move |_| {
            let _ = ran.send(());
        });
        answered.recv_timeout(Duration::from_secs(10)).unwrap();
        // Logged before the next request ran, on the same thread.
        let logged = std::fs::read_to_string(data.path().join("app.log")).unwrap();
        assert!(
            logged
                .lines()
                .any(|line| line.ends_with(" answering a request panicked")),
            "{logged}"
        );
    }

    #[test]
    fn dropping_the_engine_wakes_and_releases_its_lock() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        drop(read_just_now(data.path()));
        let (engine, told) = start(data.path(), home.path());
        next(&told, 10, |event| match event {
            Event::Shown(shown, _) if shown.status.history_read => Some(()),
            _ => None,
        });
        assert!(running(data.path()));
        drop(engine);
        let deadline = Instant::now() + Duration::from_secs(2);
        while running(data.path()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!running(data.path()));
    }

    /// A forced read that a newer Turnscope's write stops still ends.
    #[test]
    fn a_refresh_a_newer_turnscope_stops_still_ends() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let db = read_just_now(data.path());
        let (engine, told) = start(data.path(), home.path());
        next(&told, 10, |event| match event {
            Event::Shown(shown, _) if shown.status.history_read => Some(()),
            _ => None,
        });
        db.execute_batch(NEWER[1]).unwrap();
        engine.refresh_now();
        next(&told, 10, |event| {
            matches!(event, Event::Refreshed).then_some(())
        });
    }

    /// Both when this app keeps the data current and when another does.
    #[test]
    fn refresh_now_tells_the_status_even_when_nothing_changed_then_that_it_ended() {
        for another_keeps in [false, true] {
            let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            drop(read_just_now(data.path()));
            let _other = another_keeps.then(|| lock(data.path()).unwrap().unwrap());
            let (engine, told) = start(data.path(), home.path());
            // Once `keep` reports `history_read` it has installed `wake` and
            // parked; with the lock held elsewhere, `Kept` is told instead.
            next(&told, 10, |event| match event {
                Event::Shown(shown, _) if shown.status.history_read => Some(()),
                Event::Kept(None) if another_keeps => Some(()),
                _ => None,
            });
            engine.refresh_now();
            let shown = std::cell::Cell::new(false);
            next(&told, 10, |event| match event {
                Event::Shown(..) => {
                    shown.set(true);
                    None
                }
                Event::Refreshed => Some(()),
                _ => None,
            });
            assert!(shown.get(), "another keeps: {another_keeps}");
        }
    }

    #[test]
    fn secondary_engine_neither_emits_pending_alerts_nor_writes_new_ones() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let db = read_just_now(data.path());
        let now = crate::now();
        // Seed an unacknowledged pending alert AND a used-up account in use.
        db.execute(
            "INSERT INTO alert (account, limit_key, kind, window, at, detail)
             VALUES ('anthropic:a', 'five_hour', 'reset', 1, ?1, '{\"accountTitle\":\"Claude Max\"}')",
            [now],
        )
        .unwrap();
        db.execute(
            "INSERT INTO account (id, provider, kind, title, read_at, full_read_at, checked_at)
             VALUES ('anthropic:a', 'anthropic', 'subscription', 'Claude Max', ?1, ?1, ?1)",
            [now],
        )
        .unwrap();
        db.execute(
            "INSERT INTO sign_in (agent, folder, provider, since, account)
             VALUES ('claude-code', '/f', 'anthropic', ?1, 'anthropic:a')",
            [now - 60_000],
        )
        .unwrap();
        db.execute(
            "INSERT INTO reading (account, limit_key, at, name, used, starts, resets)
             VALUES ('anthropic:a', 'five_hour', ?1, '5 hours', 100.0, ?2, ?3)",
            rusqlite::params![now, now - crate::time::HOUR, now + crate::time::HOUR],
        )
        .unwrap();
        crate::db::test::response(
            &db,
            ("claude-code", "/f", "anthropic", Some("anthropic:a")),
            "r1",
            now - 1_000,
            "claude-opus-5",
            Some(0.01),
        );

        let _primary = lock(data.path()).unwrap().expect("primary holds app.lock");
        let (engine, told) = start(data.path(), home.path());
        let alerts = next(&told, 10, |event| match event {
            Event::Shown(_, alerts) => Some(alerts),
            _ => None,
        });
        assert!(alerts.is_empty(), "secondary emitted alerts: {alerts:?}");
        // Only the seeded row exists; secondary wrote no UsedUp alert.
        assert_eq!(
            crate::db::test::one::<i64>(&db, "SELECT count(*) FROM alert"),
            1
        );
        // Dropping a secondary engine interrupts the 5 s retry sleep immediately.
        let began = Instant::now();
        drop(engine);
        assert!(began.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn deferred_asked_wakes_when_asked_gap_expires_not_on_60s_tick() {
        let (_send, recv) = mpsc::channel::<Wake>();
        let mut asked = Some(Ask::Opened);
        let asked_at = Some(Instant::now());
        let began = Instant::now();
        let woke = wait_within(
            &recv,
            &mut asked,
            asked_at,
            began + Duration::from_secs(60),
            Duration::from_millis(80),
        )
        .unwrap()
        .unwrap();
        let elapsed = began.elapsed();
        assert!(
            elapsed >= Duration::from_millis(70) && elapsed < Duration::from_secs(1),
            "elapsed={elapsed:?}"
        );
        assert!(!woke.tick && !woke.written);

        // When `next_tick` is already due, waking for `asked` still flags `tick: true`.
        let mut asked = Some(Ask::Opened);
        let woke = wait_within(
            &recv,
            &mut asked,
            None,
            Instant::now() - Duration::from_millis(1),
            Duration::from_secs(30),
        )
        .unwrap()
        .unwrap();
        assert!(woke.tick);
    }

    #[test]
    fn ask_limits_surfaces_external_db_writes() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let db = read_just_now(data.path());
        let now = crate::now();
        db.execute(
            "INSERT INTO account (id, provider, kind, title, read_at, full_read_at, checked_at)
             VALUES ('anthropic:a', 'anthropic', 'subscription', 'Claude Max', ?1, ?1, ?1)",
            [now],
        )
        .unwrap();
        let (engine, told) = start(data.path(), home.path());
        next(&told, 10, |event| match event {
            Event::Shown(shown, _)
                if shown.status.history_read
                    && shown
                        .status
                        .accounts
                        .iter()
                        .any(|a| a.id == "anthropic:a" && !a.hidden) =>
            {
                Some(())
            }
            _ => None,
        });
        // Simulate an external CLI write (`turnscope accounts hide`).
        db.execute("UPDATE account SET hidden = 1 WHERE id = 'anthropic:a'", [])
            .unwrap();
        engine.ask_limits();
        next(&told, 10, |event| match event {
            Event::Shown(shown, _)
                if shown
                    .status
                    .accounts
                    .iter()
                    .any(|a| a.id == "anthropic:a" && a.hidden) =>
            {
                Some(())
            }
            _ => None,
        });
    }

    #[test]
    fn engine_drop_interrupts_again_sleep_and_skips_queued_work() {
        let stop = Arc::new(Stop::new());
        let waiting = Arc::clone(&stop);
        let handle = std::thread::spawn(move || waiting.wait(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(20));
        let began = Instant::now();
        stop.end();
        assert!(handle.join().unwrap());
        assert!(began.elapsed() < Duration::from_millis(500));

        // Queued requests after drop are not executed.
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        drop(read_just_now(data.path()));
        let (engine, _told) = start(data.path(), home.path());
        let (hold_send, hold_recv) = mpsc::channel::<()>();
        let ran = Arc::new(AtomicBool::new(false));
        let ran_flag = Arc::clone(&ran);
        Engine::ask(&engine.requests, move |_| {
            let _ = hold_recv.recv_timeout(Duration::from_secs(5));
        });
        Engine::ask(&engine.requests, move |_| {
            ran_flag.store(true, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(20));
        drop(engine);
        let _ = hold_send.send(());
        std::thread::sleep(Duration::from_millis(50));
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[test]
    fn same_ignores_limit_work_rate_changes() {
        let a: Shown = serde_json::from_str(include_str!("../tests/fixtures/status.json")).unwrap();
        let mut b = a.clone();
        b.status.accounts[0].limits[0].work = Some(status::Work {
            rate_per_hour: 99.9,
            left_minutes: None,
        });
        assert!(same(&a, &b));
        b.status.accounts[0].limits[0].left_percent = 1;
        assert!(!same(&a, &b));
    }

    #[test]
    fn used_most_handles_trailing_slash_in_project_path() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let db = read_just_now(data.path());
        let now = crate::now();
        db.execute(
            "INSERT INTO account (id, provider, kind, title, read_at, full_read_at, checked_at)
             VALUES ('anthropic:a', 'anthropic', 'subscription', 'Claude Max', ?1, ?1, ?1)",
            [now],
        )
        .unwrap();
        db.execute(
            "INSERT INTO sign_in (agent, folder, provider, since, account)
             VALUES ('claude-code', '/f', 'anthropic', ?1, 'anthropic:a')",
            [now - crate::time::HOUR],
        )
        .unwrap();
        db.execute(
            "INSERT INTO reading (account, limit_key, at, name, used, starts, resets)
             VALUES ('anthropic:a', 'five_hour', ?1, '5 hours', 20.0, ?2, ?3)",
            rusqlite::params![now, now - crate::time::HOUR, now + crate::time::HOUR],
        )
        .unwrap();
        db.execute(
            "INSERT INTO session (id, agent, project, title)
             VALUES ('claude-code:s1', 'claude-code', '/work/my-app/', 'Fix bug')",
            [],
        )
        .unwrap();
        crate::db::test::response(
            &db,
            ("claude-code", "/f", "anthropic", Some("anthropic:a")),
            "claude-code:s1",
            now - 10_000,
            "claude-opus-5",
            Some(1.0),
        );
        let showing = Showing::open(data.path(), home.path(), home.path()).unwrap();
        let sessions = showing.used_most("anthropic:a", "five_hour", now).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].project.as_deref(), Some("my-app"));
    }

    #[test]
    fn concurrent_log_rotation_preserves_full_backup() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("app.log");
        let full = "x".repeat(LOG_SIZE as usize + 128);
        std::fs::write(&path, &full).unwrap();
        std::thread::scope(|scope| {
            for i in 0..8 {
                let dir = data.path();
                scope.spawn(move || log(dir, &format!("line {i}")));
            }
        });
        assert_eq!(
            std::fs::read_to_string(data.path().join("app.log.1")).unwrap(),
            full
        );
        let current = std::fs::read_to_string(&path).unwrap();
        assert_eq!(current.lines().count(), 8, "{current}");
    }

    #[cfg(unix)]
    #[test]
    fn engine_lock_creates_private_data_dir() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("fresh-data");
        let _held = lock(&data).unwrap().unwrap();
        let mode = std::fs::metadata(&data).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}
