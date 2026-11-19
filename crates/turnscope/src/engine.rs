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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsedMost {
    /// `claude-code:0f6e3f6a-713c`.
    pub session: String,
    pub title: Option<String>,
    /// The name of its project's folder.
    pub project: Option<String>,
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
pub struct Stopped {
    pub why: String,
    /// False once a newer Turnscope has written to the data. The engine
    /// then stops for good: this is the last `Event::Kept` it tells, it
    /// writes nothing more, raises no alert, and refuses every request but
    /// `used_most` (`Engine`), so the app shows what was read.
    pub trying_again: bool,
}

/// How long the engine waits before it tries again: to open data it
/// couldn't, to keep it current after that stopped, or to take over from
/// another app.
const AGAIN: Duration = Duration::from_secs(5);

/// Keeps the data current and tells the app what to show, on threads of its
/// own. A request is never waited for: what it changes comes back as an
/// `Event`.
///
/// Requests run one at a time, in the order asked, on one thread, and
/// connects on another. Over a thread for each request: the app sends the
/// whole `Settings` each time, and of two saved in a row, the first was
/// kept in 9 of 300 tries (measured 2026-11-18). Connects apart, as an agent's command can take a
/// minute: a hide waiting behind one would show at once, go back with the
/// next status, and show again a minute later.
///
/// Once a newer Turnscope has written to the data
/// (`Stopped::trying_again`), `hide` and `save_settings` change nothing and
/// tell the status last told again, `acknowledge` does nothing, and
/// `connect` runs no command and tells why. `used_most` still answers: it
/// only reads.
pub struct Engine {
    inner: Arc<Inner>,
    requests: Sender<Work>,
    connects: Sender<Work>,
}

/// A request, as a queue runs it (`queue`).
type Work = Box<dyn FnOnce(&Inner) + Send>;

struct Inner {
    data: PathBuf,
    home: PathBuf,
    /// The `turnscope` binary agents run once connected.
    binary: PathBuf,
    tell: Box<dyn Fn(Event) + Send + Sync>,
    /// None until the data opens.
    showing: Mutex<Option<Showing>>,
    /// What `Event::Kept` last said, so it's told only when it changes.
    kept: Mutex<Option<Option<Stopped>>>,
    /// Set once the `Engine` is dropped: its threads end at their next turn.
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
            kept: Mutex::new(None),
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
            let done = match inner.showing().as_ref() {
                Some(showing) => alerts::acknowledge(&showing.db, &ids).map(|_| ()),
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
                Some(why) => Err(Error::Newer(why)),
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
/// `Engine` is dropped.
fn queue(inner: &Arc<Inner>) -> Sender<Work> {
    let (send, received) = mpsc::channel::<Work>();
    let inner = Arc::clone(inner);
    std::thread::spawn(move || {
        for work in received {
            work(&inner);
        }
    });
    send
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.inner.ended.store(true, Ordering::Relaxed);
    }
}

impl Inner {
    fn showing(&self) -> MutexGuard<'_, Option<Showing>> {
        self.showing
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
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
                        keep(&self.home, &self.data, |caught| self.caught(caught))
                    }));
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
                // one writes, until it's gone. It writes nothing, so it
                // looks for a newer Turnscope's writes itself: otherwise,
                // behind a newer app, it failed to raise alerts every 5 s
                // for good and never said to update.
                Ok(None) => {
                    let newer = match self.showing().as_ref() {
                        Some(showing) => crate::db::writing(&showing.db),
                        None => Ok(()),
                    };
                    if let Err(error @ Error::Newer(_)) = newer {
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
            std::thread::sleep(AGAIN);
        }
    }

    /// Take in what `keep` found, and tell the status if there may be
    /// something new: whether to go on.
    ///
    /// The data is current once a catch-up has read every agent: not when
    /// the lock is taken, nor when the limits are read. Said to be current
    /// after each catch-up, failed or not, one that failed each time, as
    /// when `ingest.lock` couldn't be made, never said why, and the app
    /// showed what was read before as current.
    fn caught(&self, caught: Caught) -> bool {
        let new = match caught {
            Caught::Limits => true,
            Caught::Read { new } => {
                self.kept(None);
                new
            }
            // One that failed may have read some.
            Caught::Failed(error) => {
                if self.stopped(error) {
                    return false;
                }
                true
            }
        };
        if new {
            self.give(Tell::IfChanged);
        }
        !self.ended() && self.newer().is_none()
    }

    /// Tell the app why the data isn't kept current, and log it; whether
    /// the engine stops for good, as once a newer Turnscope has written to
    /// the data.
    fn stopped(&self, error: Error) -> bool {
        let newer = matches!(error, Error::Newer(_));
        let why = error.to_string();
        log(&self.data, &why);
        self.kept(Some(Stopped {
            why,
            trying_again: !newer,
        }));
        newer
    }

    /// Log why `doing` failed; or, once a newer Turnscope has written to the
    /// data, stop for good and say so at once, not at the next catch-up, a
    /// minute later at most: until then a change asked for went back with
    /// no word why.
    fn failed(&self, doing: &str, error: Error) {
        match error {
            Error::Newer(_) => {
                self.stopped(error);
            }
            error => log(&self.data, &format!("{doing}: {error}")),
        }
    }

    /// What `Event::Kept` last said, if anything.
    fn said(&self) -> MutexGuard<'_, Option<Option<Stopped>>> {
        self.kept
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Tell `now` as `Event::Kept`, if it isn't what was last said. Never
    /// once the engine has stopped for good.
    fn kept(&self, now: Option<Stopped>) {
        let mut said = self.said();
        let done = matches!(&*said, Some(Some(stopped)) if !stopped.trying_again);
        if !done && said.as_ref() != Some(&now) {
            *said = Some(now.clone());
            drop(said);
            (self.tell)(Event::Kept(now));
        }
    }

    /// Why the engine stopped for good, once a newer Turnscope has written
    /// to the data.
    fn newer(&self) -> Option<String> {
        match &*self.said() {
            Some(Some(stopped)) if !stopped.trying_again => Some(stopped.why.clone()),
            _ => None,
        }
    }

    /// Tell the status, as `tell` says. Once the engine has stopped for
    /// good, nothing is worked out again, which would raise alerts: the
    /// status last told is told again, unless only changes are asked for.
    ///
    /// Told while `showing` is held, over after: the run thread and the
    /// queues each give, and a status worked out first could be told after
    /// a newer one, so the app showed an account it had just hidden until
    /// the next change. It can't deadlock: `tell` waits for nothing
    /// (`Engine::start`), as the app's only queues the event for its own
    /// thread, and the engine's requests only send on a channel.
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

