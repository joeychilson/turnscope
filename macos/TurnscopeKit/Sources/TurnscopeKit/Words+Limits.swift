// The words for limits: a limit's name, how it reads in a row and on its own
// line, the panel's headline and the line under it, and the menu bar text.
// Then the panel's lists, and whether an agent is connected.

import Foundation

extension Words {
    /// A limit's name, as its provider gives it: "5 hours", "Weekly",
    /// "Monthly", "Opus weekly". An API key's own limit is named for the
    /// agents that hold the key: "Pi's key".
    public func name(_ limit: LimitStatus) -> String {
        if !limit.heldBy.isEmpty { return "\(whose(limit.heldBy)) key" }
        guard let scope = limit.scope else { return limit.name }
        return "\(scope) \(limit.name.lowercased())"
    }

    /// What a limit is called within a sentence: "5 hours", "weekly", "Opus
    /// weekly".
    private func inSentence(_ limit: LimitStatus) -> String {
        limit.scope == nil && limit.heldBy.isEmpty ? name(limit).lowercased() : name(limit)
    }

    /// Who a key belongs to, by the agents that hold it: "Pi's", "OpenCode
    /// and Pi's".
    private func whose(_ agents: [String]) -> String {
        "\(list(agents.map(agent), "and"))'s"
    }

    /// What a limit's figure is left of. For a weekly or monthly limit on
    /// every model, as its provider names it: "this week", "this month". For
    /// any other: "of 5 hours", "of Opus weekly".
    public func of(_ limit: LimitStatus) -> String {
        guard limit.scope == nil, limit.heldBy.isEmpty else { return "of \(name(limit))" }
        return switch limit.name.lowercased() {
        case "weekly": "this week"
        case "monthly": "this month"
        default: "of \(inSentence(limit))"
        }
    }

    /// When a limit resets, as a phrase: "Resets 3 PM", "Resets in 2h". Nil
    /// for a limit that doesn't reset, or that reset since it was read.
    private func resets(_ limit: LimitStatus) -> String? {
        guard let window = limit.window, limit.resetSince == nil else { return nil }
        return "Resets \(at(window.resets))"
    }

    /// How a limit stands in a few words, for a row: "5 hours runs out 2
    /// PM", "Weekly resets Sun 12 AM", "5 hours back 3:40 AM". Money that
    /// never resets is just its name, "Credits", since its figure says how
    /// much.
    func short(_ limit: LimitStatus) -> String {
        let name = name(limit)
        switch limit.outlook {
        case .usedUp(let back):
            return "\(name) back " + (back.map(at) ?? "later")
        case .runsOut(let likely):
            return "\(name) \(plural(limit) ? "run" : "runs") out \(at(likely))"
        case .lasts, .unknown:
            if let window = limit.window, limit.resetSince == nil {
                return "\(name) resets \(at(window.resets))"
            }
            return limit.money == nil ? "\(name) lasts" : name
        }
    }

    /// What a row says under an account's name while its limits aren't
    /// shown. A refused login says "Sign in again", plus "to update" if it
    /// has limits read before, and an expired one "Open Codex to update".
    /// Otherwise it's which limit its figure is for and what happens to it
    /// next, as [`short`] says it. If every limit reset since it was read, it
    /// names the last one to reset, which decides it: "Weekly reset Sat 12
    /// AM". A key with no limit says it has none. With no limit read, it
    /// says why: "Couldn't be read yet", "Signed out" or "Not read yet".
    public func status(_ account: AccountStatus) -> String {
        if account.refused {
            return account.limits.isEmpty ? "Sign in again" : "Sign in again to update"
        }
        if account.state.why == .expired {
            return "Open \(account.agents.first.map(agent) ?? "its agent") to update"
        }
        if let limit = account.decidingLimit {
            return limit.resetSince.map { "\(name(limit)) reset \(ago($0))" } ?? short(limit)
        }
        if let spend = account.spend { return spentLine(spend) }
        return switch account.state.why {
        case .readFailed: "Couldn't be read yet"
        case .signedOut: "Signed out"
        default: "Not read yet"
        }
    }

    /// The figure on an account's right, and the quieter text after it. It's
    /// what's left of the limit its line names: "87%", or "$10" then "of
    /// $30". For a key with no limit, it's what it cost: "$84" then "this
    /// month". It's "—" when nothing is known.
    public func rowFigure(_ account: AccountStatus) -> (figure: String, then: String?) {
        if let limit = account.decidingLimit { return (left(limit), size(limit)) }
        if let spend = account.spend {
            return (spentFigure(spend), unknown(spend) ? nil : "this month")
        }
        return ("—", nil)
    }

