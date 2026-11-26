// Opening at login via ServiceManagement's main app service (docs/app.md#opening-at-login).
// Read off the main thread: SMAppService.mainApp.status measured 1.7 to 6.2 ms (Nov 2026).

import AppKit
import Observation
import ServiceManagement

/// Reads and changes whether macOS opens the app at login, and tells the core what it says.
/// With `--fixture`, nothing of the Mac's is read or changed, and it starts off.
@MainActor
@Observable
final class Login {
    private let bridge: Bridge

    init(bridge: Bridge) {
        self.bridge = bridge
    }

    func read() {
        guard bridge.isLive else {
            bridge.send(.openAtLoginRead(status: .off))
            return
        }
        Task { bridge.send(.openAtLoginRead(status: await Task.detached { Self.status() }.value)) }
    }

    /// Returns what macOS says once it's set.
    func set(_ on: Bool) -> OpenAtLogin {
        var status: OpenAtLogin = on ? .on : .off
        if bridge.isLive {
            do {
                if on {
                    try SMAppService.mainApp.register()
                } else {
                    try SMAppService.mainApp.unregister()
                }
            } catch {
                bridge.log("open at login: \(error.localizedDescription)")
                NSSound.beep()
            }
            status = Self.status()
        }
        bridge.send(.openAtLoginRead(status: status))
        return status
    }

    func openSettings() {
        SMAppService.openSystemSettingsLoginItems()
    }

    private nonisolated static func status() -> OpenAtLogin {
        switch SMAppService.mainApp.status {
        case .enabled: .on
        case .requiresApproval: .needsApproval
        default: .off
        }
    }
}
