// Opening at login via ServiceManagement's main app service (docs/app.md#opening-at-login).
// Checked and updated off the main thread: SMAppService.mainApp.status measured 1.7 to 6.2 ms (Nov 2026).

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
    private var queue: Task<Void, Never>?

    func check() {
        let previous = queue
        queue = Task {
            await previous?.value
            let next = await Task.detached { Self.readStatus() }.value
            status = next
        }
    }

    func set(_ on: Bool, isLive: Bool, log: @escaping @MainActor (String) -> Void) {
        let previous = queue
        guard isLive else {
            queue = Task {
                await previous?.value
                withMotion { status = on ? .on : .off }
            }
            return
        }
        queue = Task {
            await previous?.value
            let (next, errorMessage) = await Task.detached { () -> (Status, String?) in
                var message: String?
                do {
                    if on {
                        try SMAppService.mainApp.register()
                    } else {
                        try SMAppService.mainApp.unregister()
                    }
                } catch {
                    message = error.localizedDescription
                }
                return (Self.readStatus(), message)
            }.value
            if let errorMessage {
                log("open at login: \(errorMessage)")
                NSSound.beep()
            }
            withMotion { status = next }
        }
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
