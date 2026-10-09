// Small facts views need from what serve sends, each read directly from it:
// a limit's state is its outlook's kind, an account's is its deciding limit's,
// and so on. Nothing here decides anything serve didn't.

import Foundation

/// How a limit stands, from calmest to most urgent. It sets the color.
public enum Standing: Sendable {
    /// It lasts, or there's no way to tell.
    case lasts
    /// At its current pace, it runs out before it resets.
    case runningOut
    /// It is used up.
    case usedUp
}

extension LimitStatus {
    /// How it stands, from its outlook.
    public var standing: Standing {
        switch outlook {
        case .usedUp: .usedUp
        case .runsOut: .runningOut
        case .lasts, .unknown: .lasts
        }
    }
}

extension AccountStatus {
    /// The limit that decides how it stands.
    public var decidingLimit: LimitStatus? {
        limits.first { $0.key == deciding }
    }

    /// How it stands: the same as its deciding limit.
    public var standing: Standing { decidingLimit?.standing ?? .lasts }

    /// Whether it is an API-key account rather than a subscription.
    public var apiKey: Bool { kind == .apiKey }

    /// Whether its login was refused. If so, it's read again only after its
    /// agent signs in again.
    public var refused: Bool {
        state.why == .signIn
    }

    /// When the last of its limits reset, if every one has reset since it
    /// was read. This happens to an account an agent was switched away from:
    /// it has room, but use elsewhere isn't seen until it's read again. Its
    /// deciding limit is then the last one to reset, as serve decides it.
    public var refilledAt: Date? { decidingLimit?.resetSince }

    /// Whether another shown (not hidden) account in `accounts` has the same
    /// title, so the label must tell them apart. The headline, the rows and
    /// notifications all use this one rule to tell accounts apart.
    public func twinned(among accounts: [AccountStatus]) -> Bool {
        accounts.contains { $0.id != id && !$0.hidden && $0.title == title }
    }
}
