// Settings, a page of the panel, in three tabs, each short enough to see
// whole: General, what the menu bar shows, which notifications are sent, and
// updating; Agents, those Turnscope serves over MCP; and Accounts, every
// account found, each to show or hide. Each takes effect at once; there is
// nothing to save.

import ServiceManagement
import SwiftUI
import TurnscopeKit

struct SettingsPage: View {
    @Environment(Store.self) private var store
    @Environment(Navigation.self) private var navigation
    @Environment(\.panelActions) private var actions
    @AppStorage(Preference.menuShows) private var menuShows = "all"
    @AppStorage(Preference.notifyRunningOut) private var runningOut = true
    @AppStorage(Preference.notifyUsedUp) private var usedUp = true
    @AppStorage(Preference.notifyBack) private var back = true
    @AppStorage(Preference.notifyMilestones) private var milestones = false
    @AppStorage(Preference.notifyRecap) private var recap = false
    @State private var atLogin = SMAppService.mainApp.status == .enabled
    @State private var sent = false
    @State private var height: CGFloat = 400

    var body: some View {
        @Bindable var navigation = navigation
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 4) {
                PageTitle(title: "Settings") { navigation.go(.main) }
                Picker("Settings", selection: $navigation.tab) {
                    ForEach(Navigation.Tab.allCases) { Text($0.rawValue).tag($0) }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .controlSize(.small)
                .fixedSize()
                .padding(.top, 10)
                .padding(.trailing, 12)
            }
            .padding(.bottom, 12)
            ScrollView(.vertical) {
                Group {
                    switch navigation.tab {
                    case .general:
                        VStack(alignment: .leading, spacing: 18) {
                            menuBar
                            notifications
                            Updates()
                        }
                    case .agents: agents
                    case .accounts: AccountsTab()
                    }
                }
                .padding(.horizontal, 8)
                .padding(.bottom, 14)
                .frame(maxWidth: .infinity, alignment: .leading)
                .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { height = $0 }
            }
            // It scrolls, and shows a scroller, only when a tab is taller than
            // the screen allows: at any other size it is plain content, so
            // switching tabs never flashes a scroller.
            .scrollDisabled(height <= tallest)
            .scrollIndicators(height > tallest ? .automatic : .never)
            .scrollBounceBehavior(.basedOnSize)
            .frame(height: min(height, tallest))
        }
    }

    private var agents: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Connected agents can ask Turnscope how their limits stand, what used them and why, and pick up each other's work. Connecting runs the agent's own command for adding an MCP server.")
                .font(.system(size: 11)).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, 12)
                .padding(.bottom, 8)
            if store.agents.isEmpty {
                Text("No agent Turnscope serves is installed.")
                    .font(.system(size: 12)).foregroundStyle(.secondary).padding(.horizontal, 12)
            }
            ForEach(store.agents) { AgentRow(agent: $0) }
        }
    }

    private var menuBar: some View {
        VStack(alignment: .leading, spacing: 0) {
            SectionTitle(text: "Menu bar")
            SettingRow(title: "Show") {
                Picker("Show", selection: $menuShows) {
                    Text("All in use").tag("all")
                    Text("Most urgent").tag("one")
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .controlSize(.small)
                .fixedSize()
            }
            SettingRow(title: "Open at login") {
                Switch(label: "Open at login", on: Binding(get: { atLogin }, set: { setAtLogin($0) }))
            }
        }
    }

    private var notifications: some View {
        VStack(alignment: .leading, spacing: 0) {
            SectionTitle(text: "Notify me when")
            SettingRow(title: "A limit will run out", detail: "Before it resets, at this pace") {
                Switch(label: "A limit will run out", on: $runningOut)
            }
            SettingRow(title: "A limit is used up") { Switch(label: "A limit is used up", on: $usedUp) }
            SettingRow(title: "A used-up limit is back") { Switch(label: "A used-up limit is back", on: $back) }
            SettingRow(title: "A week or month passes a quarter", detail: "Three quarters, half and a quarter left") {
                Switch(label: "A week or month passes a quarter", on: $milestones)
            }
            SettingRow(title: "The week is over", detail: "A recap on Monday morning") {
                Switch(label: "The week is over", on: $recap)
            }
            SettingRow(title: "See how they look") {
                Button(sent ? "Sent" : "Send a Test") {
                    actions.testNotification()
                    withAnimation(.easeOut(duration: 0.15)) { sent = true }
                    Task { @MainActor in
                        try? await Task.sleep(for: .seconds(2))
                        withAnimation { sent = false }
                    }
                }
                .buttonStyle(PillButton())
                .disabled(sent)
            }
        }
    }

    /// As tall as the screen allows below the menu bar and the panel's edges.
    private var tallest: CGFloat { (NSScreen.main?.visibleFrame.height ?? 800) - 140 }

    private func setAtLogin(_ on: Bool) {
        do {
            if on { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
        } catch {
            NSSound.beep()
        }
        atLogin = SMAppService.mainApp.status == .enabled
    }
}

/// An agent, and whether it has Turnscope: connected, to connect, or to point
/// at this copy again.
private struct AgentRow: View {
    @Environment(Store.self) private var store
    var agent: AgentLink

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 10) {
                Logo(agent: agent.id, size: 14).foregroundStyle(.secondary)
                Text(agent.name).font(.system(size: 13))
                Spacer(minLength: 8)
                status
            }
            .frame(height: 30)
            if let error = store.connectErrors[agent.id] {
                Text(error).font(.system(size: 11)).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(.leading, 24)
                    .padding(.bottom, 4)
            }
        }
        .padding(.horizontal, 12)
    }

    @ViewBuilder private var status: some View {
        if store.connecting.contains(agent.id) {
            ProgressView().controlSize(.mini)
        } else {
            switch agent.status {
            case .connected:
                Label("Connected", systemImage: "checkmark")
                    .labelStyle(Checked())
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
            case .available:
                Button("Connect") { store.connect(agent.id) }
                    .buttonStyle(PillButton())
                    .help("Adds Turnscope to \(agent.name)'s MCP servers")
            case .outdated:
                Button("Update") { store.connect(agent.id) }
                    .buttonStyle(PillButton())
                    .help("\(agent.name) runs another copy of Turnscope: point it at this one")
            case .unsupported:
                Text("\(agent.name) doesn't use MCP").font(.system(size: 12)).foregroundStyle(.tertiary)
            }
        }
    }
}