/// How large `app.log` grows before it starts over. A catch-up that fails
/// each time it's tried writes about 150 bytes, every 5 s at most while
/// agents work: a day of that at least.
const LOG_SIZE: u64 = 1 << 20;

/// Add `line` to `app.log` in `data`, with the time. An app is started from
/// its icon, where no one reads its standard error, so what goes wrong
/// while it runs is kept here, for `turnscope doctor` to show.
pub fn log(data: &Path, line: &str) {
    let path = data.join("app.log");
    let full = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_SIZE);
    let mut options = std::fs::OpenOptions::new();
    match full {
        true => options.write(true).truncate(true),
        false => options.append(true),
    };
    if let Ok(mut file) = options.create(true).open(path) {
        let line = line.replace('\n', " ");
        let _ = writeln!(file, "{:.0} {line}", jiff::Timestamp::now());
    }
}

/// `app.lock` in `data`, held while one app keeps the data current, or None
/// while another does: two would only read the same history twice. Two at
/// once would still be safe, as the command line catching up beside the app
/// is: the database waits up to 10 s for the other's write.
fn lock(data: &Path) -> Result<Option<File>> {
    std::fs::create_dir_all(data)?;
    let lock = File::create(data.join("app.lock"))?;
    Ok(lock.try_lock().is_ok().then_some(lock))
}

/// The least time from one catch-up's end to the next one's start, while
/// agents write in their folders: 5 s, over catching up as soon as writes
/// settle. Many agents at work at once never let writes settle, and each
/// catch-up looks at every history file. Over 5,000 sessions, with a
/// stand-in agent writing ten of them ten times a second for 30 s, it ran
/// 9.8G instructions without the gap and 3.2G with it. The cost: the panel
/// shows what an agent wrote up to 5 s late.
const GAP: Duration = Duration::from_secs(5);

/// What `keep` found.
enum Caught {
    /// The limits, read before the first catch-up.
    Limits,
    /// A catch-up that read every agent; `new` when there may be something
    /// new to show.
    Read { new: bool },
    /// A catch-up that failed, and why.
    Failed(Error),
}

