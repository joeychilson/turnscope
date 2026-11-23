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
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use notify::Watcher as _;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::alerts::{self, Alert, Settings};
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
    /// What to show, and the alerts it raised. Told when anything it shows
    /// changed, but not when only its minute did (`Showing::next`), so
    /// `at` can be minutes old. Told too after each `hide`,
    /// `save_settings` and `connect`, changed or not, so a change the app
    /// showed at once goes back if the request failed or was refused.
    Shown(Box<Shown>, Vec<Alert>),
    /// Alerts raised but never acknowledged, as when the app quit before it
    /// could tell them: told once, when the data opens.
    Pending(Vec<Alert>),
    /// Whether the data is kept current: None once a catch-up has read
    /// every agent, or why the last one failed. Told when it changes.
    Kept(Option<Stopped>),
    /// What used a limit most, as `used_most` asked.
    UsedMost {
        account: String,
        limit: String,
        sessions: Vec<UsedMost>,
    },
    /// A `connect` ended: None, or why it failed.
    Connected {
        agent: String,
        connecting: bool,
        error: Option<String>,
    },
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
    /// Explicit user refresh (`Engine::refresh_now`): bypass cooldown and backoff.
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

struct Inner {
    data: PathBuf,
    home: PathBuf,
    binary: PathBuf,
    tell: Box<dyn Fn(Event) + Send + Sync>,
    showing: Mutex<Option<Showing>>,
    kept: Mutex<Kept>,
    wake: Mutex<Option<Sender<Wake>>>,
    ended: AtomicBool,
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
            ended: AtomicBool::new(false),
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
        self.change("hiding an account", move |showing| {
            status::hide(&showing.db, &account, hidden)
        });
    }

    /// Keep which notifications are sent.
    pub fn save_settings(&self, settings: Settings) {
        self.change("saving the settings", move |showing| {
            alerts::save_settings(&showing.db, &settings)
        });
    }

    /// Record alerts as told, so none is told again.
    pub fn acknowledge(&self, ids: Vec<i64>) {
        if ids.is_empty() {
            return;
        }
        Engine::ask(&self.requests, move |inner| {
            if inner.newer().is_some() {
                return;
            }
            let done = match inner.showing().as_mut() {
                Some(showing) => alerts::acknowledge(&mut showing.db, &ids),
                None => return,
            };
            if let Err(error) = done {
                inner.failed("acknowledging alerts", error);
            }
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
                connecting,
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

    /// Force an immediate limit read for every signed-in account, bypassing
    /// cooldowns and backoff, and always emit a `Shown` event when done.
    pub fn refresh_now(&self) {
        let sent = locked(&self.inner.wake)
            .as_ref()
            .is_some_and(|wake| wake.send(Wake::Force).is_ok());
        if !sent {
            Engine::ask(&self.requests, |inner| inner.give(Tell::Always));
        }
    }

    /// Run `change` on the open data, then tell the status again, changed
    /// or not.
    fn change(
        &self,
        name: &'static str,
        change: impl FnOnce(&Showing) -> Result<()> + Send + 'static,
    ) {
        Engine::ask(&self.requests, move |inner| {
            if inner.newer().is_none() {
                let done = match inner.showing().as_ref() {
                    Some(showing) => change(showing),
                    None => return,
                };
                if let Err(error) = done {
                    inner.failed(name, error);
                }
            }
            inner.give(Tell::Always);
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
        self.inner.ended.store(true, Ordering::Relaxed);
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
        self.ended.load(Ordering::Relaxed)
    }

    fn run(&self) {
        while !self.ended() && self.newer().is_none() {
            if self.showing().is_none() {
                match Showing::open(&self.data, &self.home, &self.binary) {
                    Ok(opened) => {
                        *self.showing() = Some(opened);
                        // Before the first status, whose alerts aren't pending.
                        self.pending();
                        self.give(Tell::Always);
                    }
                    Err(error) => {
                        if self.stopped(error) {
                            return;
                        }
                        std::thread::sleep(AGAIN);
                        continue;
                    }
                }
            }
            match lock(&self.data) {
                Ok(Some(lock)) => {
                    let kept = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        keep(&self.home, &self.data, &self.wake, |caught| {
                            self.caught(caught)
                        })
                    }));
                    *locked(&self.wake) = None;
                    drop(lock);
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
                // one writes. It writes nothing, so it looks for a newer
                // Turnscope's writes itself: else, behind a newer app, it
                // failed to raise alerts every 5 s and never said to update.
                Ok(None) => {
                    let newer = match self.showing().as_ref() {
                        Some(showing) => crate::db::writing(&showing.db),
                        None => Ok(()),
                    };
                    if let Err(error @ Error::Newer { .. }) = newer {
                        self.stopped(error);
                        return;
                    }
                    self.kept(None);
                    self.give(Tell::IfChanged);
                }
                Err(error) => {
                    if self.stopped(error) {
                        return;
                    }
                }
            }
            if !self.ended() {
                std::thread::sleep(AGAIN);
            }
        }
    }

    // Report current only once a catch-up reads every agent (not on lock or
    // limit read), so persistent failures like an unwritable lock stay visible.
    fn caught(&self, caught: Caught) -> bool {
        let (new, always) = match caught {
            Caught::Limits => (true, false),
            Caught::Read { new, forced } => {
                self.kept(None);
                (new || forced, forced)
            }
            Caught::Failed { error, forced } => {
                if self.stopped(error) {
                    return false;
                }
                (true, forced)
            }
        };
        if new {
            self.give(if always {
                Tell::Always
            } else {
                Tell::IfChanged
            });
        }
        !self.ended() && self.newer().is_none()
    }

    fn stopped(&self, error: Error) -> bool {
        let why = error.to_string();
        log(&self.data, &why);
        let stopped = match error {
            Error::Newer { replaced } => Stopped::Newer { replaced },
            _ => Stopped::Failing(why),
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
        let mut showing = self.showing();
        let next = match showing.as_mut() {
            Some(showing) if self.newer().is_some() => Ok(showing
                .shown
                .clone()
                .filter(|_| tell != Tell::IfChanged)
                .map(|shown| (shown, Vec::new()))),
            Some(showing) => showing.next(crate::now(), tell),
            None => return,
        };
        match next {
            Ok(Some((shown, alerts))) => (self.tell)(Event::Shown(Box::new(shown), alerts)),
            Ok(None) => {}
            Err(error) => self.failed("working out the status", error),
        }
    }

    fn pending(&self) {
        let pending = match self.showing().as_ref() {
            Some(showing) => showing.pending(crate::now()),
            None => return,
        };
        match pending {
            Ok(alerts) if !alerts.is_empty() => (self.tell)(Event::Pending(alerts)),
            Ok(_) => {}
            Err(error) => log(&self.data, &error.to_string()),
        }
    }
}

const LOG_SIZE: u64 = 1 << 20;

/// Append `line` with a timestamp to `app.log` in `data`, rotating past 1 MiB to `app.log.1`.
pub fn log(data: &Path, line: &str) {
    let path = data.join("app.log");
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_SIZE) {
        let _ = std::fs::rename(&path, data.join("app.log.1"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
    {
        let line = line.replace('\n', " ");
        let _ = writeln!(file, "{:.0} {line}", jiff::Timestamp::now());
    }
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
    std::fs::create_dir_all(data)?;
    crate::db::lock_within(&data.join("app.lock"), Duration::ZERO)
}

// Minimum gap between file-triggered catch-ups: 5 s, over catching up as writes
// settle (9.8G vs 3.2G instructions over 5,000 sessions with heavy writes).
const GAP: Duration = Duration::from_secs(5);

/// The least time between two reads of limits asked for
/// (`limits::refresh`): each gathers every agent's logins, the
/// Keychain's with `security`, and asks a failing provider again. A guess
/// at how soon someone opens the panel again, not measured.
const ASKED: Duration = Duration::from_secs(30);

enum Caught {
    Limits,
    Read { new: bool, forced: bool },
    Failed { error: Error, forced: bool },
}

/// Catch up when the agents' folders change, and each minute, as horizons
/// and outlooks move with the clock, reading the limits asked for first
/// (`Wake::Asked`). Tells `told` what each catch-up came to, and returns
/// once it says to stop.
fn keep(
    home: &Path,
    data: &Path,
    wake: &Mutex<Option<Sender<Wake>>>,
    mut told: impl FnMut(Caught) -> bool,
) -> Result<()> {
    let mut db = crate::db::open(data)?;
    let (changed, changes) = mpsc::channel::<Wake>();
    *locked(wake) = Some(changed.clone());
    // Any write, not only to history: macOS reports a write when its file
    // is closed, and Codex and OpenCode keep theirs open while they work.
    // Not a SQLite `-shm` file, which opening a database writes: counted, a
    // database that failed every read started a catch-up every 5 s (21 in
    // 100 s, measured 2026-11-11). An agent's writes are in its `-wal`.
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
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
    .map_err(|error| Error::Failed(error.to_string()))?;
    let mut watched: Vec<PathBuf> = Vec::new();
    let mut watch = |watcher: &mut notify::RecommendedWatcher| {
        for agent in agents::ALL {
            // Not an uninstalled agent's folder, as `~/.gemini` with only the
            // Gemini command line there: each of its writes would catch up.
            for folder in agent.installed_folders(home) {
                if watched.contains(&folder) {
                    continue;
                }
                if watcher
                    .watch(&folder, notify::RecursiveMode::Recursive)
                    .is_ok()
                {
                    watched.push(folder);
                }
            }
        }
    };
    // The minute and SQLite's `data_version`, which another connection's
    // write moves, at the last catch-up that worked: with neither moved and
    // nothing read, there's nothing new to show. Working out a status after
    // every catch-up ran 11.3G instructions, with a stand-in agent writing
    // three times a second for 30 s, against 0.4G with this.
    let mut pushed = None;
    // The limits before history: on a first launch that reads all of it (12 s
    // for a made-up 5,000 files), the app showed no account until it ended.
    if crate::online()
        && read_limits(&mut db, home, data, crate::limits::Ask::Scheduled)?
        && !told(Caught::Limits)
    {
        return Ok(());
    }
    // Limits asked for and not read yet, and when they last were.
    let (mut asked, mut asked_at, mut forced) = (false, None::<Instant>, false);
    loop {
        watch(&mut watcher);
        let was_forced = std::mem::take(&mut forced);
        let mut read_asked = false;
        if was_forced {
            (asked, asked_at) = (false, Some(Instant::now()));
            if crate::online() {
                read_asked = read_limits(&mut db, home, data, crate::limits::Ask::Force)?;
            }
        } else if asked && asked_at.is_none_or(|at| at.elapsed() >= ASKED) {
            (asked, asked_at) = (false, Some(Instant::now()));
            // Offline, the catch-up still takes in limits agents wrote.
            if crate::online() {
                read_asked = read_limits(&mut db, home, data, crate::limits::Ask::Opened)?;
            }
        }
        let caught = match crate::catch_up(&mut db, home, None) {
            Ok(read) => {
                let now = (
                    crate::time::minute(crate::now()),
                    db.pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))?,
                );
                let new = read || read_asked || pushed != Some(now);
                pushed = Some(now);
                Caught::Read {
                    new,
                    forced: was_forced,
                }
            }
            Err(error) => {
                // Once a newer Turnscope has written here, this one writes
                // nothing more, so it stops, and the app says why. Going on,
                // it showed what it read before as current.
                crate::db::writing(&db)?;
                Caught::Failed {
                    error,
                    forced: was_forced,
                }
            }
        };
        if !told(caught) {
            return Ok(());
        }
        // A write waits for a burst of writes to settle, a second at most,
        // and out the gap: the next catch-up reads what's written meanwhile.
        // Limits asked for are read at once, unless read under `ASKED` ago:
        // waiting out the gap, they were read 5 s later.
        let waited = Instant::now();
        let (mut first, mut settled) = (None::<Instant>, false);
        loop {
            let wait = match first {
                None => Duration::from_secs(60),
                Some(_) if !settled => Duration::from_millis(250),
                Some(_) => GAP.saturating_sub(waited.elapsed()),
            };
            match changes.recv_timeout(wait) {
                Ok(Wake::Stop) => return Ok(()),
                Ok(Wake::Force) => {
                    forced = true;
                    break;
                }
                Ok(wake) => {
                    asked |= wake == Wake::Asked;
                    let first = *first.get_or_insert_with(Instant::now);
                    settled |= first.elapsed() >= Duration::from_secs(1);
                }
                Err(RecvTimeoutError::Timeout) if first.is_none() || settled => break,
                Err(RecvTimeoutError::Timeout) => settled = true,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Error::Failed("the agents' folders can't be watched".into()));
                }
            }
            if asked && asked_at.is_none_or(|at| at.elapsed() >= ASKED) {
                break;
            }
        }
    }
}

/// Read the limits due (`limits::refresh`), or asked for; whether any were.
/// A failure is logged, and counts as a read so the status is told again,
/// unless a newer Turnscope wrote.
fn read_limits(
    db: &mut Connection,
    home: &Path,
    data: &Path,
    ask: crate::limits::Ask,
) -> Result<bool> {
    match crate::limits::refresh(db, home, ask) {
        Err(error @ Error::Newer { .. }) => Err(error),
        Err(error) => {
            log(data, &error.to_string());
            Ok(true)
        }
        read => read,
    }
}

struct Showing {
    db: Connection,
    data: PathBuf,
    home: PathBuf,
    binary: PathBuf,
    shown: Option<Shown>,
    agents: Option<(i64, Vec<Agent>)>,
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
        })
    }

    // Skip telling statuses that differ only in `at` (app ticks on its own clock):
    // cut 60 -> 10 statuses/hr after last read, 0/hr thereafter (measured 2026-11-18).
    fn next(&mut self, now: i64, tell: Tell) -> Result<Option<(Shown, Vec<Alert>)>> {
        let status = status::status(&self.db, now)?;
        let in_use = |id: &str| status::in_use(&status.accounts, id);
        // Cache MCP config reads for the minute (`~/.claude.json` can be megabytes).
        let agents = match &self.agents {
            Some((at, agents)) if tell != Tell::WithAgents && *at == status.at => agents
                .iter()
                .map(|agent| Agent {
                    in_use: in_use(&agent.id),
                    ..agent.clone()
                })
                .collect(),
            _ => {
                let agents = status::agents(&self.home, &self.binary, in_use);
                self.agents = Some((status.at, agents.clone()));
                agents
            }
        };
        let shown = Shown { status, agents };
        if self.shown.as_ref().is_some_and(|told| same(told, &shown)) {
            return Ok((tell != Tell::IfChanged).then_some((shown, Vec::new())));
        }
        match alerts::raise(&mut self.db, &shown.status) {
            Ok(raised) => {
                self.shown = Some(shown.clone());
                Ok(Some((shown, raised)))
            }
            Err(error @ Error::Newer { .. }) => Err(error),
            Err(error) => {
                log(&self.data, &format!("raising alerts: {error}"));
                Ok(Some((shown, Vec::new())))
            }
        }
    }

    fn pending(&self, now: i64) -> Result<Vec<Alert>> {
        let status = status::status(&self.db, now)?;
        alerts::pending(&self.db, &status, now)
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
        let (used, shares) = usage::breakdown(
            &self.db,
            account,
            Some(limit),
            None,
            usage::By::Session,
            3,
            now,
        )?;
        let mut sessions = Vec::new();
        for (at, row) in used.rows.iter().enumerate() {
            let (project_path, agent) = usage::brief(&self.db, &row.key)?;
            let project = project_path
                .as_deref()
                .and_then(|path| path.rsplit('/').next())
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
        && *accounts == b.status.accounts
        && *settings == b.status.settings
        && a.agents == b.agents
}

/// What the app shows of the data in `data` now, read once and only read:
/// no database is made or taken in, no alert is raised and nothing is
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

    #[test]
    fn data_a_newer_turnscope_wrote_is_said_and_not_tried_again() {
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
        read_just_now(data.path())
            .execute(
                "INSERT INTO account (id, provider, kind, title)
                 VALUES ('anthropic:a', 'anthropic', 'subscription', 'Claude')",
                [],
            )
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
        // The held status must arrive first (not overtaken by `hide`), and once
        // `hide` is told no older `false` may follow it. `keep`'s initial
        // `history_read` flip may emit one extra `false` before `hide` runs.
        assert!(!shown(10));
        let mut saw_hidden = false;
        for _ in 0..3 {
            if shown(10) {
                saw_hidden = true;
                break;
            }
        }
        assert!(saw_hidden);
        while let Ok(event) = told.recv_timeout(Duration::from_millis(150)) {
            if let Event::Shown(shown, _) = event {
                assert!(shown.status.accounts[0].hidden);
            }
        }
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

    #[test]
    fn the_log_starts_over_once_it_is_full() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("app.log");
        std::fs::write(&path, vec![b'x'; LOG_SIZE as usize + 1]).unwrap();
        log(data.path(), "it failed\nat once");
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
        // Nothing is asked over the network: the home holds no login to
        // read a limit with, and the prices count as read just now.
        db.execute("UPDATE state SET prices_at = ?1", [crate::now()])
            .unwrap();
        let reading = crate::db::lock(&data.path().join("ingest.lock")).unwrap();
        let (sender, messages) = mpsc::channel();
        let (home_kept, data_kept) = (home.path().to_path_buf(), data.path().to_path_buf());
        std::thread::spawn(move || {
            let wake = Mutex::new(None);
            keep(&home_kept, &data_kept, &wake, |_| sender.send(()).is_ok())
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
    /// after the gap between catch-ups.
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
            keep(&home_kept, &data_kept, &wake_kept, |_| {
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
                .next(at, tell)
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
        alerts::save_settings(&showing.db, &settings).unwrap();
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
            let (shown, _) = showing.next(at, tell).unwrap()?;
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

    #[test]
    fn log_rotates_past_limit_into_backup_file() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("app.log");
        let existing = "x".repeat(LOG_SIZE as usize + 1);
        std::fs::write(&path, &existing).unwrap();
        log(data.path(), "after rotate");
        assert_eq!(
            std::fs::read_to_string(data.path().join("app.log.1")).unwrap(),
            existing
        );
        let fresh = std::fs::read_to_string(&path).unwrap();
        assert!(fresh.trim_end().ends_with(" after rotate"), "{fresh}");
    }

    #[test]
    fn refresh_now_emits_shown_even_when_nothing_changed() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        drop(read_just_now(data.path()));
        let (engine, told) = start(data.path(), home.path());
        // Once the first catch-up reports `history_read`, `keep` has installed
        // `wake` and parked on `changes.recv_timeout`.
        next(&told, 10, |event| match event {
            Event::Shown(shown, _) if shown.status.history_read => Some(()),
            _ => None,
        });
        engine.refresh_now();
        next(&told, 10, |event| {
            matches!(event, Event::Shown(_, _)).then_some(())
        });
    }
}
