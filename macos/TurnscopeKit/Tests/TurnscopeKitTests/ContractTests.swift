// The app reads every message serve writes, and writes every request as
// serve reads it, as `contract/` shows each: every answer and
// notification decodes as its message, and every request the app makes
// encodes as the example's parameters.

import Foundation
import Testing
@testable import TurnscopeKit

private struct Answered<R: Decodable>: Decodable { var result: R }
private struct Pushed<P: Decodable>: Decodable { var params: P }

private struct Refused: Decodable {
    struct Error: Decodable {
        var code: Int
        var message: String
        var data: [String: [Int]]?
    }

    var error: Error
}

/// How the app reads each example that serve writes, by file name.
private let answers: [String: @Sendable (Data) throws -> Void] = [
    "hello.result.json": read(Answered<HelloResult>.self),
    "status.notification.json": read(Pushed<Status>.self),
    "alert.notification.json": read(Pushed<Alert>.self),
    "alerts.ack.result.json": read(Answered<Done>.self),
    "limit.breakdown.result.json": read(Answered<BreakdownResult>.self),
    "done.result.json": read(Answered<Done>.self),
    "error.json": read(Refused.self),
]

/// The parameters the app writes for each request example, by file name,
/// or nil for a request that takes none.
private let requests: [String: @Sendable () throws -> Data?] = [
    "hello.request.json": write(HelloParams(protocol: protocolVersion)),
    "alerts.ack.request.json": write(AckParams(ids: [1, 2])),
    "limit.breakdown.request.json": write(BreakdownParams(account: "anthropic:acct-1:org-1", limit: "five_hour")),
    "settings.set.request.json": write(SettingsParams(settings: Settings(
        notify: Notify(runningOut: true, usedUp: true, reset: true, signIn: true)))),
    "account.hide.request.json": write(HideParams(account: "openrouter:api", hidden: true)),
    "agent.connect.request.json": write(AgentParams(agent: "codex")),
    "agent.disconnect.request.json": write(AgentParams(agent: "codex")),
    "shutdown.request.json": { nil },
]

private func read<T: Decodable & SendableMetatype>(_: T.Type) -> @Sendable (Data) throws -> Void {
    { data in _ = try Contract.decoder.decode(T.self, from: data) }
}

private func write(_ params: some Encodable & Sendable) -> @Sendable () throws -> Data? {
    { try Contract.encoder.encode(params) }
}

@Test func everyExampleIsWhatTheAppReadsOrWrites() throws {
    let names = try examples()
    #expect(Set(names) == Set(answers.keys).union(requests.keys), "an example not checked, or one missing")
    for name in names {
        let data = try contract("\(name)")
        if let read = answers[name] {
            do { try read(data) } catch { Issue.record("\(name): \(error)") }
        } else if let write = requests[name] {
            let example = (try JSONSerialization.jsonObject(with: data) as? [String: Any])?["params"]
            let written = try write().map { try JSONSerialization.jsonObject(with: $0) }
            #expect(example as? NSDictionary == written as? NSDictionary, "\(name)")
        }
    }
}

@Test func theStatusSaysWhatTheEngineDecided() throws {
    struct Hello: Decodable { var result: HelloResult }
    let hello = try Contract.decoder.decode(Hello.self, from: contract("hello.result.json")).result
    let status = hello.status
    let claude = status.accounts[0]
    #expect(claude.inUse && claude.standing == .runningOut)
    #expect(claude.decidingLimit?.key == "five_hour")
    #expect(claude.decidingLimit?.name == "5 hours")
    guard case .runsOut(let likely)? = claude.decidingLimit?.outlook else {
        Issue.record("not running out")
        return
    }
    // Every future time carries the horizon that sets how it's worded.
    #expect(likely.horizon == .soon)
    #expect(claude.limits.map(\.standing) == [.runningOut, .lasts, .usedUp])
    let chatgpt = status.accounts[1]
    #expect(chatgpt.state == .asOf(readAt: Date(timeIntervalSince1970: 1_791_424_800), why: .expired))
    // Its weekly limit reset since it was read, so it's full, and the
    // monthly limit decides.
    #expect(chatgpt.limits[0].resetSince != nil && chatgpt.limits[0].leftPercent == 100)
    #expect(chatgpt.decidingLimit?.key == "secondary" && chatgpt.refilledAt == nil)
    let keys = status.accounts[2]
    #expect(keys.apiKey && keys.spend?.partial == true)
    #expect(keys.limits[0].money == Money(sizeUsd: 30, leftUsd: 8.7))
    #expect(keys.limits[0].heldBy == ["pi"])
    #expect(status.accounts[3].refused && status.accounts[3].hidden)
    #expect(status.accounts.map(\.recent) == [true, true, true, false, false, false])
    #expect(status.accounts[4].state == .unread(why: .signIn))
    #expect(status.accounts[5].state == .unread(why: .readFailed))
    #expect(status.agents.map(\.connection) == [.connected, .outdated, .available, .available, .available])
    #expect(hello.alerts.map(\.kind) == [.runningOut, .usedUp, .reset, .signIn])
    #expect(status.settings.notify == Notify(runningOut: true, usedUp: true, reset: true, signIn: true))
}

@Test func aValueItDoesntKnowIsAnErrorNeverAGuess() throws {
    // The app talks only to its own engine, which never sends a value the
    // app doesn't know. Anything else comes from another engine, which the
    // client replaces.
    let decode = { (json: String, type: any Decodable.Type) in
        try Contract.decoder.decode(type, from: Data(json.utf8))
    }
    #expect(throws: DecodingError.self) { try decode(#"{"kind":"later"}"#, Outlook.self) }
    #expect(throws: DecodingError.self) { try decode(#"{"kind":"unknown","reason":"new"}"#, Outlook.self) }
    #expect(throws: DecodingError.self) { try decode(#"{"kind":"gone"}"#, AccountState.self) }
    let alert = #"{"id":9,"kind":"weekly","at":0,"account":"a","accountTitle":"A","replaces":"x"}"#
    #expect(throws: DecodingError.self) { try decode(alert, Alert.self) }
    // An unknown field is skipped, as a client of any protocol version
    // does.
    let state = try Contract.decoder.decode(AccountState.self, from: Data(#"{"kind":"unread","why":"signIn","extra":1}"#.utf8))
    #expect(state == .unread(why: .signIn))
}

@Test func aFixtureIsAStatusAloneOrAsServeSendsIt() throws {
    #expect(Contract.status(from: try contract("status.notification.json")) != nil)
    #expect(Contract.status(from: try contract("hello.result.json")) != nil)
    #expect(Contract.status(from: Data("{}".utf8)) == nil)
}

@Test func aSessionsShareMayBeUnknown() throws {
    let answer = try Contract.decoder.decode(Answered<BreakdownResult>.self,
                                             from: contract("limit.breakdown.result.json"))
    #expect(answer.result.sessions.first?.sharePercent == 31.5)
    let unknown = #"{"session":"s","agent":"pi","sharePercent":null,"running":false}"#
    #expect(try Contract.decoder.decode(UsedMost.self, from: Data(unknown.utf8)).sharePercent == nil)
}
