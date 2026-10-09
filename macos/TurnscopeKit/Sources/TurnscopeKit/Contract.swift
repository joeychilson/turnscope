// The messages between `turnscope serve` and the app. Every rule behind them
// is on the Rust side; the app only shows them. Field names are camelCase and
// times are UTC milliseconds. `contract/` has a hand-written example of each
// message, which the tests decode. Fields the app doesn't show aren't modeled
// here; decoding skips them.
//
// The app only talks to the engine it ships with (`Client`), so an unknown
// value is never guessed at: it fails to decode, like anything malformed.

import Foundation

/// The protocol version this app speaks.
public let protocolVersion = 1

// MARK: Status

/// Everything the app shows, as of `at`.
public struct Status: Decodable, Equatable, Sendable {
    public var at: Date
    /// When the agents' history was last read through. Nil while it's being
    /// read for the first time.
    public var historyReadAt: Date?
    /// Every account, in the order to list them: accounts in use first,
    /// most urgent first.
    public var accounts: [AccountStatus]
    /// The agents Turnscope reads, installed or not.
    public var agents: [AgentStatus]
    public var settings: Settings
}

/// An agent Turnscope reads.
public struct AgentStatus: Decodable, Equatable, Identifiable, Sendable {
    /// The engine's id for it: "claude-code".
    public var id: String
    /// "Claude Code".
    public var name: String
    public var installed: Bool
    /// Whether it runs Turnscope's MCP server.
    public var connection: Connection
    /// Whether its responses drew on an account in the last half hour.
    public var inUse: Bool
}

/// Whether an agent runs Turnscope's MCP server.
public enum Connection: String, Decodable, Sendable {
    /// It runs this copy of Turnscope.
    case connected
    /// It runs another copy, such as one that has since moved.
    case outdated
    case available
}

/// What an account pays with.
public enum AccountKind: String, Decodable, Sendable {
    case subscription
    case apiKey
}

/// An account, how it was last read, and its limits. `title` is its name
/// without anything to tell it apart, such as "Claude Max". `label` tells it
/// apart, such as its sign-in's email.
public struct AccountStatus: Decodable, Equatable, Identifiable, Sendable {
    public var id: String
    /// Its provider's id: "anthropic".
    public var provider: String
    public var kind: AccountKind
    public var title: String
    public var label: String?
    /// The agents signed into it, or that hold its key, by id.
    public var agents: [String]
    /// Whether an agent's responses drew on it in the last half hour.
    public var inUse: Bool
    /// In use, or drawn on in the last 7 days: an account you use.
    public var recent: Bool
    public var hidden: Bool
    public var state: AccountState
    public var limits: [LimitStatus]
    /// The key of the limit that decides how it stands. If every limit
    /// reset since it was read, it's the last one to reset.
    public var deciding: String?
    /// For an API-key account, what it cost this month.
    public var spend: Spend?
}

/// How an account's limits were last read.
public enum AccountState: Decodable, Equatable, Sendable {
    /// Read on the last try.
    case live(readAt: Date)
    /// As of an earlier read, and why it hasn't been read since.
    case asOf(readAt: Date, why: Stale)
    /// Never read, and why.
    case unread(why: Stale)

    private enum Keys: String, CodingKey { case kind, readAt, why }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "live": self = .live(readAt: try c.decode(Date.self, forKey: .readAt))
        case "asOf": self = .asOf(readAt: try c.decode(Date.self, forKey: .readAt),
                                  why: try c.decode(Stale.self, forKey: .why))
        case "unread": self = .unread(why: try c.decode(Stale.self, forKey: .why))
        case let kind: throw unknownKind(kind, at: .kind, in: c)
        }
    }
}

extension AccountState {
    /// Why it isn't read now, if it isn't.
    public var why: Stale? {
        switch self {
        case .live: nil
        case .asOf(_, let why): why
        case .unread(let why): why
        }
    }
}