/// Catch up when the agents' folders change, and each minute, as horizons
/// and outlooks move with the clock. Tells `told` what each catch-up came
/// to, and returns once it says to stop.
fn keep(home: &Path, data: &Path, mut told: impl FnMut(Caught) -> bool) -> Result<()> {
    let mut db = crate::db::open(data)?;
    let (changed, changes) = mpsc::channel();
    // Any write, not only to history: macOS reports a write when its file
    // is closed, and Codex keeps a session's file open, and OpenCode its
    // database, while they work. Their other writes are what show it.
    // But not a SQLite database's `-shm` file, which Turnscope writes too
    // when it opens the database: counted, a database that fails every read,
    // with a log left beside it, started a catch-up every 5 s for good (21 in
    // 100 s, measured 2026-11-11). An agent's writes to its database are in
    // the `-wal` file.
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let ours = event.is_ok_and(|event| {
            !event.paths.is_empty()
                && event
                    .paths
                    .iter()
                    .all(|path| path.to_string_lossy().ends_with("-shm"))
        });
        if !ours {
            let _ = changed.send(());
        }
    })
    .map_err(|error| Error::Failed(error.to_string()))?;
    let mut watched: Vec<PathBuf> = Vec::new();
    let mut watch = |watcher: &mut notify::RecommendedWatcher| {
        for agent in agents::ALL {
            for (n, folder) in agent.folders(home).into_iter().enumerate() {
                // The agent's own folder may be another program's, as
                // `~/.gemini` is the Gemini command line's too: every write
                // there would start a catch-up. Until the agent is installed
                // it's looked through each minute, as a folder not there yet
                // is, and watched from the first minute it is.
                if watched.contains(&folder) || (n == 0 && !agent.installed(&folder)) {
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
    // The minute and SQLite's `data_version` at the last catch-up that
    // worked. `data_version` moves when another connection writes, as an
    // agent's MCP server does. A catch-up that read nothing new, in the same
    // minute, with no other write since, has nothing new to show, and no
    // status is worked out: as when an agent writes anything but history in
    // its folder. Chosen over working one out after every catch-up: with a
    // stand-in agent writing so three times a second for 30 s, it ran 11.3G
    // instructions that way and 0.4G with this. The cost: an agent's MCP
    // settings changed outside the app show at the next minute.
    let mut pushed = None;
    // The limits first, and `told` called, over calling it only once the
    // first catch-up is done: on a first launch that reads all of history
    // (12 s for a made-up one of 5,000 files, 176 MB), the app showed no
    // account until it ended. The catch-up then finds the limits read.
    if crate::online() {
        let read = match crate::limits::refresh(&mut db, home) {
            Ok(read) => read,
            Err(error @ Error::Newer(_)) => return Err(error),
            Err(error) => {
                log(data, &error.to_string());
                true
            }
        };
        if read && !told(Caught::Limits) {
            return Ok(());
        }
    }
    loop {
        watch(&mut watcher);
        let caught = match crate::catch_up(&mut db, home, None) {
            Ok(read) => {
                let now = (
                    crate::now().div_euclid(crate::time::MINUTE),
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
        let waited = Instant::now();
        match changes.recv_timeout(Duration::from_secs(60)) {
            Ok(()) => {
                // Let a burst of writes settle, a second at most.
                let start = Instant::now();
                while changes.recv_timeout(Duration::from_millis(250)).is_ok()
                    && start.elapsed() < Duration::from_secs(1)
                {}
                // The next catch-up reads what's written meanwhile.
                std::thread::sleep(GAP.saturating_sub(waited.elapsed()));
                while changes.try_recv().is_ok() {}
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(Error::Failed("the agents' folders can't be watched".into()));
            }
        }
    }
}

/// The open data, and what was last told of it.
struct Showing {
    db: Connection,
    data: PathBuf,
    home: PathBuf,
    binary: PathBuf,
    /// The status last told, which the app shows.
    shown: Option<Shown>,
    /// Every agent as its MCP settings were last read, and the minute
    /// they were.
    agents: Option<(i64, Vec<Agent>)>,
}

/// When a status is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tell {
    /// Only if what it shows changed, as after a catch-up.
    IfChanged,
    /// Changed or not, as after a request: a change the app showed at once
    /// goes back if the request failed.
    Always,
    /// Always, with each agent's MCP settings read again, as after a
    /// connect.
    WithAgents,
}

impl Showing {
    /// Open the data in `data`, or say why it can't be shown, as when a
    /// newer Turnscope has written to it.
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

    /// The status now, with the alerts it raised, if what it shows changed
    /// since the last one told, or `tell` says to tell it anyway. One whose
    /// alerts were raised is recorded as told; otherwise it's told without
    /// them, and the next raises them again. None is once a newer Turnscope
    /// has written: that's the error.
    ///
    /// A status is as of its minute, so it differs from the last told each
    /// minute; one that differs only so isn't told, over telling one each
    /// minute: the app counts down on its own clock. On the snapshot tests'
    /// store (`tests/cli.rs`), with nothing new read, it told 60 statuses an
    /// hour that way and 10 this way in the first hour after the last read,
    /// as outlooks moved, the readings went stale and the half hour in use
    /// ended, and none in the hours after (measured 2026-11-18). An API
    /// key's spend forecast moves with the clock too, so it has two figures
    /// (`status::two_figures`).
    fn next(&mut self, now: i64, tell: Tell) -> Result<Option<(Shown, Vec<Alert>)>> {
        let status = status::status(&self.db, now)?;
        let in_use = |id: &str| status::in_use(&status.accounts, id);
        // Each agent's MCP settings are read once a minute, and after a
        // connect: Claude Code's `~/.claude.json` can be megabytes, and a
        // status can be due every 5 s while agents work, and after each
        // request.
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
            Err(error @ Error::Newer(_)) => Err(error),
            Err(error) => {
                log(&self.data, &format!("raising alerts: {error}"));
                Ok(Some((shown, Vec::new())))
            }
        }
    }

    /// The alerts raised and not acknowledged yet.
    fn pending(&self, now: i64) -> Result<Vec<Alert>> {
        let status = status::status(&self.db, now)?;
        alerts::pending(&self.db, &status, now)
    }

    /// The sessions that used `account`'s limit `limit` most, as of the
    /// status last told: the one the app shows.
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
            let (project, agent) = usage::brief(&self.db, &row.key)?;
            sessions.push(UsedMost {
                session: row.key.clone(),
                project: project
                    .as_deref()
                    .and_then(|project| project.rsplit('/').next())
                    .map(str::to_owned),
                // By session, a row's name is its session's title.
                title: row.name.clone(),
                agent,
                share_percent: shares.as_ref().map(|shares| shares.rows[at]),
            });
        }
        Ok(sessions)
    }
}

/// Whether `a` and `b` show the same, whatever minute each is as of.
fn same(a: &Shown, b: &Shown) -> bool {
    // Every field named, so one added isn't left out.
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
        let sender = Mutex::new(sender);
        let engine = Engine::start(data.into(), home.into(), home.into(), move |event| {
            let _ = sender
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .send(event);
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
            let stopped = next(&told, 10, |event| match event {
                Event::Kept(Some(stopped)) => Some(stopped),
                _ => None,
            });
            let why = crate::db::newer().to_string();
            assert_eq!(
                stopped,
                Stopped {
                    why,
                    trying_again: false
                },
                "{newer}"
            );
        }
    }

    #[test]
    fn keeping_the_data_current_stops_once_a_newer_turnscope_writes() {
        for newer in NEWER {
            let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            // Claude Code is installed, so its folder is watched.
            let folder = home.path().join(".claude/projects/-work-a");
            std::fs::create_dir_all(&folder).unwrap();
            let db = read_just_now(data.path());
            let (_engine, told) = start(data.path(), home.path());
            next(&told, 10, |event| {
                matches!(event, Event::Kept(None)).then_some(())
            });
            db.execute_batch(newer).unwrap();
            // A write in the folder, so it catches up, 5 s after the last.
            std::fs::write(folder.join("s1.jsonl"), "{}\n").unwrap();
            let stopped = next(&told, 20, |event| match event {
                Event::Kept(Some(stopped)) => Some(stopped),
                _ => None,
            });
            assert_eq!(stopped.why, crate::db::newer().to_string(), "{newer}");
            assert!(!stopped.trying_again);
        }
    }

    /// Once a newer Turnscope has written to the data, the engine writes
    /// nothing more: a hide changes nothing, and the status told after says
    /// so; a connect runs no command, and says why.
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
        let stopped = next(&told, 20, |event| match event {
            Event::Kept(Some(stopped)) => Some(stopped),
            _ => None,
        });
        assert!(!stopped.trying_again);
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
        assert_eq!(error, Some(stopped.why));
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
        let (sender, holding, released) = (
            Mutex::new(sender),
            Mutex::new(holding),
            Mutex::new(released),
        );
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
                    let _ = holding
                        .lock()
                        .unwrap()
                        .send(shown.status.accounts[0].hidden);
                    let _ = released
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(10));
                }
                let _ = sender.lock().unwrap().send(event);
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
            failed.trying_again && failed.why.contains("ingest.lock"),
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
        let stopped = next(&told, 10, |event| match event {
            Event::Kept(Some(stopped)) => Some(stopped),
            _ => None,
        });
        assert_eq!(stopped.why, crate::db::newer().to_string());
        assert!(!stopped.trying_again);
    }

    /// One app keeps a data directory current at a time, and the next can
    /// once it's gone.
    #[test]
    fn one_app_holds_the_lock_at_a_time() {
        let data = tempfile::tempdir().unwrap();
        let first = lock(data.path()).unwrap();
        assert!(first.is_some());
        assert!(lock(data.path()).unwrap().is_none());
        drop(first);
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
        std::thread::spawn(move || keep(&home_kept, &data_kept, |_| sender.send(()).is_ok()));
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
}
