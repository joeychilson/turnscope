import SwiftUI

struct SettingsPage: View {
    @Environment(Bridge.self) private var bridge
    @Environment(UIState.self) private var ui
    @FocusState private var focused: Bool

    var body: some View {
        let panel = bridge.panel
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 4) {
                Button { ui.go(.main) } label: { Image(systemName: "chevron.left") }
                    .buttonStyle(IconButton())
                    .keyboardShortcut(.cancelAction)
                    .help("Back")
                    .accessibilityLabel("Back")
                Text("Settings")
                    .font(.system(size: 15, weight: .semibold))
                    .fixedSize()
                    .accessibilityAddTraits(.isHeader)
                Spacer()
                Choice(
                    label: "Settings",
                    options: SettingsTab.allCases.map { ($0, $0.title) },
                    selection: ui.tab
                ) { tab in
                    ui.setTab(tab)
                }
                .fixedSize()
            }
            .padding(.horizontal, 8)
            .padding(.top, 8)
            .padding(.bottom, 10)

            OverflowScroll(maxHeight: max(200, bridge.screenHeight - 140)) {
                Group {
                    switch (ui.tab, panel.waiting) {
                    case (.general, _): GeneralTab()
                    case let (_, waiting?): Waiting(model: waiting)
                    case (.agents, nil): AgentsTab()
                    case (.accounts, nil): AccountsTab()
                    }
                }
                .padding(.horizontal, 8)
                .padding(.bottom, 8)
            }
        }
        .background {
            HStack(spacing: 0) {
                tabShortcut(.general, key: "1")
                tabShortcut(.agents, key: "2")
                tabShortcut(.accounts, key: "3")
            }
        }
        .focusable()
        .focusEffectDisabled()
        .focused($focused)
        .onChange(of: ui.focusNonce, initial: true) { focused = true }
        .onKeyPress(.leftArrow) { stepTab(-1) }
        .onKeyPress(.rightArrow) { stepTab(1) }
    }

    private func tabShortcut(_ tab: SettingsTab, key: KeyEquivalent) -> some View {
        Button(tab.title) { ui.setTab(tab) }
            .keyboardShortcut(key, modifiers: .command)
            .hiddenShortcut()
    }

    private func stepTab(_ delta: Int) -> KeyPress.Result {
        let tabs = SettingsTab.allCases
        guard let index = tabs.firstIndex(of: ui.tab) else { return .ignored }
        let next = max(0, min(tabs.count - 1, index + delta))
        guard next != index else { return .handled }
        ui.setTab(tabs[next])
        return .handled
    }
}
