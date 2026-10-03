// How what the feed says is put into words: times, stretches, limits and the
// sentences built from them. Everything a person reads goes through here, so a
// limit reads the same in the menu bar, the panel and a notification.
//
// Limits read as what is left, as a fuel gauge reads, rounded down so what is
// left is never overstated. Times read as a person says them: "3:40 AM" today,
// "tomorrow 9 AM", "Thu 7:50 PM" within the week, "Oct 18" beyond it; a
// forecast is "around" a time to the ten minutes. Stretches read "1h 30m",
// and "2d 4h" past a day.

import Foundation

/// Words as of `now`, in `calendar`'s time zone.
public struct Words: Sendable {
    public var now: Date
    public var calendar: Calendar

    /// The time words are as of, whatever time they are given, when a
    /// fixture froze the clock at its feed's time, so what it shows reads the
    /// same whenever it is drawn. Set once, before anything is drawn.
    nonisolated(unsafe) public static var frozen: Date?

    public init(now: Date? = nil, calendar: Calendar = .autoupdatingCurrent) {
        self.now = Words.frozen ?? now ?? .now
        self.calendar = calendar
    }

    // MARK: Figures

    /// What is left, "67%", rounded down; "—" when unknown.
    public func percent(_ value: Double?) -> String {
        guard let value else { return "—" }
        return "\(Int(max(0, min(100, value)).rounded(.down)))%"
    }

    /// How long `seconds` is: "45m", "1h 30m", "2d 4h".
    func stretch(_ seconds: TimeInterval) -> String {
        let minutes = max(0, Int(seconds / 60))
        let (hours, rest) = (minutes / 60, minutes % 60)
        if hours >= 24 {
            return hours % 24 == 0 ? "\(hours / 24)d" : "\(hours / 24)d \(hours % 24)h"
        }
        if hours > 0 { return rest > 0 ? "\(hours)h \(rest)m" : "\(hours)h" }
        return "\(rest)m"
    }

    /// When `date` is, as a person says it: "3:40 AM", "tomorrow 9 AM", "Thu
    /// 7:50 PM", "Oct 18"; with `around`, to the nearest ten minutes.
    func clock(_ date: Date, around: Bool = false) -> String {
        var date = date
        if around {
            let ten = 600.0
            date = Date(timeIntervalSinceReferenceDate: (date.timeIntervalSinceReferenceDate / ten).rounded() * ten)
        }
        let time = format(date, calendar.component(.minute, from: date) == 0 ? "h a" : "h:mm a")
        if calendar.isDate(date, inSameDayAs: now) { return time }
        if let tomorrow = calendar.date(byAdding: .day, value: 1, to: now),
           calendar.isDate(date, inSameDayAs: tomorrow) {
            return "tomorrow \(time)"
        }
        if date > now, date.timeIntervalSince(now) < 6 * 86_400 {
            return "\(format(date, "EEE")) \(time)"
        }
        return format(date, "MMM d")
    }

    /// The day `date` falls on, as a headline says it: "today", "tomorrow",
    /// "Thursday".
    func day(_ date: Date) -> String {
        if calendar.isDate(date, inSameDayAs: now) { return "today" }
        if let tomorrow = calendar.date(byAdding: .day, value: 1, to: now),
           calendar.isDate(date, inSameDayAs: tomorrow) {
            return "tomorrow"
        }
        return format(date, "EEEE")
    }

    /// When `date` is, loosely, as a headline says it: "this evening",
    /// "tonight", "tomorrow morning", "Saturday night".
    func when(_ date: Date) -> String {
        let part = partOfDay(date)
        guard calendar.isDate(date, inSameDayAs: now) else { return "\(day(date)) \(part)" }
        return part == "night" ? "tonight" : "this \(part)"
    }

    /// The part of the day `date` falls in: "morning", "evening".
    func partOfDay(_ date: Date) -> String {
        switch calendar.component(.hour, from: date) {
        case 5..<12: "morning"
        case 12..<17: "afternoon"
        case 17..<22: "evening"
        default: "night"
        }
    }

