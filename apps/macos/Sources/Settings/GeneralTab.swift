import AppKit
import SwiftUI

struct GeneralTab: View {
    @Environment(Bridge.self) private var bridge
    @Environment(Login.self) private var login
    @Environment(Notifier.self) private var notifier
    @Environment(Updater.self) private var updater
    @Environment(\.closePanel) private var closePanel
    @State private var copiedDiagnostics = false
    @State private var copies = 0

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
                    get: { bridge.panel.settings.openAtLogin != .off },
                    set: { on in
                        withMotion {
                            _ = login.set(on)
                        }
                    }
                )
            )
            if bridge.panel.settings.openAtLogin == .needsApproval {
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

    /// Shown once the settings are read: before, a switch would show a guess.
    @ViewBuilder private var notifications: some View {
        if let current = bridge.panel.settings.notify {
            SettingGroup(title: "Notify me when") {
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
                        isOn: notify(\.runningOut, in: current)
                    )
                    SettingRow("A limit is used up", isOn: notify(\.usedUp, in: current))
                    SettingRow(
                        "A limit is back",
                        help: "After it was used up, once the account can be used again",
                        isOn: notify(\.reset, in: current)
                    )
                    SettingRow(
                        "An account needs signing in",
                        help: "Its login was refused, so its limits can't be read",
                        isOn: notify(\.signIn, in: current)
                    )
                }
                .disabled(bridge.panel.stopped)
            }
        }
    }

    private var updates: some View {
        SettingGroup(title: "Updates") {
            SettingRow(
                title: "Turnscope \(bridge.panel.version)",
                help: "Right-click to copy diagnostics or reveal the log"
            ) {
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
            .flashesCopied($copiedDiagnostics, copies: copies)
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
        Task {
            copyToPasteboard(await bridge.diagnostics())
            copies += 1
        }
    }

    private func notify(_ keyPath: WritableKeyPath<NotifySettings, Bool>, in notify: NotifySettings) -> Binding<Bool> {
        Binding(
            get: { notify[keyPath: keyPath] },
            set: { value in
                var changed = notify
                changed[keyPath: keyPath] = value
                bridge.send(.setNotify(notify: changed))
            }
        )
    }
}