    /// When a limit's window resets, shown beside its name: "Resets 6:20
    /// PM", "Resets in 2h". If it's used up, when it's back: "Back Sun 12
    /// AM". If it reset since it was read: "Reset Sat 10 PM". Nil if it never
    /// resets.
    public func window(_ limit: LimitStatus) -> String? {
        if let reset = limit.resetSince { return "Reset \(ago(reset))" }
        if case .usedUp(let back) = limit.outlook { return back.map { "Back \(at($0))" } ?? "Used up" }
        return resets(limit)
    }

    /// What's ahead for a limit, if anything, shown beside its name before
    /// when it resets: when it runs out, "Runs out Wed 10:10 AM".
    public func ahead(_ limit: LimitStatus) -> String? {
        guard limit.resetSince == nil, case .runsOut(let likely) = limit.outlook else { return nil }
        return "Runs out \(at(likely))"
    }

    /// The size of a limit of money, shown after what's left of it: "of
    /// $30". Nil for a percent limit.
    public func size(_ limit: LimitStatus) -> String? {
        limit.money.map { "of \(dollars($0.sizeUsd))" }
    }

    /// The one thing to know, in large type, and the detail behind it:
    /// whether you can keep going. It's based on the most urgent of `inUse`,
    /// which serve lists most urgent first. The account is told apart from
    /// others in `accounts` with the same title. For a subscription with no
    /// limit read, it's not known, so it says why instead.
    public func verdict(_ inUse: [AccountStatus], among accounts: [AccountStatus], hiddenInUse: Bool = false)
        -> (headline: String, detail: String, standing: Standing) {
        guard let account = inUse.first else {
            return ("Nothing in use", hiddenInUse ? "Only accounts you hid are in use." : "", .lasts)
        }
        guard let limit = account.decidingLimit else {
            if account.refused {
                let open = account.agents.first.map { "Open \(agent($0)) to sign in." }
                return ("Sign in to \(account.title) again", open ?? "Its limits can't be read until then.", .lasts)
            }
            if let spend = account.spend {
                return ("You can keep going", pace(spend) ?? "Your \(account.title) has no limit.", .lasts)
            }
            return ("\(account.title)'s limits are unknown", trouble(account) ?? "They aren't read yet.", .lasts)
        }
        // Which account it is, when another has the same title.
        let which = account.twinned(among: accounts) ? account.label.map { "\($0) · " } ?? "" : ""
        switch limit.outlook {
        case .usedUp(let back):
            let after = back.map { "Back \(at($0))." }
            return ("\(account.title) is used up", which + (after ?? "It isn't known when it's back."), .usedUp)
        case .runsOut(let likely):
            let headline = likely.horizon == .soon
                ? "\(account.title) runs out \(unbroken("in \(stretch(likely.at.timeIntervalSince(now)))"))"
                : "\(account.title) runs out \(unbroken(when(likely.at)))"
            // When it's back, which is what you plan around.
            let back = limit.window.map { "Back \(at($0.resets))." }
            return (headline, back.map { which + $0 } ?? "", .runningOut)
        case .unknown(let reason):
            return ("You can keep going", which + unforecast(limit, reason), .lasts)
        case .lasts:
            return ("You can keep going", which + lasting(limit, of: account), .lasts)
        }
    }

    /// What's known about an account's deciding limit when it isn't
    /// forecast, as a sentence: what's left of it and when it resets. It
    /// never says the limit lasts, since that isn't known. "99% left of 5
    /// hours; it resets in 4h 50m." or "$8.81 of $30 left; it doesn't reset."
    private func unforecast(_ limit: LimitStatus, _ reason: UnknownReason) -> String {
        let standing = limit.money == nil
            ? "\(left(limit)) left \(of(limit))"
            : "\(left(limit))\(size(limit).map { " \($0)" } ?? "") left"
        let resets = limit.window.map { "; it resets \(at($0.resets))" } ?? ""
        return switch reason {
        case .notEnoughData: "\(standing)\(resets)."
        case .stale: "As last read, \(standing)."
        case .rolling: "\(standing), in a window that rolls."
        case .noReset: "\(standing); it doesn't reset."
        }
    }

