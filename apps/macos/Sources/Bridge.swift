// Swift bridge to `libturnscope_app.a` (crates/turnscope-app).
// Owns the opaque Rust `TurnscopeApp` handle, decodes `AppSync` view models,
// forwards user actions synchronously, and dispatches one-shot OS side effects.

import Foundation
import Observation
import os
import SwiftUI

// MARK: - Models (decoded from Rust `turnscope-app`)

enum Standing: String, Codable, Equatable {
    case lasts, runningOut, usedUp
}

enum Page: String, Codable, Equatable {
    case main, settings
}

enum SettingsTab: String, Codable, CaseIterable, Equatable {
    case general, agents, accounts

    var title: String {
        switch self {
        case .general: "General"
        case .agents: "Agents"
        case .accounts: "Accounts"
        }
    }
}

enum MenuShows: String, Codable, Equatable {
    case all, one
}

struct Launch: Decodable, Equatable {
    var fixture: String?
    var snapshot: String?
    var opening: Opening?
    var data: String?
    var home: String?
}

struct Opening: Codable, Equatable {
    var account: Bool
    var unused: Bool
    var settings: SettingsTab?
}

struct NotifySettings: Codable, Equatable {
    var runningOut: Bool
    var usedUp: Bool
    var reset: Bool
    var signIn: Bool
}

struct AlertNote: Decodable, Equatable {
    var id: Int64
    var account: String
    var replaces: String
    var signIn: Bool
    var title: String
    var subtitle: String
    var body: String
}

enum Effect: Decodable, Equatable {
    case postAlerts(alerts: [AlertNote])
    case removeNotifications(identifiers: [String])
    case savePreferences(menuShows: MenuShows, dismissedConnect: [String])

    private enum CodingKeys: String, CodingKey {
        case kind, alerts, identifiers, menuShows, dismissedConnect
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "postAlerts":
            self = .postAlerts(alerts: try c.decode([AlertNote].self, forKey: .alerts))
        case "removeNotifications":
            self = .removeNotifications(identifiers: try c.decode([String].self, forKey: .identifiers))
        case "savePreferences":
            self = .savePreferences(
                menuShows: try c.decode(MenuShows.self, forKey: .menuShows),
                dismissedConnect: try c.decode([String].self, forKey: .dismissedConnect)
            )
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "Unknown effect: \(other)")
        }
    }
}

struct MenuBarSlot: Decodable, Equatable {
    var provider: String
    var standing: Standing
    var figure: String
}

struct MenuBarModel: Decodable, Equatable {
    var slots: [MenuBarSlot]
    var spoken: String
    var outOfDate: Bool

    static let empty = MenuBarModel(slots: [], spoken: "Turnscope", outOfDate: false)
}

struct WaitingModel: Decodable, Equatable {
    var problem: String?
    var retries: Bool
}

struct VerdictModel: Decodable, Equatable {
    var headline: String
    var detail: String
    var standing: Standing
}

struct LimitLineModel: Decodable, Equatable, Identifiable {
    var key: String
    var name: String
    var ahead: String?
    var window: String?
    var left: String
    var size: String?
    var spoken: String
    var leftPercent: UInt8
    var timeLeftFraction: Double?
    var standing: Standing

    var id: String { key }
}

struct UsedMostModel: Decodable, Equatable, Identifiable {
    var session: String
    var title: String
    var detail: String
    var projectPath: String?

    var id: String { session }
}

struct AccountRowModel: Decodable, Equatable, Identifiable {
    var id: String
    var provider: String
    var title: String
    var label: String?
    var called: String
    var standing: Standing
    var status: String
    var figure: String
    var figureThen: String?
    var figureSpoken: String
    var collapsedSpoken: String
    var openable: Bool
    var limits: [LimitLineModel]
    var note: String?
    var usedMostHeading: String?
    var usedMost: [UsedMostModel]
}

struct HiddenAccountModel: Decodable, Equatable, Identifiable {
    var id: String
    var provider: String
    var title: String
    var label: String?
    var via: String?
    var called: String
}

struct ConnectOfferModel: Decodable, Equatable {
    var id: String
    var name: String
    var note: String?
    var configured: String?
    var changing: Bool
    var outdated: Bool
    var verb: String
    var help: String
}

enum FooterNoteModel: Decodable, Equatable {
    case stale(text: String, help: String, spoken: String)
    case reading(text: String, help: String, spoken: String)
    case refreshing(text: String, spoken: String)

