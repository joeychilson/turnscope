// The client and the store over a stand-in for `turnscope serve` on a real
// Unix socket: the handshake on every connection, pushes, requests and their
// errors, reconnecting after serve goes away or stops answering, starting
// serve when nothing listens, and replacing a serve of another engine.

import Foundation
import Testing
@testable import TurnscopeKit

/// A stand-in serve that answers `hello` with the contract's example, and
/// every other request with what `other` returns.
private func serve(at path: String = socketPath(),
                   other: @escaping FakeServe.Answer = { _, _, _ in .done }) throws -> FakeServe {
    let hello = Reply.of(try helloResult())
    return FakeServe(at: path) { method, params, serve in
        method == "hello" ? hello : other(method, params, serve)
    }
}

/// A client and a store over `serve`. The binary reports `version`, which
/// defaults to the engine named in the contract's `hello`.
@MainActor
private func store(over serve: FakeServe, version: String? = nil) throws -> (Client, Store) {
    let client = Client(binary: try binary(socket: serve.path, started: marker(), version: version))
    return (client, Store(client: client))
}

/// Wait until `client` is connected and `store` has heard so and has a
/// status. What the client hears reaches the store a moment later.
@MainActor
private func connected(_ client: Client, _ store: Store) async {
    await until { client.phase == .connected && store.status != nil && store.failure == nil }
}

/// A client and a store over `serve`, started, once they're connected.
@MainActor
private func connected(to serve: FakeServe) async throws -> (Client, Store) {
    let (client, store) = try store(over: serve)
    client.start()
    await connected(client, store)
    return (client, store)
}

@MainActor
@Test func everyConnectionBeginsWithHelloWhoseAnswerIsTheWholeStatus() async throws {
    let serve = try serve()
    defer { serve.close() }
    let (client, store) = try store(over: serve)
    var alerts: [AlertKind] = []
    store.onAlert = { alerts.append($0.kind) }
    // Whether what's shown was out of date, each time the store said it
    // changed.
    var outOfDate: [Bool] = []
    store.onChange = { [weak store] in outOfDate.append(store?.failure != nil) }
    client.start()
    await until { store.status != nil }
    #expect(serve.methods.first == "hello")
    #expect(serve.params(of: "hello").first?["protocol"] as? Int == protocolVersion)
    #expect(store.all.count == 6)
    #expect(store.inUse.map(\.id) == ["anthropic:acct-1:org-1"])
    // The alerts kept for a client come with the handshake.
    #expect(alerts == [.runningOut, .usedUp, .reset, .signIn])
    #expect(client.phase == .connected)
    // When serve goes away, the client says so and connects again. It says
    // hello again, which is all it takes to get back in sync.
    serve.drop()
    await until { client.phase != .connected }
    #expect(await until { serve.methods.filter { $0 == "hello" }.count == 2 })
    await connected(client, store)
    #expect(store.failure == nil)
    // The store said so as serve went away, and again once it was back.
    #expect(outOfDate.contains(true) && outOfDate.last == false)
    client.stop()
}

@MainActor
@Test func whatServePushesIsTakenAsItComes() async throws {
    let serve = try serve()
    defer { serve.close() }
    let (client, store) = try await connected(to: serve)
    var told: [Int] = []
    store.onAlert = { told.append($0.id) }
    var status = try helloResult()["status"] as? [String: Any] ?? [:]
    status["accounts"] = []
    serve.push("status", status)
    #expect(await until { store.all.isEmpty })
    let alert = try JSONSerialization.jsonObject(with: contract("alert.notification.json")) as? [String: Any]
    var pushed = alert?["params"] as? [String: Any] ?? [:]
    pushed["id"] = 42
    serve.push("alert", pushed)
    await until { told.contains(42) }
    #expect(told.last == 42)
    store.acknowledge([42])
    await until { !serve.params(of: "alerts.ack").isEmpty }
    #expect(serve.params(of: "alerts.ack").first?["ids"] as? [Int] == [42])
    client.stop()
}

