// How what serve decided is put into words, as of 12:00 UTC on Wednesday 30
// September 2026, in UTC.

import Foundation
import Testing
@testable import TurnscopeKit

private let noon = ISO8601DateFormatter().date(from: "2026-09-30T12:00:00Z")!

private var words: Words {
    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = TimeZone(identifier: "UTC")!
    return Words(now: noon, calendar: calendar,
                 names: ["claude-code": "Claude Code", "codex": "Codex", "pi": "Pi"])
}

private func at(_ text: String) -> Date { ISO8601DateFormatter().date(from: text)! }

/// The time `text` with `horizon`, as serve gives a future time.
private func moment(_ text: String, _ horizon: Horizon) -> Moment { Moment(at: at(text), horizon: horizon) }

/// A five-hour limit that resets at 15:00, with `left` percent left and the
/// outlook `outlook`.
private func limit(_ outlook: Outlook, key: String = "five_hour", name: String = "5 hours",
                   left: Int = 20, resets: String = "2026-09-30T15:00:00Z", horizon: Horizon = .soon,
                   money: Money? = nil) -> LimitStatus {
    LimitStatus(key: key, name: name, scope: nil,
                window: LimitWindow(resets: Moment(at: at(resets), horizon: horizon)),
                leftPercent: left, money: money, resetSince: nil, heldBy: [], outlook: outlook)
}

private func account(_ limits: [LimitStatus], id: String = "anthropic:a", title: String = "Claude Max",
                     label: String? = nil, inUse: Bool = true, recent: Bool? = nil,
                     state: AccountState = .live(readAt: noon),
                     kind: AccountKind = .subscription, spend: Spend? = nil) -> AccountStatus {
    AccountStatus(id: id, provider: "anthropic", kind: kind, title: title, label: label, agents: ["claude-code"],
                  inUse: inUse, recent: recent ?? inUse, hidden: false, state: state,
                  limits: limits, deciding: limits.first?.key, spend: spend)
}

/// Likely to run out at 14:00.
private let runsOut = Outlook.runsOut(likely: moment("2026-09-30T14:00:00Z", .soon))

@Test func timesAreSaidAsTheirHorizonCallsFor() {
    #expect(words.at(at("2026-09-30T14:10:00Z"), .soon) == "in 2h 10m")
    #expect(words.at(at("2026-09-30T18:40:00Z"), .today) == "6:40 PM")
    #expect(words.at(at("2026-10-01T09:00:00Z"), .thisWeek) == "tomorrow 9 AM")
    #expect(words.at(at("2026-10-03T19:50:00Z"), .thisWeek) == "Sat 7:50 PM")
    #expect(words.at(at("2026-10-18T16:37:00Z"), .later) == "Oct 18")
    // A forecast is shown as the engine rounded it.
    #expect(words.at(moment("2026-09-30T19:50:00Z", .today)) == "7:50 PM")
    #expect(words.at(moment("2026-10-03T20:00:00Z", .thisWeek)) == "Sat 8 PM")
    // Roughly, as a headline words a forecast.
    #expect(words.when(at("2026-10-03T19:51:00Z")) == "Saturday evening")
    #expect(words.when(at("2026-10-01T00:40:00Z")) == "tonight")
    #expect(words.ago(at("2026-09-30T00:00:00Z")) == "12 AM")
    #expect(words.ago(at("2026-09-29T23:59:00Z")) == "yesterday 11:59 PM")
    #expect(words.stretch(52 * 3600) == "2d 4h")
}

@Test func amountsAreGroupedAndWhatIsLeftIsRoundedDown() {
    #expect(words.dollars(0) == "$0")
    #expect(words.dollars(9.956) == "$9.96")
    #expect(words.dollars(9.956, down: true) == "$9.95")
    #expect(words.dollars(9.996) == "$10" && words.dollars(9.996, down: true) == "$9.99")
    // Rounding down allows a hair over, so 9.95, held as 9.9499…, doesn't
    // lose a cent.
    #expect(words.dollars(9.95, down: true) == "$9.95")
    #expect(words.dollars(84.9, down: true) == "$84")
    #expect(words.dollars(1250.4) == "$1,250")
    #expect(words.dollars(1_234_567.9) == "$1,234,568")
}

