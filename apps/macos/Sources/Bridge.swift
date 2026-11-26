// The handle to `libturnscope_app.a` (`crates/turnscope-app`), and the panel's state that
// stays in Swift.

import Foundation
import Observation
import os
import SwiftUI

// MARK: - C Helpers

private func takeRustString(_ ptr: UnsafeMutablePointer<CChar>?) -> String? {
    guard let ptr else { return nil }
    defer { turnscope_app_free_string(ptr) }
    return String(cString: ptr)
}

/// These models are strings, numbers and flags, which JSONEncoder can't fail on.
private func json(_ value: some Encodable) -> String {
    String(decoding: try! JSONEncoder().encode(value), as: UTF8.self)
}

func snapshotPanel(_ launch: Launch) -> SnapshotResponse {
    let config = json(SnapshotConfig(fixture: launch.fixture, data: launch.data, home: launch.home))
    let answer = config.withCString { takeRustString(turnscope_app_snapshot($0)) } ?? ""
    do {
        return try JSONDecoder().decode(SnapshotResponse.self, from: Data(answer.utf8))
    } catch {
        return .err(error: "Turnscope: the snapshot can't be read: \(error)", code: 1)
    }
}

private func rustWakeCallback(_ ctx: UnsafeMutableRawPointer?) {
    guard let ctx else { return }
    let bridge = Unmanaged<Bridge>.fromOpaque(ctx).takeUnretainedValue()
    bridge.scheduleSync()
}

// MARK: - Local Ephemeral UI State (zero FFI/JSON on click)

@MainActor
@Observable
final class UIState {
    var page: Page = .main
    var tab: SettingsTab = .general
    var openAccount: String?
    var unusedOpen = false
    var hiddenShown = false
    var pendingAccount: String?
    var focusNonce = 0

    func reset() {
        page = .main
        tab = .general
        openAccount = nil
        unusedOpen = false
        hiddenShown = false
        pendingAccount = nil
    }

    func panelDidShow() {
        focusNonce &+= 1
    }

    func apply(_ opening: Opening, panel: PanelModel) {
        if opening.account {
            openAccount = panel.inUse.first(where: \.openable)?.id
        }
        if opening.unused {
            unusedOpen = true
        }
        if let tab = opening.settings {
            page = .settings
            self.tab = tab
        }
    }

    func openInPlace(_ account: String, panel: PanelModel) {
        if panel.unused.contains(where: { $0.id == account }) {
            unusedOpen = true
        } else if !panel.inUse.contains(where: { $0.id == account })
            && !panel.recent.contains(where: { $0.id == account })
        {
            return
        }
        page = .main
        openAccount = account
    }

    func toggleAccount(_ id: String) {
        withMotion {
            openAccount = (openAccount == id) ? nil : id
        }
    }

    func toggleUnused(panel: PanelModel) {
        withMotion {
            unusedOpen.toggle()
            if !unusedOpen {
                if let open = openAccount, panel.unused.contains(where: { $0.id == open }) {
                    openAccount = nil
                }
                hiddenShown = false
            }
        }
    }

    func toggleHidden() {
        withMotion {
            hiddenShown.toggle()
        }
    }

    func go(_ page: Page) {
        withMotion(fade: true) {
            self.page = page
        }
    }

    func setTab(_ tab: SettingsTab) {
        withMotion(fade: true) {
            self.tab = tab
        }
    }

    func didHide(_ id: String) {
        if openAccount == id {
            openAccount = nil
        }
    }
}

// MARK: - Observable App State Bridge

@MainActor
@Observable
final class Bridge {
    private(set) var panel: PanelModel = .empty
    /// Visible height of the screen the panel opens on, for overflow scroll caps.
    var screenHeight: CGFloat = 800

    let isLive: Bool
    var onSync: ((AppSync) -> Void)?

    private var handle: OpaquePointer?
    private let location: SnapshotConfig
    private nonisolated let wakePending = OSAllocatedUnfairLock(initialState: false)
    private let decoder = JSONDecoder()

    /// Initialize for live or `--fixture` mode. Exits with code 2 if `--fixture` can't be read.
    init(launch: Launch, fromDiskImage: Bool, preferences: Preferences) {
        self.isLive = launch.fixture == nil
        self.location = SnapshotConfig(fixture: launch.fixture, data: launch.data, home: launch.home)
        let config = json(StartConfig(
            fixture: launch.fixture,
            data: launch.data,
            home: launch.home,
            fromDiskImage: fromDiskImage,
            menuShows: preferences.menuShows,
            dismissedConnect: preferences.dismissedConnect
        ))
        var errorPtr: UnsafeMutablePointer<CChar>?
        let ctx = Unmanaged.passUnretained(self).toOpaque()
        let started = config.withCString { ptr in
            turnscope_app_start(ptr, ctx, rustWakeCallback, &errorPtr)
        }
        guard let started else {
            let message = takeRustString(errorPtr) ?? "Turnscope: failed to start"
            fputs("\(message)\n", stderr)
            exit(2)
        }
        self.handle = started
    }

    /// Initialize with a fixed `PanelModel` for headless `--snapshot` rendering.
    init(snapshot: PanelModel) {
        self.isLive = false
        self.location = SnapshotConfig(fixture: "snapshot", data: nil, home: nil)
        self.panel = snapshot
    }

    func sync() {
        guard let handle else { return }
        take(takeRustString(turnscope_app_sync(handle)), from: "turnscope_app_sync")
    }

    func send(_ action: AppAction) {
        guard let handle else { return }
        let answer = json(action).withCString { takeRustString(turnscope_app_action(handle, $0)) }
        take(answer, from: "turnscope_app_action")
    }

    func log(_ line: String) {
        guard let handle else { return }
        line.withCString { turnscope_app_log(handle, $0) }
    }

    /// Computes the diagnostics report off the main thread because `doctor` checks
    /// Keychain logins and scans SQLite.
    func diagnostics() async -> String {
        let config = json(location)
        return await Task.detached(priority: .userInitiated) {
            config.withCString { takeRustString(turnscope_app_diagnostics($0)) } ?? ""
        }.value
    }

    func logURL() -> URL? {
        guard let handle,
              let path = takeRustString(turnscope_app_log_path(handle))
        else { return nil }
        return URL(fileURLWithPath: path)
    }

    nonisolated func scheduleSync() {
        let shouldDispatch = wakePending.withLock { pending -> Bool in
            if pending { return false }
            pending = true
            return true
        }
        guard shouldDispatch else { return }
        DispatchQueue.main.async { [weak self] in
            self?.drainWake()
        }
    }

    private func drainWake() {
        wakePending.withLock { $0 = false }
        // Leave effects queued in Rust until `applicationDidFinishLaunching` wires `onSync`.
        guard onSync != nil else { return }
        sync()
    }

    /// Applies the `AppSync` that `call` answered, or says why there's none.
    private func take(_ answer: String?, from call: String) {
        guard let answer else { return reportError("\(call) returned null") }
        do {
            apply(try decoder.decode(AppSync.self, from: Data(answer.utf8)))
        } catch {
            reportError("failed to decode AppSync: \(error)")
        }
    }

    private func reportError(_ message: String) {
        fputs("Turnscope: \(message)\n", stderr)
        log(message)
    }

    private func apply(_ sync: AppSync) {
        let update = {
            if self.panel != sync.panel { self.panel = sync.panel }
        }
        if sync.animated {
            withMotion(update)
        } else {
            update()
        }
        onSync?(sync)
    }
}
