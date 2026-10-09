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
}
