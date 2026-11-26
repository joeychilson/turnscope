//! Shared app core for Turnscope: owns the in-process engine, state, wording,
//! view model generation, and the C ABI linked into native shells.

pub mod state;
pub mod view;
pub mod words;

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Once, OnceLock};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use turnscope::engine::{Engine, Event, Shown};

use crate::state::{Action, Call, Preferences, State};
use crate::view::{AppSync, PanelModel};
use crate::words::Words;

const PREFERENCES_FILE: &str = "preferences.json";

/// Resolves the bundled `Helpers/turnscope` (on macOS) or sibling binary next to the app
/// rather than searching `PATH`, because `connect` writes this path into agent configs.
pub fn binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let name = format!("turnscope{}", std::env::consts::EXE_SUFFIX);
    let Some(folder) = exe.parent() else {
        return PathBuf::from(name);
    };
    #[cfg(target_os = "macos")]
    if folder.file_name().is_some_and(|n| n == "MacOS") {
        return folder.with_file_name("Helpers").join(name);
    }
    folder.join(name)
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartConfig {
    pub fixture: Option<PathBuf>,
    pub data: Option<PathBuf>,
    pub home: Option<PathBuf>,
    #[serde(default)]
    pub connect_disabled_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotConfig {
    pub fixture: Option<PathBuf>,
    pub data: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum SnapshotOutput {
    Ok { panel: Box<PanelModel> },
    Err { error: String, code: i32 },
}

/// Callback invoked from engine threads when the shell should call `turnscope_app_sync`.
pub type WakeFn = extern "C" fn(*mut c_void);

type WakeCallback = Box<dyn Fn() + Send + Sync>;

enum WakeSlot {
    /// Engine started, waiting for `engine.set(started)` before exposing `wake`.
    Starting(bool),
    Ready(WakeCallback),
    Stopped,
}

struct RawWake {
    ctx: *mut c_void,
    cb: WakeFn,
}

impl RawWake {
    fn call(&self) {
        (self.cb)(self.ctx);
    }
}

// SAFETY: the host keeps `ctx` valid until `turnscope_app_stop` returns, and `cb` is safe
// to call from any thread (`turnscope_app.h`).
unsafe impl Send for RawWake {}
unsafe impl Sync for RawWake {}

/// Running app core. Dropping it clears the wake callback and stops the engine cleanly.
pub struct TurnscopeApp {
    core: Arc<Mutex<State>>,
    engine: Arc<OnceLock<Engine>>,
    wake: Arc<Mutex<WakeSlot>>,
    prefs_io: Mutex<()>,
    data: Option<PathBuf>,
}

impl TurnscopeApp {
    /// Start a live or fixture app core. `wake` may be invoked from engine threads while an
    /// internal engine lock is held; it should dispatch asynchronously to the host event loop.
    pub fn start(
        config: StartConfig,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<TurnscopeApp, String> {
        Self::start_inner(config, Some(Box::new(wake)))
    }

    fn start_inner(
        config: StartConfig,
        wake: Option<WakeCallback>,
    ) -> Result<TurnscopeApp, String> {
        turnscope::db::started();
        let (home, data) = turnscope::places(config.home, config.data);
        let runs_engine = config.fixture.is_none();
        let mut state = match config.fixture.as_deref() {
            Some(path) => State::fixed(read_fixture(path)?),
            None => {
                let mut state = State::default();
                state.preferences = load_preferences(&data);
                state
            }
        };
        state.connect_disabled_reason = config.connect_disabled_reason.filter(|s| !s.is_empty());

        let core = Arc::new(Mutex::new(state));
        let engine: Arc<OnceLock<Engine>> = Arc::new(OnceLock::new());
        let wake_slot = Arc::new(Mutex::new(WakeSlot::Starting(false)));

        if runs_engine {
            install_panic_hook(data.clone());
            let core_for_cb = Arc::clone(&core);
            let weak = Arc::downgrade(&engine);
            let wake_for_cb = Arc::clone(&wake_slot);
            let started = Engine::start(data.clone(), home, binary(), move |event| {
                let (calls, should_wake) = match event {
                    Event::UsedMost {
                        account,
                        limit,
                        sessions,
                    } => {
                        let resolved = state::resolve_used(sessions);
                        let wake = lock(&core_for_cb).took_used_most(account, limit, resolved);
                        (Vec::new(), wake)
                    }
                    other => lock(&core_for_cb).took(other, turnscope::now()),
                };
                if !calls.is_empty()
                    && let Some(held) = weak.upgrade()
                {
                    send(held.get(), calls);
                }
                if should_wake {
                    let mut slot = lock(&wake_for_cb);
                    match &mut *slot {
                        WakeSlot::Starting(missed) => *missed = true,
                        WakeSlot::Ready(cb) => cb(),
                        WakeSlot::Stopped => {}
                    }
                }
            });
            let _ = engine.set(started);
        }

        // Install `wake` only after `engine.set(started)` finishes so early events never
        // race with engine publication or call `wake` while startup is still in flight.
        if let Some(cb) = wake {
            let mut slot = lock(&wake_slot);
            let missed = matches!(*slot, WakeSlot::Starting(true));
            *slot = WakeSlot::Ready(cb);
            if missed && let WakeSlot::Ready(cb) = &*slot {
                cb();
            }
        } else {
            *lock(&wake_slot) = WakeSlot::Stopped;
        }

        Ok(TurnscopeApp {
            core,
            engine,
            wake: wake_slot,
            prefs_io: Mutex::new(()),
            data: runs_engine.then_some(data),
        })
    }

    /// Drain pending side effects and build the current `AppSync`.
    pub fn sync(&self) -> AppSync {
        lock(&self.core).sync(turnscope::now())
    }

    /// Apply a user action, persist changed preferences, dispatch engine calls, and return
    /// the updated `AppSync`.
    pub fn apply(&self, action: Action) -> AppSync {
        let now = turnscope::now();
        let (calls, prefs_dirty, sync) = {
            let mut core = lock(&self.core);
            let calls = core.apply(action);
            let prefs_dirty = core.take_preferences_to_save().is_some();
            let sync = core.sync(now);
            (calls, prefs_dirty, sync)
        };
        if prefs_dirty && let Some(data) = &self.data {
            let _io = lock(&self.prefs_io);
            let prefs = lock(&self.core).preferences.clone();
            save_preferences(data, &prefs);
        }
        send(self.engine.get(), calls);
        sync
    }

    /// Append a line to `app.log` in the active data directory (no-op in fixture mode).
    pub fn log(&self, line: &str) {
        if let Some(data) = &self.data {
            turnscope::engine::log(data, line);
        }
    }

    /// Path to `app.log` (or the data directory if no log exists yet), or `None` in fixture mode.
    pub fn log_path(&self) -> Option<PathBuf> {
        let data = self.data.as_ref()?;
        let log = data.join("app.log");
        Some(if log.exists() { log } else { data.clone() })
    }
}

impl Drop for TurnscopeApp {
    fn drop(&mut self) {
        // Clear under lock before `self.engine` drops so any in-flight `wake` completes
        // before `Drop` returns and no later engine event can invoke it.
        *lock(&self.wake) = WakeSlot::Stopped;
    }
}

static PANIC_HOOK: Once = Once::new();
static PANIC_LOG_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

fn install_panic_hook(data: PathBuf) {
    *lock(&PANIC_LOG_DIR) = Some(data);
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Some(dir) = lock(&PANIC_LOG_DIR).clone() {
                turnscope::engine::log(&dir, &format!("panicked: {info}"));
            }
            previous(info);
        }));
    });
}

