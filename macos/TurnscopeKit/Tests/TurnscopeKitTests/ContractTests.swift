// The app reads what `turnscope watch` writes, and writes what it reads, as
// `contract/` holds them; the Rust tests check the other side of each file.

import Foundation
import Testing
@testable import TurnscopeKit

@Test func theFeedIsRead() throws {
    let feed = try Contract.decoder.decode(Feed.self, from: contract("feed.json"))
    #expect(feed.version == Feed.version)
    #expect(!feed.reading)
    #expect(feed.accounts.map(\.id) == ["claude:a", "chatgpt:b", "supergrok:c", "api:openrouter"])
    let claude = feed.accounts[0]
    #expect(claude.title == "Claude Max")
    #expect(claude.inUse)
    #expect(claude.standing == .runningOut)
    #expect(claude.decidingLimit?.key == "five_hour")
    #expect(claude.decidingLimit?.runsOutAt == ISO8601DateFormatter().date(from: "2026-09-30T14:00:00Z"))
    #expect(claude.limits[1].leftAtReset == 43.2)
    #expect(claude.limits[1].reserve == 10)
    #expect(claude.limits[0].reserve == -40)
    #expect(claude.limits[0].budget == 6.67)
    #expect(claude.limits[1].budget == nil)
    #expect(claude.usedMost.first?.title == "Rethink the architecture")
    #expect(claude.advice == .reread(context: 966_000))
    #expect(feed.accounts[2].problem == .signIn)
    #expect(feed.accounts[3].hidden)
    #expect(feed.accounts.map(\.apiKey) == [false, false, false, true])
    #expect(feed.agents.map(\.status) == [.connected, .available, .outdated, .unsupported])
    #expect(feed.agents.map(\.logo) == ["providers/anthropic", "providers/openai", "providers/opencode", "agents/pi"])
    #expect(feed.reads.map(\.name) == ["Claude Code", "Codex", "OpenCode", "Pi", "Grok Build"])
    #expect(feed.accounts[0].agents.map(\.name) == ["Claude Code"])
}

@Test func everyOtherLineIsRead() throws {
    let messages = try lines("out.jsonl").map(Message.decode)
    guard case .alert(let alert) = messages[0] else { Issue.record("not an alert"); return }
    #expect(alert.kind == .runningOut)
    #expect(alert.left == 20)
    guard case .recap(let weeks) = messages[1] else { Issue.record("not a recap"); return }
    #expect(weeks.first?.used == 84)
    #expect(messages[2] == .reply(id: 2, error: "codex isn't on this Mac's PATH"))
    #expect(messages[3] == .reply(id: 3, error: nil))
    guard case .alert(let signIn) = messages[4] else { Issue.record("not an alert"); return }
    #expect(signIn.kind == .signIn)
    guard case .alert(let early) = messages[5] else { Issue.record("not an alert"); return }
    #expect(early.kind == .resetEarly)
    #expect(early.left == 100)
}

@Test func everyRequestIsWrittenAsTheContractHasIt() throws {
    let requests: [Request] = [
        .hide(account: "api:openrouter", hidden: true),
        .connect(agent: "codex"),
        .panel(open: true),
    ]
    let expected = try lines("in.jsonl")
    #expect(expected.count == requests.count)
    for (index, request) in requests.enumerated() {
        let written = try JSONSerialization.jsonObject(with: request.line(id: index + 1)) as? NSDictionary
        let contract = try JSONSerialization.jsonObject(with: expected[index]) as? NSDictionary
        #expect(written == contract)
    }
}
