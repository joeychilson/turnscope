// Settings, a page of the panel with three tabs. A tab taller than the screen
// allows scrolls under its title.

import SwiftUI
import TurnscopeKit

struct SettingsPage: View {
    @Environment(Store.self) private var store
    @Environment(Navigation.self) private var navigation
    @State private var height: CGFloat = 400

    var body: some View {
        @Bindable var navigation = navigation
        VStack(alignment: .leading, spacing: 0) {
            // The page's title with the Back button beside it, and the tabs.
            // They line up with the cards' edges.
            HStack(spacing: 4) {
                Button { navigation.go(.main) } label: { Image(systemName: "chevron.left") }
                    .buttonStyle(IconButton())
                    .keyboardShortcut(.cancelAction)
                    .help("Back")
                    .accessibilityLabel("Back")
                Text("Settings").font(.system(size: 15, weight: .semibold)).fixedSize()
                    .accessibilityAddTraits(.isHeader)
                Spacer()
                Choice(label: "Settings", options: SettingsTab.allCases.map { ($0, $0.title) },
                       selection: $navigation.tab)
                    .fixedSize()
            }
            .padding(.horizontal, 8)
            .padding(.top, 8)
            .padding(.bottom, 10)
            ScrollView(.vertical) {
                Group {
                    switch navigation.tab {
                    case .general: GeneralTab()
                    // The other tabs list what the status holds. Before the
                    // first one, they say what the panel says.
                    case _ where store.status == nil: Waiting(failure: store.failure)
                    case .agents: AgentsTab()
                    case .accounts: AccountsTab()
                    }
                }
                .padding(.horizontal, 8)
                .padding(.bottom, 8)
                .frame(maxWidth: .infinity, alignment: .leading)
                .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { height = $0 }
            }
            // It scrolls, and shows a scroller, only when a tab is taller than
            // the screen allows. Otherwise it's plain content, so switching
            // tabs never flashes a scroller.
            .scrollDisabled(height <= tallest)
            .scrollIndicators(height > tallest ? .automatic : .never)
            .scrollBounceBehavior(.basedOnSize)
            .frame(height: min(height, tallest))
        }
    }

    /// The most height the screen allows, below the menu bar and inside the
    /// panel's edges.
    private var tallest: CGFloat { (NSScreen.main?.visibleFrame.height ?? 800) - 140 }
}
