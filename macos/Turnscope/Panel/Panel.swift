// The menu bar panel, which is Turnscope as a person sees it: the one thing to
// know, large, whether they can keep going; each account in use, the most
// urgent first; and the accounts not in use, folded into a line. Settings and
// Accounts are pages of it, sliding in from the side they lie on.
//
// It floats in a still, transparent window and draws its own glass, edge and
// shadow, so its height moves on the same spring as what is inside it and
// nothing jumps (`App.swift`).

import SwiftUI
import TurnscopeKit

/// How wide the panel is.
let panelWidth: CGFloat = 340

/// What the panel asks of the app around it.
struct PanelActions {
    var checkForUpdates: () -> Void = {}
    var quit: () -> Void = {}
    var close: () -> Void = {}
    /// Send a notification as a limit running out would, to see how they look.
    var testNotification: () -> Void = {}
}

extension EnvironmentValues {
    @Entry var panelActions = PanelActions()
}

/// The panel as it floats: its own glass, edge and shadow.
struct Floating: View {
    /// Room around the panel in its window, for its shadow.
    static let gutter: CGFloat = 28

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: 14, style: .continuous)
        Panel()
            .background(Glass())
            .clipShape(shape)
            .overlay(shape.strokeBorder(Color.primary.opacity(0.12), lineWidth: 0.5))
            .shadow(color: .black.opacity(0.28), radius: 18, y: 8)
            .padding(.horizontal, Floating.gutter)
            .padding(.bottom, Floating.gutter * 2)
            .frame(maxHeight: .infinity, alignment: .top)
    }
}

/// The system's popover material, behind the window.
struct Glass: NSViewRepresentable {
    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = .popover
        view.blendingMode = .behindWindow
        view.state = .active
        return view
    }

    func updateNSView(_ view: NSVisualEffectView, context: Context) {}
}

struct Panel: View {
    @Environment(Store.self) private var store
    @Environment(Navigation.self) private var navigation

    var body: some View {
        ZStack(alignment: .top) {
            switch navigation.page {
            case .main: FirstPage().transition(slide)
            case .settings: SettingsPage().transition(slide)
            }
        }
        .frame(width: panelWidth)
        .fixedSize(horizontal: false, vertical: true)
        .clipped()
    }

    private var slide: AnyTransition {
        let forward = navigation.forward
        return .asymmetric(insertion: .move(edge: forward ? .trailing : .leading).combined(with: .opacity),
                           removal: .move(edge: forward ? .leading : .trailing).combined(with: .opacity))
    }
}

/// The panel's first page.
private struct FirstPage: View {
    @Environment(Store.self) private var store
    @Environment(Navigation.self) private var navigation

    var body: some View {
        @Bindable var navigation = navigation
        VStack(alignment: .leading, spacing: 0) {
            if store.feed == nil {
                Opening(failure: store.failure)
            } else {
                Verdict(inUse: store.inUse, agents: store.all.isEmpty ? store.agents : nil, reading: store.reading)
                    .padding(.horizontal, 20)
                    .padding(.top, 18)
                    .padding(.bottom, 14)
                VStack(spacing: 2) {
                    ForEach(store.inUse) { account in
                        AccountRow(account: account,
                                   open: navigation.open == account.id,
                                   alone: store.inUse.count == 1,
                                   twin: store.inUse.filter { $0.title == account.title }.count > 1,
                                   headlined: account.id == store.inUse.first?.id) {
                            withAnimation(spring) {
                                navigation.open = navigation.open == account.id ? nil : account.id
                            }
                        }
                    }
                }
                .padding(.horizontal, 8)
                if !store.resting.isEmpty || !store.hidden.isEmpty {
                    Resting(accounts: store.resting, hidden: store.hidden, open: $navigation.restingOpen)
                        .padding(.horizontal, 8)
                        .padding(.top, 6)
                }
                ConnectNudge()
                    .padding(.horizontal, 8)
                    .padding(.top, 4)
            }
            Footer()
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
        }
    }
}

/// One line, only while an agent in use now can't ask Turnscope about its
/// limits: it isn't connected, or points at another copy. It can be put
/// away for good, agent by agent.
private struct ConnectNudge: View {
    @Environment(Store.self) private var store
    @AppStorage("dismissedConnect") private var dismissed = ""

