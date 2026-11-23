// Swift bridge to `libturnscope_app.a` (`crates/turnscope-app`).
// Owns the opaque Rust `TurnscopeApp` handle, decodes `AppSync` view models,
// forwards domain actions synchronously, and holds ephemeral Swift UI state.

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

func snapshotPanel(_ launch: Launch) -> SnapshotResponse {
    let cfg = SnapshotConfig(
        fixture: launch.fixture,
        data: launch.data,
        home: launch.home
    )
    do {
        let encoded = try JSONEncoder().encode(cfg)
        guard let cfgText = String(data: encoded, encoding: .utf8),
              let json = cfgText.withCString({ takeRustString(turnscope_app_snapshot($0)) }),
              let data = json.data(using: .utf8)
        else {
            return .err("Turnscope: snapshot returned empty response", 1)
        }
        return try JSONDecoder().decode(SnapshotResponse.self, from: data)
    } catch {
        return .err("Turnscope: snapshot couldn't be decoded: \(error)", 1)
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
        withAnimation(spring) {
            openAccount = (openAccount == id) ? nil : id
        }
    }

    func toggleUnused(panel: PanelModel) {
        withAnimation(spring) {
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
        withAnimation(spring) {
            hiddenShown.toggle()
        }
    }

    func go(_ page: Page, tab: SettingsTab? = nil) {
        withAnimation(spring) {
            if let tab { self.tab = tab }
            self.page = page
        }
    }

    func setTab(_ tab: SettingsTab) {
        withAnimation(spring) {
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
    private(set) var menuBar: MenuBarModel = .empty
    private(set) var counting = false
    /// Visible height of the screen the panel opens on, for overflow scroll caps.
    var screenHeight: CGFloat = 800

    let isLive: Bool
    var onSync: ((AppSync) -> Void)?

    private var handle: OpaquePointer?
    private let location: SnapshotConfig
    private nonisolated let wakePending = OSAllocatedUnfairLock(initialState: false)
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    /// Initialize for live or `--fixture` mode. Exits with code 2 if `--fixture` can't be read.
    init(launch: Launch, fromDiskImage: Bool, preferences: Preferences) {
        self.isLive = launch.fixture == nil
        self.location = SnapshotConfig(fixture: launch.fixture, data: launch.data, home: launch.home)
        let cfg = StartConfig(
            fixture: launch.fixture,
            data: launch.data,
            home: launch.home,
            fromDiskImage: fromDiskImage,
            menuShows: preferences.menuShows,
            dismissedConnect: preferences.dismissedConnect
        )
        let cfgText: String
        do {
            let encoded = try encoder.encode(cfg)
            guard let text = String(data: encoded, encoding: .utf8) else {
                fatalError("StartConfig encoded to non-UTF8 bytes")
            }
            cfgText = text
        } catch {
            fatalError("Failed to encode StartConfig: \(error)")
        }

        var errorPtr: UnsafeMutablePointer<CChar>?
        let ctx = Unmanaged.passUnretained(self).toOpaque()
        let started = cfgText.withCString { ptr in
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
        guard let json = takeRustString(turnscope_app_sync(handle)) else {
            reportError("turnscope_app_sync returned null")
            return
        }
        guard let sync = decodeSync(json) else { return }
        apply(sync)
    }

    func send(_ action: AppAction) {
        guard let handle else { return }
        let text: String
        do {
            let data = try encoder.encode(action)
            guard let utf8 = String(data: data, encoding: .utf8) else {
                reportError("AppAction encoded to non-UTF8 bytes")
                return
            }
            text = utf8
        } catch {
            reportError("failed to encode AppAction: \(error)")
            return
        }
        guard let json = text.withCString({ takeRustString(turnscope_app_action(handle, $0)) }) else {
            reportError("turnscope_app_action returned null")
            return
        }
        guard let sync = decodeSync(json) else { return }
        apply(sync)
    }

    func log(_ line: String) {
        guard let handle else { return }
        line.withCString { turnscope_app_log(handle, $0) }
    }

    /// Computes the diagnostics report off the main thread because `doctor` checks
    /// Keychain logins and scans SQLite.
    func diagnostics() async -> String {
        let fallback = "Turnscope \(panel.version)\n"
        guard let encoded = try? encoder.encode(location),
              let cfgText = String(data: encoded, encoding: .utf8)
        else { return fallback }
        return await Task.detached(priority: .userInitiated) {
            cfgText.withCString { takeRustString(turnscope_app_diagnostics($0)) } ?? fallback
        }.value
    }

    func logURL() -> URL? {
        guard let handle,
              let path = takeRustString(turnscope_app_log_path(handle))
        else { return nil }
        return URL(fileURLWithPath: path)
    }

    func stop() {
        guard let handle else { return }
        self.handle = nil
        turnscope_app_stop(handle)
    }

    nonisolated func scheduleSync() {
        let shouldDispatch = wakePending.withLock { pending -> Bool in
            if pending { return false }
            pending = true
            return true
        }
        guard shouldDispatch else { return }
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated {
                self?.drainWake()
            }
        }
    }

    private func drainWake() {
        wakePending.withLock { $0 = false }
        // Leave effects queued in Rust until `applicationDidFinishLaunching` wires `onSync`.
        guard onSync != nil else { return }
        sync()
    }

    private func decodeSync(_ json: String) -> AppSync? {
        guard let data = json.data(using: .utf8) else {
            reportError("AppSync JSON was not valid UTF-8")
            return nil
        }
        do {
            return try decoder.decode(AppSync.self, from: data)
        } catch {
            reportError("failed to decode AppSync: \(error)")
            return nil
        }
    }

    private func reportError(_ message: String) {
        fputs("Turnscope: \(message)\n", stderr)
        log(message)
    }

    private func apply(_ sync: AppSync) {
        let update = {
            if self.panel != sync.panel { self.panel = sync.panel }
            if self.menuBar != sync.menuBar { self.menuBar = sync.menuBar }
            if self.counting != sync.counting { self.counting = sync.counting }
        }
        if sync.animated {
            withAnimation(spring, update)
        } else {
            update()
        }
        onSync?(sync)
    }
}
