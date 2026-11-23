// Settings → General: menu bar display, open at login, notification toggles, and updates.

import AppKit
import SwiftUI

struct GeneralTab: View {
    @Environment(Bridge.self) private var bridge
    @Environment(Login.self) private var login
    @Environment(Notifier.self) private var notifier
    @Environment(Updater.self) private var updater
    @Environment(\.closePanel) private var closePanel
    @State private var copiedDiagnostics = false
    @State private var clearCopied: Task<Void, Never>?

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            menuBar
            notifications
            updates
        }
    }

    private var menuBar: some View {
        SettingGroup(title: "Menu bar") {
            SettingRow(title: "Show") {
                Choice(
                    label: "Show",
                    options: [(MenuShows.all, "All in use"), (.one, "Most urgent")],
                    selection: bridge.panel.settings.menuShows
                ) { shows in
                    bridge.send(.setMenuShows(shows: shows))
                }
                .fixedSize()
            }
            SettingRow(
                "Open at login",
                isOn: Binding(
                    get: { login.status.isOn },
                    set: { on in
                        withAnimation(spring) {
                            login.set(on, isLive: bridge.isLive) { bridge.log($0) }
                        }
                    }
                )
            )
            if login.status == .needsApproval {
                SettingRow(title: "Allow it in System Settings") {
                    Button("Open") {
                        closePanel()
                        login.openSettings()
                    }
                    .buttonStyle(PillButton())
                    .accessibilityLabel("Open Login Items in System Settings")
                }
                .transition(appears)
            }
        }
    }

    private var notifications: some View {
        let settings = bridge.panel.settings
        return SettingGroup(title: "Notify me when") {
            if notifier.denied {
                SettingRow(title: "Notifications are off for Turnscope") {
                    Button("Open") {
                        closePanel()
                        notifier.openSettings()
                    }
                    .buttonStyle(PillButton())
                    .accessibilityLabel("Open Notifications in System Settings")
                }
                .transition(appears)
            }
            Group {
                SettingRow(
                    "A limit will run out",
                    help: "Before it resets, at your pace",
                    isOn: notify(\.runningOut)
                )
                SettingRow("A limit is used up", isOn: notify(\.usedUp))
                SettingRow(
                    "A limit is back",
                    help: "After it was used up, once the account can be used again",
                    isOn: notify(\.reset)
                )
                SettingRow(
                    "An account needs signing in",
                    help: "Its login was refused, so its limits can't be read",
                    isOn: notify(\.signIn)
                )
            }
            .disabled(!settings.canEditNotify)
        }
    }

    private var updates: some View {
        SettingGroup(title: "Updates") {
            SettingRow(title: "Turnscope \(bridge.panel.version)") {
                HStack(spacing: 8) {
                    if copiedDiagnostics {
                        Text("Copied")
                            .font(.small)
                            .foregroundStyle(.secondary)
                            .transition(.opacity)
                    }
                    if updater.available {
                        Button("Check now") {
                            closePanel()
                            updater.check()
                        }
                        .buttonStyle(PillButton())
                        .accessibilityLabel("Check for updates now")
                    }
                }
            }
            .contentShape(Rectangle())
            .help("Right-click to copy diagnostics or reveal the log")
            .onTapGesture {
                if NSEvent.modifierFlags.contains(.option) {
                    copyDiagnostics()
                }
            }
            .contextMenu {
                Button("Copy Diagnostics", action: copyDiagnostics)
                if let logURL = bridge.logURL() {
                    Button("Reveal Log in Finder") {
                        closePanel()
                        NSWorkspace.shared.activateFileViewerSelecting([logURL])
                    }
                }
            }
            if updater.available {
                SettingRow(
                    "Check automatically",
                    isOn: Binding(
                        get: { updater.checksAutomatically },
                        set: { updater.setAutomatic($0) }
                    )
                )
            }
        }
    }

    private func copyDiagnostics() {
        copyToPasteboard(bridge.diagnostics())
        clearCopied?.cancel()
        withAnimation(.easeOut(duration: 0.12)) { copiedDiagnostics = true }
        clearCopied = Task { @MainActor in
            try? await Task.sleep(for: .seconds(1.5))
            guard !Task.isCancelled else { return }
            withAnimation(.easeOut(duration: 0.16)) { copiedDiagnostics = false }
        }
    }

    private func notify(_ keyPath: WritableKeyPath<NotifySettings, Bool>) -> Binding<Bool> {
        Binding(
            get: { bridge.panel.settings.notify?[keyPath: keyPath] ?? true },
            set: { value in
                guard var current = bridge.panel.settings.notify else { return }
                current[keyPath: keyPath] = value
                bridge.send(.setNotify(notify: current))
            }
        )
    }
}
