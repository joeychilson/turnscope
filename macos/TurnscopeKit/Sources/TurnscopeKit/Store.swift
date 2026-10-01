// What the app shows, as the engine last said it, and what the person is
// doing with it: which agent is connecting, and what went wrong doing it.
// Views read it and ask through it; it asks the engine.

import Foundation
import Observation

@MainActor
@Observable
public final class Store {
    /// The last feed, kept while a newer one comes, and after the engine
    /// stops, so the panel never empties for a restart.
    public private(set) var feed: Feed?
    /// Why the engine isn't running, while it isn't.
    public private(set) var failure: String?
    /// The agents being connected now.
    public private(set) var connecting: Set<String> = []
    /// Why connecting an agent failed, by its id, until it is tried again.
    public private(set) var connectErrors: [String: String] = [:]

    /// Called with each alert and recap, to tell as a notification.
    public var onAlert: (Alert) -> Void = { _ in }
    public var onRecap: ([Week]) -> Void = { _ in }
    /// Called with each feed, after it is taken.
    public var onFeed: (Feed) -> Void = { _ in }

    private let engine: Engine?
    /// Whether the panel is open, as the engine was last told.
    private var panelOpen = false

    /// A store over `engine`; none for a fixed `feed`, as previews and tests
    /// show.
    public init(engine: Engine?, feed: Feed? = nil) {
        self.engine = engine
        self.feed = feed
        engine?.onMessage = { [weak self] message in self?.take(message) }
        engine?.onPhase = { [weak self] phase in
            guard let self else { return }
            if case .failed(let why) = phase { failure = why } else { failure = nil }
            // An engine started again knows nothing of the panel.
            if phase == .running, panelOpen { self.engine?.send(.panel(open: true)) }
        }
    }

    // MARK: What is shown

    /// The accounts shown: all but those the person hid.
    public var shown: [Account] { (feed?.accounts ?? []).filter { !$0.hidden } }
    /// The accounts in use, most urgent first.
    public var inUse: [Account] { shown.filter(\.inUse) }
    /// The rest, those with most room first.
    public var resting: [Account] { shown.filter { !$0.inUse } }
    /// The accounts the person hid.
    public var hidden: [Account] { all.filter(\.hidden) }
    /// Every account, hidden ones too, as Settings lists them.
    public var all: [Account] { feed?.accounts ?? [] }
    public var agents: [AgentLink] { feed?.agents ?? [] }
    /// Whether the engine is still reading agents' history for the first time.
    public var reading: Bool { feed?.reading ?? false }

    // MARK: What is asked

    /// Hide `account`, or show it again.
    public func setHidden(_ account: String, _ hidden: Bool) {
        // Shown at once; the next feed confirms it. One the engine refused
        // brings no feed, as nothing changed, so it is put back here.
        mark(account, hidden: hidden)
        engine?.send(.hide(account: account, hidden: hidden)) { [weak self] error in
            if error != nil { self?.mark(account, hidden: !hidden) }
        }
    }

    private func mark(_ account: String, hidden: Bool) {
        if let index = feed?.accounts.firstIndex(where: { $0.id == account }) {
            feed?.accounts[index].hidden = hidden
        }
    }

    /// Connect the agent `id`.
    public func connect(_ id: String) {
        connecting.insert(id)
        connectErrors[id] = nil
        engine?.send(.connect(agent: id)) { [weak self] error in
            self?.connecting.remove(id)
            self?.connectErrors[id] = error
        }
    }

    /// Say the panel opened or closed, so the feed says what used each limit
    /// only while it can be seen.
    public func setPanelOpen(_ open: Bool) {
        guard open != panelOpen else { return }
        panelOpen = open
        engine?.send(.panel(open: open))
    }

    private func take(_ message: Message) {
        switch message {
        case .feed(let feed):
            self.feed = feed
            onFeed(feed)
        case .alert(let alert): onAlert(alert)
        case .recap(let weeks): onRecap(weeks)
        case .reply: break
        }
    }
}
