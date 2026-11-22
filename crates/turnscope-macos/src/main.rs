//! Turnscope, the menu bar app: the menu bar item, the panel it opens, notifications and
//! updates (docs/app.md). The engine runs in the app's own process (`engine`). The panel is
//! drawn with Rune; the item, notifications, opening at login and updates are AppKit and its
//! frameworks, called through objc2.

mod engine;
mod launch;
mod login;
mod menu_bar;
mod notifier;
mod panel;
mod preferences;
mod settings;
mod standing;
mod state;
mod style;
mod symbols;
mod updater;
mod words;

use std::cell::Cell;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2_app_kit::{
    NSPopUpMenuWindowLevel, NSWindowCollectionBehavior, NSWorkspace, NSWorkspaceDidWakeNotification,
};
use objc2_foundation::NSNotification;
use rune::{App, AppHandle, AppSpec, DisplayTarget, RunOptions, Tx, WindowOptions, WindowSpec};
use turnscope::alerts::Alert;
use turnscope::engine::Shown;

use launch::{Launch, Opening};
use preferences::Preferences;
use standing::*;
use state::{Page, State, minute};
use style::{GUTTER, PANEL_WIDTH};

/// The app's state, from any thread.
pub static HANDLE: OnceLock<AppHandle<State>> = OnceLock::new();
/// When the app started, from which the panel waits for the item's place (`when_placed`).
static STARTED: OnceLock<Instant> = OnceLock::new();
/// Whether the app ticks each minute: while something shown counts down with the clock, in the
/// menu bar or the open panel, and not otherwise, so a closed panel with nothing urgent never
/// wakes the app. The tick's thread waits for it (`tick`).
static COUNTING: AtomicBool = AtomicBool::new(false);
static TICKER: OnceLock<std::thread::Thread> = OnceLock::new();

thread_local! {
    /// When the panel last closed. A click on the item takes focus from the panel, which
    /// closes it, before the item's own action runs: that click closes it and doesn't open it
    /// again.
    static CLOSED: Cell<Option<Instant>> = const { Cell::new(None) };
}

// What the engine gives

/// Takes in a status the engine gave, with the alerts it raised.
pub fn took(s: &mut State, shown: Shown, alerts: Vec<Alert>, tx: &mut Tx) {
    // While the panel is open, a status's changes move, as a click's do, rather than show at
    // once, as Rune shows a change no input caused.
    if s.panel_open {
        tx.animated();
    }
    let taken = take(s, shown);
    drew(s);
    notifier::renewed(&taken.readable);
    for (account, limit) in taken.asks {
        engine::used_most(account, limit);
    }
    if !alerts.is_empty() {
        notifier::tell(s, alerts);
    }
}

/// What a status asks for once it's taken in.
#[derive(Debug, PartialEq)]
struct Taken {
    /// What used which account's limit to ask the engine, as `(account, limit)`.
    asks: Vec<(String, String)>,
    /// The accounts whose sign-in notification goes: each that can be read, on the first
    /// status, and after that each that couldn't be read and now can.
    readable: Vec<String>,
}

/// Takes in `shown`, and opens what waited for the first status.
fn take(s: &mut State, shown: Shown) -> Taken {
    let first = s.status.is_none();
    let refused: Vec<String> = s
        .all()
        .iter()
        .filter(|a| a.refused())
        .map(|a| a.id.clone())
        .collect();
    s.take(shown);
    if let Some(opening) = s.opening.take() {
        when_placed(move |s, _| {
            show_panel(s);
            apply(s, opening);
        });
    }
    // Unless the panel was closed or went to Settings while it waited.
    if let Some(clicked) = s.clicked.take()
        && s.panel_open
        && s.page == Page::Main
    {
        s.open_in_place(&clicked);
    }
    let readable = s
        .all()
        .iter()
        .filter(|a| !a.refused() && (first || refused.contains(&a.id)))
        .map(|a| a.id.clone())
        .collect();
    // Each account shown without what used it, as one that appeared since the panel opened,
    // and the open one again, as its sessions move with the status.
    let asks = match s.panel_open {
        true => asks(s, |a| s.open.as_deref() == Some(a.id.as_str())),
        false => Vec::new(),
    };
    Taken { asks, readable }
}