fn load_preferences(data: &Path) -> Preferences {
    let path = data.join(PREFERENCES_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Preferences::default(),
        Err(err) => {
            turnscope::engine::log(data, &format!("reading {PREFERENCES_FILE}: {err}"));
            return Preferences::default();
        }
    };
    serde_json::from_str(&text).unwrap_or_else(|err| {
        turnscope::engine::log(data, &format!("invalid {PREFERENCES_FILE}: {err}"));
        Preferences::default()
    })
}

fn save_preferences(data: &Path, prefs: &Preferences) {
    if let Err(err) = std::fs::create_dir_all(data) {
        turnscope::engine::log(data, &format!("creating data dir for preferences: {err}"));
        return;
    }
    let json = match serde_json::to_string_pretty(prefs) {
        Ok(json) => json,
        Err(err) => {
            turnscope::engine::log(data, &format!("serializing preferences: {err}"));
            return;
        }
    };
    let tmp = data.join(format!("{PREFERENCES_FILE}.{}.tmp", std::process::id()));
    let dest = data.join(PREFERENCES_FILE);
    if let Err(err) = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &dest)) {
        let _ = std::fs::remove_file(&tmp);
        turnscope::engine::log(data, &format!("saving {PREFERENCES_FILE}: {err}"));
    }
}