@Test func aLimitSaysWhenItResetsAndWhenItRunsOut() {
    // When it resets and when it runs out are separate phrases; the row puts
    // both beside its name.
    #expect(words.window(limit(runsOut)) == "Resets in 3h")
    #expect(words.ahead(limit(runsOut)) == "Runs out in 2h")
    let week = limit(.lasts, name: "Weekly", left: 60, resets: "2026-10-04T00:00:00Z", horizon: .thisWeek)
    #expect(words.name(week) == "Weekly")
    #expect(words.window(week) == "Resets Sun 12 AM")
    #expect(words.ahead(week) == nil)
    #expect(words.short(week) == "Weekly resets Sun 12 AM")
    #expect(words.of(week) == "this week")
    // Used up: when it's back replaces when it resets, and there's no
    // "Runs out".
    let spent = limit(.usedUp(back: moment("2026-09-30T15:00:00Z", .soon)), left: 0)
    #expect(words.window(spent) == "Back in 3h")
    #expect(words.ahead(spent) == nil)
    #expect(words.ahead(limit(.unknown(reason: .notEnoughData), left: 99)) == nil)
    // Reset since it was read, so it's full, as serve says.
    var reset = limit(.unknown(reason: .stale), left: 100)
    reset.resetSince = at("2026-09-30T11:00:00Z")
    #expect(words.window(reset) == "Reset 11 AM")
    #expect(words.ahead(reset) == nil)
    #expect(words.left(reset) == "100%")
    let credits = LimitStatus(key: "credits", name: "Credits", scope: nil, window: nil,
                              leftPercent: 29, money: Money(sizeUsd: 30, leftUsd: 8.705),
                              resetSince: nil, heldBy: [], outlook: .unknown(reason: .noReset))
    #expect(words.left(credits) == "$8.70")
    #expect(words.size(credits) == "of $30")
    #expect(words.window(credits) == nil && words.ahead(credits) == nil)
}

@Test func anAccountWhoseEveryLimitResetSpeaksForTheLastToReset() {
    var five = limit(.unknown(reason: .stale), left: 100)
    five.resetSince = at("2026-09-30T11:00:00Z")
    var week = limit(.unknown(reason: .stale), key: "seven_day", name: "Weekly", left: 100)
    week.resetSince = at("2026-09-30T11:30:00Z")
    var full = account([five, week], inUse: false)
    full.deciding = "seven_day"
    #expect(full.refilledAt == at("2026-09-30T11:30:00Z"))
    #expect(words.status(full) == "Weekly reset 11:30 AM")
    #expect(words.rowFigure(full) == ("100%", nil))
    #expect(words.opened(full) == words.sinceReset)
}

@Test func theVerdictSaysTheMostUrgentAndWhenItsBack() {
    let out = account([limit(runsOut)])
    let verdict = words.verdict([out], among: [out])
    #expect(verdict.headline == "Claude Max runs out in\u{00A0}2h")
    #expect(verdict.detail == "Back in 3h.")
    #expect(verdict.standing == .runningOut)
    let mayLast = account([limit(.runsOut(likely: moment("2026-10-03T06:00:00Z", .thisWeek)),
                                 resets: "2026-10-04T00:00:00Z", horizon: .thisWeek)])
    #expect(words.verdict([mayLast], among: [mayLast]).headline == "Claude Max runs out Saturday\u{00A0}morning")
    #expect(words.verdict([mayLast], among: [mayLast]).detail == "Back Sun 12 AM.")
    let used = account([limit(.usedUp(back: moment("2026-09-30T15:00:00Z", .soon)), left: 0)])
    #expect(words.verdict([used], among: [used]) == ("Claude Max is used up", "Back in 3h.", .usedUp))
    let fine = account([limit(.lasts, left: 80)])
    #expect(words.verdict([fine], among: [fine]).detail == "At this pace, every limit lasts until it resets.")
    let early = account([limit(.unknown(reason: .notEnoughData), left: 99)])
    #expect(words.verdict([early], among: [early]).detail == "99% left of 5 hours; it resets in 3h.")
    // Nothing in use, so the accounts below say the rest.
    #expect(words.verdict([], among: []) == ("Nothing in use", "", .lasts))
    // A subscription in use with no limit read: whether it has room isn't
    // known, so it says why.
    let new = account([], state: .unread(why: .readFailed))
    #expect(words.verdict([new], among: [new])
        == ("Claude Max's limits are unknown", "It couldn't be read yet.", .lasts))
    let gone = account([], state: .unread(why: .signedOut))
    #expect(words.verdict([gone], among: [gone]).detail == "No agent here is signed in to it now.")
    #expect(words.unused(3, besideOthers: true) == "3 more accounts")
    #expect(words.unused(1, besideOthers: false) == "1 account")
    // Next to another shown account with the same name, the headline says
    // which one it is.
    let mine = account([limit(runsOut)], label: "me@example.com")
    let other = account([limit(.lasts, left: 90)], id: "anthropic:b", label: "work@example.com",
                        inUse: false)
    #expect(words.verdict([mine], among: [mine, other]).detail == "me@example.com · Back in 3h.")
    var hidden = other
    hidden.hidden = true
    #expect(words.verdict([mine], among: [mine, hidden]).detail == "Back in 3h.")
}

