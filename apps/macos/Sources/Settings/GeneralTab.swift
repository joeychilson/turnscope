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
                    get: { login.status.isOn },
                    set: { on in
                        login.set(on, isLive: bridge.isLive) { bridge.log($0) }
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

    /// Shown once the settings are read: before, a switch would show a guess.
    @ViewBuilder private var notifications: some View {
        let options = bridge.panel.settings.notifyOptions
        if !options.isEmpty {
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
                ForEach(options) { option in
                    SettingRow(option.title, help: option.help, isOn: notify(option.kind, in: options))
                }
                .disabled(bridge.panel.stopped)
            }
        }
    }

    private var updates: some View {
        SettingGroup(title: updater.available ? "Updates" : "About") {
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
                        .disabled(!updater.canCheckForUpdates)
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
                if bridge.isLive {
                    Button("Reveal Log in Finder") {
                        closePanel()
                        if let logURL = bridge.logURL() {
                            NSWorkspace.shared.activateFileViewerSelecting([logURL])
                        }
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

    private func notify(_ kind: NotifyKind, in options: [NotifyOptionModel]) -> Binding<Bool> {
        let isOn: (NotifyKind) -> Bool = { target in
            options.first(where: { $0.kind == target })?.enabled ?? false
        }
        return Binding(
            get: { isOn(kind) },
            set: { value in
                bridge.send(.setNotify(notify: NotifySettings(
                    runningOut: kind == .runningOut ? value : isOn(.runningOut),
                    usedUp: kind == .usedUp ? value : isOn(.usedUp),
                    reset: kind == .reset ? value : isOn(.reset),
                    signIn: kind == .signIn ? value : isOn(.signIn)
                )))
            }
        )
    }
}
