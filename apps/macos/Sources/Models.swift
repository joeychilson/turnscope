// Wire models exchanged with `libturnscope_app.a` (`crates/turnscope-app/src/view.rs`
// and `state.rs`) plus launch argument parsing. Pure Foundation so `cargo test` can
// compile this file directly to verify the Rust <-> Swift JSON contract.

import Foundation

// MARK: - Launch Arguments (docs/app.md#launch-arguments)

enum Page: Equatable {
    case main, settings
}

enum SettingsTab: String, CaseIterable, Equatable {
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

enum Standing: String, Codable, Equatable {
    case lasts, runningOut, usedUp
}

enum MenuShows: String, Codable, Equatable {
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
        version: "", hasStatus: false, stopped: false,
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