@MainActor
@Test func aPushItCantReadSaysWhatsShownIsOutOfDateAndConnectsAgain() async throws {
    let serve = try serve()
    defer { serve.close() }
    let (client, store) = try await connected(to: serve)
    serve.push("status", ["accounts": "not a list"])
    await until { store.failure != nil }
    #expect(store.failure?.hasPrefix("Turnscope couldn't read what its engine sent") == true)
    // The last status it read stays, marked out of date, until hello on the
    // next connection brings it up to date.
    #expect(store.status != nil)
    await connected(client, store)
    #expect(serve.methods.filter { $0 == "hello" }.count == 2)
    serve.push("alert", ["id": "not a number"])
    await until { store.failure != nil }
    #expect(store.failure?.hasPrefix("Turnscope couldn't read what its engine sent") == true)
    client.stop()
}

@MainActor
@Test func aServeThatDoesntAnswerHelloIsSaidAndTriedAgain() async throws {
    let serve = FakeServe { _, _, _ in .silence }
    defer { serve.close() }
    let (client, store) = try store(over: serve)
    client.helloWait = .milliseconds(300)
    client.start()
    await until { store.failure != nil }
    #expect(store.failure == "Turnscope's engine didn't answer")
    #expect(await until { serve.methods.filter { $0 == "hello" }.count >= 2 })
    client.stop()
}

@MainActor
@Test func whatServeRefusesIsPutBackOrSaid() async throws {
    let serve = try serve { method, _, _ in
        switch method {
        case "account.hide": .error("no account is openai:acct-2")
        case "agent.connect": .error("couldn't run codex: is Codex installed?")
        default: .done
        }
    }
    defer { serve.close() }
    let (client, store) = try await connected(to: serve)
    store.setHidden(true, for: "openai:acct-2")
    // It's hidden at once, before serve answers, and put back once serve
    // refuses.
    #expect(store.hidden.contains { $0.id == "openai:acct-2" })
    await until { !store.hidden.contains { $0.id == "openai:acct-2" } }
    #expect(serve.params(of: "account.hide").first?["hidden"] as? Bool == true)
    store.connect("codex")
    #expect(store.changingAgents == ["codex": .connected])
    await until { store.agentErrors["codex"] != nil }
    #expect(store.agentErrors["codex"] == "couldn't run codex: is Codex installed?")
    #expect(store.changingAgents.isEmpty)
    client.stop()
}

@MainActor
@Test func anAgentShowsConnectedOnceServeHasConnectedIt() async throws {
    let serve = try serve()
    defer { serve.close() }
    let (client, store) = try await connected(to: serve)
    let connection = { (id: String) in store.status?.agents.first { $0.id == id }?.connection }
    // Before the next status says so, so the old button doesn't come back
    // in between.
    store.connect("codex")
    await until { store.changingAgents.isEmpty }
    #expect(connection("codex") == .connected)
    store.disconnect("claude-code")
    await until { store.changingAgents.isEmpty }
    #expect(connection("claude-code") == .available)
    client.stop()
}

@MainActor
@Test func theSettingsChangeAtOnceAndGoToServeWhole() async throws {
    let serve = try serve()
    defer { serve.close() }
    let (client, store) = try await connected(to: serve)
    store.setNotify { $0.reset = false }
    #expect(store.notify?.reset == false)
    await until { !serve.params(of: "settings.set").isEmpty }
    let sent = serve.params(of: "settings.set").first?["settings"] as? [String: Any]
    #expect((sent?["notify"] as? [String: Bool]) == ["runningOut": true, "usedUp": true, "reset": false, "signIn": true])
    client.stop()
}

@MainActor
@Test func aRefusedSettingsChangeGoesBackToWhatServeLastSent() async throws {
    // Serve sends a status with other settings, then refuses the change.
    var status = try helloResult()["status"] as? [String: Any] ?? [:]
    status["settings"] = ["notify": ["runningOut": true, "usedUp": true, "reset": true, "signIn": false]]
    let newer = try JSONSerialization.data(withJSONObject: status)
    let serve = try serve { method, _, serve in
        guard method == "settings.set" else { return .done }
        serve.push("status", (try? JSONSerialization.jsonObject(with: newer)) ?? [:])
        return .error("the settings couldn't be saved")
    }
    defer { serve.close() }
    let (client, store) = try await connected(to: serve)
    store.setNotify { $0.reset = false }
    #expect(store.notify?.reset == false)
    await until { store.notify?.signIn == false && store.notify?.reset == true }
    // It's what serve sent, not what was shown before the change, and not
    // the change.
    #expect(store.notify == Notify(runningOut: true, usedUp: true, reset: true, signIn: false))
    client.stop()
}

