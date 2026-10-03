// The engine client, over shell scripts that stand in for `turnscope watch`
// and write lines of the contract as it would.

import Foundation
import Testing
@testable import TurnscopeKit

private let alert = #"{"alert":{"account":"claude:a","title":"Claude Max","label":null,"limit":"5 hours","scope":null,"kind":"used_up","at":null,"left":0.0}}"#

@MainActor
@Test func aLineWrittenInPiecesIsReadWhole() async throws {
    let half = alert.index(alert.startIndex, offsetBy: 40)
    let engine = Engine(binary: try script("""
        printf '%s' '\(alert[..<half])'
        sleep 0.2
        printf '%s\\n%s\\n' '\(alert[half...])' '\(alert)'
        exec cat >/dev/null
        """))
    var messages: [Message] = []
    engine.onMessage = { messages.append($0) }
    engine.start()
    await until { messages.count == 2 }
    engine.stop()
    guard case .alert(let told) = messages.first else { Issue.record("no alert: \(messages)"); return }
    #expect(told.kind == .usedUp)
    #expect(messages.count == 2)
}

@MainActor
@Test func anEngineThatEndsSaysWhyAndIsStartedAgain() async throws {
    let runs = FileManager.default.temporaryDirectory.appending(path: "runs-\(UUID().uuidString)")
    // The first run leaves half a line and fails; the second writes a whole
    // one, which the first's half must not spoil.
    let engine = Engine(binary: try script("""
        if [ -e '\(runs.path)' ]; then
          printf '%s\\n' '\(alert)'
          exec cat >/dev/null
        fi
        touch '\(runs.path)'
        printf '{"alert":{"acc'
        echo 'turnscope: the ledger is locked' >&2
        exit 1
        """))
    var phases: [Engine.Phase] = []
    var messages: [Message] = []
    engine.onPhase = { phases.append($0) }
    engine.onMessage = { messages.append($0) }
    engine.start()
    await until { !messages.isEmpty }
    engine.stop()
    #expect(phases.contains(.failed(why: "the ledger is locked")))
    #expect(phases.last == .running)
    guard case .alert(let told) = messages.first else { Issue.record("no alert: \(messages)"); return }
    #expect(told.account == "claude:a")
}

@MainActor
@Test func aRequestToAnEngineThatStoppedReadingFailsQuietly() async throws {
    // It closes its input, says so, and lives on: writing to it would raise
    // SIGPIPE, which ends the app, were the pipe not told not to.
    let engine = Engine(binary: try script("exec 0<&-\nprintf '%s\\n' '\(alert)'\nsleep 5"))
    var closed = false
    engine.onMessage = { _ in closed = true }
    engine.start()
    await until { closed }
    var replies: [String?] = []
    for _ in 0..<3 {
        engine.send(.panel(open: true)) { replies.append($0) }
    }
    await until { replies.count == 3 }
    engine.stop()
    #expect(replies.allSatisfy { $0 == "Turnscope's engine isn't running" }, "\(replies)")
}

@MainActor
@Test func repliesGoToTheirRequests() async throws {
    // It answers each request it reads with its id, as watch does.
    let engine = Engine(binary: try script("""
        while read -r line; do
          id=$(printf '%s' "$line" | sed 's/.*"id":\\([0-9]*\\).*/\\1/')
          printf '{"reply":{"id":%s,"error":"no %s"}}\\n' "$id" "$id"
        done
        """))
    engine.start()
    var replies: [String?] = []
    engine.send(.panel(open: true)) { replies.append($0) }
    engine.send(.connect(agent: "codex")) { replies.append($0) }
    await until { replies.count == 2 }
    engine.stop()
    #expect(replies == ["no 1", "no 2"])
}
