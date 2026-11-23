//! Shared desktop app core for Turnscope: owns the in-process engine, state, wording,
//! view model generation, and a small C ABI for native OS shells (`apps/macos`, etc.).

pub mod launch;
pub mod standing;
pub mod state;
pub mod view;
pub mod words;

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use turnscope::engine::{Engine, Shown};

use crate::launch::{Launch, Opening};
use crate::state::{Action, Core, MenuShows, Preferences, State};
use crate::view::PanelModel;

/// Resolves the bundled `Helpers/turnscope` (or sibling binary under `target/`)
/// rather than searching `PATH`, because `connect` writes this path into agent configs.
pub fn binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let folder = exe.parent().unwrap_or(Path::new("/"));
    if folder.file_name().is_some_and(|name| name == "MacOS") {
        folder.with_file_name("Helpers").join("turnscope")
    } else {
        folder.join("turnscope")
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartConfig {
    pub fixture: Option<PathBuf>,
    pub opening: Option<Opening>,
    pub data: Option<PathBuf>,
    pub home: Option<PathBuf>,
    #[serde(default)]
    pub from_disk_image: bool,
    #[serde(default)]
    pub menu_shows: MenuShows,
    #[serde(default)]
    pub dismissed_connect: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotConfig {
    pub fixture: Option<PathBuf>,
    pub opening: Option<Opening>,
    pub data: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum SnapshotOutput {
    Ok { ok: Box<PanelModel> },
    Err { error: String, code: i32 },
}

/// Callback invoked from engine threads when the shell should call `turnscope_app_sync`.
pub type WakeFn = extern "C" fn(*mut c_void);

#[derive(Clone, Copy)]
struct Wake {
    ctx: *mut c_void,
    cb: WakeFn,
}

// SAFETY: `ctx` is an opaque host pointer kept valid until `turnscope_app_stop` returns;
// `TurnscopeApp::drop` clears the mutex-guarded `Wake` so no callback runs after stop.
unsafe impl Send for Wake {}
unsafe impl Sync for Wake {}

impl Wake {
    fn trigger(self) {
        (self.cb)(self.ctx);
    }
}

/// Opaque handle exposed over C ABI.
pub struct TurnscopeApp {
    core: Arc<Mutex<Core>>,
    wake: Arc<Mutex<Option<Wake>>>,
    engine: Option<Arc<OnceLock<Engine>>>,
    data: Option<PathBuf>,
    home: Option<PathBuf>,
}

impl Drop for TurnscopeApp {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.wake.lock() {
            *guard = None;
        }
        self.engine = None;
    }
}

static PANIC_DATA: OnceLock<PathBuf> = OnceLock::new();

fn install_panic_hook(data: PathBuf) {
    if PANIC_DATA.set(data).is_ok() {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Some(dir) = PANIC_DATA.get() {
                turnscope::engine::log(dir, &format!("panicked: {info}"));
            }
            previous(info);
        }));
    }
}

fn read_fixture(path: &Path) -> Result<Shown, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("Turnscope: {} can't be read: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("Turnscope: {} can't be read: {error}", path.display()))
}

fn lock_core(core: &Mutex<Core>) -> std::sync::MutexGuard<'_, Core> {
    core.lock()
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

unsafe fn read_c_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: Caller passes a valid NUL-terminated C string.
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

