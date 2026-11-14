// What the app was started with, and what a store with no engine shows.

import Foundation
import Testing
@testable import TurnscopeKit

@Test func theCommandLineSaysWhatToShowAndWhatToPassOn() {
    let launch = Launch(["Turnscope", "--data", "/tmp/data", "--open", "account,unused", "--home", "/tmp/home"])
    #expect(launch.passed == ["--data", "/tmp/data", "--home", "/tmp/home"])
    #expect(launch.opening == Opening("account,unused"))
    #expect(launch.opening?.account == true && launch.opening?.unused == true)
    #expect(launch.fixture == nil && launch.snapshot == nil)
    // `--open` alone opens the panel with nothing open in it. An option
    // after it is never read as what to open.
    let bare = Launch(["Turnscope", "--open", "--data", "/tmp/data"])
    #expect(bare.opening == Opening(""))
    #expect(bare.passed == ["--data", "/tmp/data"])
    #expect(Launch(["Turnscope"]).opening == nil)
    #expect(Launch(["Turnscope", "--data"]).passed.isEmpty)
    // Settings opens at a tab. An unknown name opens nothing.
    #expect(Opening("agents").settings == .agents)
    #expect(Opening("settings,account").settings == .general)
    #expect(Opening("nothing") == Opening(""))
    #expect(SettingsTab.accounts.title == "Accounts")
}

@MainActor
@Test func aFixtureIsShownAsOfItsOwnTime() throws {
    let store = Store(fixture: example("status.notification.json"))
    #expect(store.all.count == 6)
    // The words are as of the status's time, whenever it's drawn.
    #expect(store.words(now: .distantFuture).now == store.status?.at)
    #expect(store.words().agent("claude-code") == "Claude Code")
    // A status only as serve pushes it, not in the answer to hello.
    #expect(Store(fixture: example("hello.result.json")).status == nil)
    #expect(Store(fixture: "/nonexistent.json").status == nil)
    #expect(Store(fixture: nil).status == nil)
}

@MainActor
@Test func theAgentToOfferConnectingIsOneInUseThatCantAsk() throws {
    let store = Store(fixture: example("status.notification.json"))
    // Claude Code, the agent in use, is connected, so there's none to offer.
    #expect(store.toConnect(dismissed: []) == nil)
    var status = try #require(store.status)
    status.agents[1].inUse = true
    let using = Store(client: nil, status: status)
    #expect(using.toConnect(dismissed: [])?.id == "codex")
    #expect(using.toConnect(dismissed: ["codex"]) == nil)
    // Run from its disk image, the app offers none.
    #expect(Store(client: nil, status: status, fromDiskImage: true).toConnect(dismissed: []) == nil)
}

/// The contract's example as on a first launch, once the limits are read
/// and before any history is: no account is known to be in use, or used
/// this week.
@MainActor
private func firstLaunch() throws -> Status {
    var status = try #require(Store(fixture: example("status.notification.json")).status)
    status.historyRead = false
    for index in status.accounts.indices {
        status.accounts[index].inUse = false
        status.accounts[index].recent = false
    }
    return status
}

@MainActor
@Test func untilHistoryIsReadEveryAccountShownCountsAsInUse() throws {
    var status = try firstLaunch()
    let shown = status.accounts.filter { !$0.hidden }.map(\.id)
    let reading = Store(client: nil, status: status)
    // None is folded away, and the headline is about the first, the most
    // urgent, as serve lists them until then.
    #expect(reading.inUse.map(\.id) == shown)
    #expect(reading.recent.isEmpty && reading.unused.isEmpty)
    let verdict = reading.verdict(in: reading.words())
    #expect(verdict.headline != "Nothing in use")
    #expect(verdict == reading.words().verdict(Array(reading.inUse.prefix(1))))
    // Once it's read, serve's word holds: nothing in use, the rest folded.
    status.historyRead = true
    let read = Store(client: nil, status: status)
    #expect(read.inUse.isEmpty && read.unused.map(\.id) == shown)
    #expect(read.verdict(in: read.words()).headline == "Nothing in use")
}

@MainActor
@Test func withNoAccountTheHeadlineSaysWhyAsSettingsDoes() throws {
    // As `hello` answers on a first launch, before the limits are read.
    var status = try firstLaunch()
    status.accounts = []
    let reading = Store(client: nil, status: status)
    #expect(reading.verdict(in: reading.words()).headline == "Getting your limits")
    status.historyRead = true
    let read = Store(client: nil, status: status)
    #expect(read.verdict(in: read.words()).headline == "No accounts found yet")
}
