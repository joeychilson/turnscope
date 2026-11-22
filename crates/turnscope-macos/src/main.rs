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
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2_app_kit::{
    NSPopUpMenuWindowLevel, NSWindowCollectionBehavior, NSWorkspace, NSWorkspaceDidWakeNotification,
};
use objc2_foundation::NSNotification;
use rune::{App, AppHandle, AppSpec, DisplayTarget, RunOptions, Tx, WindowOptions, WindowSpec};
use turnscope::engine::Shown;
use turnscope::time::{MINUTE, minute};

use launch::{Launch, Opening};
use preferences::Preferences;
use state::{Page, State};
use style::PANEL_WIDTH;

pub static HANDLE: OnceLock<AppHandle<State>> = OnceLock::new();
static STARTED: OnceLock<Instant> = OnceLock::new();
/// Whether the ticker wakes each minute: a closed panel with nothing urgent never wakes the app.
static COUNTING: AtomicBool = AtomicBool::new(false);
static TICKER: OnceLock<std::thread::Thread> = OnceLock::new();

thread_local! {
    /// Focus loss closes the panel before the status item click fires; ignore
    /// immediate clicks so clicking the item closes instead of reopening.
    static CLOSED: Cell<Option<Instant>> = const { Cell::new(None) };
}

pub fn drew(s: &State) {
    menu_bar::draw(s);
    let counting = s.panel_open || s.urgent();
    COUNTING.store(counting, Ordering::Release);
    if counting && let Some(ticker) = TICKER.get() {
        ticker.unpark();
    }
}

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

/// Runs `f` once macOS has placed the status item, or 2 s after launch: until then its window
/// is off-screen, and it was placed 1.55 to 1.65 s after launch in a debug build (measured
/// 2026-11-18). Counted from each call instead, a notification click waited 2 s every time
/// while the item was hidden in the menu bar's settings.
fn when_placed(f: impl FnOnce(&mut State) + Send + 'static) {
    let Some(handle) = HANDLE.get() else { return };
    handle.update_if(move |s, _| {
        if menu_bar::placed() || waited() {
            f(s);
            return true;
        }
        // Only the main thread can see the item's place: asked there again in 50 ms.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            when_placed(f);
        });
        false
    });
}

pub fn open_with(opening: Opening) {
    when_placed(move |s| {
        show_panel(s);
        apply(s, opening);
    });
}

fn waited() -> bool {
    STARTED
        .get()
        .is_none_or(|at| at.elapsed() >= Duration::from_secs(2))
}

pub fn toggle_panel() {
    let Some(handle) = HANDLE.get() else { return };
    if handle.is_visible() {
        // Close via `close_window` so `on_close` runs; hiding skips it and leaves `panel_open` true.
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

pub fn open_panel(account: Option<String>) {
    when_placed(move |s| {
        show_panel(s);
        let Some(account) = account else { return };
        if s.status.is_none() {
            s.clicked = Some(account);
        } else {
            s.open_in_place(&account);
        }
    });
}

fn show_panel(s: &mut State) {
    let Some(handle) = HANDLE.get() else { return };
    if s.panel_open {
        handle.focus();
        return;
    }
    s.reset();
    s.tallest = Some(settings::tallest());
    s.panel_open = true;
    s.now = minute(turnscope::now());
    if engine::running() {
        // Pre-fetch so expanding a row doesn't jump a second time up to 250 ms later.
        for (account, limit) in engine::asks(s, |_| true) {
            engine::ask(|e| e.used_most(account, limit));
        }
        notifier::check();
        login::check();
        engine::ask(|e| e.ask_limits());
        s.checks_automatically = updater::automatic();
    }
    if let Some(place) = menu_bar::panel_place() {
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

fn poll(s: &mut State, clock: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    if s.grow_due.is_some_and(|due| clock >= due) {
        // Animated, or Rune would show the bars at their level at once.
        tx.animated();
        s.grow_due = None;
        changed = true;
    }
    if s.closed.as_ref().is_some_and(|(_, due)| clock >= *due) {
        s.closed = None;
        changed = true;
    }
    changed
}

fn tick() {
    let ticker = std::thread::spawn(|| {
        loop {
            while !COUNTING.load(Ordering::Acquire) {
                std::thread::park();
            }
            let now = turnscope::now();
            let next = minute(now) + MINUTE;
            std::thread::sleep(Duration::from_millis((next - now).max(0) as u64 + 50));
            if !COUNTING.load(Ordering::Acquire) {
                continue;
            }
            if let Some(handle) = HANDLE.get() {
                handle.update_if(|s, _| ticked(s));
            }
        }
    });
    let _ = TICKER.set(ticker.thread().clone());
}

/// Sleep timers pause during macOS sleep, so refresh immediately on wake.
fn tick_on_wake() {
    let woke = RcBlock::new(|_: NonNull<NSNotification>| {
        if let Some(handle) = HANDLE.get() {
            handle.update_if(|s, _| ticked(s));
        }
    });
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(NSWorkspaceDidWakeNotification),
            None,
            None,
            &woke,
        );
    }
}

fn ticked(s: &mut State) -> bool {
    drew(s);
    if s.panel_open {
        s.now = minute(turnscope::now());
    }
    s.panel_open
}

/// Sets what `field` picks to `value`, from any thread, and redraws the open panel if it
/// changed.
pub fn set<T: PartialEq + Send + 'static>(field: fn(&mut State) -> &mut T, value: T) {
    if let Some(handle) = HANDLE.get() {
        handle.update_if(move |s, _| {
            let held = field(s);
            let changed = *held != value;
            *held = value;
            changed && s.panel_open
        });
    }
}

/// True when running from a mounted DMG or App Translocation path, whose
/// binary path vanishes after eject or quit.
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

fn spec() -> AppSpec<State> {
    AppSpec::<State> {
        families: ("System Font", "ui-monospace"),
        fonts: style::fonts(),
        poll: Some(poll),
        on_key: Some(panel::on_key),
        ..AppSpec::new(
            "Turnscope",
            (PANEL_WIDTH + 2.0, 420.0),
            State::default,
            panel::view,
        )
    }
}

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

/// What the engine reads in `data` now, without raising alerts or catching up.
fn read(home: &Path, data: &Path) -> Shown {
    match turnscope::engine::shown(data, home, &engine::binary()) {
        Ok(shown) => shown,
        Err(error) => {
            eprintln!("Turnscope: {} can't be read: {error}", data.display());
            std::process::exit(1);
        }
    }
}

/// Draws the panel as `panel-light.png` and `panel-dark.png` in `folder` and quits, for
/// `scripts/screenshots.sh` and `scripts/check-app.sh`.
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
        let mut s = State::fixed(shown.clone());
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
    let runs_engine = fixed.is_none();
    if runs_engine {
        let _ = engine::DATA.set(data.clone());
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
                // Changes made on a fixed status go nowhere: its preferences aren't kept.
                Some(shown) => State::fixed(shown),
                None => State {
                    preferences: Preferences::read(),
                    ..State::default()
                },
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
            open_with(opening);
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
