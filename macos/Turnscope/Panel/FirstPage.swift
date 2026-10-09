// The panel's first page, Turnscope as you see it, from the top:
//
// - the one thing to know, large: whether you can keep going;
// - each account in use, most urgent first;
// - the accounts used this week, most left first;
// - the rest, folded into one line;
// - the footer: Settings and Quit.
//
// One account at a time opens in place, wherever it's listed.

import SwiftUI
import TurnscopeKit

struct FirstPage: View {
    @Environment(Store.self) private var store
    @Environment(Navigation.self) private var navigation

    var body: some View {
        let inUse = store.inUse
        let recent = store.recent
        let unused = store.unused
        let hidden = store.hidden
        VStack(alignment: .leading, spacing: 0) {
            if store.status == nil {
                Waiting(failure: store.failure)
            } else {
                Verdict()
                    .padding(.horizontal, 20)
                    .padding(.top, 18)
                    .padding(.bottom, 14)
                VStack(alignment: .leading, spacing: 0) {
                    VStack(spacing: Row.spacing) {
                        ForEach(inUse) { AccountRow(account: $0, headlined: $0.id == inUse.first?.id) }
                    }
                    .padding(.horizontal, 8)
                    // Accounts used this week are always shown, under a
                    // heading that says what they are. They open in place
                    // like the rest.
                    if !recent.isEmpty {
                        VStack(alignment: .leading, spacing: Row.spacing) {
                            SectionTitle(text: "Used this week")
                            ForEach(recent) { AccountRow(account: $0) }
                        }
                        .padding(.horizontal, 8)
                        .padding(.top, inUse.isEmpty ? 0 : Row.groups)
                    }
                    if !unused.isEmpty || !hidden.isEmpty {
                        UnusedAccounts(accounts: unused, hidden: hidden,
                                       besideOthers: !inUse.isEmpty || !recent.isEmpty)
                            .padding(.horizontal, 8)
                            .padding(.top, Row.spacing)
                    }
                    ConnectNudge()
                        .padding(.horizontal, 8)
                        .padding(.top, 4)
                }
            }
            Footer()
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
        }
        // While an account is open, ask what used it.
        .onChange(of: navigation.open) { _, open in store.watch(open) }
    }
}

/// One line, shown only while an agent in use can't ask Turnscope about its
/// limits, because it isn't connected or points at another copy. It can be
/// dismissed for good, one agent at a time.
private struct ConnectNudge: View {
    @Environment(Store.self) private var store
    @AppStorage(Preference.dismissedConnect) private var dismissed = ""

    var body: some View {
        let away = Set(dismissed.split(separator: ",").map(String.init))
        if let agent = store.toConnect(dismissed: away) {
            HStack(spacing: Row.gap) {
                Logo(agent: agent, size: Row.logo).foregroundStyle(.secondary)
                // If connecting failed, say why instead of asking.
                if let error = store.agentErrors[agent.id] {
                    Text(error).font(.line).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                } else {
                    Text(store.words().connection(agent, named: true))
                        .font(.line).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 6)
                ConnectButton(agent: agent)
                Button {
                    withAnimation(spring) { dismissed = (away.union([agent.id])).sorted().joined(separator: ",") }
                } label: {
                    Image(systemName: "xmark")
                }
                .buttonStyle(IconButton(width: 22, height: 16))
                .help("Don't suggest this again")
                .accessibilityLabel("Don't suggest connecting \(agent.name) again")
            }
            .padding(Row.padding)
            .transition(appears)
        }
    }
}

/// Shown while the first status is on its way, or to say why none can come.
private struct Waiting: View {
    var failure: String?

    var body: some View {
        VStack(spacing: 8) {
            if let failure {
                Text("Turnscope can't read its data").font(.system(size: 13, weight: .semibold))
                Text(failure).font(.line).foregroundStyle(.secondary)
                    .multilineTextAlignment(.center).fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                Text("It tries again on its own.").font(.small).foregroundStyle(.tertiary)
            } else {
                ProgressView().controlSize(.small)
                Text("Reading your agents' history…").font(.line).foregroundStyle(.secondary)
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, minHeight: 140)
    }
}

/// The one thing to know, in large type: whether you can keep going, or,
/// before any account is found, why none is. Under it is when it's back, or
/// what's left of a limit that isn't forecast.
private struct Verdict: View {
    @Environment(Store.self) private var store
    @Environment(Navigation.self) private var navigation

    var body: some View {
        let words = store.words(now: navigation.now)
        let hiddenInUse = store.hidden.contains(where: \.inUse)
        let verdict = store.all.isEmpty
            ? words.welcome(store.agents, reads: store.agentNames, reading: store.reading)
            : words.verdict(store.inUse, among: store.all, hiddenInUse: hiddenInUse)
        VStack(alignment: .leading, spacing: 4) {
            Text(verdict.headline)
                .font(.system(size: 20, weight: .semibold))
                .foregroundStyle(verdict.standing.color)
                .fixedSize(horizontal: false, vertical: true)
                .contentTransition(.opacity)
                .accessibilityAddTraits(.isHeader)
            if !verdict.detail.isEmpty {
                Text(verdict.detail)
                    .font(.name)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .textSelection(.enabled)
    }
}

/// Settings and Quit, and how current the data is. While the engine is
/// down, it says what's shown may be out of date. While the engine is still
/// reading agents' history for the first time, it says that. When a
/// scheduled check found an update, a button beside Settings shows it.
private struct Footer: View {
    @Environment(Store.self) private var store
    @Environment(Navigation.self) private var navigation
    @Environment(Updater.self) private var updater
    @Environment(\.panelActions) private var actions

    var body: some View {
        HStack(spacing: 2) {
            if store.status != nil, let failure = store.failure {
                Text("Not up to date · trying again")
                    .font(.small)
                    .foregroundStyle(.secondary)
                    .help(failure)
                    .accessibilityLabel("Not up to date: \(failure). Trying again.")
                    .padding(.leading, 6)
                    .transition(appears)
            } else if store.reading && !store.all.isEmpty {
                // Before any account is found, the headline says this
                // instead.
                HStack(spacing: 6) {
                    Spinner()
                    Text("Reading your agents' history…")
                        .font(.small)
                        .foregroundStyle(.secondary)
                }
                .help("Turnscope reads your agents' history once, when it first runs. Limits show up as soon as they are read.")
                .accessibilityElement(children: .combine)
                .accessibilityLabel("Reading your agents' history")
                .padding(.leading, 6)
                .transition(appears)
            }
            Spacer()
            // Escape closes the panel from its first page. Its other pages
            // use it to go back.
            Button("Close", action: actions.close)
                .keyboardShortcut(.cancelAction)
                .frame(width: 0, height: 0)
                .opacity(0)
                .accessibilityHidden(true)
            if updater.waiting {
                Button("Update available") {
                    actions.close()
                    updater.check()
                }
                .buttonStyle(PillButton())
                .help("Shows what's new, and asks before installing it")
                .padding(.trailing, 4)
                .transition(appears)
            }
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

/// A small arc that turns while something is in progress. SwiftUI draws it,
/// so a snapshot shows it as the panel does.
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