/// Redraws the menu bar item, and ticks while something counts down.
fn drew(s: &State) {
    menu_bar::draw(s);
    let counting = s.panel_open || s.urgent();
    COUNTING.store(counting, Ordering::Relaxed);
    if counting && let Some(ticker) = TICKER.get() {
        ticker.unpark();
    }
}

/// Opens what `opening` names, as `--open` does.
fn apply(s: &mut State, opening: Opening) {
    if opening.account {
        s.open = s.in_use().first().map(|a| a.id.clone());
    }
    if opening.unused {
        s.unused_open = true;
    }
    if let Some(tab) = opening.settings {
        s.page = Page::Settings;
        s.tab = tab;
    }
}

// Requests

/// Hides an account from the panel, the menu bar and alerts, or shows it again: at once, and
/// back if the engine refuses, as it gives the status again either way.
pub fn hide(s: &mut State, id: &str, hidden: bool) {
    s.mark_hidden(id, hidden);
    drew(s);
    engine::hide(id.to_owned(), hidden);
}

/// Connects the agent `id`, or disconnects it. It shows as working until it's done, then as
/// connected or not, and shows why if it fails.
pub fn connect(s: &mut State, id: &str, connecting: bool) {
    if !engine::running() {
        return;
    }
    let to = if connecting {
        Connection::Connected
    } else {
        Connection::Available
    };
    s.changing_agents.insert(id.to_owned(), to);
    s.agent_errors.remove(id);
    engine::connect(id.to_owned(), connecting);
}

/// Asks what used the account `id`'s deciding limit most.
pub fn ask_used(s: &State, id: &str) {
    if let Some(limit) = s
        .all()
        .iter()
        .find(|a| a.id == id)
        .and_then(|a| a.deciding.clone())
    {
        engine::used_most(id.to_owned(), limit);
    }
}

/// What used which account's deciding limit to ask the engine, as `(account, limit)`: for each
/// account shown that `again` names, or whose sessions aren't in for that limit.
fn asks(s: &State, again: impl Fn(&Account) -> bool) -> Vec<(String, String)> {
    s.all()
        .iter()
        .filter(|a| !a.hidden)
        .filter_map(|a| {
            let limit = a.deciding.as_ref()?;
            let have = s.used.get(&a.id).is_some_and(|(of, _)| of == limit);
            (again(a) || !have).then(|| (a.id.clone(), limit.clone()))
        })
        .collect()
}

// The panel

/// Runs `f` once the item has its place in the menu bar to hang the panel from: at once, or
/// as soon as it has one, or 2 s after launch without one, as when it's hidden in the menu
/// bar's settings; the panel then opens at the screen's top right (`menu_bar::panel_place`).
/// Until then the item's window is below the screen, and a panel opened from it hung off the
/// screen's bottom-left corner, out of sight. It took its place 0.2 s after the event loop
/// started, 1.55 to 1.65 s after launch in a debug build (measured 2026-11-18): a fixed second
/// after launch was too soon. The 2 s count from launch: counted from each call, a click on a
/// notification while the item was hidden opened the panel 2 s late, every time.
fn when_placed(f: impl FnOnce(&mut State, &mut Tx) + Send + 'static) {
    let Some(handle) = HANDLE.get() else { return };
    handle.update(move |s, tx| {
        if menu_bar::placed() || waited() {
            f(s, tx);
            return;
        }
        let Some(handle) = HANDLE.get().cloned() else {
            return;
        };
        // Only the main thread can see the item's place: asked there each 50 ms.
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(50));
                let (tell, told) = mpsc::channel();
                handle.update_if(move |_, _| {
                    let _ = tell.send(menu_bar::placed() || waited());
                    false
                });
                if told.recv().unwrap_or(true) {
                    break;
                }
            }
            handle.update(f);
        });
    });
}

/// Whether the 2 s after launch that the panel waits for the item's place are over.
fn waited() -> bool {
    STARTED
        .get()
        .is_none_or(|at| at.elapsed() >= Duration::from_secs(2))
}

/// The item's action: opens the panel under it, or closes it.
pub fn toggle_panel() {
    let Some(handle) = HANDLE.get() else { return };
    if handle.is_visible() {
        // Closed as a click elsewhere closes it, so `closed` runs. A hide skips `on_close`, and
        // the focus loss after it closes nothing, as the panel is hidden by then: the panel was
        // left marked open, and no click opened it again.
        handle.update(|_, tx| tx.close_window());
        return;
    }
    if CLOSED
        .get()
        .is_some_and(|at| at.elapsed() < Duration::from_millis(250))
    {
        return;
    }
    handle.update(|s, _| show_panel(s));
}