    /// What lasts of `account`, given that its deciding limit, `limit`, lasts
    /// until it resets. It says every limit lasts if every one does, or else
    /// just that one. Then it names the first other limit that doesn't last,
    /// since a limit on a single model can run out while the whole account
    /// has room.
    private func lasting(_ limit: LimitStatus, of account: AccountStatus) -> String {
        let others = account.limits.filter { $0.id != limit.id && $0.resetSince == nil }
        let current: Bool = if case .live = account.state { true } else { false }
        let every = others.allSatisfy { if case .lasts = $0.outlook { true } else { false } }
        let resets = !account.limits.contains { $0.window == nil && $0.resetSince == nil }
        let what = every ? "every limit lasts" : "\(inSentence(limit)) lasts"
        let lasts = "\(what)\(resets || !every ? " until it resets" : "")"
        let sentence = current ? "At this pace, \(lasts)." : "As last read, \(lasts)."
        guard let other = others.first(where: { $0.standing != .lasts }) else { return sentence }
        switch other.outlook {
        case .usedUp(let back):
            return "\(sentence) \(name(other)) is used up\(back.map { ", back \(at($0))" } ?? "")."
        case .runsOut(let likely):
            return "\(sentence) \(name(other)) runs out \(at(likely))."
        case .lasts, .unknown:
            return sentence
        }
    }

    /// What an opened account says under its limits that they can't say
    /// themselves: why they can't be read; that use elsewhere isn't seen, if
    /// every limit reset since it was read; or, for a key with no limit,
    /// where the month's cost is headed. For the account the headline is
    /// about, with no limits to show, it says nothing, since the headline
    /// already does.
    public func opened(_ account: AccountStatus, headlined: Bool = false) -> String? {
        if headlined && account.limits.isEmpty { return nil }
        if let trouble = trouble(account) { return trouble }
        if account.refilledAt != nil { return sinceReset }
        guard account.limits.isEmpty else { return nil }
        return account.spend.flatMap(pace)
    }

    /// What can't be known about an account when every limit reset since it
    /// was read.
    var sinceReset: String {
        "Full since the reset, unless used on the web or another device."
    }

    /// What the panel says before any account is found. While the agents'
    /// history is still being read for the first time, it says limits are on
    /// their way. After that, it says none of the agents it reads is here, or
    /// that those here aren't signed in.
    public func welcome(_ agents: [AgentStatus], reads: [String], reading: Bool = false)
        -> (headline: String, detail: String, standing: Standing) {
        if reading {
            return ("Getting your limits", "They show up here in a moment.", .lasts)
        }
        guard !agents.isEmpty else {
            return ("No agents found",
                    "Turnscope reads \(list(reads, "and")). Sign in to one, and its limits show up here.",
                    .lasts)
        }
        return ("No accounts found yet",
                "Once \(list(agents.map(\.name), "or")) is signed in, its limits show up here.", .lasts)
    }

    /// `names` listed as in a sentence, with `last` before the final one.
    private func list(_ names: [String], _ last: String) -> String {
        guard let final = names.last, names.count > 1 else { return names.first ?? "" }
        return "\(names.dropLast().joined(separator: ", ")) \(last) \(final)"
    }

    /// Why an account's limits can't be read now, as a sentence, after when
    /// the ones shown were read: "As read 9 AM; it couldn't be read since."
    /// Nil when they can be read.
    func trouble(_ account: AccountStatus) -> String? {
        let with = account.agents.first.map(agent) ?? "its agent"
        return switch account.state {
        case .unread(.signIn): "Open \(with) to sign in again."
        case .unread(.expired): "Its login expired. It's read once you use \(with)."
        case .unread(.readFailed): "It couldn't be read yet."
        case .unread(.signedOut): "No agent here is signed in to it now."
        case .asOf(let read, .signIn): "As read \(ago(read)); open \(with) to sign in again."
        case .asOf(let read, .readFailed): "As read \(ago(read)); it couldn't be read since."
        case .asOf(let read, .expired): "As read \(ago(read)); read again once you use \(with)."
        case .asOf(let read, .signedOut) where account.refilledAt == nil:
            "As read \(ago(read)); no agent here is signed in to it now."
        default: nil
        }
    }