@MainActor
@Test func whatUsedALimitIsAskedOnlyForTheAccountOpen() async throws {
    let used = Reply.of(["sessions": [["session": "claude-code:s", "title": "Fix it", "project": "app",
                                       "agent": "claude-code", "sharePercent": 31.5, "running": true]]])
    let serve = try serve { method, _, _ in method == "limit.breakdown" ? used : .done }
    defer { serve.close() }
    let (client, store) = try await connected(to: serve)
    #expect(!serve.methods.contains("limit.breakdown"))
    let claude = try #require(store.all.first { $0.id == "anthropic:acct-1:org-1" })
    store.watch(claude.id)
    await until { !store.usedMost(claude).isEmpty }
    let asked = serve.params(of: "limit.breakdown")
    #expect(asked.allSatisfy { $0["account"] as? String == claude.id })
    #expect(asked.first?["limit"] as? String == "five_hour")
    #expect(store.usedMost(claude).first?.sharePercent == 31.5)
    // They're for its deciding limit. Once another limit decides it, they
    // aren't shown until they're asked for again.
    var decided = claude
    decided.deciding = "seven_day"
    #expect(store.usedMost(decided).isEmpty)
    // An account not in use is asked about the same way, once it's open.
    let chatgpt = try #require(store.all.first { $0.id == "openai:acct-2" })
    store.watch(chatgpt.id)
    await until { !store.usedMost(chatgpt).isEmpty }
    #expect(serve.params(of: "limit.breakdown").last?["limit"] as? String == "secondary")
    client.stop()
}

@MainActor
@Test func whatUsedEachAccountListedIsAskedAsThePanelOpens() async throws {
    let serve = try serve { method, _, _ in method == "limit.breakdown" ? .of(["sessions": []]) : .done }
    defer { serve.close() }
    let (client, store) = try await connected(to: serve)
    store.askWhatUsedEach()
    let listed = Set(store.all.filter { !$0.hidden && $0.deciding != nil }.map(\.id))
    await until { serve.params(of: "limit.breakdown").count == listed.count }
    #expect(Set(serve.params(of: "limit.breakdown").compactMap { $0["account"] as? String }) == listed)
    client.stop()
}

@MainActor
@Test func whenNothingListensServeIsStartedAndConnectedTo() async throws {
    let path = socketPath()
    let started = marker()
    let client = Client(binary: try binary(socket: path, started: started))
    let store = Store(client: client)
    client.start()
    // The client ran `serve`. It comes up a moment later, as serve does.
    #expect(await until { FileManager.default.fileExists(atPath: started.path) })
    let serve = try serve(at: path)
    defer { serve.close() }
    await until { store.status != nil }
    #expect(client.phase == .connected)
    client.stop()
}

@MainActor
@Test func aServeOfAnotherEngineIsAskedToExitOnceAndItsSuccessorKept() async throws {
    // As after an update, when the running serve was started by the app's
    // previous version.
    let serve = try serve()
    defer { serve.close() }
    let (client, store) = try store(over: serve, version: "turnscope 9.9.9")
    client.start()
    await until { serve.methods.contains("shutdown") }
    #expect(serve.methods == ["hello", "shutdown"])
    // It takes no parameters, so it's sent none: JSON-RPC doesn't allow
    // null there.
    #expect(serve.calls(of: "shutdown").first?.keys.contains("params") == false)
    #expect(store.status == nil)
    // It exits. The serve connected to next is kept, whatever engine it
    // runs, so two apps can't take turns ending each other's serve.
    serve.drop()
    await connected(client, store)
    #expect(serve.methods == ["hello", "shutdown", "hello"])
    #expect(store.status != nil)
    client.stop()
}

@MainActor
@Test func aServeOfAnotherEngineThatDoesntExitIsKeptAfterAWhile() async throws {
    let serve = try serve { method, _, _ in method == "shutdown" ? .silence : .done }
    defer { serve.close() }
    let (client, store) = try store(over: serve, version: "turnscope 9.9.9")
    client.exitWait = .milliseconds(300)
    client.start()
    // It neither answers nor exits. The client stops waiting, connects
    // again, and keeps it.
    await connected(client, store)
    #expect(serve.methods == ["hello", "shutdown", "hello"])
    client.stop()
}