/// Opens the panel on its first page with `account` open in place, as a notification about it
/// asks. Clicked before the first status, as when the click launched the app, the account
/// opens once the status arrives, since no account is known before.
pub fn open_panel(account: Option<String>) {
    when_placed(move |s, _| {
        show_panel(s);
        let Some(account) = account else { return };
        if s.status.is_none() {
            s.clicked = Some(account);
        } else {
            s.open_in_place(&account);
        }
    });
}

/// Opens the panel under the item, on its first page with nothing open, or brings it forward
/// if it's open.
fn show_panel(s: &mut State) {
    let Some(handle) = HANDLE.get() else { return };
    if s.panel_open {
        handle.focus();
        return;
    }
    s.reset();
    s.panel_open = true;
    s.now = minute(turnscope::now());
    // What used each account is asked as the panel opens, so an account opens with them
    // already there. Asked for only once it was open, they came up to a quarter of a second
    // later and pushed the rows below it down a second time.
    for (account, limit) in asks(s, |_| true) {
        engine::used_most(account, limit);
    }
    notifier::check();
    login::check();
    engine::poke();
    s.checks_automatically = updater::automatic();
    if let Some(place) = menu_bar::panel_place(PANEL_WIDTH as f64, GUTTER as f64) {
        handle.place(DisplayTarget::Primary, place);
    }
    menu_bar::highlight(true);
    drew(s);
    handle.open();
}

fn closed(s: &mut State, _: &mut Tx) {
    s.panel_open = false;
    s.clicked = None;
    s.reset();
    menu_bar::highlight(false);
    drew(s);
    CLOSED.set(Some(Instant::now()));
}

/// Grows the open row's bars a moment after it opens. Rune shows a change no input caused at
/// once, so the bars stood at their level without growing, unless it's animated.
fn poll(s: &mut State, clock: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    if s.grow_due.is_some_and(|due| clock >= due) {
        tx.animated();
        s.grown = true;
        s.grow_due = None;
        changed = true;
    }
    if s.closed.as_ref().is_some_and(|(_, due)| clock >= *due) {
        s.closed = None;
        changed = true;
    }
    changed
}

/// Ticks each minute, the finest unit the words use for time, while something counts down.
fn tick() {
    let ticker = std::thread::spawn(|| {
        loop {
            // Parked while nothing counts down, it wakes only when something does.
            while !COUNTING.load(Ordering::Relaxed) {
                std::thread::park();
            }
            let now = turnscope::now();
            let next = minute(now) + 60_000;
            std::thread::sleep(Duration::from_millis((next - now).max(0) as u64 + 50));
            if !COUNTING.load(Ordering::Relaxed) {
                continue;
            }
            if let Some(handle) = HANDLE.get() {
                handle.update_if(|s, _| ticked(s));
            }
        }
    });
    let _ = TICKER.set(ticker.thread().clone());
}

/// Ticks once the Mac wakes. The tick's sleep stops while the Mac sleeps, so for up to a
/// minute after it woke, the item showed the countdowns of when it went to sleep.
fn tick_on_wake() {
    let woke = RcBlock::new(|_: NonNull<NSNotification>| {
        engine::poke();
        if let Some(handle) = HANDLE.get() {
            handle.update_if(|s, _| ticked(s));
        }
    });
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    // The center keeps the block as long as the app runs.
    unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceDidWakeNotification),
            None,
            None,
            &woke,
        );
    }
}

/// Moves what counts down to the minute it is: the menu bar item, and the open panel. Whether
/// the panel changed.
fn ticked(s: &mut State) -> bool {
    drew(s);
    if s.panel_open {
        s.now = minute(turnscope::now());
    }
    s.panel_open
}

// Small things the views need

pub fn beep() {
    objc2_app_kit::NSBeep();
}

pub fn open_url(url: &str) {
    let Some(url) =
        objc2_foundation::NSURL::URLWithString(&objc2_foundation::NSString::from_str(url))
    else {
        return;
    };
    objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&url);
}

