// Keeping Turnscope up to date, with Sparkle: it reads the appcast each
// release publishes on GitHub (`SUFeedURL` in Info.plist), checks a newer
// release's signature against the public key the app carries
// (`SUPublicEDKey`), and asks the person before putting it in place and
// relaunching. The release workflow signs each disk image and writes the
// appcast (`scripts/release.sh`).
//
// A build without a public key, as one made from a checkout is, never
// checks, so it can't be offered an update it couldn't verify.

import AppKit
import Observation
import Sparkle

@MainActor
@Observable
final class Updater: NSObject, SPUStandardUserDriverDelegate {
    @ObservationIgnored private var controller: SPUStandardUpdaterController?

    /// Whether this copy can update itself: it carries a public key.
    var available: Bool { controller != nil }

    func start() {
        let key = Bundle.main.object(forInfoDictionaryKey: "SUPublicEDKey") as? String ?? ""
        guard !key.isEmpty else { return }
        controller = SPUStandardUpdaterController(startingUpdater: true, updaterDelegate: nil,
                                                  userDriverDelegate: self)
    }

    /// Look for an update now, showing what it finds.
    func check() {
        NSApp.activate()
        controller?.checkForUpdates(nil)
    }

    /// Whether it looks for updates on its own.
    var automatic: Bool {
        get { controller?.updater.automaticallyChecksForUpdates ?? false }
        set { controller?.updater.automaticallyChecksForUpdates = newValue }
    }

    // A menu bar app has no window to bring forward for an update found in
    // the background; Sparkle then reminds gently, as it asks.
    nonisolated var supportsGentleScheduledUpdateReminders: Bool { true }
}
