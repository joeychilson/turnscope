// The feed `turnscope watch` writes, and the other lines it writes and reads,
// as `contract/` at the repository's root holds them. Every rule that decides
// what they say is on the Rust side (`crates/cli/src/feed.rs`); the app only
// shows them. A change here is a change to the contract, made with that side.

import Foundation

/// Everything the app shows, as of `at`.
public struct Feed: Decodable, Equatable, Sendable {
    public var version: Int
    public var at: Date
    public var accounts: [Account]
    public var agents: [AgentLink]

    /// The feed's version this app reads.
    public static let version = 6
}

/// An account, as the app shows it. `title` is what it is called before
/// anything tells it apart, "Claude Max"; `label` tells it apart, as its
/// sign-in's email.
public struct Account: Decodable, Equatable, Identifiable, Sendable {
    public var id: String
    public var title: String
    public var label: String?
    /// A provider's id, naming its logo: "anthropic".
    public var logo: String
    /// The agents signed into it, by id: "claude-code".
    public var agents: [String]
    public var inUse: Bool
    public var hidden: Bool
    public var problem: Problem?
    public var standing: Standing
    /// The key of the limit that matters most now.
    public var deciding: String?
    public var limits: [Limit]
    public var usedMost: [Used]
    public var advice: Advice?

    /// The limit that matters most now.
    public var decidingLimit: Limit? {
        limits.first { $0.key == deciding }
    }
}

/// How a limit stands, from calmest to most urgent.
public enum Standing: String, Decodable, Comparable, Sendable {
    case lasts
    case runningOut = "running_out"
    case usedUp = "used_up"

    private var rank: Int {
        switch self {
        case .lasts: 0
        case .runningOut: 1
        case .usedUp: 2
        }
    }

    public static func < (a: Standing, b: Standing) -> Bool { a.rank < b.rank }
}

/// Why an account's limits can't be read now.
public enum Problem: String, Decodable, Sendable {
    /// Its sign-in or key was refused.
    case signIn = "sign_in"
    case unavailable, unrecognized, unsent
}

/// A limit, as last read, and where its pace leads.
public struct Limit: Decodable, Equatable, Identifiable, Sendable {
    public var key: String
    /// Its provider's name for it: "5 hours", "Weekly".
    public var name: String
    /// The one model it applies to, when not all.
    public var scope: String?
    /// How long its window is, in hours.
    public var hours: Int?
    /// Percent left; unknown when its window reset since it was read.
    public var left: Double?
    public var resetsAt: Date?
    public var runsOutAt: Date?
    public var leftAtReset: Double?
    /// Points to spare against spending it evenly until it resets; below
    /// zero, how far ahead of that it is used.
    public var reserve: Double?
    /// While it runs out, the most it can rise, in points an hour, and still
    /// last until it resets.
    public var budget: Double?
    public var standing: Standing

    public var id: String { key }
}

/// A session that used a limit's window.
public struct Used: Decodable, Equatable, Identifiable, Sendable {
    public var session: String
    public var title: String?
    public var project: String?
    public var agent: String
    /// What it took, in points of the limit's percent.
    public var share: Double
    public var active: Bool

    public var id: String { session }
}

/// One piece of advice, drawn from the session that used a limit most.
public enum Advice: Decodable, Equatable, Sendable {
    /// It read its context, which grew to `context` tokens, again with every reply.
    case reread(context: Int)
    /// Its subagents took `share` points of the window, most on `model`.
    case subagents(share: Double, model: String)

    private enum Keys: String, CodingKey { case kind, context, share, model }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .kind) {
        case "reread": self = .reread(context: try c.decode(Int.self, forKey: .context))
        case "subagents":
            self = .subagents(share: try c.decode(Double.self, forKey: .share),
                              model: try c.decode(String.self, forKey: .model))
        case let kind:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "no advice \(kind)")
        }
    }

}

