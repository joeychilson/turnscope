// Opening at login via ServiceManagement's main app service (docs/app.md#opening-at-login).
// Checked off the main thread: SMAppService.mainApp.status measured 1.7 to 6.2 ms (Nov 2026).

import AppKit
import Observation
import ServiceManagement

@MainActor
@Observable
final class Login {
    enum Status {
        case off, on, needsApproval

        var isOn: Bool { self != .off }
    }

    private(set) var status: Status = .off

    func check() {
        Task { status = await Task.detached { Self.readStatus() }.value }
    }

    func set(_ on: Bool, isLive: Bool, log: (String) -> Void) {
        guard isLive else {
            status = on ? .on : .off
            return
        }
        do {
            if on {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
        } catch {
            log("open at login: \(error.localizedDescription)")
            NSSound.beep()
        }
        status = Self.readStatus()
    }

    func openSettings() {
        SMAppService.openSystemSettingsLoginItems()
    }

    private nonisolated static func readStatus() -> Status {
        switch SMAppService.mainApp.status {
        case .enabled: .on
        case .requiresApproval: .needsApproval
        default: .off
        }
    }
}