    var body: some View {
        let used = Set(store.inUse.flatMap(\.agents))
        let away = Set(dismissed.split(separator: ",").map(String.init))
        if let agent = store.agents.first(where: {
            used.contains($0.id) && !away.contains($0.id) && ($0.status == .available || $0.status == .outdated)
        }) {
            HStack(spacing: 8) {
                Logo(agent: agent.id, size: 13).foregroundStyle(.secondary)
                Text(agent.status == .outdated ? "\(agent.name) runs an older Turnscope" : "\(agent.name) can't see your limits")
                    .font(.system(size: 12)).foregroundStyle(.secondary).lineLimit(1)
                Spacer(minLength: 6)
                if store.connecting.contains(agent.id) {
                    ProgressView().controlSize(.mini)
                } else {
                    Button(agent.status == .outdated ? "Update" : "Connect") { store.connect(agent.id) }
                        .buttonStyle(PillButton())
                        .help("Adds Turnscope to \(agent.name)'s MCP servers, so it can pace itself")
                }
                Button {
                    withAnimation(spring) { dismissed = (away.union([agent.id])).sorted().joined(separator: ",") }
                } label: {
                    Image(systemName: "xmark").font(.system(size: 9, weight: .semibold))
                }
                .buttonStyle(.plain)
                .foregroundStyle(.tertiary)
                .help("Don't suggest this again")
                .accessibilityLabel("Don't suggest connecting \(agent.name) again")
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .transition(appears)
        }
    }
}

/// While the first feed comes, or why none can.
private struct Opening: View {
    var failure: String?

    var body: some View {
        VStack(spacing: 8) {
            if let failure {
                Text("Turnscope can't read its data").font(.system(size: 13, weight: .semibold))
                Text(failure).font(.system(size: 12)).foregroundStyle(.secondary)
                    .multilineTextAlignment(.center).fixedSize(horizontal: false, vertical: true)
                Text("It tries again on its own.").font(.system(size: 11)).foregroundStyle(.tertiary)
            } else {
                ProgressView().controlSize(.small)
                Text("Reading your agents' history").font(.system(size: 12)).foregroundStyle(.secondary)
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, minHeight: 140)
    }
}

/// The one thing to know, large: whether the person can keep going, or,
/// before any account is found, why none is.
private struct Verdict: View {
    @Environment(Navigation.self) private var navigation
    var inUse: [Account]
    /// The agents found, when no account is.
    var agents: [AgentLink]?
    /// Whether agents' history is still read for the first time.
    var reading = false

    var body: some View {
        let words = Words(now: Words.frozen ?? navigation.now)
        let verdict = agents.map { words.welcome($0, reading: reading) }
            .map { (headline: $0.headline, detail: $0.detail, standing: Standing.lasts) }
            ?? words.verdict(inUse)
        VStack(alignment: .leading, spacing: 4) {
            Text(verdict.headline)
                .font(.system(size: 20, weight: .semibold))
                .foregroundStyle(verdict.standing.color)
                .fixedSize(horizontal: false, vertical: true)
                .contentTransition(.opacity)
                .accessibilityAddTraits(.isHeader)
            Text(verdict.detail)
                .font(.system(size: 13))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .textSelection(.enabled)
    }
}

/// Settings and Quit, and how current what is shown is: only while the
/// engine is down, that what is shown may be out of date; and while it is
/// still reading agents' history for the first time, that it is.
private struct Footer: View {
    @Environment(Store.self) private var store
    @Environment(Navigation.self) private var navigation
    @Environment(\.panelActions) private var actions

    var body: some View {
        HStack(spacing: 2) {
            if store.feed != nil, let failure = store.failure {
                Text("Not up to date · trying again")
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                    .help(failure)
                    .accessibilityLabel("Not up to date: \(failure). Trying again.")
                    .padding(.leading, 6)
                    .transition(appears)
            } else if store.reading {
                HStack(spacing: 6) {
                    Spinner()
                    Text("Reading your agents' history…")
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                }
                .help("Turnscope reads your agents' history once, when it first runs. Limits show up as soon as they are read.")
                .accessibilityElement(children: .combine)
                .accessibilityLabel("Reading your agents' history")
                .padding(.leading, 6)
                .transition(appears)
            }
            Spacer()
            // Escape closes the panel from its first page; its other pages
            // take it to go back.
            Button("Close", action: actions.close)
                .keyboardShortcut(.cancelAction)
                .frame(width: 0, height: 0)
                .opacity(0)
                .accessibilityHidden(true)
            Button { navigation.go(.settings) } label: { Image(systemName: "gearshape") }
                .buttonStyle(IconButton())
                .keyboardShortcut(",", modifiers: .command)
                .help("Settings")
                .accessibilityLabel("Settings")
            Button(action: actions.quit) { Image(systemName: "power") }
                .buttonStyle(IconButton())
                .keyboardShortcut("q", modifiers: .command)
                .help("Quit Turnscope")
                .accessibilityLabel("Quit Turnscope")
        }
    }
}

/// A small arc turning while something is under way, drawn by SwiftUI, so a
/// snapshot shows it as the panel does.
private struct Spinner: View {
    @State private var turning = false

    var body: some View {
        Circle()
            .trim(from: 0, to: 0.7)
            .stroke(.secondary, style: StrokeStyle(lineWidth: 1.5, lineCap: .round))
            .frame(width: 9, height: 9)
            .rotationEffect(.degrees(turning ? 360 : 0))
            .animation(.linear(duration: 0.9).repeatForever(autoreverses: false), value: turning)
            .onAppear { turning = true }
            .accessibilityHidden(true)
    }
}