/// Whether the app runs from its disk image, or from the read-only copy macOS runs an app from
/// when it's opened where it was downloaded (App Translocation). Either is gone once the image
/// is ejected or the app quits, and so would be the engine an agent was connected to.
fn from_disk_image() -> bool {
    let bundle = objc2_foundation::NSBundle::mainBundle()
        .bundlePath()
        .to_string();
    if bundle.contains("/AppTranslocation/") {
        return true;
    }
    let Ok(path) = std::ffi::CString::new(bundle) else {
        return false;
    };
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    unsafe {
        libc::statfs(path.as_ptr(), &mut stat) == 0 && stat.f_flags & libc::MNT_RDONLY as u32 != 0
    }
}

// Starting

fn blank() -> State {
    State::new(Preferences::default())
}

fn spec() -> AppSpec<State> {
    AppSpec::<State> {
        families: ("System Font", "ui-monospace"),
        fonts: style::fonts(),
        poll: Some(poll),
        on_key: Some(panel::on_key),
        ..AppSpec::new("Turnscope", (PANEL_WIDTH + 2.0, 420.0), blank, panel::view)
    }
}

/// The status in the file at `path`, as `--fixture` names it.
fn fixture(path: &Path) -> Shown {
    let read = std::fs::read_to_string(path).map_err(|error| error.to_string());
    match read.and_then(|json| serde_json::from_str(&json).map_err(|error| error.to_string())) {
        Ok(shown) => shown,
        Err(error) => {
            eprintln!("Turnscope: {} can't be read: {error}", path.display());
            std::process::exit(2);
        }
    }
}

/// The status the engine works out from the data in `data`, for the agents in `home`, as it is
/// now. It only reads: no alert is raised, and nothing is caught up.
fn read(home: &Path, data: &Path) -> Shown {
    match turnscope::engine::shown(data, home, &engine::binary()) {
        Ok(shown) => shown,
        Err(error) => {
            eprintln!("Turnscope: {} can't be read: {error}", data.display());
            std::process::exit(1);
        }
    }
}

/// Draws the panel as `panel-light.png` and `panel-dark.png` in `folder`, with what `opening`
/// names open, and quits. It shows the status in `fixture`, as `scripts/screenshots.sh` draws
/// the README's, or else what the engine reads in `home` and `data`, as `scripts/check-app.sh`
/// checks a built app does.
fn snapshot(
    folder: &Path,
    fixture_path: Option<&Path>,
    opening: Option<Opening>,
    home: &Path,
    data: &Path,
) {
    if let Err(error) = std::fs::create_dir_all(folder) {
        eprintln!("Turnscope: {} can't be made: {error}", folder.display());
        std::process::exit(2);
    }
    let shown = match fixture_path {
        Some(path) => fixture(path),
        None => read(home, data),
    };
    for theme in ["light", "dark"] {
        let mut s = State::fixed(shown.clone(), Preferences::default());
        if let Some(opening) = opening {
            apply(&mut s, opening);
        }
        let out = folder.join(format!("panel-{theme}.png"));
        // Drawn on a canvas taller than any panel, and cut to the panel's frame: Rune sizes the
        // image before it fits the window to what's in it.
        let args = [
            "--snap",
            &out.to_string_lossy(),
            "--crop",
            panel::FRAME,
            "--theme",
            theme,
            "--scale",
            "2",
        ]
        .map(str::to_owned);
        let mut spec = spec();
        spec.size.1 = 2000.0;
        let app = App::new(spec).init(move || s).srgb_blend(true).args(args);
        if let Err(error) = app.try_run() {
            eprintln!("Turnscope: the panel can't be drawn: {error}");
            std::process::exit(1);
        }
    }
}

