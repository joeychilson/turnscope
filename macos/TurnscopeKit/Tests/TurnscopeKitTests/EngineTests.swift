// The client and the store against the real engine: `turnscope serve` from
// the binary `TURNSCOPE_BINARY` names, off the network, over a scratch data
// directory and the snapshot tests' home, which holds two made-up sessions.
// It runs only when that variable is set, to an absolute path:
//
//   TURNSCOPE_BINARY="$PWD/target/debug/turnscope" swift test --package-path macos/TurnscopeKit

import Foundation
import Testing
@testable import TurnscopeKit

private let realEngine = ProcessInfo.processInfo.environment["TURNSCOPE_BINARY"]

@MainActor
@Test(.enabled(if: realEngine != nil, "set TURNSCOPE_BINARY to a turnscope binary"))
func theRealEngineIsStartedSaysHelloAndTakesChanges() async throws {
    let binary = URL(fileURLWithPath: try #require(realEngine))
    let data = FileManager.default.temporaryDirectory.appending(path: "ts-\(UUID().uuidString.prefix(8))")
    defer { try? FileManager.default.removeItem(at: data) }
    let home = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()
        .appending(path: "tests/fixtures/home")
    // Stay off the network while this test runs. The engine it starts reads
    // this variable.
    setenv("TURNSCOPE_OFFLINE", "1", 1)
    defer { unsetenv("TURNSCOPE_OFFLINE") }
    let client = Client(binary: binary, arguments: ["--data", data.path, "--home", home.path])
    let store = Store(client: client)
    client.start()
    // Nothing is listening, so the client starts serve, which reads the
    // history first.
    await until { store.status?.historyReadAt != nil }
    #expect(client.phase == .connected)
    #expect(store.status?.agents.isEmpty == false)
    // A change goes through serve, which then pushes a status that has it.
    store.setNotify { $0.reset = false }
    await until { store.notify?.reset == false && store.failure == nil }
    let _: Done = try await client.request("shutdown", Done())
    client.stop()
    #expect(store.notify?.reset == false)
}