    /// What the menu bar shows for an account: what's left, "68%", in amber
    /// while it's running out; how long it has, "1h 40m", once that's within
    /// three hours; or when it's back, "Back 5:40 AM". A key with no limit
    /// shows what it cost this month, "$84".
    public func figure(_ account: AccountStatus) -> String {
        guard let limit = account.decidingLimit else {
            if account.refused { return "Sign in" }
            return account.spend.map(spentFigure) ?? "—"
        }
        switch limit.outlook {
        case .usedUp(let back): return back.map { "Back \(clock($0))" } ?? "Used up"
        case .runsOut(let likely):
            guard likely.horizon == .soon else { return left(limit) }
            return stretch(likely.at.timeIntervalSince(now))
        case .lasts, .unknown: return left(limit)
        }
    }

    /// What the menu bar shows for an account, as VoiceOver reads it after
    /// its name: "20% left", "runs out in 1 hour 40 minutes", "back 5:40
    /// AM", "$84 this month".
    public func spokenFigure(_ account: AccountStatus) -> String {
        guard let limit = account.decidingLimit else {
            if account.refused { return "sign in again" }
            return account.spend.map(spent) ?? "not read yet"
        }
        switch limit.outlook {
        case .usedUp(let back): return back.map { "back \(clock($0))" } ?? "used up"
        case .runsOut(let likely):
            guard likely.horizon == .soon else { return "\(left(limit)) left, running out" }
            return "runs out in \(spoken(likely.at.timeIntervalSince(now)))"
        case .lasts, .unknown: return "\(left(limit)) left"
        }
    }

    /// How long `seconds` is, as said aloud: "1 hour 40 minutes".
    private func spoken(_ seconds: TimeInterval) -> String {
        let minutes = max(0, Int(seconds / 60))
        let (hours, rest) = (minutes / 60, minutes % 60)
        if hours == 0 { return counted(rest, "minute") }
        return rest == 0 ? counted(hours, "hour") : "\(counted(hours, "hour")) \(counted(rest, "minute"))"
    }

    /// `phrase` kept on one line, so a headline breaks before the time it
    /// gives, not inside it.
    private func unbroken(_ phrase: String) -> String {
        phrase.replacingOccurrences(of: " ", with: "\u{00A0}")
    }

    /// A session's share of a limit, to the nearest point, since shares are
    /// approximate: "13%", or "<1%" under half a point.
    func share(_ points: Double) -> String {
        points < 0.5 ? "<1%" : "\(Int(points.rounded()))%"
    }

    /// What a session that used a limit says under its title: its share if
    /// known, where it ran, and, if `agents`, the agent it ran in: "40% ·
    /// app", "12% · api · Codex".
    public func used(_ used: UsedMost, agents: Bool) -> String {
        ([used.sharePercent.map(share), used.project, agents ? agent(used.agent) : nil] as [String?])
            .compactMap { $0 }.joined(separator: " · ")
    }

    /// What an account says under its name where it's listed to show or
    /// hide: "via OpenCode and Pi". Nil when no agent is signed in to it.
    public func via(_ account: AccountStatus) -> String? {
        guard !account.agents.isEmpty else { return nil }
        return "via " + list(account.agents.map(agent), "and")
    }

    /// The line that accounts neither in use nor used this week fold into:
    /// "3 more accounts" below other accounts, or "3 accounts" on its own.
    public func unused(_ count: Int, besideOthers: Bool) -> String {
        counted(count, besideOthers ? "more account" : "account")
    }

    /// The line that hidden accounts fold into: "2 hidden".
    public func hidden(_ count: Int) -> String {
        "\(count) hidden"
    }

    /// Whether `agent` runs Turnscope's MCP server, as Settings says it
    /// under its name: "Connected", "Not connected", "Uses another
    /// Turnscope". With `named`, as the panel offers to connect it: "Codex
    /// isn't connected", "Codex uses another Turnscope".
    public func connection(_ agent: AgentStatus, named: Bool = false) -> String {
        let name = agent.name
        return switch agent.connection {
        case .connected: named ? "\(name) is connected" : "Connected"
        case .available: named ? "\(name) isn't connected" : "Not connected"
        case .outdated: named ? "\(name) uses another Turnscope" : "Uses another Turnscope"
        }
    }

    /// Whether a limit's name is plural, as for money: "Credits run out".
    func plural(_ limit: LimitStatus) -> Bool {
        limit.money != nil && name(limit).hasSuffix("s")
    }
}