/// An agent Turnscope can serve over MCP, and whether it does.
public struct AgentLink: Decodable, Equatable, Identifiable, Sendable {
    public enum Status: String, Decodable, Sendable {
        /// It runs this copy of Turnscope.
        case connected
        /// It runs another copy, as one since moved.
        case outdated
        case available, unsupported
    }

    public var id: String
    public var name: String
    public var status: Status
}

/// An alert the engine sends once, for the app to tell as a notification.
public struct Alert: Decodable, Equatable, Sendable {
    public enum Kind: String, Decodable, Sendable {
        case runningOut = "running_out"
        case usedUp = "used_up"
        case back, unused
        case threeQuartersLeft = "three_quarters_left"
        case halfLeft = "half_left"
        case quarterLeft = "quarter_left"
    }

    public var account: String
    public var title: String
    public var label: String?
    /// The limit's name, as its provider gives it.
    public var limit: String
    public var scope: String?
    public var kind: Kind
    /// When it runs out, for `runningOut`; when it resets, for the rest.
    public var at: Date?
    public var left: Double?

    public init(account: String, title: String, label: String?, limit: String, scope: String?,
                kind: Kind, at: Date?, left: Double?) {
        (self.account, self.title, self.label, self.limit) = (account, title, label, limit)
        (self.scope, self.kind, self.at, self.left) = (scope, kind, at, left)
    }
}

/// How an account's week went, for the Monday recap.
public struct Week: Decodable, Equatable, Sendable {
    public var account: String
    public var title: String
    public var label: String?
    public var limit: String
    /// The most of it used, in percent, which can be above 100.
    public var used: Double
    public var usedUpAt: Date?
    public var project: String?
}

/// A line `turnscope watch` writes.
public enum Message: Equatable, Sendable {
    case feed(Feed)
    case alert(Alert)
    case recap([Week])
    case reply(id: Int, error: String?)

    /// The line `line` as a message.
    public static func decode(_ line: Data) throws -> Message {
        try Contract.decoder.decode(Line.self, from: line).message
    }

    private struct Reply: Decodable { var id: Int; var error: String? }

    private struct Line: Decodable {
        var message: Message

        private enum Keys: String, CodingKey { case feed, alert, recap, reply }

        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: Keys.self)
            if let feed = try c.decodeIfPresent(Feed.self, forKey: .feed) {
                message = .feed(feed)
            } else if let alert = try c.decodeIfPresent(Alert.self, forKey: .alert) {
                message = .alert(alert)
            } else if let weeks = try c.decodeIfPresent([Week].self, forKey: .recap) {
                message = .recap(weeks)
            } else if let reply = try c.decodeIfPresent(Reply.self, forKey: .reply) {
                message = .reply(id: reply.id, error: reply.error)
            } else {
                throw DecodingError.dataCorrupted(.init(codingPath: [], debugDescription: "a line of no kind known"))
            }
        }
    }
}

/// What the app asks of `turnscope watch`.
public enum Request: Equatable, Sendable {
    case hide(account: String, hidden: Bool)
    case connect(agent: String)
    /// The panel opened or closed: what used each limit is in the feed only
    /// while it is open.
    case panel(open: Bool)

    /// The request as the line that asks it, numbered `id`.
    public func line(id: Int) -> Data {
        var fields: [String: Any] = ["id": id]
        switch self {
        case .hide(let account, let hidden):
            fields["do"] = "hide"
            fields["account"] = account
            fields["hidden"] = hidden
        case .connect(let agent):
            fields["do"] = "connect"
            fields["agent"] = agent
        case .panel(let open):
            fields["do"] = "panel"
            fields["open"] = open
        }
        var line = (try? JSONSerialization.data(withJSONObject: fields, options: [.sortedKeys])) ?? Data()
        line.append(0x0A)
        return line
    }
}

/// How the contract's JSON is read.
public enum Contract {
    /// Snake-case keys, and times in RFC 3339.
    public static var decoder: JSONDecoder {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        decoder.dateDecodingStrategy = .iso8601
        return decoder
    }
}
