// Wire models exchanged with `libturnscope_app.a` (`crates/turnscope-app/src/view.rs`
// and `state.rs`), launch argument parsing, and panel placement. Pure Foundation /
// CoreGraphics so `cargo test` can compile this file directly to verify the contract.

import CoreGraphics
import Foundation

// MARK: - Panel Placement

func panelFrame(
    midX: CGFloat,
    underY: CGFloat,
    visible: CGRect,
    panelWidth: CGFloat,
    gutter: CGFloat
) -> CGRect {
    let left = max(visible.minX + 6, min(midX - panelWidth / 2, visible.maxX - panelWidth - 6))
    let top = underY - 6
    let height = max(200, top - visible.minY)
    return CGRect(
        x: left - gutter,
        y: visible.minY,
        width: panelWidth + gutter * 2,
        height: height
    )
}

// MARK: - Launch Arguments (docs/app.md#launch-arguments)

enum Page {
    case main, settings
}

enum SettingsTab: String, CaseIterable {
    case general, agents, accounts

    var title: String {
        switch self {
        case .general: "General"
        case .agents: "Agents"
        case .accounts: "Accounts"
        }
    }
}

struct Opening: Equatable {
    var account = false
    var unused = false
    var settings: SettingsTab?

    static func parse(_ names: String) -> Opening {
        var opening = Opening()
        for name in names.split(separator: ",") {
            switch name {
            case "account": opening.account = true
            case "unused": opening.unused = true
            case "settings": opening.settings = .general
            case "agents": opening.settings = .agents
            case "accounts": opening.settings = .accounts
            default: break
            }
        }
        return opening
    }
}

struct Launch: Equatable {
    var fixture: String?
    var snapshot: String?
    var opening: Opening?
    var data: String?
    var home: String?

    static func parse(_ arguments: [String]) -> Launch {
        let args = Array(arguments.dropFirst())
        func value(_ option: String) -> String? {
            guard let index = args.firstIndex(of: option), index + 1 < args.count else {
                return nil
            }
            let next = args[index + 1]
            return next.hasPrefix("--") ? nil : next
        }
        return Launch(
            fixture: value("--fixture"),
            snapshot: value("--snapshot"),
            opening: args.contains("--open") ? Opening.parse(value("--open") ?? "") : nil,
            data: value("--data"),
            home: value("--home")
        )
    }
}

// MARK: - Decoded from Rust (`turnscope-app`)

enum Standing: String, Codable {
    case lasts, runningOut, usedUp
}

enum MenuShows: String, Codable {
    case all, one
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
}

struct MenuBarSlot: Decodable, Equatable {
    var provider: String
    var standing: Standing
    var figure: String
    var outOfDate: Bool
}

struct MenuBarModel: Decodable, Equatable {
    var slots: [MenuBarSlot]
    var spoken: String
    var outOfDate: Bool
}

struct WaitingModel: Decodable, Equatable {
    var reading: String
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
    var id: String
    var sessionId: String
    var title: String
    var detail: String
    var projectPath: String?
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

struct AccountModel: Decodable, Equatable, Identifiable {
    var id: String
    var provider: String
    var title: String
    var label: String?
    var via: String?
    var called: String
    var shown: Bool
}

struct ConnectOfferModel: Decodable, Equatable {
    var id: String
    var name: String
    var note: String?
    var configured: String?
    var changing: Bool
    var verb: String
    var help: String
}

struct FooterNoteModel: Decodable, Equatable {
    var text: String
    var help: String?
    var spoken: String
    var busy: Bool
}

struct EmptyMessage: Decodable, Equatable {
    var headline: String
    var detail: String
}

struct AgentRowModel: Decodable, Equatable, Identifiable {
    var id: String
    var name: String
    var note: String?
    var configured: String?
    var changing: String?
    var connected: Bool
    var enabled: Bool
}

struct SettingsModel: Decodable, Equatable {
    var menuShows: MenuShows
    var notify: NotifySettings?
    var agentsEmpty: EmptyMessage?
    var agentsTitle: String
    var agents: [AgentRowModel]
    var subscriptions: [AccountModel]
    var apiKeys: [AccountModel]

    static let empty = SettingsModel(
        menuShows: .all, notify: nil,
        agentsEmpty: nil, agentsTitle: "", agents: [],
        subscriptions: [], apiKeys: []
    )
}

struct PanelModel: Decodable, Equatable {
    var version: String
    var stopped: Bool
    var canRefresh: Bool
    var waiting: WaitingModel?
    var verdict: VerdictModel?
    var inUse: [AccountRowModel]
    var recent: [AccountRowModel]
    var unused: [AccountRowModel]
    var hidden: [AccountModel]
    var unusedFoldLabel: String?
    var hiddenFoldLabel: String?
    var connectOffer: ConnectOfferModel?
    var footerNote: FooterNoteModel?
    var settings: SettingsModel

    static let empty = PanelModel(
        version: "", stopped: false, canRefresh: false,
        waiting: WaitingModel(reading: "", problem: nil, retries: false),
        verdict: nil, inUse: [], recent: [], unused: [], hidden: [],
        unusedFoldLabel: nil, hiddenFoldLabel: nil, connectOffer: nil,
        footerNote: nil, settings: .empty
    )
}

struct AppSync: Decodable {
    var animated: Bool
    var counting: Bool
    var menuBar: MenuBarModel
    var panel: PanelModel
    var effects: [Effect]
}

// MARK: - Encoded to Rust (`turnscope-app`)

struct StartConfig: Encodable {
    var fixture: String?
    var data: String?
    var home: String?
    var fromDiskImage: Bool
    var menuShows: MenuShows
    var dismissedConnect: [String]
}

struct SnapshotConfig: Encodable {
    var fixture: String?
    var data: String?
    var home: String?
}

enum SnapshotResponse: Decodable {
    case ok(panel: PanelModel)
    case err(error: String, code: Int32)
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
}
