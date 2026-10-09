// Settings, a page of the panel with three tabs: General, Agents and Accounts.
// Each fits without scrolling. Changes take effect at once; there's no Save.
// Rows look like the panel's: a logo, a name with one line under it, and a
// control on the right.

import SwiftUI
import TurnscopeKit

struct SettingsPage: View {
    @Environment(Navigation.self) private var navigation
    @State private var height: CGFloat = 400

    var body: some View {
        @Bindable var navigation = navigation
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 4) {
                // The page's title, with the Back button beside it.
                HStack(spacing: 4) {
                    Button { navigation.go(.main) } label: { Image(systemName: "chevron.left") }
                        .buttonStyle(IconButton())
                        .keyboardShortcut(.cancelAction)
                        .help("Back")
                        .accessibilityLabel("Back")
                    Text("Settings").font(.system(size: 15, weight: .semibold)).fixedSize()
                        .accessibilityAddTraits(.isHeader)
                    Spacer()
                }
                .padding(.horizontal, 10)
                .padding(.top, 10)
                Choice(label: "Settings", options: SettingsTab.allCases.map { ($0, $0.title) },
                       selection: $navigation.tab)
                    .fixedSize()
                    .padding(.top, 10)
                .padding(.trailing, 12)
            }
            .padding(.bottom, 12)
            ScrollView(.vertical) {
                Group {
                    switch navigation.tab {
                    case .general: GeneralTab()
                    case .agents: AgentsTab()
                    case .accounts: AccountsTab()
                    }
                }
                .padding(.horizontal, 8)
                .padding(.bottom, 14)
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
