// How limits and times are put into words, as of 12:00 UTC on Wednesday 30
// September 2026, in UTC.

import Foundation
import Testing
@testable import TurnscopeKit

private let noon = ISO8601DateFormatter().date(from: "2026-09-30T12:00:00Z")!

private var words: Words {
    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = TimeZone(identifier: "UTC")!
    return Words(now: noon, calendar: calendar)
}

private func at(_ text: String) -> Date { ISO8601DateFormatter().date(from: text)! }

private func limit(_ standing: Standing, hours: Int? = 5, left: Double? = 20,
                   resets: String? = "2026-09-30T15:00:00Z", out: String? = nil,
                   atReset: Double? = nil, reserve: Double? = nil, budget: Double? = nil) -> Limit {
    Limit(key: "k", name: "5 hours", scope: nil, hours: hours, left: left,
          resetsAt: resets.map(at), runsOutAt: out.map(at), leftAtReset: atReset, reserve: reserve,
          budget: budget, standing: standing)
}

@Test func timesReadAsAPersonSaysThem() {
    #expect(words.clock(at("2026-09-30T15:40:00Z")) == "3:40 PM")
    #expect(words.clock(at("2026-09-30T15:00:00Z")) == "3 PM")
    #expect(words.clock(at("2026-10-01T09:00:00Z")) == "tomorrow 9 AM")
    // Thursday is tomorrow; Saturday is within the week.
    #expect(words.clock(at("2026-10-03T19:51:00Z"), around: true) == "Sat 7:50 PM")
    #expect(words.clock(at("2026-10-18T16:37:00Z")) == "Oct 18")
    #expect(words.day(at("2026-10-03T19:51:00Z")) == "Saturday")
    #expect(words.partOfDay(at("2026-10-03T19:51:00Z")) == "evening")
    #expect(words.when(at("2026-10-03T19:51:00Z")) == "Saturday evening")
    #expect(words.when(at("2026-09-30T18:00:00Z")) == "this evening")
    #expect(words.when(at("2026-09-30T23:00:00Z")) == "tonight")
    #expect(words.when(at("2026-10-01T08:00:00Z")) == "tomorrow morning")
}

@Test func stretchesReadInHoursAndDays() {
    #expect(words.stretch(45 * 60) == "45m")
    #expect(words.stretch(90 * 60) == "1h 30m")
    #expect(words.stretch(2 * 3600) == "2h")
    // 52 hours: 2 days and 4.
    #expect(words.stretch(52 * 3600) == "2d 4h")
    #expect(words.stretch(-10) == "0m")
}

@Test func whatIsLeftIsNeverOverstated() {
    #expect(words.percent(67.9) == "67%")
    #expect(words.percent(nil) == "—")
    #expect(words.percent(104) == "100%")
}

@Test func aLimitSaysWhenItRunsOutOrWhatIsLeft() {
    // Runs out at 14:00, an hour before the 15:00 reset.
    let out = limit(.runningOut, out: "2026-09-30T14:00:00Z")
    #expect(words.sentence(out) == "Runs out 2 PM, 1h before it resets.")
    #expect(words.short(out) == "5 hours runs out 2 PM")
    #expect(words.figure(Account.with([out])) == "2h")
    // Days off, the menu bar says what is left rather than count down.
    let week = limit(.runningOut, hours: 168, left: 53, resets: "2026-10-04T00:00:00Z",
                     out: "2026-10-03T06:00:00Z")
    #expect(words.figure(Account.with([week])) == "53%")
    let lasts = limit(.lasts, hours: 168, left: 60, resets: "2026-10-04T00:00:00Z", atReset: 43.2)
    #expect(words.sentence(lasts) == "About 43% left when it resets Sun 12 AM.")
    #expect(words.short(lasts) == "Week lasts · resets Sun 12 AM")
    #expect(words.of(lasts) == "this week")
    let spent = limit(.usedUp, left: 0)
    #expect(words.sentence(spent) == "Used up. Back 3 PM.")
    #expect(words.figure(Account.with([spent])) == "Back 3 PM")
}

@Test func aLimitSaysHowItStandsAgainstAnEvenPace() {
    #expect(words.reserve(limit(.lasts, reserve: 8.9)) == "8% in reserve")
    #expect(words.reserve(limit(.runningOut, reserve: -2.1)) == "3% over pace")
    #expect(words.reserve(limit(.lasts, reserve: 0.6)) == "On pace")
    #expect(words.reserve(limit(.lasts, reserve: -0.6)) == "On pace")
    #expect(words.reserve(limit(.lasts)) == nil)
    // Used up says when it is back, not how far over it went.
    #expect(words.reserve(limit(.usedUp, left: 0, reserve: -40)) == nil)
}

