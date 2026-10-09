// Settings → General: the menu bar, opening at login, notifications, and
// updates. Notification settings belong to the engine, so `turnscope config`
// and the app agree. Menu bar settings belong to the app.

import ServiceManagement
import SwiftUI
import TurnscopeKit

struct GeneralTab: View {
    @Environment(Store.self) private var store
    @Environment(\.panelActions) private var actions
    @AppStorage(Preference.menuShows) private var menuShows = MenuShows.all
    @State private var login = SMAppService.mainApp.status
    /// Whether Turnscope's notifications are turned off.
    @State private var off = false

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            menuBar
            notifications
            Updates()
        }
    }

    private var menuBar: some View {
        VStack(alignment: .leading, spacing: 0) {
            SectionTitle(text: "Menu bar")
            SettingRow(title: "Show") {
                Choice(label: "Show", options: [(MenuShows.all, "All in use"), (.one, "Most urgent")],
                       selection: $menuShows)
                    .fixedSize()
            }
            SettingRow("Open at login", isOn: Binding(get: { login == .enabled || login == .requiresApproval },
                                                      set: { setAtLogin($0) }))
            // macOS may hold it until it's allowed in System Settings.
            if login == .requiresApproval {
                SettingRow(title: "Allow it in System Settings") {
                    Button("Open") { SMAppService.openSystemSettingsLoginItems() }
                        .buttonStyle(PillButton())
                        .accessibilityLabel("Open Login Items in System Settings")
                }
                .transition(appears)
            }
        }
    }

    private var notifications: some View {
        VStack(alignment: .leading, spacing: 0) {
            SectionTitle(text: "Notify me when")
            if off {
                SettingRow(title: "Notifications are off for Turnscope") {
                    Button("Open") {
                        let id = Bundle.main.bundleIdentifier ?? ""
                        if let url = URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=\(id)") {
                            NSWorkspace.shared.open(url)
                        }
                    }
                    .buttonStyle(PillButton())
                    .accessibilityLabel("Open Notifications in System Settings")
                }
                .transition(appears)
            }
            // Each finishes the sentence "Notify me when". More about when
            // it's sent shows on hover.
            SettingRow("A limit will run out", help: "Before it resets, at your pace",
                       isOn: notify(\.runningOut))
            SettingRow("A limit is used up", isOn: notify(\.usedUp))
            SettingRow("A limit is back", help: "After it was used up, once the account can be used again",
                       isOn: notify(\.reset))
            SettingRow("An account needs signing in", help: "Its login was refused, so its limits can't be read",
                       isOn: notify(\.signIn))
        }
        // Checked again each time Settings shows, since they may have just
        // been turned on.
        .task {
            let now = await actions.notificationsOff()
            withAnimation(spring) { off = now }
        }
    }

    /// A switch for one of the engine's notification settings.
    private func notify(_ kind: WritableKeyPath<Notify, Bool>) -> Binding<Bool> {
        Binding(get: { store.notify?[keyPath: kind] ?? true },
                set: { on in store.setNotify { $0[keyPath: kind] = on } })
    }

    private func setAtLogin(_ on: Bool) {
        do {
            if on { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
        } catch {
            NSSound.beep()
        }
        withAnimation(spring) { login = SMAppService.mainApp.status }
    }
}

/// The version, Check now, and Check automatically, in rows like every
/// other setting.
private struct Updates: View {
    @Environment(Updater.self) private var updater
    @Environment(\.panelActions) private var actions
    @State private var automatic = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SectionTitle(text: "Updates")
            let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
            SettingRow(title: "Turnscope \(version ?? "")") {
                if updater.available {
                    Button("Check now") {
                        actions.close()
                        updater.check()
                    }
                    .buttonStyle(PillButton())
                    .accessibilityLabel("Check for updates now")
                }
            }
            if updater.available {
                SettingRow("Check automatically",
                           isOn: Binding(get: { automatic }, set: { automatic = $0; updater.automatic = $0 }))
            }
        }
        .onAppear { automatic = updater.automatic }
    }
}
