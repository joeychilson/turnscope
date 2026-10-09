// What the app shows, as serve last sent it, plus what's in progress: which
// agents are being connected and what went wrong. Views read from it and act
// through it; it talks to serve.

import Foundation
import Observation

@MainActor
@Observable
public final class Store {
    /// The last status. It's kept until a newer one arrives, even while
    /// serve is away, so the panel never goes empty during a restart.
    public private(set) var status: Status?
    /// Why serve isn't connected, or nil while it is.
    public private(set) var failure: String?
    /// The agents being connected or disconnected right now, by id.
    public private(set) var changingAgents: Set<String> = []
    /// Why connecting or disconnecting an agent failed, by agent id. It's
    /// kept until that's tried again.
    public private(set) var agentErrors: [String: String] = [:]
    /// What used each account's deciding limit, by account id, with the key
    /// of the limit it's for. It's asked for as the panel opens, and again
    /// with each status while the panel has that account open.
    private var used: [String: (limit: String, sessions: [UsedMost])] = [:]

    /// Called with each alert to post, from `hello` and as they're pushed.
    @ObservationIgnored public var onAlert: (Alert) -> Void = { _ in }
    /// Called after each new status is taken in, and each time serve
    /// connects or fails, since what's shown is then out of date or current
    /// again.
    @ObservationIgnored public var onChange: () -> Void = {}

    @ObservationIgnored private let client: Client?
    /// The task that takes in what the client hears, when there's a client.
    @ObservationIgnored private var listening: Task<Void, Never>?
    /// The account the panel has open. The store asks what used its deciding
    /// limit.
    @ObservationIgnored private var watched: String?
    /// The settings serve last sent. A change serve refuses goes back to
    /// these.
    @ObservationIgnored private var served: Settings?
    /// Every agent's name, by id, as the status gives them.
    private var names: [String: String] = [:]
    /// For a fixed status, the time its words are as of, whatever the clock
    /// says. It's the status's own time, so it reads the same whenever it's
    /// drawn.
    @ObservationIgnored private let frozen: Date?

    /// A store over `client`, or with no client for a fixed `status`, as
    /// `--fixture` and `--snapshot` show.
    public init(client: Client?, status: Status? = nil) {
        self.client = client
        frozen = client == nil ? status?.at : nil
        if let status { show(status) }
        guard let client else { return }
        listening = Task { [weak self] in
            for await event in client.events {
                self?.take(event)
            }
        }
    }

    /// A store with no engine, showing the status in the file at `path`, as
    /// `--fixture` names it. There's no status if the file can't be read.
    public convenience init(fixture path: String?) {
        let status = path
            .flatMap { FileManager.default.contents(atPath: $0) }
            .flatMap(Contract.status)
        self.init(client: nil, status: status)
    }

    /// The accounts in use, most urgent first, as serve orders them.
    public var inUse: [AccountStatus] { all.filter { !$0.hidden && $0.inUse } }
    /// The accounts not in use but used this week, most left first.
    public var recent: [AccountStatus] { all.filter { !$0.hidden && !$0.inUse && $0.recent } }
    /// The accounts not used this week, most left first.
    public var unused: [AccountStatus] { all.filter { !$0.hidden && !$0.inUse && !$0.recent } }
    /// The accounts hidden from the menu bar and the panel.
    public var hidden: [AccountStatus] { all.filter(\.hidden) }
    /// Every account, hidden ones too, as Settings lists them.
    public var all: [AccountStatus] { status?.accounts ?? [] }
    /// The agents installed here, each with whether it runs Turnscope.
    public var agents: [AgentStatus] { status?.agents.filter(\.installed) ?? [] }
    /// The names of every agent Turnscope reads, installed or not.
    public var agentNames: [String] { status?.agents.map(\.name) ?? [] }
    /// Whether the agents' history is still being read for the first time.
    public var reading: Bool { status != nil && status?.historyReadAt == nil }
    /// Which notifications are turned on.
    public var notify: Notify? { status?.settings.notify }

    /// Words as of `now`, or as of a fixed status's time, with agents named
    /// as in the status.
    public func words(now: Date = .now) -> Words {
        Words(now: frozen ?? now, names: names)
    }

    /// The agent in use to offer to connect: one that can't ask Turnscope
    /// about its limits, because it isn't connected or points at another
    /// copy. Agents in `dismissed`, where the offer was put away, are
    /// skipped.
    public func toConnect(dismissed: Set<String>) -> AgentStatus? {
        agents.first { $0.inUse && $0.connection != .connected && !dismissed.contains($0.id) }
    }

    // MARK: Requests to serve