fn read_fixture(path: &Path) -> Result<Shown, String> {
    std::fs::read_to_string(path)
        .and_then(|text| Ok(serde_json::from_str(&text)?))
        .map_err(|error| format!("Turnscope: {} can't be read: {error}", path.display()))
}

enum CStrError {
    Null,
    Utf8(std::str::Utf8Error),
}

/// # Safety
/// `ptr` must be null or a valid NUL-terminated C string.
unsafe fn read_c_str<'a>(ptr: *const c_char) -> Result<&'a str, CStrError> {
    if ptr.is_null() {
        return Err(CStrError::Null);
    }
    // SAFETY: Caller passes a valid NUL-terminated C string.
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map_err(CStrError::Utf8)
}

/// # Safety
/// `json` must be null or a valid NUL-terminated C string.
unsafe fn read_config<T: DeserializeOwned>(json: *const c_char) -> Result<T, String> {
    let text = match unsafe { read_c_str(json) } {
        Ok(text) => text,
        Err(CStrError::Null) => return Err("Turnscope: no config".to_owned()),
        Err(CStrError::Utf8(error)) => {
            return Err(format!("Turnscope: invalid UTF-8 in config: {error}"));
        }
    };
    serde_json::from_str(text).map_err(|error| format!("Turnscope: invalid config: {error}"))
}

