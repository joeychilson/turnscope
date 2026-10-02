// The store, over an engine that is a shell script writing the contract's
// feed and answering requests as `turnscope watch` would.

import Foundation
import Testing
@testable import TurnscopeKit

/// An engine that writes the contract's feed on one line, then answers each
/// request with `error`, as JSON.
@MainActor
private func engine(answering error: String) throws -> Engine {
    let line = try JSONSerialization.data(withJSONObject: JSONSerialization.jsonObject(with: contract("feed.json")))
    let feed = FileManager.default.temporaryDirectory.appending(path: "turnscope-feed-\(UUID().uuidString)")
    try (#"{"feed":"# + String(decoding: line, as: UTF8.self) + "}\n").write(to: feed, atomically: true, encoding: .utf8)
    return Engine(binary: try script("cat '\(feed.path)'\n" + replying(error)))
}

@MainActor
@Test func anAccountTheEngineWontHideIsShownAgain() async throws {
    let engine = try engine(answering: #""the ledger is read-only""#)
    let store = Store(engine: engine)
    engine.start()
    await until { store.feed != nil }
    #expect(store.resting.map(\.id) == ["chatgpt:b", "supergrok:c"])
    store.setHidden("chatgpt:b", true)
    // Hidden at once, before the engine answers.
    #expect(store.resting.map(\.id) == ["supergrok:c"])
    await until { store.resting.count == 2 }
    engine.stop()
    #expect(store.resting.map(\.id) == ["chatgpt:b", "supergrok:c"])
}

@MainActor
@Test func anAccountTheEngineHidesStaysHidden() async throws {
    let engine = try engine(answering: "null")
    let store = Store(engine: engine)
    engine.start()
    await until { store.feed != nil }
    var answered = false
    store.setHidden("chatgpt:b", true)
    engine.send(.panel(open: false)) { _ in answered = true }
    await until { answered }
    engine.stop()
    #expect(store.hidden.map(\.id) == ["chatgpt:b", "api:openrouter"])
}
