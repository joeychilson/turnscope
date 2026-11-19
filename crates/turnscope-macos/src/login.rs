//! Opening at login, with ServiceManagement's main app service (docs/app.md#opening-at-login).

use objc2_service_management::{SMAppService, SMAppServiceStatus};

/// Whether the app opens at login.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Login {
    Off,
    On,
    /// It's registered, but macOS holds it until it's allowed in System Settings.
    NeedsApproval,
}

impl Login {
    /// Whether its switch is on: macOS may only be holding it.
    pub fn at_login(self) -> bool {
        self != Login::Off
    }
}

/// Reads whether the app opens at login into the state, off the main thread: macOS answers in
/// 1.7 to 6.2 ms (measured November 2026), which held up the panel as it opened.
pub fn check() {
    std::thread::spawn(|| {
        let login = status();
        if let Some(handle) = crate::HANDLE.get() {
            handle.update_if(move |s, _| {
                let changed = s.login != login;
                s.login = login;
                changed
            });
        }
    });
}

/// Whether the app opens at login now.
fn status() -> Login {
    let status = unsafe { SMAppService::mainAppService().status() };
    match status {
        SMAppServiceStatus::Enabled => Login::On,
        SMAppServiceStatus::RequiresApproval => Login::NeedsApproval,
        _ => Login::Off,
    }
}

/// Opens the app at login, or stops, and says how it is afterwards. If macOS refuses, it
/// beeps, and it stays as it was. On a fixed status, whose changes go nowhere, it only says so.
pub fn set(on: bool) -> Login {
    if !crate::engine::running() {
        return if on { Login::On } else { Login::Off };
    }
    let service = unsafe { SMAppService::mainAppService() };
    let done = unsafe {
        if on {
            service.registerAndReturnError()
        } else {
            service.unregisterAndReturnError()
        }
    };
    if let Err(error) = done {
        crate::engine::log(&format!("open at login: {}", error.localizedDescription()));
        crate::beep();
    }
    status()
}

/// Opens Login Items in System Settings.
pub fn open_settings() {
    unsafe { SMAppService::openSystemSettingsLoginItems() };
}