fn send(engine: Option<&Engine>, calls: Vec<Call>) {
    let Some(engine) = engine else { return };
    for call in calls {
        match call {
            Call::UsedMost { account, limit } => engine.used_most(account, limit),
            Call::AskLimits => engine.ask_limits(),
            Call::RefreshNow => engine.refresh_now(),
            Call::Hide { account, hidden } => engine.hide(account, hidden),
            Call::Connect { agent, connecting } => engine.connect(agent, connecting),
            Call::SaveSettings(settings) => engine.save_settings(settings),
            Call::Acknowledge(id) => engine.acknowledge(id),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn into_c_string(text: String) -> *mut c_char {
    CString::new(text)
        .or_else(|err| {
            let mut bytes = err.into_vec();
            bytes.retain(|&b| b != 0);
            CString::new(bytes)
        })
        .map_or(std::ptr::null_mut(), CString::into_raw)
}

fn serialize_c<T: Serialize>(value: &T) -> *mut c_char {
    match serde_json::to_string(value) {
        Ok(json) => into_c_string(json),
        Err(_) => std::ptr::null_mut(),
    }
}

fn catch_ptr<T>(f: impl FnOnce() -> *mut T) -> *mut T {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(std::ptr::null_mut())
}

fn catch_void(f: impl FnOnce()) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
}

/// Build a plain-text diagnostics report (version, `turnscope::doctor`'s notes and the last
/// 20 lines of `app.log`) ready to copy into a bug report.
pub fn diagnostics(config: SnapshotConfig) -> String {
    let mut out = format!("Turnscope {}\n", env!("CARGO_PKG_VERSION"));
    if config.fixture.is_some() {
        out += "Mode: fixture\n";
        return out;
    }
    let (home, data) = turnscope::places(config.home, config.data);
    out += &format!("Binary: {}\n", binary().display());
    match turnscope::db::read(&data) {
        Some(db) => match turnscope::doctor(&db, &home, turnscope::now()) {
            Ok(notes) if notes.is_empty() => out += "Doctor: Nothing to fix.\n",
            Ok(notes) => {
                out += "Doctor:\n";
                for note in notes {
                    out += &format!("- {note}\n");
                }
            }
            Err(error) => out += &format!("Doctor failed: {error}\n"),
        },
        None => out += "Doctor: database not readable yet.\n",
    }
    if let Ok(log) = std::fs::read_to_string(data.join("app.log")) {
        let lines: Vec<&str> = log.lines().collect();
        if !lines.is_empty() {
            let start = lines.len().saturating_sub(20);
            out += "\nRecent app.log:\n";
            for line in &lines[start..] {
                out += line;
                out.push('\n');
            }
        }
    }
    out
}

/// Build a one-shot `PanelModel` for `--snapshot`.
pub fn snapshot(config: SnapshotConfig) -> Result<PanelModel, (String, i32)> {
    turnscope::db::started();
    let (home, data) = turnscope::places(config.home, config.data);
    let shown = match config.fixture.as_deref() {
        Some(path) => read_fixture(path).map_err(|err| (err, 2))?,
        None => turnscope::engine::shown(&data, &home, &binary()).map_err(|error| {
            (
                format!("Turnscope: {} can't be read: {error}", data.display()),
                1,
            )
        })?,
    };
    let at = shown.status.at;
    let state = State::fixed(shown);
    Ok(view::build_panel(&state, &Words::new(at)))
}

/// Build a one-shot panel snapshot JSON for `--snapshot`.
///
/// # Safety
/// `config_json` must be null or a valid NUL-terminated UTF-8 C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_snapshot(config_json: *const c_char) -> *mut c_char {
    catch_ptr(|| {
        let panel = unsafe { read_config(config_json) }
            .map_err(|error| (error, 2))
            .and_then(snapshot);
        let output = match panel {
            Ok(panel) => SnapshotOutput::Ok {
                panel: Box::new(panel),
            },
            Err((error, code)) => SnapshotOutput::Err { error, code },
        };
        serialize_c(&output)
    })
}

/// Start a live or fixture app core.
///
/// # Safety
/// `config_json` must be null or a valid NUL-terminated C string; `error_out` must be null or writable;
/// `ctx` must remain valid until `turnscope_app_stop` returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_start(
    config_json: *const c_char,
    ctx: *mut c_void,
    wake: Option<WakeFn>,
    error_out: *mut *mut c_char,
) -> *mut TurnscopeApp {
    if !error_out.is_null() {
        // SAFETY: Caller owns `error_out`.
        unsafe { *error_out = std::ptr::null_mut() };
    }
    catch_ptr(|| {
        let wake_cb: Option<WakeCallback> = wake.map(|cb| {
            let raw = RawWake { ctx, cb };
            Box::new(move || raw.call()) as WakeCallback
        });
        match unsafe { read_config(config_json) }
            .and_then(|config| TurnscopeApp::start_inner(config, wake_cb))
        {
            Ok(app) => Box::into_raw(Box::new(app)),
            Err(err) => {
                if !error_out.is_null() {
                    // SAFETY: Caller owns `error_out`.
                    unsafe { *error_out = into_c_string(err) };
                }
                std::ptr::null_mut()
            }
        }
    })
}

/// Clear `wake`, stop the engine, and free `app`.
///
/// # Safety
/// `app` must be null or a live pointer returned by `turnscope_app_start` and not yet stopped.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_stop(app: *mut TurnscopeApp) {
    if app.is_null() {
        return;
    }
    catch_void(|| {
        // SAFETY: `app` was allocated by `Box::into_raw` in `turnscope_app_start`.
        let _ = unsafe { Box::from_raw(app) };
    });
}

/// Drain pending side effects and return the current `AppSync` JSON.
///
/// # Safety
/// `app` must be null or a live pointer returned by `turnscope_app_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_sync(app: *mut TurnscopeApp) -> *mut c_char {
    catch_ptr(|| {
        // SAFETY: Valid pointer from `turnscope_app_start`.
        let Some(app) = (unsafe { app.as_ref() }) else {
            return std::ptr::null_mut();
        };
        serialize_c(&app.sync())
    })
}

