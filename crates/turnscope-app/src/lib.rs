//! Shared app core for Turnscope: owns the in-process engine, state, wording,
//! view model generation, and the C ABI linked into `apps/macos`.

pub mod standing;
pub mod state;
pub mod view;
pub mod words;

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use turnscope::engine::{Engine, Shown};

use crate::state::{Action, Call, Core, MenuShows, Preferences, State};
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartConfig {
    pub fixture: Option<PathBuf>,
    pub data: Option<PathBuf>,
    pub home: Option<PathBuf>,
    #[serde(default)]
    pub from_disk_image: bool,
    #[serde(default)]
    pub menu_shows: MenuShows,
    #[serde(default)]
    pub dismissed_connect: Vec<String>,
}

#[derive(Debug, Deserialize)]
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

#[derive(Clone, Copy)]
struct Wake {
    ctx: *mut c_void,
    cb: WakeFn,
}

// SAFETY: `ctx` is an opaque host pointer kept valid until `turnscope_app_stop` returns;
// `TurnscopeApp::drop` clears the mutex-guarded `Wake` so no callback runs after stop.
unsafe impl Send for Wake {}

impl Wake {
    fn trigger(self) {
        (self.cb)(self.ctx);
    }
}

pub struct TurnscopeApp {
    core: Arc<Mutex<Core>>,
    wake: Arc<Mutex<Option<Wake>>>,
    engine: Option<Arc<OnceLock<Engine>>>,
    data: Option<PathBuf>,
}

impl Drop for TurnscopeApp {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.wake.lock() {
            *guard = None;
        }
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
    std::fs::read_to_string(path)
        .and_then(|text| Ok(serde_json::from_str(&text)?))
        .map_err(|error| format!("Turnscope: {} can't be read: {error}", path.display()))
}

/// # Safety
/// `json` must be null or a valid NUL-terminated C string.
unsafe fn read_config<T: DeserializeOwned>(json: *const c_char) -> Result<T, String> {
    let text = unsafe { read_c_str(json) }.ok_or("Turnscope: no config")?;
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
            Call::Acknowledge(id) => engine.acknowledge(vec![id]),
        }
    }
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

pub fn start(
    config: StartConfig,
    wake: Option<WakeFn>,
    ctx: *mut c_void,
) -> Result<TurnscopeApp, String> {
    turnscope::db::started();
    let (home, data) = turnscope::places(config.home, config.data);
    let runs_engine = config.fixture.is_none();
    let mut state = match config.fixture.as_deref() {
        Some(path) => State::fixed(read_fixture(path)?),
        None => State {
            preferences: Preferences {
                menu_shows: config.menu_shows,
                dismissed_connect: config.dismissed_connect.into_iter().collect(),
            },
            ..State::default()
        },
    };
    state.from_disk_image = config.from_disk_image;

    let core = Arc::new(Mutex::new(Core::new(state)));
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
                send(held.get(), calls);
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
    let state = State::fixed(shown);
    Ok(view::build_panel(&state))
}