    private enum CodingKeys: String, CodingKey {
        case kind, text, help, spoken
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let text = try c.decode(String.self, forKey: .text)
        let spoken = try c.decode(String.self, forKey: .spoken)
        switch try c.decode(String.self, forKey: .kind) {
        case "stale":
            self = .stale(text: text, help: try c.decode(String.self, forKey: .help), spoken: spoken)
        case "reading":
            self = .reading(text: text, help: try c.decode(String.self, forKey: .help), spoken: spoken)
        case "refreshing":
            self = .refreshing(text: text, spoken: spoken)
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "Unknown footer note: \(other)")
        }
    }
}

struct EmptyMessage: Decodable, Equatable {
    var headline: String
    var detail: String
}

struct SnippetModel: Decodable, Equatable, Identifiable {
    var title: String
    var detail: String
    var file: String
    var json: String

    var id: String { title }
}

struct AgentRowModel: Decodable, Equatable, Identifiable {
    var id: String
    var name: String
    var note: String?
    var configured: String?
    var changing: String?
    var connected: Bool
    var enabled: Bool
    var snippets: [SnippetModel]
}

struct AccountSwitchModel: Decodable, Equatable, Identifiable {
    var id: String
    var provider: String
    var title: String
    var label: String?
    var via: String?
    var called: String
    var shown: Bool
    var enabled: Bool
}

struct SettingsModel: Decodable, Equatable {
    var menuShows: MenuShows
    var notify: NotifySettings?
    var canEditNotify: Bool
    var agentsEmpty: EmptyMessage?
    var agentsTitle: String
    var agents: [AgentRowModel]
    var accountsEmpty: EmptyMessage?
    var subscriptions: [AccountSwitchModel]
    var apiKeys: [AccountSwitchModel]

    static let empty = SettingsModel(
        menuShows: .all, notify: nil, canEditNotify: false,
        agentsEmpty: nil, agentsTitle: "", agents: [],
        accountsEmpty: nil, subscriptions: [], apiKeys: []
    )
}

struct PanelModel: Decodable, Equatable {
    var version: String
    var hasStatus: Bool
    var stopped: Bool
    var fromDiskImage: Bool
    var waiting: WaitingModel?
    var verdict: VerdictModel?
    var inUse: [AccountRowModel]
    var recent: [AccountRowModel]
    var unused: [AccountRowModel]
    var hidden: [HiddenAccountModel]
    var unusedFoldLabel: String?
    var hiddenFoldLabel: String?
    var connectOffer: ConnectOfferModel?
    var footerNote: FooterNoteModel?
    var settings: SettingsModel

    static let empty = PanelModel(
        version: "", hasStatus: false, stopped: false, fromDiskImage: false,
        waiting: WaitingModel(problem: nil, retries: false),
        verdict: nil, inUse: [], recent: [], unused: [], hidden: [],
        unusedFoldLabel: nil, hiddenFoldLabel: nil, connectOffer: nil,
        footerNote: nil, settings: .empty
    )
}

struct AppSync: Decodable, Equatable {
    var animated: Bool
    var counting: Bool
    var menuBar: MenuBarModel
    var panel: PanelModel
    var effects: [Effect]
}

// MARK: - Actions & Configs (encoded to Rust)

private struct StartConfig: Encodable {
    var fixture: String?
    var data: String?
    var home: String?
    var fromDiskImage: Bool
    var menuShows: MenuShows
    var dismissedConnect: [String]
}

private struct SnapshotConfig: Encodable {
    var fixture: String?
    var data: String?
    var home: String?
}

enum SnapshotResponse: Decodable {
    case ok(PanelModel)
    case err(String, Int32)

    private enum CodingKeys: String, CodingKey {
        case ok, error, code
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if let panel = try c.decodeIfPresent(PanelModel.self, forKey: .ok) {
            self = .ok(panel)
        } else {
            self = .err(
                try c.decode(String.self, forKey: .error),
                try c.decode(Int32.self, forKey: .code)
            )
        }
    }
}

enum AppAction: Encodable {
    case panelOpened
    case panelClosed
    case setHidden(id: String, hidden: Bool)
    case setConnected(id: String, connecting: Bool)
    case setNotify(notify: NotifySettings)
    case setMenuShows(shows: MenuShows)
    case dismissConnect(id: String)
    case acknowledgeAlert(id: Int64, signInAccount: String?)
    case refreshNow
    case tick

