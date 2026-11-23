// Settings page with Back button, three tabs, and a scroll view active only when needed.

import SwiftUI

struct SettingsPage: View {
    @Environment(Bridge.self) private var bridge

    var body: some View {
        let panel = bridge.panel
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 4) {
                Button { bridge.go(.main) } label: { Image(systemName: "chevron.left") }
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
                    selection: panel.tab
                ) { tab in
                    withAnimation(spring) {
                        bridge.send(.setTab(tab: tab))
                    }
                }
                .fixedSize()
            }
            .padding(.horizontal, 8)
            .padding(.top, 8)
            .padding(.bottom, 10)

            SettingsScrollView(maxHeight: max(200, bridge.screenHeight - 140)) {
                Group {
                    switch panel.tab {
                    case .general: GeneralTab()
                    case _ where !panel.hasStatus: Waiting(model: panel.waiting)
                    case .agents: AgentsTab()
                    case .accounts: AccountsTab()
                    }
                }
                .padding(.horizontal, 8)
                .padding(.bottom, 8)
            }
        }
    }
}
