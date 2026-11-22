//! Posts the engine's alerts as notifications, each under the id it gives (`Alert::replaces`),
//! so a reset replaces the warning before it. A sign-in notification is removed once its
//! account can be read (docs/app.md#notifications).

use std::ptr::NonNull;
use std::sync::OnceLock;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{AnyThread, define_class, msg_send};
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{
    NSArray, NSBundle, NSDictionary, NSError, NSObjectProtocol, NSString, NSURL,
};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent, UNNotification,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNNotificationSettings, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};

use crate::state::State;
use turnscope::alerts::{Alert, Kind};

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "TurnscopeNotifier"]
    #[ivars = ()]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        /// Shown as a banner even while the panel is open.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            done: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            done.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::Sound,));
        }

        /// A click opens the panel with its account open in place.
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            done: &block2::DynBlock<dyn Fn()>,
        ) {
            let info = response.notification().request().content().userInfo();
            let account = info
                .objectForKey(&NSString::from_str("account"))
                .and_then(|value| value.downcast::<NSString>().ok())
                .map(|value| value.to_string());
            crate::open_panel(account);
            done.call(());
        }
    }
);

thread_local! {
    static DELEGATE: std::cell::OnceCell<Retained<Delegate>> = const { std::cell::OnceCell::new() };
}

/// macOS posts notifications only for `.app` bundles, not bare `cargo run` binaries.
fn bundled() -> bool {
    static BUNDLED: OnceLock<bool> = OnceLock::new();
    *BUNDLED.get_or_init(|| {
        let bundle = NSBundle::mainBundle();
        bundle.bundleIdentifier().is_some() && bundle.bundlePath().to_string().ends_with(".app")
    })
}

fn center() -> Option<Retained<UNUserNotificationCenter>> {
    bundled().then(UNUserNotificationCenter::currentNotificationCenter)
}

pub fn start() {
    let Some(center) = center() else { return };
    DELEGATE.with(|cell| {
        let delegate = cell.get_or_init(|| {
            let delegate = Delegate::alloc().set_ivars(());
            unsafe { msg_send![super(delegate), init] }
        });
        center.setDelegate(Some(ProtocolObject::from_ref(&**delegate)));
    });
    let asked = RcBlock::new(|_granted: objc2::runtime::Bool, _error: *mut NSError| check());
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
        &asked,
    );
}

pub fn check() {
    let Some(center) = center() else { return };
    let got = RcBlock::new(|settings: NonNull<UNNotificationSettings>| {
        let denied =
            unsafe { settings.as_ref() }.authorizationStatus() == UNAuthorizationStatus::Denied;
        crate::set(|s| &mut s.notifications_denied, denied);
    });
    center.getNotificationSettingsWithCompletionHandler(&got);
}

/// Acknowledges each alert only after macOS accepts it, so refused alerts retry
/// once notification permission is granted.
pub fn tell(s: &State, alerts: Vec<Alert>) {
    if alerts.is_empty() {
        return;
    }
    let Some(center) = center() else { return };
    let words = s.words_now();
    for alert in alerts {
        let note = words.note(&alert, s.all());
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&note.title));
        content.setSubtitle(&NSString::from_str(&note.subtitle));
        content.setBody(&NSString::from_str(&note.body));
        content.setSound(Some(&UNNotificationSound::defaultSound()));
        content.setThreadIdentifier(&NSString::from_str(&alert.account));
        let info = NSDictionary::from_slices(
            &[&*NSString::from_str("account")],
            &[&*NSString::from_str(&alert.account)],
        );
        // SAFETY: a dictionary of strings is a dictionary of objects.
        let info: Retained<NSDictionary> = unsafe { Retained::cast_unchecked(info) };
        unsafe { content.setUserInfo(&info) };
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&alert.replaces),
            &content,
            None,
        );
        let id = alert.id;
        let sign_in = (alert.kind == Kind::SignIn).then(|| alert.account.clone());
        let added = RcBlock::new(move |error: *mut NSError| {
            if !error.is_null() {
                return;
            }
            crate::engine::ask(|e| e.acknowledge(vec![id]));
            // The account may have been read again while it was posted.
            if let Some(account) = sign_in.clone()
                && let Some(handle) = crate::HANDLE.get()
            {
                handle.update_if(move |s, _| {
                    if s.all().iter().any(|a| a.id == account && !a.refused()) {
                        renewed(&[account]);
                    }
                    false
                });
            }
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&added));
    }
}

/// Removes the sign-in notification of each of `accounts`, which can be read, also one left
/// from a previous run.
pub fn renewed(accounts: &[String]) {
    if accounts.is_empty() {
        return;
    }
    let Some(center) = center() else { return };
    let ids: Vec<Retained<NSString>> = accounts
        .iter()
        .map(|id| NSString::from_str(&turnscope::alerts::replaces(id, None)))
        .collect();
    center.removeDeliveredNotificationsWithIdentifiers(&NSArray::from_retained_slice(&ids));
}

/// Opens Turnscope's notifications in System Settings.
pub fn open_settings() {
    let id = NSBundle::mainBundle()
        .bundleIdentifier()
        .map(|id| id.to_string())
        .unwrap_or_default();
    let url =
        format!("x-apple.systempreferences:com.apple.Notifications-Settings.extension?id={id}");
    if let Some(url) = NSURL::URLWithString(&NSString::from_str(&url)) {
        NSWorkspace::sharedWorkspace().openURL(&url);
    }
}