/// Build a one-shot panel snapshot JSON for `--snapshot`.
///
/// # Safety
/// `config_json` must be null or a valid NUL-terminated UTF-8 C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_snapshot(config_json: *const c_char) -> *mut c_char {
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
    match unsafe { read_config(config_json) }.and_then(|config| start(config, wake, ctx)) {
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
    let action = match serde_json::from_str::<Action>(text) {
        Ok(action) => action,
        Err(error) => {
            if let Some(data) = &app.data {
                turnscope::engine::log(data, &format!("invalid action JSON: {error}"));
            }
            return std::ptr::null_mut();
        }
    };
    let now = turnscope::now();
    let (calls, sync) = {
        let mut core = lock_core(&app.core);
        let calls = core.apply(action, now);
        let sync = core.sync();
        (calls, sync)
    };
    send(app.engine.as_ref().and_then(|cell| cell.get()), calls);
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

/// Build a plain-text diagnostics report from a JSON `SnapshotConfig`.
/// Safe to invoke from a background thread (never touches `TurnscopeApp`).
///
/// # Safety
/// `config_json` must be null or a valid NUL-terminated UTF-8 C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn turnscope_app_diagnostics(config_json: *const c_char) -> *mut c_char {
    let report = match unsafe { read_config::<SnapshotConfig>(config_json) } {
        Ok(config) if config.fixture.is_some() => diagnostics(None, None),
        Ok(config) => {
            let (home, data) = turnscope::places(config.home, config.data);
            diagnostics(Some(&data), Some(&home))
        }
        Err(error) => error,
    };
    into_c_string(report)
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
        let cfg = serde_json::json!({ "fixture": fixture }).to_string();
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
        assert_eq!(sync["panel"]["verdict"]["standing"], "runningOut");
        assert_eq!(sync["menuBar"]["slots"].as_array().unwrap().len(), 1);

        let hide_action =
            CString::new(r#"{"kind":"setHidden","id":"anthropic:acct-1:org-1","hidden":true}"#)
                .unwrap();
        let after_ptr = unsafe { turnscope_app_action(app, hide_action.as_ptr()) };
        let after_json = unsafe { CStr::from_ptr(after_ptr) }
            .to_str()
            .unwrap()
            .to_owned();
        unsafe { turnscope_app_free_string(after_ptr) };
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
    fn snapshot_reads_fixture_and_builds_agents() {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../turnscope/tests/fixtures/status.json");
        let panel = snapshot(SnapshotConfig {
            fixture: Some(fixture),
            data: None,
            home: None,
        })
        .unwrap();
        assert!(panel.has_status);
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

        let cfg = serde_json::json!({ "data": data.path(), "home": home.path() }).to_string();
        let cfg_c = CString::new(cfg).unwrap();
        let ptr = unsafe { turnscope_app_diagnostics(cfg_c.as_ptr()) };
        let ffi_report = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
        unsafe { turnscope_app_free_string(ptr) };
        assert!(
            ffi_report.contains("sample diagnostic line"),
            "{ffi_report}"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn swift_models_match_rust_wire_contract() {
        use crate::standing::Standing;
        use crate::state::Effect;
        use crate::view::*;
        use crate::words::AlertNote;
        use turnscope::alerts::Notify;

        let full_row = AccountRowModel {
            id: "anthropic:a".into(),
            provider: "anthropic".into(),
            title: "Claude Max".into(),
            label: Some("me@example.com".into()),
            called: "Claude Max (me@example.com)".into(),
            standing: Standing::RunningOut,
            status: "Runs out in 50m".into(),
            figure: "50m".into(),
            figure_then: Some("· 12%".into()),
            figure_spoken: "50m left".into(),
            collapsed_spoken: "Claude Max, Runs out in 50m, 50m left".into(),
            openable: true,
            limits: vec![
                LimitLineModel {
                    key: "five_hour".into(),
                    name: "5 hours".into(),
                    ahead: Some("Runs out in 50m".into()),
                    window: Some("Resets 3:00 PM".into()),
                    left: "12%".into(),
                    size: None,
                    spoken: "12% left".into(),
                    left_percent: 12,
                    time_left_fraction: Some(0.42),
                    standing: Standing::RunningOut,
                },
                LimitLineModel {
                    key: "credits".into(),
                    name: "Credits".into(),
                    ahead: None,
                    window: None,
                    left: "$0".into(),
                    size: Some("of $30".into()),
                    spoken: "$0 of $30 left".into(),
                    left_percent: 0,
                    time_left_fraction: None,
                    standing: Standing::UsedUp,
                },
            ],
            note: Some("As of 2:00 PM".into()),
            used_most_heading: Some("Used most of 5 hours".into()),
            used_most: vec![UsedMostModel {
                id: "claude-code:0f6e-1234".into(),
                session_id: "0f6e-1234".into(),
                title: "Refactor bridge".into(),
                detail: "34% · turnscope".into(),
                project_path: Some("/tmp/turnscope".into()),
            }],
        };

        let syncs = vec![
            AppSync {
                animated: true,
                counting: true,
                menu_bar: MenuBarModel {
                    slots: vec![
                        MenuBarSlot {
                            provider: "anthropic".into(),
                            standing: Standing::Lasts,
                            figure: "88%".into(),
                        },
                        MenuBarSlot {
                            provider: "openai".into(),
                            standing: Standing::UsedUp,
                            figure: "Back 4:00 PM".into(),
                        },
                    ],
                    spoken: "Claude Max 88%".into(),
                    out_of_date: true,
                },
                panel: PanelModel {
                    version: "0.3.2".into(),
                    has_status: true,
                    stopped: false,
                    waiting: None,
                    verdict: Some(VerdictModel {
                        headline: "Claude Max runs out in 50m".into(),
                        detail: "Back 3:00 PM.".into(),
                        standing: Standing::RunningOut,
                    }),
                    in_use: vec![full_row],
                    recent: vec![],
                    unused: vec![],
                    hidden: vec![AccountModel {
                        id: "openai:b".into(),
                        provider: "openai".into(),
                        title: "ChatGPT Pro".into(),
                        label: Some("work@example.com".into()),
                        via: Some("Codex".into()),
                        called: "ChatGPT Pro".into(),
                        shown: false,
                    }],
                    unused_fold_label: Some("1 hidden".into()),
                    hidden_fold_label: Some("1 hidden".into()),
                    connect_offer: Some(ConnectOfferModel {
                        id: "claude-code".into(),
                        name: "Claude Code".into(),
                        note: Some("Uses another Turnscope".into()),
                        configured: Some("/old/turnscope".into()),
                        changing: false,
                        verb: "Reconnect".into(),
                        help: "Points Claude Code at this copy".into(),
                    }),
                    footer_note: Some(FooterNoteModel {
                        text: "Not up to date".into(),
                        help: Some("Offline".into()),
                        spoken: "Not up to date: Offline".into(),
                        busy: false,
                    }),
                    settings: SettingsModel {
                        menu_shows: MenuShows::All,
                        notify: Some(Notify {
                            running_out: true,
                            used_up: false,
                            reset: true,
                            sign_in: false,
                        }),
                        can_edit_notify: true,
                        agents_empty: None,
                        agents_title: "Let these agents check their limits".into(),
                        agents: vec![AgentRowModel {
                            id: "claude-code".into(),
                            name: "Claude Code".into(),
                            note: Some("Uses another Turnscope".into()),
                            configured: Some("/old/turnscope".into()),
                            changing: Some("Connecting…".into()),
                            connected: true,
                            enabled: false,
                        }],
                        accounts_empty: None,
                        subscriptions: vec![AccountModel {
                            id: "anthropic:a".into(),
                            provider: "anthropic".into(),
                            title: "Claude Max".into(),
                            label: Some("me@example.com".into()),
                            via: Some("Claude Code".into()),
                            called: "Claude Max (me@example.com)".into(),
                            shown: true,
                        }],
                        api_keys: vec![],
                    },
                },
                effects: vec![
                    Effect::PostAlerts {
                        alerts: vec![AlertNote {
                            id: 7,
                            account: "anthropic:a".into(),
                            replaces: "turnscope-limit-anthropic:a".into(),
                            sign_in: false,
                            title: "Claude Max".into(),
                            subtitle: "5 hours".into(),
                            body: "Runs out in 50m".into(),
                        }],
                    },
                    Effect::RemoveNotifications {
                        identifiers: vec!["turnscope-sign-in-anthropic:a".into()],
                    },
                    Effect::SavePreferences {
                        menu_shows: MenuShows::One,
                        dismissed_connect: vec!["codex".into()],
                    },
                ],
            },
            AppSync {
                animated: false,
                counting: false,
                menu_bar: MenuBarModel {
                    slots: vec![],
                    spoken: "Turnscope".into(),
                    out_of_date: false,
                },
                panel: PanelModel {
                    version: "0.3.2".into(),
                    has_status: false,
                    stopped: true,
                    waiting: Some(WaitingModel {
                        problem: Some("database locked".into()),
                        retries: true,
                    }),
                    verdict: None,
                    in_use: vec![],
                    recent: vec![],
                    unused: vec![],
                    hidden: vec![],
                    unused_fold_label: None,
                    hidden_fold_label: None,
                    connect_offer: None,
                    footer_note: Some(FooterNoteModel {
                        text: "Reading…".into(),
                        help: Some("First run".into()),
                        spoken: "Reading".into(),
                        busy: true,
                    }),
                    settings: SettingsModel {
                        menu_shows: MenuShows::One,
                        notify: None,
                        can_edit_notify: false,
                        agents_empty: Some(EmptyMessage {
                            headline: "No agents".into(),
                            detail: "Install one".into(),
                        }),
                        agents_title: "Move Turnscope".into(),
                        agents: vec![],
                        accounts_empty: Some(EmptyMessage {
                            headline: "No accounts".into(),
                            detail: "Sign in".into(),
                        }),
                        subscriptions: vec![],
                        api_keys: vec![],
                    },
                },
                effects: vec![],
            },
            AppSync {
                animated: true,
                counting: false,
                menu_bar: MenuBarModel {
                    slots: vec![],
                    spoken: "Turnscope".into(),
                    out_of_date: false,
                },
                panel: PanelModel {
                    version: "0.3.2".into(),
                    has_status: true,
                    stopped: false,
                    waiting: None,
                    verdict: None,
                    in_use: vec![],
                    recent: vec![],
                    unused: vec![],
                    hidden: vec![],
                    unused_fold_label: None,
                    hidden_fold_label: None,
                    connect_offer: None,
                    footer_note: Some(FooterNoteModel {
                        text: "Refreshing limits…".into(),
                        help: None,
                        spoken: "Refreshing limits".into(),
                        busy: true,
                    }),
                    settings: SettingsModel {
                        menu_shows: MenuShows::All,
                        notify: None,
                        can_edit_notify: false,
                        agents_empty: None,
                        agents_title: String::new(),
                        agents: vec![],
                        accounts_empty: None,
                        subscriptions: vec![],
                        api_keys: vec![],
                    },
                },
                effects: vec![],
            },
        ];

        let snapshots = vec![
            SnapshotOutput::Ok {
                panel: Box::new(syncs[0].panel.clone()),
            },
            SnapshotOutput::Err {
                error: "unreadable".into(),
                code: 2,
            },
        ];

        let dir = tempfile::tempdir().unwrap();
        let syncs_path = dir.path().join("syncs.json");
        let snaps_path = dir.path().join("snapshots.json");
        std::fs::write(&syncs_path, serde_json::to_string(&syncs).unwrap()).unwrap();
        std::fs::write(&snaps_path, serde_json::to_string(&snapshots).unwrap()).unwrap();

        let driver = dir.path().join("main.swift");
        std::fs::write(
            &driver,
            r#"
import Foundation

struct Out: Encodable {
    var actions: [AppAction]
    var start: StartConfig
    var snapshot: SnapshotConfig
}

let args = CommandLine.arguments
let decoder = JSONDecoder()
let syncs = try decoder.decode([AppSync].self, from: Data(contentsOf: URL(fileURLWithPath: args[1])))
precondition(syncs.count == 3)

// Verify every optional field Rust sends as Some decodes non-nil (decodeIfPresent would silently drop renamed keys).
let p0 = syncs[0].panel
precondition(p0.verdict?.headline == "Claude Max runs out in 50m")
let row = p0.inUse[0]
precondition(row.label == "me@example.com")
precondition(row.figureThen == "· 12%")
precondition(row.note == "As of 2:00 PM")
precondition(row.usedMostHeading == "Used most of 5 hours")
precondition(row.limits[0].ahead == "Runs out in 50m")
precondition(row.limits[0].window == "Resets 3:00 PM")
precondition(row.limits[0].timeLeftFraction == 0.42)
precondition(row.limits[1].size == "of $30")
precondition(row.usedMost[0].sessionId == "0f6e-1234")
precondition(row.usedMost[0].projectPath == "/tmp/turnscope")
precondition(p0.hidden[0].label == "work@example.com")
precondition(p0.hidden[0].via == "Codex")
precondition(p0.unusedFoldLabel == "1 hidden")
precondition(p0.hiddenFoldLabel == "1 hidden")
precondition(p0.connectOffer?.note == "Uses another Turnscope")
precondition(p0.connectOffer?.configured == "/old/turnscope")
precondition(p0.footerNote == FooterNoteModel(text: "Not up to date", help: "Offline", spoken: "Not up to date: Offline", busy: false))
precondition(p0.settings.notify == NotifySettings(runningOut: true, usedUp: false, reset: true, signIn: false))
precondition(p0.settings.agents[0].note == "Uses another Turnscope")
precondition(p0.settings.agents[0].configured == "/old/turnscope")
precondition(p0.settings.agents[0].changing == "Connecting…")
precondition(p0.settings.subscriptions[0].label == "me@example.com")
precondition(p0.settings.subscriptions[0].via == "Claude Code")
precondition(syncs[0].effects == [
    .postAlerts(alerts: [AlertNote(id: 7, account: "anthropic:a", replaces: "turnscope-limit-anthropic:a", signIn: false, title: "Claude Max", subtitle: "5 hours", body: "Runs out in 50m")]),
    .removeNotifications(identifiers: ["turnscope-sign-in-anthropic:a"]),
    .savePreferences(menuShows: .one, dismissedConnect: ["codex"]),
])

let p1 = syncs[1].panel
precondition(p1.waiting?.problem == "database locked")
precondition(p1.footerNote == FooterNoteModel(text: "Reading…", help: "First run", spoken: "Reading", busy: true))
precondition(p1.settings.agentsEmpty == EmptyMessage(headline: "No agents", detail: "Install one"))
precondition(p1.settings.accountsEmpty == EmptyMessage(headline: "No accounts", detail: "Sign in"))

precondition(syncs[2].panel.footerNote == FooterNoteModel(text: "Refreshing limits…", help: nil, spoken: "Refreshing limits", busy: true))

let snaps = try decoder.decode([SnapshotResponse].self, from: Data(contentsOf: URL(fileURLWithPath: args[2])))
precondition(snaps.count == 2)
if case let .ok(panel) = snaps[0] {
    precondition(panel.verdict?.headline == "Claude Max runs out in 50m")
} else {
    preconditionFailure("expected .ok snapshot")
}
if case let .err(message, code) = snaps[1] {
    precondition(message == "unreadable" && code == 2)
} else {
    preconditionFailure("expected .err snapshot")
}

// Launch argument parsing.
precondition(Launch.parse(["Turnscope"]) == Launch())
precondition(
    Launch.parse(["Turnscope", "--fixture", "f.json", "--snapshot", "out", "--data", "d", "--home", "h", "--open", "account,unused"])
        == Launch(fixture: "f.json", snapshot: "out", opening: Opening(account: true, unused: true, settings: nil), data: "d", home: "h")
)
precondition(Launch.parse(["Turnscope", "--fixture", "--open"]) == Launch(opening: Opening()))
precondition(Opening.parse("settings").settings == .general)
precondition(Opening.parse("agents").settings == .agents)
precondition(Opening.parse("accounts").settings == .accounts)

// Panel placement (under item, above Dock, screen-edge clamping, multi-display coordinates).
let laptop = CGRect(x: 0, y: 0, width: 1512, height: 949)
let under = panelFrame(midX: 1200, underY: 949, visible: laptop, panelWidth: 340, gutter: 28)
precondition(under == CGRect(x: 1200 - 170 - 28, y: 0, width: 396, height: 943))
let docked = CGRect(x: 0, y: 80, width: 1512, height: 869)
precondition(panelFrame(midX: 1200, underY: 949, visible: docked, panelWidth: 340, gutter: 28).height == 863)
let rightEdge = panelFrame(midX: 1500, underY: 949, visible: laptop, panelWidth: 340, gutter: 28)
precondition(rightEdge.minX + 28 + 340 == 1512 - 6)
let leftEdge = panelFrame(midX: 10, underY: 949, visible: laptop, panelWidth: 340, gutter: 28)
precondition(leftEdge.minX + 28 == 6)
let narrow = CGRect(x: 0, y: 0, width: 300, height: 949)
precondition(panelFrame(midX: 150, underY: 949, visible: narrow, panelWidth: 340, gutter: 28).minX + 28 == 6)
let secondary = CGRect(x: -1920, y: 982, width: 1920, height: 1055)
let multi = panelFrame(midX: -300, underY: 2037, visible: secondary, panelWidth: 340, gutter: 28)
precondition(multi == CGRect(x: -300 - 170 - 28, y: 982, width: 396, height: 1049))

let out = Out(
    actions: [
        .panelOpened,
        .panelClosed,
        .setHidden(id: "anthropic:a", hidden: true),
        .setConnected(id: "claude-code", connecting: true),
        .setNotify(notify: NotifySettings(runningOut: true, usedUp: false, reset: true, signIn: false)),
        .setMenuShows(shows: .all),
        .setMenuShows(shows: .one),
        .dismissConnect(id: "codex"),
        .acknowledgeAlert(id: 7, signInAccount: nil),
        .acknowledgeAlert(id: 8, signInAccount: "anthropic:a"),
        .refreshNow,
        .tick,
    ],
    start: StartConfig(
        fixture: "f.json",
        data: "/tmp/d",
        home: "/tmp/h",
        fromDiskImage: true,
        menuShows: .one,
        dismissedConnect: ["codex"]
    ),
    snapshot: SnapshotConfig(fixture: "f.json", data: "/tmp/d", home: "/tmp/h")
)
let data = try JSONEncoder().encode(out)
print(String(data: data, encoding: .utf8)!)
"#,
        )
        .unwrap();

        let models =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/macos/Sources/Models.swift");
        let bin = dir.path().join("contract");
        let compile = std::process::Command::new("xcrun")
            .args(["swiftc", "-swift-version", "6", "-warnings-as-errors"])
            .arg(&models)
            .arg(&driver)
            .arg("-o")
            .arg(&bin)
            .output()
            .expect("xcrun swiftc should run on macOS");
        assert!(
            compile.status.success(),
            "swiftc failed:\n{}",
            String::from_utf8_lossy(&compile.stderr)
        );

        let run = std::process::Command::new(&bin)
            .arg(&syncs_path)
            .arg(&snaps_path)
            .output()
            .expect("contract binary should run");
        assert!(
            run.status.success(),
            "Swift contract failed:\n{}",
            String::from_utf8_lossy(&run.stderr)
        );

        #[derive(Deserialize)]
        struct SwiftOut {
            actions: Vec<Action>,
            start: StartConfig,
            snapshot: SnapshotConfig,
        }
        let parsed: SwiftOut = serde_json::from_slice(&run.stdout)
            .expect("Rust should decode every Action/Config emitted by Swift");
        assert_eq!(
            parsed.actions,
            vec![
                Action::PanelOpened,
                Action::PanelClosed,
                Action::SetHidden {
                    id: "anthropic:a".into(),
                    hidden: true,
                },
                Action::SetConnected {
                    id: "claude-code".into(),
                    connecting: true,
                },
                Action::SetNotify {
                    notify: Notify {
                        running_out: true,
                        used_up: false,
                        reset: true,
                        sign_in: false,
                    },
                },
                Action::SetMenuShows {
                    shows: MenuShows::All,
                },
                Action::SetMenuShows {
                    shows: MenuShows::One,
                },
                Action::DismissConnect { id: "codex".into() },
                Action::AcknowledgeAlert {
                    id: 7,
                    sign_in_account: None,
                },
                Action::AcknowledgeAlert {
                    id: 8,
                    sign_in_account: Some("anthropic:a".into()),
                },
                Action::RefreshNow,
                Action::Tick,
            ]
        );
        assert_eq!(parsed.start.fixture.as_deref(), Some(Path::new("f.json")));
        assert_eq!(parsed.start.data.as_deref(), Some(Path::new("/tmp/d")));
        assert_eq!(parsed.start.home.as_deref(), Some(Path::new("/tmp/h")));
        assert!(parsed.start.from_disk_image);
        assert_eq!(parsed.start.menu_shows, MenuShows::One);
        assert_eq!(parsed.start.dismissed_connect, vec!["codex"]);
        assert_eq!(
            parsed.snapshot.fixture.as_deref(),
            Some(Path::new("f.json"))
        );
        assert_eq!(parsed.snapshot.data.as_deref(), Some(Path::new("/tmp/d")));
        assert_eq!(parsed.snapshot.home.as_deref(), Some(Path::new("/tmp/h")));
    }
}
