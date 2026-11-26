// Updates via Sparkle with gentle scheduled reminders (docs/app.md#updates).
// Starts only when `SUPublicEDKey` is set in Info.plist.

import AppKit
import Observation
import Sparkle

@MainActor
@Observable
final class Updater: NSObject, SPUStandardUserDriverDelegate {
    private var controller: SPUStandardUpdaterController?
    private var canCheckObservation: NSKeyValueObservation?
    var available: Bool { controller != nil }
    private(set) var canCheckForUpdates = false
    private(set) var waiting = false
    private(set) var checksAutomatically = false

    func start() {
        let key = Bundle.main.object(forInfoDictionaryKey: "SUPublicEDKey") as? String ?? ""
        guard !key.isEmpty else { return }
        let controller = SPUStandardUpdaterController(
            startingUpdater: true,
            updaterDelegate: nil,
            userDriverDelegate: self
        )
        self.controller = controller
        canCheckForUpdates = controller.updater.canCheckForUpdates
        canCheckObservation = controller.updater.observe(\.canCheckForUpdates, options: [.new]) { [weak self] _, change in
            let canCheck = change.newValue ?? false
            Task { @MainActor [weak self] in
                self?.canCheckForUpdates = canCheck
            }
        }
        refreshAutomatic()
    }

    func refreshAutomatic() {
        checksAutomatically = controller?.updater.automaticallyChecksForUpdates ?? false
    }

    func check() {
        NSApp.activate()
        controller?.checkForUpdates(nil)
    }

    func setAutomatic(_ on: Bool) {
        controller?.updater.automaticallyChecksForUpdates = on
        checksAutomatically = on
    }

    nonisolated var supportsGentleScheduledUpdateReminders: Bool { true }

    nonisolated func standardUserDriverShouldHandleShowingScheduledUpdate(
        _ update: SUAppcastItem,
        andInImmediateFocus immediateFocus: Bool
    ) -> Bool {
        immediateFocus
    }

    nonisolated func standardUserDriverWillHandleShowingUpdate(
        _ handleShowingUpdate: Bool,
        forUpdate update: SUAppcastItem,
        state: SPUUserUpdateState
    ) {
        guard !handleShowingUpdate else { return }
        MainActor.assumeIsolated { waiting = true }
    }

    nonisolated func standardUserDriverWillFinishUpdateSession() {
        MainActor.assumeIsolated { waiting = false }
    }
}
