//! Updates, with Sparkle (docs/app.md#updates). Sparkle is loaded from the bundle's
//! Contents/Frameworks when the app starts, rather than linked, so the app builds and runs from
//! `cargo` without it, as a copy that can't update itself.
//!
//! A menu bar app has no window to bring forward, so Sparkle would show an update found by a
//! scheduled check behind other apps. Instead, unless Sparkle can show it in focus, as just
//! after launch, the panel says one is waiting (Sparkle's "gentle reminders").

use std::cell::OnceCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject};
use objc2::{AnyThread, MainThreadMarker, define_class, msg_send};
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSBundle, NSString};

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TurnscopeUpdaterDriver"]
    #[ivars = ()]
    struct Driver;

    impl Driver {
        #[unsafe(method(supportsGentleScheduledUpdateReminders))]
        fn gentle(&self) -> Bool {
            Bool::YES
        }

        #[unsafe(method(standardUserDriverShouldHandleShowingScheduledUpdate:andInImmediateFocus:))]
        fn should_show(&self, _update: &AnyObject, immediate_focus: Bool) -> Bool {
            immediate_focus
        }

        #[unsafe(method(standardUserDriverWillHandleShowingUpdate:forUpdate:state:))]
        fn will_show(&self, handle_showing: Bool, _update: &AnyObject, _state: &AnyObject) {
            if !handle_showing.as_bool() {
                crate::set(|s| &mut s.update_available, true);
            }
        }

        #[unsafe(method(standardUserDriverWillFinishUpdateSession))]
        fn finished(&self) {
            crate::set(|s| &mut s.update_available, false);
        }
    }
);

struct Sparkle {
    controller: Retained<AnyObject>,
    _driver: Retained<Driver>,
}

thread_local! {
    static SPARKLE: OnceCell<Option<Sparkle>> = const { OnceCell::new() };
}

/// Starts Sparkle, if this copy can update itself: it has a public key, and Sparkle is in its
/// bundle. Call on the main thread.
pub fn start() {
    SPARKLE.with(|cell| {
        cell.get_or_init(load);
    });
}

fn load() -> Option<Sparkle> {
    let bundle = NSBundle::mainBundle();
    let key = bundle
        .objectForInfoDictionaryKey(&NSString::from_str("SUPublicEDKey"))
        .and_then(|key| key.downcast::<NSString>().ok())
        .map(|key| key.to_string())
        .unwrap_or_default();
    if key.is_empty() {
        return None;
    }
    let path = bundle
        .privateFrameworksPath()?
        .stringByAppendingPathComponent(&NSString::from_str("Sparkle.framework"));
    let sparkle = NSBundle::bundleWithPath(&path)?;
    if !unsafe { sparkle.load() } {
        crate::engine::log("updates: Sparkle didn't load");
        return None;
    }
    let class = AnyClass::get(c"SPUStandardUpdaterController")?;
    let driver: Retained<Driver> = {
        let driver = Driver::alloc().set_ivars(());
        unsafe { msg_send![super(driver), init] }
    };
    let controller: Retained<AnyObject> = unsafe {
        let made: *mut AnyObject = msg_send![class, alloc];
        let made: *mut AnyObject = msg_send![
            made,
            initWithStartingUpdater: Bool::YES,
            updaterDelegate: std::ptr::null::<AnyObject>(),
            userDriverDelegate: &*driver
        ];
        Retained::from_raw(made)?
    };
    Some(Sparkle {
        controller,
        _driver: driver,
    })
}

fn with<T>(f: impl FnOnce(&AnyObject) -> T) -> Option<T> {
    SPARKLE.with(|cell| {
        cell.get()
            .and_then(|sparkle| sparkle.as_ref())
            .map(|sparkle| f(&sparkle.controller))
    })
}

pub fn available() -> bool {
    with(|_| ()).is_some()
}

pub fn check() {
    with(|controller| {
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).activate();
        }
        let _: () =
            unsafe { msg_send![controller, checkForUpdates: std::ptr::null::<AnyObject>()] };
    });
}

pub fn automatic() -> bool {
    with(|controller| unsafe {
        let updater: *mut AnyObject = msg_send![controller, updater];
        let on: Bool = msg_send![updater, automaticallyChecksForUpdates];
        on.as_bool()
    })
    .unwrap_or(false)
}

pub fn set_automatic(on: bool) {
    with(|controller| unsafe {
        let updater: *mut AnyObject = msg_send![controller, updater];
        let _: () = msg_send![updater, setAutomaticallyChecksForUpdates: Bool::new(on)];
    });
}