/// Why an account's limits aren't read now.
public enum Stale: String, Decodable, Sendable {
    /// No agent here is signed into it now.
    case signedOut
    /// Its provider refused the login.
    case signIn
    /// Its provider couldn't be reached, or gave an answer that wasn't
    /// understood.
    case readFailed
    /// Every login to it has expired. Its agent renews one the next time
    /// it's used. This isn't a refusal, so nothing asks to sign in.
    case expired
}

/// One limit as last read, with its outlook.
public struct LimitStatus: Decodable, Equatable, Identifiable, Sendable {
    public var key: String
    /// Its provider's name for it: "5 hours", "Weekly".
    public var name: String
    /// The one model it applies to, if it doesn't apply to all.
    public var scope: String?
    public var window: LimitWindow?
    /// Percent left, rounded down so it's never overstated. It's 100 if its
    /// window reset since it was read.
    public var leftPercent: Int
    /// For a limit of money, its size and what's left.
    public var money: Money?
    /// When its window reset, if it has since it was read. It's shown full,
    /// but use elsewhere isn't seen until it's read again.
    public var resetSince: Date?
    /// For an API key's own limit, the agents that hold the key, by id.
    public var heldBy: [String]
    public var outlook: Outlook

    public var id: String { key }
}

/// A limit's window.
public struct LimitWindow: Decodable, Equatable, Sendable {
    /// When it resets, and how soon that is.
    public var resets: Moment
}

/// A future time, with how soon it is, which decides how it's worded.
public struct Moment: Decodable, Equatable, Sendable {
    public var at: Date
    public var horizon: Horizon
}

/// A limit of money.
public struct Money: Decodable, Equatable, Sendable {
    public var sizeUsd: Double
    public var leftUsd: Double
}

/// Where a limit is headed, as the engine decided.
public enum Outlook: Decodable, Equatable, Sendable {
    /// It lasts until it resets.
    case lasts
    /// At the pace so far, it runs out before it resets, likely at this
    /// time.
    case runsOut(likely: Moment)
    /// It's used up, and back at its reset, if it has one.
    case usedUp(back: Moment?)
    /// There's no way to tell.
    case unknown(reason: UnknownReason)

    private enum Keys: String, CodingKey { case kind, likely, back, reason }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "lasts": self = .lasts
        case "runsOut": self = .runsOut(likely: try c.decode(Moment.self, forKey: .likely))
        case "usedUp": self = .usedUp(back: try c.decodeIfPresent(Moment.self, forKey: .back))
        case "unknown": self = .unknown(reason: try c.decode(UnknownReason.self, forKey: .reason))
        case let kind: throw unknownKind(kind, at: .kind, in: c)
        }
    }
}

/// Why a limit's outlook can't be told.
public enum UnknownReason: String, Decodable, Sendable {
    /// Too little of its window has been read to tell a pace.
    case notEnoughData
    /// Its readings are too old: no agent here is signed in, or it reset since.
    case stale
    /// It never resets, like credits.
    case noReset
    /// Its window rolls over the last few hours.
    case rolling
}

/// How soon a time is, which decides how it's worded: a countdown, a time
/// of day, a day of the week, or a date.
public enum Horizon: String, Decodable, Sendable {
    /// Within three hours.
    case soon
    /// Later today.
    case today
    /// Within a week.
    case thisWeek
    /// Further off.
    case later
}

/// What an API-key account cost this calendar month, at list prices.
public struct Spend: Decodable, Equatable, Sendable {
    /// So far this month.
    public var monthUsd: Double
    /// The whole month at the pace so far. Nil until a day of it has
    /// passed, since a pace from a few hours says nothing about a month.
    public var forecastUsd: Double?
    /// Whether some usage had no known price, so the real cost is higher.
    public var partial: Bool
}

/// The settings serve keeps.
public struct Settings: Codable, Equatable, Sendable {
    public var notify: Notify
}