/// Apply a JSON `Action` and return the updated `AppSync` JSON.
///
/// # Safety
/// `app` must be null or a live pointer returned by `turnscope_app_start`; `action_json` must be null or a valid NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_action(
    app: *mut TurnscopeApp,
    action_json: *const c_char,
) -> *mut c_char {
    catch_ptr(|| {
        // SAFETY: Valid pointer from `turnscope_app_start`.
        let Some(app) = (unsafe { app.as_ref() }) else {
            return std::ptr::null_mut();
        };
        let text = match unsafe { read_c_str(action_json) } {
            Ok(text) => text,
            Err(CStrError::Null) => return std::ptr::null_mut(),
            Err(CStrError::Utf8(error)) => {
                app.log(&format!("invalid UTF-8 in action JSON: {error}"));
                return std::ptr::null_mut();
            }
        };
        let action = match serde_json::from_str::<Action>(text) {
            Ok(action) => action,
            Err(error) => {
                app.log(&format!("invalid action JSON: {error}"));
                return std::ptr::null_mut();
            }
        };
        serialize_c(&app.apply(action))
    })
}

/// Append a line to `app.log` in the active data directory.
///
/// # Safety
/// `app` must be null or a live pointer from `turnscope_app_start`; `line` must be null or a valid NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_log(app: *mut TurnscopeApp, line: *const c_char) {
    catch_void(|| {
        // SAFETY: Valid pointer from `turnscope_app_start`.
        let Some(app) = (unsafe { app.as_ref() }) else {
            return;
        };
        if let Ok(line) = unsafe { read_c_str(line) } {
            app.log(line);
        }
    });
}

/// Build a plain-text diagnostics report from a JSON `SnapshotConfig`.
/// Safe to invoke from a background thread (never touches `TurnscopeApp`).
///
/// # Safety
/// `config_json` must be null or a valid NUL-terminated UTF-8 C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_diagnostics(config_json: *const c_char) -> *mut c_char {
    catch_ptr(|| {
        into_c_string(unsafe { read_config(config_json) }.map_or_else(|error| error, diagnostics))
    })
}

/// Return the path to `app.log` (or the data directory if no log exists yet),
/// or NULL in fixture mode.
///
/// # Safety
/// `app` must be null or a live pointer returned by `turnscope_app_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_log_path(app: *mut TurnscopeApp) -> *mut c_char {
    catch_ptr(|| {
        // SAFETY: Valid pointer from `turnscope_app_start`.
        let Some(app) = (unsafe { app.as_ref() }) else {
            return std::ptr::null_mut();
        };
        app.log_path().map_or(std::ptr::null_mut(), |target| {
            into_c_string(target.to_string_lossy().into_owned())
        })
    })
}