    /// Hide `account`, or show it again. It changes at once, and changes
    /// back if serve refuses.
    public func setHidden(_ hidden: Bool, for account: String) {
        mark(account, hidden: hidden)
        guard let client else { return }
        Task {
            do {
                let _: Done = try await client.request("account.hide", HideParams(account: account, hidden: hidden))
            } catch {
                mark(account, hidden: !hidden)
            }
        }
    }

    private func mark(_ account: String, hidden: Bool) {
        if let index = status?.accounts.firstIndex(where: { $0.id == account }) {
            status?.accounts[index].hidden = hidden
        }
    }

    /// Add Turnscope's MCP server to the agent `id`.
    public func connect(_ id: String) {
        run("agent.connect", on: id, then: .connected)
    }

    /// Remove Turnscope's MCP server from the agent `id`.
    public func disconnect(_ id: String) {
        run("agent.disconnect", on: id, then: .available)
    }

    /// Have serve run the agent `id`'s own command for `method`. It shows as
    /// working until it's done, then as `connection`, and shows why if it
    /// fails.
    private func run(_ method: String, on id: String, then connection: Connection) {
        guard let client else { return }
        changingAgents.insert(id)
        agentErrors[id] = nil
        Task {
            defer { changingAgents.remove(id) }
            do throws(ClientError) {
                let _: Done = try await client.request(method, AgentParams(agent: id))
                // Shown now, so the old button doesn't come back until the
                // status serve pushes next says the same.
                if let index = status?.agents.firstIndex(where: { $0.id == id }) {
                    status?.agents[index].connection = connection
                }
            } catch {
                agentErrors[id] = error.description
            }
        }
    }

    /// Change which notifications are sent. It changes at once and serve
    /// keeps it. If serve refuses a change, it goes back to what serve last
    /// sent, not to what was shown before, since a status may have come in
    /// between.
    public func setNotify(_ change: (inout Notify) -> Void) {
        guard var settings = status?.settings else { return }
        let before = settings
        change(&settings.notify)
        status?.settings = settings
        guard let client else { return }
        Task {
            do {
                let _: Done = try await client.request("settings.set", SettingsParams(settings: settings))
            } catch {
                status?.settings = served ?? before
            }
        }
    }

    /// Tell serve these alerts were shown, so it doesn't send them again.
    public func acknowledge(_ ids: [Int]) {
        guard !ids.isEmpty, let client else { return }
        Task { _ = try? await client.request("alerts.ack", AckParams(ids: ids), as: Done.self) }
    }

    /// The sessions that used `account`'s deciding limit most, as last asked
    /// for. None if they were for a limit that no longer decides it.
    public func usedMost(_ account: AccountStatus) -> [UsedMost] {
        guard let used = used[account.id], used.limit == account.deciding else { return [] }
        return used.sessions
    }

    /// Ask what used each account the panel lists, as it opens, so an
    /// account opens with them already there. Asked for only once it was
    /// open, they came up to a quarter of a second later and pushed the rows
    /// below it down a second time.
    public func askWhatUsedEach() {
        for account in all where !account.hidden { ask(account) }
    }

    /// Set which account the panel has open, or nil if none is or the panel
    /// is closed. What used it is asked for now, and again with each status
    /// while it stays open.
    public func watch(_ account: String?) {
        guard account != watched else { return }
        watched = account
        askWhatUsed()
    }

    /// Ask what used the account the panel has open.
    private func askWhatUsed() {
        guard let id = watched, let account = all.first(where: { $0.id == id }) else { return }
        ask(account)
    }

    /// Ask what used `account`'s deciding limit.
    private func ask(_ account: AccountStatus) {
        guard let client, let limit = account.deciding else { return }
        Task {
            let asked = BreakdownParams(account: account.id, limit: limit)
            if let breakdown = try? await client.request("limit.breakdown", asked, as: BreakdownResult.self) {
                used[account.id] = (limit, breakdown.sessions)
            }
        }
    }

    // MARK: Events from serve

    private func take(_ event: Client.Event) {
        switch event {
        case .phase(.failed(let why)):
            failure = why
            onChange()
        case .phase(.connected):
            failure = nil
            onChange()
        // What's shown is out of date only once connecting fails, not while
        // a planned reconnect, such as replacing another engine's serve, is
        // under way.
        case .phase(.connecting): break
        case .hello(let hello):
            take(hello.status)
            hello.alerts.forEach(onAlert)
        case .status(let status): take(status)
        case .alert(let alert): onAlert(alert)
        }
    }

    private func take(_ status: Status) {
        show(status)
        served = status.settings
        askWhatUsed()
        onChange()
    }

    private func show(_ status: Status) {
        self.status = status
        names = Dictionary(status.agents.map { ($0.id, $0.name) }, uniquingKeysWith: { first, _ in first })
    }
}