/// Which notifications to send.
public struct Notify: Codable, Equatable, Sendable {
    /// A limit in use will run out before it resets, at its pace.
    public var runningOut: Bool
    /// A limit in use is used up.
    public var usedUp: Bool
    /// A limit that was used up is back.
    public var reset: Bool
    /// An account in use needs signing in again.
    public var signIn: Bool
}

// MARK: Alerts and requests

/// A notification to show, raised once by the engine and kept until
/// acknowledged.
public struct Alert: Decodable, Equatable, Sendable {
    public var id: Int
    public var kind: AlertKind
    public var account: String
    /// The account's title, and what tells it apart: "Claude Max (me@example.com)".
    public var accountTitle: String
    /// The limit's key, for an alert about a limit.
    public var limit: String?
    /// The limit's name.
    public var limitName: String?
    /// The notification this one replaces. There's one per limit, or per
    /// account, so "back" replaces the one saying the limit was used up.
    public var replaces: String
    /// When the limit resets, or reset.
    public var resets: Moment?
    /// For `runningOut`, when it likely runs out.
    public var runsOut: Moment?
}

/// What an alert is about.
public enum AlertKind: String, Decodable, Sendable {
    case runningOut, usedUp, reset, signIn
}

/// A session that used a limit, and roughly how much of it.
public struct UsedMost: Decodable, Equatable, Identifiable, Sendable {
    public var session: String
    public var title: String?
    public var project: String?
    public var agent: String
    /// Roughly how many percentage points of the limit it used. Nil when
    /// that can't be known, such as for a limit with no window start.
    public var sharePercent: Double?
    public var running: Bool

    public var id: String { session }
}

/// The answer to `hello`, read after the engine it names (`Client.greet`).
public struct HelloResult: Decodable, Equatable, Sendable {
    public var status: Status
    /// Alerts no client has acknowledged, oldest first.
    public var alerts: [Alert]
}

/// `hello`'s parameters.
struct HelloParams: Encodable, Sendable {
    var `protocol`: Int
}

/// `alerts.ack`'s parameters.
struct AckParams: Encodable, Sendable { var ids: [Int] }

/// `limit.breakdown`'s parameters, and its answer.
struct BreakdownParams: Encodable, Sendable {
    var account: String
    var limit: String
}
struct BreakdownResult: Decodable, Sendable { var sessions: [UsedMost] }

/// `settings.set`'s parameters.
struct SettingsParams: Encodable, Sendable { var settings: Settings }

/// `account.hide`'s parameters.
struct HideParams: Encodable, Sendable {
    var account: String
    var hidden: Bool
}

/// `agent.connect`'s and `agent.disconnect`'s parameters.
struct AgentParams: Encodable, Sendable { var agent: String }

/// The answer to a request when the app only needs to know it was done.
/// `shutdown` takes no parameters, so it isn't sent one of these.
struct Done: Codable, Sendable {}

// MARK: Reading and writing JSON

/// How the contract's JSON is read and written.
public enum Contract {
    /// Reads times as UTC milliseconds.
    public static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .millisecondsSince1970
        return decoder
    }()

    static let encoder: JSONEncoder = {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .millisecondsSince1970
        return encoder
    }()

    /// A status read from a file as a fixture: a status on its own, a
    /// `status` notification, or a `hello` answer.
    public static func status(from data: Data) -> Status? {
        struct Pushed: Decodable { var params: Status }
        struct Hello: Decodable { var result: HelloResult }
        return (try? decoder.decode(Status.self, from: data))
            ?? (try? decoder.decode(Pushed.self, from: data))?.params
            ?? (try? decoder.decode(Hello.self, from: data))?.result.status
    }
}

/// The error for a `kind` that no case matches, at `key` in `container`.
private func unknownKind<Key: CodingKey>(_ kind: String, at key: Key, in container: KeyedDecodingContainer<Key>)
    -> DecodingError {
    .dataCorruptedError(forKey: key, in: container, debugDescription: "no kind is \(kind)")
}