@Test func theMenuBarSaysWhatIsLeftOrHowLongOrWhenItIsBack() {
    #expect(words.figure(account([limit(runsOut)])) == "2h")
    #expect(words.spokenFigure(account([limit(runsOut)])) == "runs out in 2 hours")
    #expect(words.figure(account([limit(.lasts, left: 67)])) == "67%")
    let used = account([limit(.usedUp(back: moment("2026-09-30T15:00:00Z", .soon)), left: 0)])
    #expect(words.figure(used) == "Back 3 PM")
    // Back soon, but after midnight, so the day is given, in the spoken
    // figure too.
    var late = words
    late.now = at("2026-09-30T22:30:00Z")
    let overnight = account([limit(.usedUp(back: moment("2026-10-01T01:00:00Z", .soon)), left: 0)])
    #expect(late.figure(overnight) == "Back tomorrow 1 AM")
    #expect(late.spokenFigure(overnight) == "back tomorrow 1 AM")
    let refused = account([], state: .unread(why: .signIn))
    #expect(words.figure(refused) == "Sign in")
    #expect(words.status(refused) == "Sign in again")
    let keys = account([], id: "openrouter:api", title: "OpenRouter API key", kind: .apiKey,
                       spend: Spend(monthUsd: 84.2, forecastUsd: 210, partial: false))
    #expect(words.figure(keys) == "$84")
    #expect(words.rowFigure(keys) == ("$84", "this month"))
    #expect(words.opened(keys) == "On pace for about $210 this month.")
    let unknown = Spend(monthUsd: 0, forecastUsd: 0, partial: true)
    // Early in the month there's no forecast, so none is mentioned.
    #expect(words.pace(Spend(monthUsd: 1, forecastUsd: nil, partial: false)) == nil)
    #expect(words.spent(unknown) == "Cost unknown")
}

@Test func aSessionThatUsedALimitSaysItsShareWhereItIsKnown() {
    var used = UsedMost(session: "codex:s", title: "Fix it", project: "app", agent: "codex",
                        sharePercent: 12.6, running: false)
    #expect(words.used(used, agents: true) == "13% · app · Codex")
    used.sharePercent = 0.2
    #expect(words.used(used, agents: false) == "<1% · app")
    // For a limit with no window start, the share can't be known.
    used.sharePercent = nil
    #expect(words.used(used, agents: false) == "app")
}