@MainActor
@Test func aServeOfAnotherEngineIsReplacedThoughItsStatusCantBeRead() async throws {
    // This app can't read this engine's status. The field that names the
    // engine is read first, so it's replaced anyway.
    let hello = Reply.of(try helloResult())
    let older = Reply.of(["server": "turnscope 0.0.1", "status": ["accounts": "not a list"], "alerts": []])
    let serve = FakeServe { method, _, serve in
        guard method == "hello" else { return .done }
        return serve.methods.filter { $0 == "hello" }.count == 1 ? older : hello
    }
    defer { serve.close() }
    let (client, store) = try store(over: serve)
    client.start()
    await until { serve.methods.contains("shutdown") }
    #expect(store.failure == nil)
    serve.drop()
    await connected(client, store)
    #expect(serve.methods == ["hello", "shutdown", "hello"])
    #expect(store.status != nil)
    client.stop()
}

@MainActor
@Test func aServeThatDoesntSpeakTheProtocolIsAskedToExit() async throws {
    let hello = Reply.of(try helloResult())
    let serve = FakeServe { method, _, serve in
        guard method == "hello" else { return .done }
        return serve.methods.filter { $0 == "hello" }.count == 1
            ? .error(code: -32001, "this serve speaks protocol 2")
            : hello
    }
    defer { serve.close() }
    let (client, store) = try store(over: serve)
    client.start()
    await until { serve.methods.contains("shutdown") }
    serve.drop()
    await connected(client, store)
    #expect(serve.methods == ["hello", "shutdown", "hello"])
    #expect(store.status != nil)
    client.stop()
}

@MainActor
@Test func aServeThatStillDoesntSpeakTheProtocolIsntAskedToExitAgain() async throws {
    let serve = FakeServe { method, _, _ in
        method == "hello" ? .error(code: -32001, "this serve speaks protocol 2") : .done
    }
    defer { serve.close() }
    let (client, store) = try store(over: serve)
    client.start()
    await until { serve.methods.contains("shutdown") }
    serve.drop()
    // The serve after it is kept, so the client says why it can't connect,
    // and keeps trying.
    await until { store.failure != nil }
    #expect(store.failure == "this serve speaks protocol 2")
    #expect(await until { serve.methods.filter { $0 == "hello" }.count >= 3 })
    #expect(serve.methods.filter { $0 == "shutdown" }.count == 1)
    client.stop()
}

@Test func theWaitToConnectAgainDoublesAndStartsOverOnlyAfterAConnectionHeld() {
    var backoff = Backoff()
    let waits = (0..<8).map { _ in backoff.next() }
    #expect(waits == [.milliseconds(500), .seconds(1), .seconds(2), .seconds(4), .seconds(8), .seconds(16),
                      .seconds(30), .seconds(30)])
    // A connection that dropped within thirty seconds changes nothing.
    backoff.ended(after: .seconds(29))
    #expect(backoff.next() == .seconds(30))
    backoff.ended(after: .seconds(31))
    #expect(backoff.next() == .milliseconds(500))
    #expect(backoff.next() == .seconds(1))
}

@MainActor
@Test func aHelloOfItsOwnEngineItCantReadIsSaidAndTriedAgain() async throws {
    let unreadable = Reply.of(["server": try engine()])
    let serve = FakeServe { method, _, _ in method == "hello" ? unreadable : .done }
    defer { serve.close() }
    let (client, store) = try store(over: serve)
    client.start()
    await until { store.failure != nil }
    #expect(store.failure?.hasPrefix("Turnscope couldn't read what its engine sent") == true)
    #expect(store.status == nil)
    #expect(!serve.methods.contains("shutdown"))
    // It connects again on its own and says hello again.
    #expect(await until { serve.methods.filter { $0 == "hello" }.count >= 2 })
    client.stop()
}

@MainActor
@Test func whatIsAskedWhileNotConnectedSaysSo() async throws {
    let client = Client(binary: URL(fileURLWithPath: "/nonexistent/turnscope"))
    await #expect(throws: ClientError.notConnected) {
        let _: Done = try await client.request("shutdown")
    }
    let store = Store(client: client)
    client.start()
    await until { store.failure != nil }
    #expect(store.failure == "Turnscope couldn't run its engine, /nonexistent/turnscope")
    client.stop()
}
