// Updates, with Sparkle. It reads the appcast each release publishes on
// GitHub (`SUFeedURL` in Info.plist), checks a new release's signature with
// the public key in the app (`SUPublicEDKey`), and asks before installing and
// relaunching. The release workflow signs each disk image and writes the
// appcast (`scripts/build-release.sh`).
//
// A build without a public key, such as one from a checkout, never checks, so
// it can't be offered an update it couldn't verify.
//
// A menu bar app has no window to bring forward, so Sparkle would show an
// update found by a scheduled check behind other apps. Instead, unless Sparkle
// can show it in focus, as just after launch, the panel says one is waiting
// (Sparkle's "gentle reminders").

import AppKit
import Observation
import Sparkle

@MainActor
@Observable
final class Updater: NSObject, SPUStandardUserDriverDelegate {
    /// Sparkle's controller, set once it starts with a key. It's observed,
    /// so anything that reads `available` updates with it.
    private var controller: SPUStandardUpdaterController?

    /// Whether this copy can update itself, which it can if it has a public
    /// key.
    var available: Bool { controller != nil }

    /// Whether a scheduled check found an update that waits to be shown,
    /// until its update session ends.
    private(set) var waiting = false

    func start() {
        let key = Bundle.main.object(forInfoDictionaryKey: "SUPublicEDKey") as? String ?? ""
        guard !key.isEmpty else { return }
        controller = SPUStandardUpdaterController(startingUpdater: true, updaterDelegate: nil,
                                                  userDriverDelegate: self)
    }

    /// Check for an update now and show what's found. An update that's
    /// waiting is brought forward.
    func check() {
        NSApp.activate()
        controller?.checkForUpdates(nil)
    }

    /// Whether it checks for updates automatically.
    var automatic: Bool {
        get { controller?.updater.automaticallyChecksForUpdates ?? false }
        set { controller?.updater.automaticallyChecksForUpdates = newValue }
    }

    // Sparkle calls these on the main thread.

    nonisolated var supportsGentleScheduledUpdateReminders: Bool { true }

    nonisolated func standardUserDriverShouldHandleShowingScheduledUpdate(
        _ update: SUAppcastItem, andInImmediateFocus immediateFocus: Bool) -> Bool {
        immediateFocus
    }

    nonisolated func standardUserDriverWillHandleShowingUpdate(
        _ handleShowingUpdate: Bool, forUpdate update: SUAppcastItem, state: SPUUserUpdateState) {
        guard !handleShowingUpdate else { return }
        MainActor.assumeIsolated { waiting = true }
    }

    nonisolated func standardUserDriverWillFinishUpdateSession() {
        MainActor.assumeIsolated { waiting = false }
    }
}