@Test func anOpenedAccountSaysWhatItsLimitsCant() {
    let failed = account([limit(.lasts, left: 50)], state: .asOf(readAt: noon, why: .readFailed))
    #expect(words.opened(failed) == "As read 12 PM; it couldn't be read since.")
    let gone = account([limit(.unknown(reason: .stale), left: 50)], inUse: false,
                       state: .asOf(readAt: at("2026-09-30T09:00:00Z"), why: .signedOut))
    #expect(words.opened(gone) == "As read 9 AM; no agent here is signed in to it now.")
    let refused = account([limit(.lasts, left: 50)], state: .asOf(readAt: noon, why: .signIn))
    #expect(words.opened(refused) == "As read 12 PM; open Claude Code to sign in again.")
    // Every login expired. An agent renews it when next used, so no sign-in
    // is needed.
    let expired = account([limit(.lasts, left: 50)], state: .asOf(readAt: noon, why: .expired))
    #expect(words.opened(expired) == "As read 12 PM; read again once you use Claude Code.")
    #expect(words.status(expired) == "Open Claude Code to update")
    #expect(!expired.refused)
    // Never read, each says why, and what to do where there's something.
    let lapsed = account([], state: .unread(why: .expired))
    #expect(words.status(lapsed) == "Open Claude Code to update")
    #expect(words.opened(lapsed) == "Its login expired. It's read once you use Claude Code.")
    let unreachable = account([], state: .unread(why: .readFailed))
    #expect(words.status(unreachable) == "Couldn't be read yet")
    #expect(words.opened(unreachable) == "It couldn't be read yet.")
    // When it runs out is on its limit lines, so it isn't repeated.
    #expect(words.opened(account([limit(runsOut)])) == nil)
    // A key with no limit says so under its name, so it isn't repeated.
    #expect(words.opened(account([], id: "anthropic:api", title: "Anthropic API key", kind: .apiKey)) == nil)
}

@Test func aNoteSaysWhichAccountThenWhatHappenedThenWhatItMeans() {
    let accounts = [account([limit(runsOut)], label: "joey@example.com"),
                    account([], id: "anthropic:b", label: "work@example.com", inUse: false)]
    let alert = { (kind: AlertKind) in
        Alert(id: 1, kind: kind, account: "anthropic:a", accountTitle: "Claude Max (joey@example.com)",
              limit: "five_hour", limitName: "5 hours", replaces: "limit:anthropic:a:five_hour",
              resets: moment("2026-09-30T15:00:00Z", .soon), runsOut: moment("2026-09-30T14:00:00Z", .soon))
    }
    let out = words.note(alert(.runningOut), accounts: accounts)
    #expect(out == Words.Note(title: "Claude Max · joey", subtitle: "5 hours runs out 2 PM",
                              body: "Back 3 PM."))
    #expect(words.note(alert(.usedUp), accounts: accounts).subtitle == "5 hours is used up")
    #expect(words.note(alert(.usedUp), accounts: accounts).body == "Back 3 PM.")
    #expect(words.note(alert(.reset), accounts: accounts).subtitle == "5 hours is back")
    #expect(words.note(alert(.reset), accounts: accounts).body == "20% left.")
    let signIn = words.note(alert(.signIn), accounts: accounts)
    #expect(signIn.subtitle == "Sign in again" && signIn.body == "Open Claude Code to sign in.")
    // For an account not in the status, the title is the alert's name for
    // it.
    #expect(words.note(alert(.usedUp)).title == "Claude Max (joey@example.com)")
    // If another account's label has the same part before the "@", the
    // whole label is used.
    let alike = [account([limit(runsOut)], label: "joey@example.com"),
                 account([], id: "anthropic:b", label: "joey@example.net", inUse: false)]
    #expect(words.note(alert(.usedUp), accounts: alike).title == "Claude Max · joey@example.com")
    // Back days later, as when a weekly limit runs out, given as a clock
    // time.
    var days = alert(.runningOut)
    days.resets = moment("2026-10-03T00:00:00Z", .thisWeek)
    days.runsOut = moment("2026-09-30T20:00:00Z", .today)
    #expect(words.note(days, accounts: accounts).body == "Back Sat 12 AM.")
}

@Test func theFoldsAndAgentsSayWhatTheyHold() {
    #expect(words.hidden(2) == "2 hidden")
    var codex = AgentStatus(id: "codex", name: "Codex", installed: true, connection: .available, inUse: true)
    #expect(words.connection(codex) == "Not connected")
    #expect(words.connection(codex, named: true) == "Codex isn't connected")
    codex.connection = .outdated
    #expect(words.connection(codex) == "Uses another Turnscope")
    #expect(words.connection(codex, named: true) == "Codex uses another Turnscope")
    codex.connection = .connected
    #expect(words.connection(codex) == "Connected")
}