/// Start the app core from a parsed `StartConfig`.
pub fn start(
    config: StartConfig,
    wake: Option<WakeFn>,
    ctx: *mut c_void,
) -> Result<TurnscopeApp, String> {
    turnscope::db::started();
    let (home, data) = turnscope::places(config.home, config.data);
    let fixed = match config.fixture.as_deref() {
        Some(path) => Some(read_fixture(path)?),
        None => None,
    };
    let runs_engine = fixed.is_none();
    let mut state = match fixed {
        Some(shown) => State::fixed(shown),
        None => State {
            preferences: Preferences {
                menu_shows: config.menu_shows,
                dismissed_connect: config.dismissed_connect.into_iter().collect(),
                kept: true,
            },
            ..State::default()
        },
    };
    state.from_disk_image = config.from_disk_image;
    state.opening = config.opening;

    let core = Arc::new(Mutex::new(Core::new(state, runs_engine)));
    let wake = Arc::new(Mutex::new(wake.map(|cb| Wake { ctx, cb })));

    let engine = if runs_engine {
        install_panic_hook(data.clone());
        let core_for_cb = Arc::clone(&core);
        let wake_for_cb = Arc::clone(&wake);
        let cell: Arc<OnceLock<Engine>> = Arc::new(OnceLock::new());
        let weak = Arc::downgrade(&cell);
        let started = Engine::start(data.clone(), home.clone(), binary(), move |event| {
            let (calls, should_wake) = lock_core(&core_for_cb).took(event);
            if let Some(held) = weak.upgrade() {
                calls.send(held.get());
            }
            if should_wake
                && let Ok(guard) = wake_for_cb.lock()
                && let Some(wake) = *guard
            {
                wake.trigger();
            }
        });
        let _ = cell.set(started);
        Some(cell)
    } else {
        None
    };

    Ok(TurnscopeApp {
        core,
        wake,
        engine,
        data: runs_engine.then_some(data),
        home: runs_engine.then_some(home),
    })
}

/// Build a plain-text diagnostics report (version, `turnscope::doctor`, and
/// the tail of `app.log`) ready to copy into a bug report.
pub fn diagnostics(data: Option<&Path>, home: Option<&Path>) -> String {
    let mut out = format!("Turnscope {}\n", env!("CARGO_PKG_VERSION"));
    let (Some(data), Some(home)) = (data, home) else {
        out += "Mode: fixture\n";
        return out;
    };
    out += &format!("Binary: {}\n", binary().display());
    match turnscope::db::read(data) {
        Some(db) => match turnscope::doctor(&db, data, home, turnscope::now()) {
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
    let mut state = State::fixed(shown);
    if let Some(opening) = config.opening {
        state.apply_opening(opening);
    }
    Ok(view::build_panel(&state))
}

/// Parse process arguments into a JSON `Launch` object.
///
/// # Safety
/// `argv` must be either null (when `argc <= 0`) or point to `argc` valid NUL-terminated UTF-8 C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_parse_launch(
    argc: i32,
    argv: *const *const c_char,
) -> *mut c_char {
    let mut args = Vec::new();
    if argc > 0 && !argv.is_null() {
        // SAFETY: Caller passes standard `argc`/`argv`.
        let slice = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
        for &ptr in slice {
            if let Some(arg) = unsafe { read_c_str(ptr) } {
                args.push(arg.to_owned());
            }
        }
    }
    serialize_c(&Launch::parse(&args))
}

/// Build a one-shot panel snapshot JSON for `--snapshot`.
///
/// # Safety
/// `config_json` must be null or a valid NUL-terminated UTF-8 C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_snapshot(config_json: *const c_char) -> *mut c_char {
    let config = unsafe { read_c_str(config_json) }
        .and_then(|s| serde_json::from_str::<SnapshotConfig>(s).ok())
        .unwrap_or_default();
    let output = match snapshot(config) {
        Ok(ok) => SnapshotOutput::Ok { ok: Box::new(ok) },
        Err((error, code)) => SnapshotOutput::Err { error, code },
    };
    serialize_c(&output)
}