    private func format(_ date: Date, _ pattern: String) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.calendar = calendar
        formatter.timeZone = calendar.timeZone
        formatter.dateFormat = pattern
        return formatter.string(from: date)
    }

    // MARK: Limits

    /// What a limit is called: "5 hours", "Week", "Month", "Opus week".
    public func name(_ limit: Limit) -> String {
        let span: String
        switch limit.hours {
        case 5: span = "5 hours"
        case 168: span = "Week"
        case let hours? where (672...744).contains(hours): span = "Month"
        default: span = spanName(limit.name)
        }
        guard let scope = limit.scope else { return span }
        return "\(scope) \(span.lowercased())"
    }

    /// What a limit's figure is left of: "this week", "of 5 hours".
    public func of(_ limit: Limit) -> String {
        switch name(limit) {
        case "Week": "this week"
        case "Month": "this month"
        case let other: "of \(other.lowercased())"
        }
    }

    /// How a limit stands in a few words, for a row: "5 hours runs out 2 AM",
    /// "Week lasts · resets Sun", "5 hours back 3:40 AM".
    public func short(_ limit: Limit) -> String {
        let name = name(limit)
        switch limit.standing {
        case .usedUp:
            return "\(name) back " + (limit.resetsAt.map { clock($0) } ?? "later")
        case .runningOut:
            return "\(name) runs out " + (limit.runsOutAt.map { clock($0, around: true) } ?? "soon")
        case .lasts:
            return "\(name) lasts" + (limit.resetsAt.map { " · resets \(clock($0))" } ?? "")
        }
    }

    /// A limit's sentence: when it runs out, how much will be left when it
    /// resets, or when a used-up one is back.
    public func sentence(_ limit: Limit) -> String {
        switch limit.standing {
        case .usedUp:
            return limit.resetsAt.map { "Used up. Back \(clock($0))." } ?? "Used up."
        case .runningOut:
            guard let out = limit.runsOutAt else { return "Runs out soon at this pace." }
            guard let resets = limit.resetsAt else { return "Runs out \(clock(out, around: true))." }
            return "Runs out \(clock(out, around: true)), \(stretch(resets.timeIntervalSince(out))) before it resets."
        case .lasts:
            guard let resets = limit.resetsAt else { return "Lasts at this pace." }
            let left = limit.leftAtReset.map { "About \(percent($0)) left" } ?? "Room left"
            return "\(left) when it resets \(clock(resets))."
        }
    }

    /// How a limit stands against spending it evenly until it resets:
    /// "8% in reserve", "3% over pace", within a point "On pace"; nil when
    /// that isn't known or it is used up. What is to spare is rounded down
    /// and what is over rounded up, so neither flatters.
    func reserve(_ limit: Limit) -> String? {
        guard limit.standing != .usedUp, let reserve = limit.reserve else { return nil }
        if reserve >= 1 { return "\(Int(reserve.rounded(.down)))% in reserve" }
        if reserve <= -1 { return "\(Int((-reserve).rounded(.up)))% over pace" }
        return "On pace"
    }

    /// What to keep to for a limit running out to last until it resets:
    /// "Stay under 16% a day to last" for a window longer than a day, an
    /// hour for a shorter one; nil unless the feed gives a budget. Rounded
    /// down, so keeping to it lasts.
    func budget(_ limit: Limit) -> String? {
        guard let budget = limit.budget else { return nil }
        let daily = (limit.hours ?? 0) > 24
        let rate = daily ? budget * 24 : budget
        let figure = rate >= 1 ? "\(Int(rate.rounded(.down)))" : "\(max(0.1, (rate * 10).rounded(.down) / 10))"
        return "Stay under \(figure)% \(daily ? "a day" : "an hour") to last."
    }

    /// The line under a limit's sentence: what to keep to while it runs out,
    /// or else how it stands against an even pace.
    public func pace(_ limit: Limit) -> String? {
        budget(limit) ?? reserve(limit)
    }

    // MARK: The panel

    /// The one thing to know, large, and what backs it up: whether the person
    /// can keep going, from the most urgent of `inUse`, listed most urgent
    /// first as the feed lists them.
    public func verdict(_ inUse: [Account]) -> (headline: String, detail: String, standing: Standing) {
        guard let account = inUse.first else {
            return ("Nothing in use", "No agent has drawn on a limit in the last half hour.", .lasts)
        }
        // None in use has a limit to go by: one's sign-in was refused, or
        // they are keys that carry none.
        guard let limit = account.decidingLimit else {
            if let refused = inUse.first(where: { $0.problem == .signIn }) {
                let open = refused.agents.first.map { "Open \(agentName($0)) to sign in. " } ?? ""
                return ("Sign in to \(refused.title) again", open + "Its limits can't be read until then.", .lasts)
            }
            return ("You can keep going", "No limit applies to what is in use.", .lasts)
        }
        // Which it is, when another in use is called the same.
        let twin = inUse.dropFirst().contains { $0.title == account.title }
        let which = twin ? account.label.map { "\($0) · " } ?? "" : ""
        switch limit.standing {
        case .usedUp:
            let back = limit.resetsAt.map { "Back \(clock($0)), in \(stretch($0.timeIntervalSince(now)))." }
            return ("\(account.title) is used up", which + (back ?? "It isn't known when it's back."), .usedUp)
        case .runningOut:
            let out = limit.runsOutAt ?? now
            let headline = soon(out)
                ? "\(account.title) runs out in \(stretch(out.timeIntervalSince(now)))"
                : "\(account.title) runs out \(when(out))"
            // What to keep to, which the person can act on; when that isn't
            // known, how early it runs out.
            let gap = limit.resetsAt.map { "At this pace, \(stretch($0.timeIntervalSince(out))) before it resets." }
            return (headline, which + (budget(limit) ?? gap ?? "At this pace."), .runningOut)
        case .lasts:
            return ("You can keep going", "Every limit lasts until it resets at this pace.", .lasts)
        }
    }

    /// What the panel says before any account is found, as [`verdict`] says
    /// it once one is: while the engine is still `reading` agents' history
    /// for the first time, that limits are on their way, since none found may
    /// only mean none found yet; after, that no agent it reads is here, or
    /// that those here aren't signed in.
    public func welcome(_ agents: [AgentLink], reading: Bool = false)
        -> (headline: String, detail: String, standing: Standing) {
        if reading {
            return ("Getting your limits", "They show up here in a moment.", .lasts)
        }
        guard !agents.isEmpty else {
            return ("No agents found",
                    "Turnscope reads Claude Code, Codex, OpenCode, Pi and Grok Build. Sign in to one, and its limits show up here.",
                    .lasts)
        }
        let names = agents.map(\.name)
        let named = names.count > 1
            ? "\(names.dropLast().joined(separator: ", ")) or \(names[names.count - 1])"
            : names[0]
        return ("No accounts found yet", "Once \(named) is signed in, its limits show up here.", .lasts)
    }

    /// Why an account's limits can't be read now, in a sentence; nil when
    /// they can.
    public func trouble(_ account: Account) -> String? {
        switch account.problem {
        case nil: nil
        case .signIn:
            "Its sign-in was refused. Open \(account.agents.first.map(agentName) ?? "its agent") to sign in again."
        case .unavailable: "Its provider couldn't be reached just now, so these figures may be out of date."
        case .unrecognized: "Its provider answered in a way this version of Turnscope doesn't understand."
        case .unsent: "Turnscope couldn't ask its provider just now."
        }
    }

    /// What the menu bar says of an account: what is left, "68%", in amber
    /// while it runs out; how long it has, "1h 40m", once that is within
    /// three hours, as the headline says it; when it is back, "Back 5:40 AM".
    /// A countdown days long reads as a reset, and says less than what is
    /// left.
    public func figure(_ account: Account) -> String {
        guard let limit = account.decidingLimit else {
            return account.problem == .signIn ? "Sign in" : "—"
        }
        switch limit.standing {
        case .usedUp: return limit.resetsAt.map { "Back \(clock($0))" } ?? "Used up"
        case .runningOut:
            guard let out = limit.runsOutAt, soon(out) else { return percent(limit.left) }
            return stretch(out.timeIntervalSince(now))
        case .lasts: return percent(limit.left)
        }
    }

    /// Whether `date` is soon enough to count down to: within three hours.
    private func soon(_ date: Date) -> Bool {
        date.timeIntervalSince(now) < 3 * 3600
    }

    /// The advice, short for its line and whole for its tooltip.
    public func advice(_ advice: Advice) -> (short: String, whole: String) {
        switch advice {
        case .reread(let context):
            let tokens = tokens(context)
            return ("Start fresh per task. Replies re-read \(tokens).",
                    "The session that used most of it re-read its context, which grew to \(tokens) tokens, on every reply. Starting a fresh session per task makes the limit last longer.")
        case .subagents(let share, let model):
            return ("Run fewer \(model) subagents at once.",
                    "Its subagents took \(String(format: "%.1f", share)) points of it, most on \(model). Running fewer at once, or on a smaller model, makes the limit last longer.")
        }
    }

    /// A session's share of a limit, to a tenth of a point: "12.5%".
    public func share(_ points: Double) -> String {
        String(format: "%.1f%%", points)
    }

    /// A count of tokens, to three figures: "950", "38.4K", "966K", "1.25M".
    func tokens(_ count: Int) -> String {
        let units: [(Double, String)] = [(1e9, "B"), (1e6, "M"), (1e3, "K")]
        let value = Double(count)
        guard let (size, unit) = units.first(where: { value >= $0.0 }) else { return "\(count)" }
        let scaled = value / size
        let digits = scaled >= 100 ? 0 : scaled >= 10 ? 1 : 2
        var text = String(format: "%.\(digits)f", scaled)
        if text.contains(".") {
            while text.hasSuffix("0") { text.removeLast() }
            if text.hasSuffix(".") { text.removeLast() }
        }
        return text + unit
    }

    /// What an account says under its name in a list: what tells it apart,
    /// its label, or the agents that use it, "via OpenCode, Pi"; nil when
    /// nothing does.
    public func subtitle(_ account: Account) -> String? {
        if let label = account.label { return label }
        guard !account.agents.isEmpty else { return nil }
        return "via " + account.agents.map(agentName).joined(separator: ", ")
    }

    /// An agent's name, from its id.
    public func agentName(_ id: String) -> String {
        switch id {
        case "claude-code": "Claude Code"
        case "codex": "Codex"
        case "opencode": "OpenCode"
        case "pi": "Pi"
        case "grok": "Grok Build"
        default: id
        }
    }

    // MARK: Notifications

    /// A notification's words, each fact on a line of its own.
    public struct Note: Equatable, Sendable {
        public var title: String
        public var body: String
        /// Whether it may break through Focus: it asks for something now.
        public var urgent: Bool
    }

    /// What an alert says.
    public func note(_ alert: Alert) -> Note {
        // As the panel names it, "week" or "Opus week", within a sentence.
        let span = spanName(alert.limit).lowercased()
        let limit = alert.scope.map { "\($0) \(span)" } ?? span
        /// Its lines: what tells the account apart, then `facts`.
        func body(_ facts: String?...) -> String {
            ([alert.label] + facts).compactMap { $0 }.joined(separator: "\n")
        }
        switch alert.kind {
        case .runningOut:
            let at = alert.at.map { clock($0, around: true) } ?? "soon"
            return Note(title: "\(alert.title) runs out \(at)",
                        body: body("At this pace, before the \(limit) resets"), urgent: true)
        case .usedUp:
            return Note(title: "\(alert.title) is used up",
                        body: body(alert.at.map { "Back \(clock($0))" }), urgent: true)
        case .back:
            return Note(title: "\(alert.title) is back", body: body("The \(limit) limit reset"), urgent: false)
        case .unused, .threeQuartersLeft, .halfLeft, .quarterLeft:
            return Note(title: "\(percent(alert.left)) of \(alert.title)'s \(limit) left",
                        body: body(alert.at.map { "Resets \(clock($0))" }), urgent: false)
        case .signIn:
            return Note(title: "\(alert.title): sign in again",
                        body: body("Open its agent to sign in again"), urgent: false)
        }
    }

    /// What the Monday recap says of the week each account had.
    public func recap(_ weeks: [Week]) -> Note {
        let lines = weeks.map { week in
            // Which it is, when another in the recap is called the same.
            let twin = weeks.filter { $0.title == week.title }.count > 1
            let name = twin ? week.label.map { "\(week.title) · \($0)" } ?? week.title : week.title
            let used = "\(Int(week.used.rounded()))%"
            return "\(name) used \(used)" + (week.project.map { ", most on \($0)" } ?? "")
        }
        return Note(title: "Last week", body: lines.joined(separator: "\n"), urgent: false)
    }

    /// What is said once an account in use can't be read for its sign-in.
    public func signIn(_ account: Account) -> Note {
        let agents = account.agents.map(agentName)
        let open = agents.first.map { "Open \($0) to sign in again" } ?? "Sign in again"
        return Note(title: "\(account.title): sign in again",
                    body: [account.label, open].compactMap { $0 }.joined(separator: "\n"), urgent: false)
    }

    /// A limit's name as its provider gives it, as a span: "Weekly" is "Week".
    private func spanName(_ name: String) -> String {
        switch name.lowercased() {
        case "weekly": "Week"
        case "monthly": "Month"
        default: name
        }
    }
}