@Test func beforeAnyAccountTheWelcomeSaysWhy() {
    #expect(words.welcome([], reads: ["Claude Code", "Codex"], reading: true).headline == "Getting your limits")
    #expect(words.welcome([], reads: ["Claude Code", "Codex"]).detail
        == "Turnscope reads Claude Code and Codex. Sign in to one, and its limits show up here.")
    let codex = AgentStatus(id: "codex", name: "Codex", installed: true, connection: .available, inUse: false)
    #expect(words.welcome([codex], reads: []).headline == "No accounts found yet")
}

@Test func theVerdictSaysOnlyWhatLastsLasts() {
    let lasts = Outlook.lasts
    let week = limit(lasts, key: "seven_day", name: "Weekly", left: 60,
                     resets: "2026-10-04T00:00:00Z", horizon: .thisWeek)
    // Its weekly limit lasts, but its Opus weekly limit, for one model, is
    // used up.
    var opus = limit(.usedUp(back: moment("2026-10-03T09:00:00Z", .thisWeek)), key: "seven_day_opus",
                     name: "Weekly", left: 0,
                     resets: "2026-10-03T09:00:00Z", horizon: .thisWeek)
    opus.scope = "Opus"
    let scoped = account([week, opus])
    #expect(words.verdict([scoped], among: [scoped]) == ("You can keep going",
        "At this pace, weekly lasts until it resets. Opus weekly is used up, back Sat 9 AM.", .lasts))
    // Next to a limit that can't be forecast, it says only that the
    // deciding one lasts.
    let credits = limit(.unknown(reason: .noReset), key: "credits", name: "Credits", left: 29,
                        money: Money(sizeUsd: 30, leftUsd: 8.7))
    let mixed = account([week, credits])
    #expect(words.verdict([mixed], among: [mixed]).detail == "At this pace, weekly lasts until it resets.")
    // If it's as last read, it says so.
    let stale = account([week], state: .asOf(readAt: noon, why: .readFailed))
    #expect(words.verdict([stale], among: [stale]).detail == "As last read, every limit lasts until it resets.")
    // A limit that can't be forecast is never said to last. What's left of
    // it is given instead.
    let reasons: [(UnknownReason, String)] = [
        (.notEnoughData, "50% left of 5 hours; it resets in 3h."),
        (.stale, "As last read, 50% left of 5 hours."),
        (.rolling, "50% left of 5 hours, in a window that rolls."),
        (.noReset, "50% left of 5 hours; it doesn't reset."),
    ]
    for (reason, said) in reasons {
        let unknown = account([limit(.unknown(reason: reason), left: 50)])
        #expect(words.verdict([unknown], among: [unknown]) == ("You can keep going", said, .lasts))
    }
    // When it's back is worded for its horizon, as in the row.
    let spent = account([limit(.usedUp(back: moment("2026-10-03T09:00:00Z", .thisWeek)), left: 0)])
    #expect(words.verdict([spent], among: [spent]).detail == "Back Sat 9 AM.")
}

@Test func aLimitIsCalledWhatItsProviderCallsIt() {
    var opus = limit(.lasts, key: "seven_day_opus", name: "Weekly", left: 50)
    opus.scope = "Opus"
    #expect(words.name(opus) == "Opus weekly")
    #expect(words.of(opus) == "of Opus weekly")
    #expect(words.of(limit(.lasts, name: "Monthly", left: 50)) == "this month")
    #expect(words.of(limit(.lasts, left: 50)) == "of 5 hours")
    var held = limit(.lasts, key: "key-1", name: "Credits", left: 50)
    held.heldBy = ["codex", "pi"]
    #expect(words.name(held) == "Codex and Pi's key")
    // A limit that reset since it was read doesn't say when it resets next,
    // which isn't known until it's read again.
    var reset = limit(.lasts, left: 100)
    reset.resetSince = at("2026-09-30T11:00:00Z")
    #expect(words.short(reset) == "5 hours lasts")
}

@Test func anAccountSaysWhichAgentsItsReadThrough() {
    var shared = account([])
    #expect(words.via(shared) == "via Claude Code")
    shared.agents = ["claude-code", "codex", "pi"]
    #expect(words.via(shared) == "via Claude Code, Codex and Pi")
    shared.agents = []
    #expect(words.via(shared) == nil)
}