/// Start a live or fixture app core.
///
/// # Safety
/// `config_json` must be null or a valid NUL-terminated C string; `error_out` must be null or writable;
/// `ctx` must remain valid for `wake` calls until `turnscope_app_stop` returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_start(
    config_json: *const c_char,
    ctx: *mut c_void,
    wake: Option<WakeFn>,
    error_out: *mut *mut c_char,
) -> *mut TurnscopeApp {
    let config = match unsafe { read_c_str(config_json) } {
        Some(s) => match serde_json::from_str::<StartConfig>(s) {
            Ok(config) => config,
            Err(err) => {
                if !error_out.is_null() {
                    // SAFETY: Caller owns `error_out`.
                    unsafe {
                        *error_out = into_c_string(format!("Turnscope: invalid config: {err}"))
                    };
                }
                return std::ptr::null_mut();
            }
        },
        None => StartConfig::default(),
    };
    match start(config, wake, ctx) {
        Ok(app) => Box::into_raw(Box::new(app)),
        Err(err) => {
            if !error_out.is_null() {
                // SAFETY: Caller owns `error_out`.
                unsafe { *error_out = into_c_string(err) };
            }
            std::ptr::null_mut()
        }
    }
}

/// Drain pending side effects and return the current `AppSync` JSON.
///
/// # Safety
/// `app` must be null or a live pointer returned by `turnscope_app_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_sync(app: *mut TurnscopeApp) -> *mut c_char {
    // SAFETY: Valid pointer from `turnscope_app_start`.
    let Some(app) = (unsafe { app.as_ref() }) else {
        return std::ptr::null_mut();
    };
    let sync = lock_core(&app.core).sync();
    serialize_c(&sync)
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
    // SAFETY: Valid pointer from `turnscope_app_start`.
    let Some(app) = (unsafe { app.as_ref() }) else {
        return std::ptr::null_mut();
    };
    let Some(text) = (unsafe { read_c_str(action_json) }) else {
        return std::ptr::null_mut();
    };
    let Ok(action) = serde_json::from_str::<Action>(text) else {
        return std::ptr::null_mut();
    };
    let now = turnscope::now();
    let (calls, sync) = {
        let mut core = lock_core(&app.core);
        let calls = core.apply(action, now);
        let sync = core.sync();
        (calls, sync)
    };
    calls.send(app.engine.as_ref().and_then(|cell| cell.get()));
    serialize_c(&sync)
}

/// Append a line to `app.log` in the active data directory.
///
/// # Safety
/// `app` must be null or a live pointer from `turnscope_app_start`; `line` must be null or a valid NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_log(app: *mut TurnscopeApp, line: *const c_char) {
    // SAFETY: Valid pointer from `turnscope_app_start`.
    let Some(app) = (unsafe { app.as_ref() }) else {
        return;
    };
    if let (Some(data), Some(line)) = (&app.data, unsafe { read_c_str(line) }) {
        turnscope::engine::log(data, line);
    }
}

/// Build a plain-text diagnostics report for the active data directory.
///
/// # Safety
/// `app` must be null or a live pointer returned by `turnscope_app_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_diagnostics(app: *mut TurnscopeApp) -> *mut c_char {
    // SAFETY: Valid pointer from `turnscope_app_start`.
    let Some(app) = (unsafe { app.as_ref() }) else {
        return std::ptr::null_mut();
    };
    into_c_string(diagnostics(app.data.as_deref(), app.home.as_deref()))
}

/// Return the path to `app.log` (or the data directory if no log exists yet),
/// or NULL in fixture mode.
///
/// # Safety
/// `app` must be null or a live pointer returned by `turnscope_app_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_log_path(app: *mut TurnscopeApp) -> *mut c_char {
    // SAFETY: Valid pointer from `turnscope_app_start`.
    let Some(app) = (unsafe { app.as_ref() }) else {
        return std::ptr::null_mut();
    };
    let Some(data) = &app.data else {
        return std::ptr::null_mut();
    };
    let log = data.join("app.log");
    let target = if log.exists() { log } else { data.clone() };
    into_c_string(target.to_string_lossy().into_owned())
}