@Test func aLimitRunningOutSaysWhatToKeepToToLast() {
    // A week's 0.6875 points an hour are 16.5 a day, said as 16.
    let week = limit(.runningOut, hours: 168, left: 55, reserve: 7, budget: 0.6875)
    #expect(words.budget(week) == "Stay under 16% a day to last.")
    // What to keep to is said over how it stands against an even pace.
    #expect(words.pace(week) == "Stay under 16% a day to last.")
    #expect(words.pace(limit(.lasts, reserve: 8.9)) == "8% in reserve")
    // Five hours are kept to by the hour.
    #expect(words.budget(limit(.runningOut, budget: 6.67)) == "Stay under 6% an hour to last.")
    // Under a point a day, to a tenth, and never said as nothing.
    #expect(words.budget(limit(.runningOut, hours: 168, budget: 0.01)) == "Stay under 0.2% a day to last.")
    #expect(words.budget(limit(.runningOut, hours: 168, budget: 0.001)) == "Stay under 0.1% a day to last.")
    #expect(words.budget(limit(.lasts)) == nil)
}

@Test func theVerdictIsTheMostUrgentAccounts() {
    let out = Account.with([limit(.runningOut, out: "2026-09-30T14:00:00Z")])
    let verdict = words.verdict([out])
    #expect(verdict.headline == "Claude Max runs out in 2h")
    #expect(verdict.detail == "At this pace, 1h before it resets.")
    // With a budget, the detail says what to keep to.
    let week = Account.with([limit(.runningOut, hours: 168, left: 53, resets: "2026-10-04T00:00:00Z",
                                   out: "2026-10-03T06:00:00Z", budget: 0.67)])
    #expect(words.verdict([week]).headline == "Claude Max runs out Saturday morning")
    #expect(words.verdict([week]).detail == "Stay under 16% a day to last.")
    #expect(words.verdict([]).headline == "Nothing in use")
    // Two in use called the same: the detail says which.
    var twin = out
    (twin.id, twin.label) = ("b", "work@example.com")
    var first = out
    first.label = "joey@example.com"
    #expect(words.verdict([first, twin]).detail == "joey@example.com · At this pace, 1h before it resets.")
}

@Test func anAccountInUseWithNoLimitIsNotNothingInUse() {
    var refused = Account.with([])
    refused.problem = .signIn
    #expect(words.verdict([refused]) == ("Sign in to Claude Max again",
                                         "Open Claude Code to sign in. Its limits can't be read until then.",
                                         .lasts))
    #expect(words.verdict([Account.with([])]).headline == "You can keep going")
    #expect(words.trouble(refused) == "Its sign-in was refused. Open Claude Code to sign in again.")
    #expect(words.trouble(Account.with([])) == nil)
}

@Test func beforeAnyAccountTheWelcomeSaysWhy() {
    #expect(words.welcome([]).headline == "No agents found")
    let agents = [AgentLink(id: "claude-code", name: "Claude Code", status: .available),
                  AgentLink(id: "codex", name: "Codex", status: .available),
                  AgentLink(id: "grok", name: "Grok", status: .connected)]
    #expect(words.welcome(agents) == ("No accounts found yet",
                                      "Once Claude Code, Codex or Grok is signed in, its limits show up here."))
}

@Test func tokensReadToThreeFigures() {
    #expect(words.tokens(950) == "950")
    #expect(words.tokens(38_400) == "38.4K")
    #expect(words.tokens(966_000) == "966K")
    #expect(words.tokens(1_250_000) == "1.25M")
}

@Test func alertsReadAsShortNotes() {
    let alert = Alert(account: "a", title: "Claude Max", label: "joey@example.com", limit: "Weekly",
                      scope: nil, kind: .runningOut, at: at("2026-10-03T19:51:00Z"), left: 20)
    let note = words.note(alert)
    #expect(note.title == "Claude Max runs out Sat 7:50 PM")
    #expect(note.body == "joey@example.com\nAt this pace, before the week resets")
    #expect(note.urgent)
    // Each says which account, on a line of its own.
    var back = alert
    back.kind = .back
    #expect(words.note(back) == .init(title: "Claude Max is back",
                                      body: "joey@example.com\nThe week limit reset", urgent: false))
    // A week left unused reads as the quarters do: what is left, then when
    // it resets. Tomorrow at 9 AM, with 62.8% left.
    var unused = alert
    (unused.kind, unused.at, unused.left) = (.unused, at("2026-10-01T09:00:00Z"), 62.8)
    #expect(words.note(unused) == .init(title: "62% of Claude Max's week left",
                                        body: "joey@example.com\nResets tomorrow 9 AM", urgent: false))
    var scoped = unused
    (scoped.kind, scoped.scope, scoped.left, scoped.label) = (.halfLeft, "Opus", 50, nil)
    #expect(words.note(scoped).title == "50% of Claude Max's Opus week left")
}

@Test func aRefusedSignInSaysWhereToSignIn() {
    var account = Account.with([])
    account.label = "joey@example.com"
    #expect(words.signIn(account) == .init(title: "Claude Max: sign in again",
                                           body: "joey@example.com\nOpen Claude Code to sign in again",
                                           urgent: false))
}

extension Account {
    /// Claude Max in use, with `limits`, the first deciding.
    static func with(_ limits: [Limit]) -> Account {
        Account(id: "a", title: "Claude Max", label: nil, logo: "anthropic",
                agents: ["claude-code"], inUse: true, hidden: false, problem: nil,
                standing: limits.map(\.standing).max() ?? .lasts, deciding: limits.first?.key,
                limits: limits, usedMost: [], advice: nil)
    }
}