/// Free a string returned by `turnscope_app_*`.
///
/// # Safety
/// `text` must be null or a pointer returned by a `turnscope_app_*` function and not previously freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_free_string(text: *mut c_char) {
    if !text.is_null() {
        // SAFETY: Frees string allocated by `into_c_string`.
        let _ = unsafe { CString::from_raw(text) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::MenuShows;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// A string the core returned, freed.
    unsafe fn taken(ptr: *mut c_char) -> String {
        let text = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
        unsafe { turnscope_app_free_string(ptr) };
        text
    }

    #[test]
    fn ffi_round_trip_with_fixture_builds_panel_and_handles_actions() {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../turnscope/tests/fixtures/status.json");
        let cfg = serde_json::json!({ "fixture": fixture }).to_string();
        let cfg_c = CString::new(cfg).unwrap();
        // Sentinel confirms `turnscope_app_start` clears `*error_out` on success.
        let mut err: *mut c_char = std::ptr::dangling_mut();
        let app =
            unsafe { turnscope_app_start(cfg_c.as_ptr(), std::ptr::null_mut(), None, &mut err) };
        assert!(!app.is_null());
        assert!(err.is_null());

        let sync_json = unsafe { taken(turnscope_app_sync(app)) };
        let sync: serde_json::Value = serde_json::from_str(&sync_json).unwrap();
        assert_eq!(sync["hasStatus"], true);
        assert!(sync["panel"].is_null());
        assert_eq!(sync["menuBar"]["slots"].as_array().unwrap().len(), 1);
        // As of the fixture's time too, not the clock's.
        assert_eq!(sync["menuBar"]["slots"][0]["figure"], "50m");

        let open_action = CString::new(r#"{"panelOpened":{}}"#).unwrap();
        let opened_json = unsafe { taken(turnscope_app_action(app, open_action.as_ptr())) };
        let opened: serde_json::Value = serde_json::from_str(&opened_json).unwrap();
        assert_eq!(
            opened["panel"]["verdict"]["headline"],
            "Claude Max runs out in\u{a0}50m"
        );
        assert_eq!(opened["panel"]["verdict"]["standing"], "runningOut");

        let hide_action =
            CString::new(r#"{"setHidden":{"id":"anthropic:acct-1:org-1","hidden":true}}"#).unwrap();
        let after_json = unsafe { taken(turnscope_app_action(app, hide_action.as_ptr())) };
        let after: serde_json::Value = serde_json::from_str(&after_json).unwrap();
        assert_eq!(after["panel"]["inUse"].as_array().unwrap().len(), 0);
        assert_eq!(after["panel"]["hidden"].as_array().unwrap().len(), 2);

        unsafe { turnscope_app_stop(app) };
    }

    #[test]
    fn live_core_wakes_shell_and_stops_cleanly() {
        extern "C" fn count_wake(ctx: *mut c_void) {
            let counter = unsafe { &*(ctx as *const AtomicUsize) };
            counter.fetch_add(1, Ordering::Release);
        }

        let wakes = AtomicUsize::new(0);
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        // Mark prices and limits current so the test doesn't fetch models.dev over the network.
        let db = turnscope::db::open(data.path()).unwrap();
        db.execute(
            "UPDATE state SET prices_at = ?1, limits_at = ?1",
            [turnscope::now()],
        )
        .unwrap();
        drop(db);
        let cfg = serde_json::json!({
            "data": data.path(),
            "home": home.path(),
        })
        .to_string();
        let cfg_c = CString::new(cfg).unwrap();
        let mut err: *mut c_char = std::ptr::null_mut();
        let app = unsafe {
            turnscope_app_start(
                cfg_c.as_ptr(),
                (&wakes as *const AtomicUsize) as *mut c_void,
                Some(count_wake),
                &mut err,
            )
        };
        assert!(!app.is_null());
        let deadline = Instant::now() + Duration::from_secs(5);
        while wakes.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(wakes.load(Ordering::Acquire) > 0);

        let open_action = CString::new(r#"{"panelOpened":{}}"#).unwrap();
        let sync_json = unsafe { taken(turnscope_app_action(app, open_action.as_ptr())) };
        let sync: serde_json::Value = serde_json::from_str(&sync_json).unwrap();
        assert!(sync["panel"]["verdict"].is_object());

        // Invalid UTF-8 in action JSON returns NULL and logs without panicking.
        let bad_utf8: [c_char; 2] = [-1i8 as c_char, 0];
        assert!(unsafe { turnscope_app_action(app, bad_utf8.as_ptr()) }.is_null());
        let log = std::fs::read_to_string(data.path().join("app.log")).unwrap_or_default();
        assert!(log.contains("invalid UTF-8"), "{log}");

        // Stopping before `wakes` leaves scope ensures no callback fires after teardown.
        unsafe { turnscope_app_stop(app) };
    }

    #[test]
    fn preferences_persist_in_data_dir_and_not_in_fixture_mode() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let db = turnscope::db::open(data.path()).unwrap();
        db.execute(
            "UPDATE state SET prices_at = ?1, limits_at = ?1",
            [turnscope::now()],
        )
        .unwrap();
        drop(db);

        {
            let app = TurnscopeApp::start(
                StartConfig {
                    data: Some(data.path().into()),
                    home: Some(home.path().into()),
                    ..StartConfig::default()
                },
                || {},
            )
            .unwrap();
            app.apply(Action::SetMenuShows {
                shows: MenuShows::One,
            });
            app.apply(Action::DismissConnect { id: "codex".into() });
        }

        let reopened = TurnscopeApp::start(
            StartConfig {
                data: Some(data.path().into()),
                home: Some(home.path().into()),
                ..StartConfig::default()
            },
            || {},
        )
        .unwrap();
        let sync = reopened.apply(Action::PanelOpened {});
        assert_eq!(sync.panel.unwrap().settings.menu_shows, MenuShows::One);
        drop(reopened);

        // Fixture mode never writes preferences.json.
        let scratch = tempfile::tempdir().unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../turnscope/tests/fixtures/status.json");
        let app = TurnscopeApp::start(
            StartConfig {
                fixture: Some(fixture),
                data: Some(scratch.path().into()),
                ..StartConfig::default()
            },
            || {},
        )
        .unwrap();
        app.apply(Action::SetMenuShows {
            shows: MenuShows::One,
        });
        assert!(!scratch.path().join(PREFERENCES_FILE).exists());
    }

    #[test]
    fn concurrent_preference_writes_serialize_without_errors() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let db = turnscope::db::open(data.path()).unwrap();
        db.execute(
            "UPDATE state SET prices_at = ?1, limits_at = ?1",
            [turnscope::now()],
        )
        .unwrap();
        drop(db);

        let app = Arc::new(
            TurnscopeApp::start(
                StartConfig {
                    data: Some(data.path().into()),
                    home: Some(home.path().into()),
                    ..StartConfig::default()
                },
                || {},
            )
            .unwrap(),
        );
        std::thread::scope(|scope| {
            for t in 0..4 {
                let app = Arc::clone(&app);
                scope.spawn(move || {
                    for i in 0..20 {
                        app.apply(Action::DismissConnect {
                            id: format!("agent-{t}-{i}"),
                        });
                    }
                });
            }
        });
        app.apply(Action::SetMenuShows {
            shows: MenuShows::One,
        });
        drop(app);

        let log = std::fs::read_to_string(data.path().join("app.log")).unwrap_or_default();
        assert!(
            !log.contains(PREFERENCES_FILE),
            "unexpected preferences write error in app.log: {log}"
        );
        let saved = load_preferences(data.path());
        assert_eq!(saved.menu_shows, MenuShows::One);
        assert_eq!(saved.dismissed_connect.len(), 80);
    }

    /// `scripts/screenshots.sh` draws the panel this way.
    #[test]
    fn a_snapshot_is_drawn_as_of_its_fixture() {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../turnscope/tests/fixtures/status.json");
        let panel = snapshot(SnapshotConfig {
            fixture: Some(fixture),
            data: None,
            home: None,
        })
        .unwrap();
        assert_eq!(
            panel.verdict.unwrap().headline,
            "Claude Max runs out in\u{a0}50m"
        );
    }

    /// Each of the last lines logged is said once, whether or not doctor can run.
    #[test]
    fn diagnostics_says_the_version_and_each_line_logged_once() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        // One more than doctor's five, each said once.
        for n in 1..=6 {
            turnscope::engine::log(data.path(), &format!("sample line {n}."));
        }
        let logged = |report: &str| {
            (1..=6)
                .map(|n| report.matches(&format!("sample line {n}.")).count())
                .collect::<Vec<_>>()
        };
        let unread = diagnostics(SnapshotConfig {
            fixture: None,
            data: Some(data.path().into()),
            home: Some(home.path().into()),
        });
        assert!(
            unread.contains("Doctor: database not readable yet."),
            "{unread}"
        );
        assert_eq!(logged(&unread), [1; 6], "{unread}");

        drop(turnscope::db::open(data.path()).unwrap());
        let cfg = serde_json::json!({ "data": data.path(), "home": home.path() }).to_string();
        let cfg_c = CString::new(cfg).unwrap();
        let ptr = unsafe { turnscope_app_diagnostics(cfg_c.as_ptr()) };
        let report = unsafe { taken(ptr) };
        assert!(report.contains(&format!("Turnscope {}", env!("CARGO_PKG_VERSION"))));
        assert_eq!(logged(&report), [1; 6], "{report}");
        let fixture = SnapshotConfig {
            fixture: Some("status.json".into()),
            data: None,
            home: None,
        };
        assert!(diagnostics(fixture).contains("Mode: fixture"));
    }
}
