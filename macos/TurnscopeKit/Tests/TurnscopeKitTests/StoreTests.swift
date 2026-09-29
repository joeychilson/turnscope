// The store, over an engine that is a shell script writing the contract's
// feed and answering requests as `turnscope watch` would.

import Foundation
import Testing
@testable import TurnscopeKit

/// An engine that writes the contract's feed on one line, then answers each
/// request with `error`, as JSON.
@MainActor
private func engine(answering error: String) throws -> Engine {
    let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()
    let pretty = try Data(contentsOf: root.appending(path: "contract/feed.json"))
    let line = try JSONSerialization.data(withJSONObject: JSONSerialization.jsonObject(with: pretty))
    let folder = FileManager.default.temporaryDirectory.appending(path: "turnscope-\(UUID().uuidString)")
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    try (#"{"feed":"# + String(decoding: line, as: UTF8.self) + "}\n").write(
        to: folder.appending(path: "feed"), atomically: true, encoding: .utf8)
    let url = folder.appending(path: "turnscope")
    try """
        #!/bin/sh
        cat '\(folder.path)/feed'
        while read -r line; do
          id=$(printf '%s' "$line" | sed 's/.*"id":\\([0-9]*\\).*/\\1/')
          printf '{"reply":{"id":%s,"error":%s}}\\n' "$id" '\(error)'
        done
        """.write(to: url, atomically: true, encoding: .utf8)
    try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
    return Engine(binary: url)
}

@MainActor
private func until(_ done: () -> Bool) async {
    for _ in 0..<500 where !done() {
        try? await Task.sleep(for: .milliseconds(10))
    }
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