/// Every account found, the subscriptions apart from the API keys, each with
/// a switch to leave it out of the menu bar and the panel.
private struct AccountsTab: View {
    @Environment(Store.self) private var store

    var body: some View {
        let keys = store.all.filter { $0.id.hasPrefix("api:") }
        let subscriptions = store.all.filter { !$0.id.hasPrefix("api:") }
        VStack(alignment: .leading, spacing: 18) {
            Text("Found in your agents' sign-ins and keys, which Turnscope only reads. A key two agents share is one account.")
                .font(.system(size: 11)).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.horizontal, 12)
            group("Subscriptions", subscriptions)
            group("API keys", keys)
        }
    }

    @ViewBuilder private func group(_ title: String, _ accounts: [Account]) -> some View {
        if !accounts.isEmpty {
            VStack(alignment: .leading, spacing: 0) {
                SectionTitle(text: title)
                ForEach(accounts) { AccountSwitch(account: $0) }
            }
        }
    }
}

private struct AccountSwitch: View {
    @Environment(Store.self) private var store
    var account: Account

    var body: some View {
        let words = Words()
        HStack(spacing: 10) {
            Logo(account: account, size: 14).foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 0) {
                Text(account.title).font(.system(size: 13))
                let subtitle = words.subtitle(account)
                if !subtitle.isEmpty {
                    Text(subtitle).font(.system(size: 11)).foregroundStyle(.secondary)
                        .lineLimit(1).truncationMode(.middle)
                }
            }
            Spacer(minLength: 8)
            if account.problem == .signIn {
                Text("Sign in again").font(.system(size: 11)).foregroundStyle(.tertiary)
            }
            Switch(label: "Show \(account.title)",
                   on: Binding(get: { !account.hidden },
                               set: { shown in withAnimation(spring) { store.setHidden(account.id, !shown) } }))
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 5)
    }
}

/// A check before its word.
private struct Checked: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 4) {
            configuration.icon.font(.system(size: 10, weight: .bold))
            configuration.title
        }
    }
}

/// Updating automatically, the version, and checking now.
private struct Updates: View {
    @Environment(Updater.self) private var updater
    @Environment(\.panelActions) private var actions
    @State private var automatic = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if updater.available {
                SettingRow(title: "Check for updates automatically") {
                    Switch(label: "Check for updates automatically",
                           on: Binding(get: { automatic }, set: { automatic = $0; updater.automatic = $0 }))
                }
            }
            HStack(spacing: 6) {
                Text("Turnscope \(version)").foregroundStyle(.tertiary)
                if updater.available {
                    Text("·").foregroundStyle(.quaternary)
                    Button("Check for Updates…", action: actions.checkForUpdates)
                        .buttonStyle(.plain)
                        .foregroundStyle(.secondary)
                }
                Spacer()
            }
            .font(.system(size: 11))
            .padding(.horizontal, 12)
            .padding(.top, 6)
        }
        .onAppear { automatic = updater.automatic }
    }

    private var version: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "(development)"
    }
}