/// Stop the background engine and free the app core.
///
/// # Safety
/// `app` must be null or a pointer returned by `turnscope_app_start` that has not already been stopped.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_stop(app: *mut TurnscopeApp) {
    if !app.is_null() {
        // SAFETY: Takes ownership of pointer created by `turnscope_app_start`.
        let _ = unsafe { Box::from_raw(app) };
    }
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
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn ffi_round_trip_with_fixture_builds_panel_and_handles_actions() {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../turnscope/tests/fixtures/status.json");
        let cfg = serde_json::json!({
            "fixture": fixture,
            "opening": { "account": true, "unused": false, "settings": null }
        })
        .to_string();
        let cfg_c = CString::new(cfg).unwrap();
        let mut err: *mut c_char = std::ptr::null_mut();
        let app =
            unsafe { turnscope_app_start(cfg_c.as_ptr(), std::ptr::null_mut(), None, &mut err) };
        assert!(!app.is_null());
        assert!(err.is_null());

        let sync_ptr = unsafe { turnscope_app_sync(app) };
        let sync_json = unsafe { CStr::from_ptr(sync_ptr) }
            .to_str()
            .unwrap()
            .to_owned();
        unsafe { turnscope_app_free_string(sync_ptr) };
        let sync: serde_json::Value = serde_json::from_str(&sync_json).unwrap();
        assert_eq!(
            sync["panel"]["verdict"]["headline"],
            "Claude Max runs out in\u{a0}50m"
        );
        assert_eq!(sync["menuBar"]["slots"].as_array().unwrap().len(), 1);
        assert_eq!(sync["effects"][0]["kind"], "openInitial");

        let open_action =
            CString::new(r#"{"kind":"toggleAccount","id":"anthropic:acct-1:org-1"}"#).unwrap();
        let after_ptr = unsafe { turnscope_app_action(app, open_action.as_ptr()) };
        let after_json = unsafe { CStr::from_ptr(after_ptr) }
            .to_str()
            .unwrap()
            .to_owned();
        unsafe { turnscope_app_free_string(after_ptr) };
        let after: serde_json::Value = serde_json::from_str(&after_json).unwrap();
        assert_eq!(after["panel"]["openAccount"], "anthropic:acct-1:org-1");

        unsafe { turnscope_app_stop(app) };
    }

    #[test]
    fn live_core_wakes_shell_and_stops_cleanly() {
        extern "C" fn count_wake(ctx: *mut c_void) {
            let counter = unsafe { &*(ctx as *const AtomicUsize) };
            counter.fetch_add(1, Ordering::Release);
        }

        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let wakes = AtomicUsize::new(0);
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
        let sync_ptr = unsafe { turnscope_app_sync(app) };
        let sync_json = unsafe { CStr::from_ptr(sync_ptr) }
            .to_str()
            .unwrap()
            .to_owned();
        unsafe { turnscope_app_free_string(sync_ptr) };
        let sync: serde_json::Value = serde_json::from_str(&sync_json).unwrap();
        assert_eq!(sync["panel"]["hasStatus"], true);

        unsafe { turnscope_app_stop(app) };
        let release_deadline = Instant::now() + Duration::from_secs(2);
        while turnscope::engine::running(data.path()) && Instant::now() < release_deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!turnscope::engine::running(data.path()));
    }

    #[test]
    fn snapshot_reads_fixture_and_opens_settings_tab() {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../turnscope/tests/fixtures/status.json");
        let panel = snapshot(SnapshotConfig {
            fixture: Some(fixture),
            opening: Some(Opening::parse("agents")),
            data: None,
            home: None,
        })
        .unwrap();
        assert_eq!(panel.page, crate::state::Page::Settings);
        assert_eq!(panel.tab, crate::state::Tab::Agents);
        assert_eq!(panel.settings.agents.len(), 2);
    }

    #[test]
    fn diagnostics_includes_version_doctor_and_recent_log() {
        let data = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        drop(turnscope::db::open(data.path()).unwrap());
        turnscope::engine::log(data.path(), "sample diagnostic line");
        let report = diagnostics(Some(data.path()), Some(home.path()));
        assert!(report.contains(&format!("Turnscope {}", env!("CARGO_PKG_VERSION"))));
        assert!(report.contains("sample diagnostic line"), "{report}");
        assert!(diagnostics(None, None).contains("Mode: fixture"));
    }
}