fn main() {
    let _ = STARTED.set(Instant::now());
    turnscope::db::started();
    let arguments: Vec<String> = std::env::args().collect();
    let launch = Launch::parse(&arguments);
    let (home, data) = turnscope::places(launch.home.clone(), launch.data.clone());
    if let Some(folder) = &launch.snapshot {
        snapshot(
            folder,
            launch.fixture.as_deref(),
            launch.opening,
            &home,
            &data,
        );
        return;
    }
    let fixed = launch.fixture.as_deref().map(fixture);
    if fixed.is_none() {
        let _ = engine::DATA.set(data.clone());
        // What went wrong is kept in the log, beside the data.
        let panicked = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            engine::log(&format!("panicked: {info}"));
            panicked(info);
        }));
    }
    let opening = launch.opening;
    let app = App::new(spec())
        .init(move || {
            let mut s = match fixed {
                // Changes made on a fixed status go nowhere.
                Some(shown) => State::fixed(shown, Preferences::default()),
                None => State::new(Preferences::read()),
            };
            s.from_disk_image = from_disk_image();
            s.live = true;
            s.opening = opening;
            s
        })
        .windowless()
        .srgb_blend(true)
        // Like a menu, it closes once something else is clicked.
        .on_focus(|_, focused, tx| {
            if !focused {
                tx.close_window();
            }
        })
        .on_close(closed);
    let handle = app.handle();
    let _ = HANDLE.set(handle.clone());
    let runs_engine = launch.fixture.is_none();
    let engine_handle = handle.clone();
    handle.update(move |s, _| {
        drew(s);
        // A fixed status asks nothing of macOS or Sparkle, and doesn't count down.
        if runs_engine {
            notifier::start();
            updater::start();
            tick_on_wake();
            tick();
            engine::start(home, data, engine_handle);
        } else if let Some(opening) = s.opening.take() {
            // A fixed status is there from the start.
            when_placed(move |s, _| {
                show_panel(s);
                apply(s, opening);
            });
        }
    });
    let options = WindowOptions::launcher_panel().macos(|native| {
        // As a menu is: above other windows, on every Space, and left out of window cycling.
        let behavior = NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Transient
            | NSWindowCollectionBehavior::IgnoresCycle;
        native
            .shadow(false)
            .level(NSPopUpMenuWindowLevel)
            .collection_behavior(behavior.0)
    });
    rune::run_windows_with_args(RunOptions { accessory: true }, [], move |_| {
        vec![WindowSpec::from_app("panel", app).configured_options(options)]
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example status in tests/fixtures/status.json, as `--fixture` shows it.
    fn example() -> Shown {
        serde_json::from_str(include_str!("../../turnscope/tests/fixtures/status.json")).unwrap()
    }

    const CLAUDE: &str = "anthropic:acct-1:org-1";
    const CODEX: &str = "openai:acct-2";
    const OPENROUTER: &str = "openrouter:api";
    const OPENCODE_GO: &str = "opencode-go:key-0123456789abcdef";

    fn asked(taken: &Taken) -> Vec<&str> {
        taken.asks.iter().map(|(id, _)| id.as_str()).collect()
    }

    #[test]
    fn a_notification_clicked_before_the_first_status_opens_its_account_with_what_used_it() {
        // As `open_panel` leaves it when the click launched the app.
        let mut s = State::new(Preferences::default());
        s.panel_open = true;
        s.clicked = Some(CODEX.into());
        let taken = take(&mut s, example());
        assert_eq!(s.open.as_deref(), Some(CODEX));
        // Every account shown that has a deciding limit, the one clicked among them.
        assert_eq!(asked(&taken), [CLAUDE, CODEX, OPENROUTER]);
    }

    #[test]
    fn while_the_panel_is_open_a_status_asks_what_used_each_account_that_has_none() {
        let mut s = State::new(Preferences::default());
        take(&mut s, example());
        s.panel_open = true;
        s.open = Some(CLAUDE.into());
        for (id, limit) in asks(&s, |_| true) {
            s.used.insert(id, (limit, Vec::new()));
        }
        // Only the open account's again, as its sessions move with the status.
        assert_eq!(asked(&take(&mut s, example())), [CLAUDE]);
        // Another limit decides Codex now.
        let mut changed = example();
        changed.status.accounts[1].deciding = Some("primary".into());
        assert_eq!(asked(&take(&mut s, changed)), [CLAUDE, CODEX]);
        // Closed, the panel asks for none.
        s.panel_open = false;
        assert!(take(&mut s, example()).asks.is_empty());
    }

    #[test]
    fn a_sign_in_notification_goes_on_the_first_status_and_once_its_account_is_read_again() {
        let mut s = State::new(Preferences::default());
        // Each account that can be read, for a notification left from before the app started.
        let first = take(&mut s, example()).readable;
        assert_eq!(first, [CLAUDE, CODEX, OPENROUTER, "anthropic:api"]);
        // Then none, while no account is read again.
        assert!(take(&mut s, example()).readable.is_empty());
        let mut read = example();
        read.status.accounts[3].state = turnscope::status::State::Live {
            read_at: read.status.at,
        };
        assert_eq!(take(&mut s, read).readable, [OPENCODE_GO]);
    }
}