    private enum CodingKeys: String, CodingKey {
        case kind, id, hidden, connecting, notify, shows, signInAccount
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .panelOpened:
            try c.encode("panelOpened", forKey: .kind)
        case .panelClosed:
            try c.encode("panelClosed", forKey: .kind)
        case let .setHidden(id, hidden):
            try c.encode("setHidden", forKey: .kind)
            try c.encode(id, forKey: .id)
            try c.encode(hidden, forKey: .hidden)
        case let .setConnected(id, connecting):
            try c.encode("setConnected", forKey: .kind)
            try c.encode(id, forKey: .id)
            try c.encode(connecting, forKey: .connecting)
        case let .setNotify(notify):
            try c.encode("setNotify", forKey: .kind)
            try c.encode(notify, forKey: .notify)
        case let .setMenuShows(shows):
            try c.encode("setMenuShows", forKey: .kind)
            try c.encode(shows, forKey: .shows)
        case let .dismissConnect(id):
            try c.encode("dismissConnect", forKey: .kind)
            try c.encode(id, forKey: .id)
        case let .acknowledgeAlert(id, signInAccount):
            try c.encode("acknowledgeAlert", forKey: .kind)
            try c.encode(id, forKey: .id)
            try c.encodeIfPresent(signInAccount, forKey: .signInAccount)
        case .refreshNow:
            try c.encode("refreshNow", forKey: .kind)
        case .tick:
            try c.encode("tick", forKey: .kind)
        }
    }
}

// MARK: - C helpers

private func takeRustString(_ ptr: UnsafeMutablePointer<CChar>?) -> String? {
    guard let ptr else { return nil }
    defer { turnscope_app_free_string(ptr) }
    return String(cString: ptr)
}

func parseLaunch(_ arguments: [String]) -> Launch {
    let cStrings = arguments.map { strdup($0) }
    defer { cStrings.forEach { free($0) } }
    let constPtrs = cStrings.map { UnsafePointer<CChar>($0) }
    let json = constPtrs.withUnsafeBufferPointer { buf in
        takeRustString(turnscope_app_parse_launch(Int32(buf.count), buf.baseAddress))
    }
    guard let data = json?.data(using: .utf8),
          let launch = try? JSONDecoder().decode(Launch.self, from: data)
    else { return Launch() }
    return launch
}

func snapshotPanel(_ launch: Launch) -> SnapshotResponse {
    let cfg = SnapshotConfig(
        fixture: launch.fixture,
        data: launch.data,
        home: launch.home
    )
    guard let encoded = try? JSONEncoder().encode(cfg),
          let cfgText = String(data: encoded, encoding: .utf8),
          let json = cfgText.withCString({ takeRustString(turnscope_app_snapshot($0)) }),
          let data = json.data(using: .utf8),
          let response = try? JSONDecoder().decode(SnapshotResponse.self, from: data)
    else {
        return .err("Turnscope: snapshot couldn't be prepared", 1)
    }
    return response
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

    func reset() {
        page = .main
        tab = .general
        openAccount = nil
        unusedOpen = false
        hiddenShown = false
        pendingAccount = nil
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
    private nonisolated let wakePending = OSAllocatedUnfairLock(initialState: false)
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    /// Initialize for live or `--fixture` mode. Exits with code 2 if `--fixture` can't be read.
    init(launch: Launch, fromDiskImage: Bool, preferences: Preferences) {
        self.isLive = launch.fixture == nil
        let cfg = StartConfig(
            fixture: launch.fixture,
            data: launch.data,
            home: launch.home,
            fromDiskImage: fromDiskImage,
            menuShows: preferences.menuShows,
            dismissedConnect: preferences.dismissedConnect
        )
        guard let encoded = try? encoder.encode(cfg),
              let cfgText = String(data: encoded, encoding: .utf8)
        else { fatalError("Failed to encode StartConfig") }

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
        self.panel = snapshot
    }

    func sync() {
        guard let handle,
              let json = takeRustString(turnscope_app_sync(handle)),
              let sync = decodeSync(json)
        else { return }
        apply(sync)
    }

    func send(_ action: AppAction) {
        guard let handle,
              let data = try? encoder.encode(action),
              let text = String(data: data, encoding: .utf8),
              let json = text.withCString({ takeRustString(turnscope_app_action(handle, $0)) }),
              let sync = decodeSync(json)
        else { return }
        apply(sync)
    }

    func log(_ line: String) {
        guard let handle else { return }
        line.withCString { turnscope_app_log(handle, $0) }
    }

    func diagnostics() -> String {
        guard let handle,
              let report = takeRustString(turnscope_app_diagnostics(handle))
        else { return "Turnscope \(panel.version)\n" }
        return report
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
        sync()
    }

    private func decodeSync(_ json: String) -> AppSync? {
        guard let data = json.data(using: .utf8) else { return nil }
        return try? decoder.decode(AppSync.self, from: data)
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
