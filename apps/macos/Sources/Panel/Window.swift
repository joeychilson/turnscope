// The panel: its floating frame, and its two pages.

import AppKit
import SwiftUI

let panelWidth: CGFloat = 340
let panelShape = RoundedRectangle(cornerRadius: 14, style: .continuous)

extension EnvironmentValues {
    @Entry var closePanel: () -> Void = {}
    @Entry var isSnapshot = false
}

extension View {
    func environment(
        _ bridge: Bridge, _ ui: UIState, _ updater: Updater, _ notifier: Notifier, _ login: Login
    ) -> some View {
        environment(bridge).environment(ui).environment(updater).environment(notifier).environment(login)
    }

    /// Keeps a button for its keyboard shortcut, unseen, unfocusable, and unread by VoiceOver.
    func hiddenShortcut() -> some View {
        frame(width: 0, height: 0)
            .opacity(0)
            .focusable(false)
            .accessibilityHidden(true)
    }
}

struct Floating: View {
    static let gutter: CGFloat = 28

    var body: some View {
        Panel()
            .panelEdge()
            .background {
                // Keep hit-testing on the material so WindowServer's event shape covers the whole
                // panel; disabling it makes closed rows and row gaps click-through to windows behind.
                panelShape
                    .fill(.regularMaterial)
                    .shadow(color: .black.opacity(0.28), radius: 18, y: 8)
            }
            .materialActiveAppearance(.active)
            .padding(.horizontal, Floating.gutter)
            .padding(.bottom, Floating.gutter * 2)
            .frame(maxHeight: .infinity, alignment: .top)
    }
}

extension View {
    /// Clips the panel, and what's drawn behind it so far, to its shape, and draws its edge.
    func panelEdge() -> some View {
        clipShape(panelShape)
            .overlay {
                panelShape
                    .strokeBorder(Color(nsColor: .separatorColor), lineWidth: 0.5)
                    .allowsHitTesting(false)
            }
    }
}

struct Panel: View {
    @Environment(UIState.self) private var ui
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        ZStack(alignment: .top) {
            switch ui.page {
            case .main:
                FirstPage()
                    .transition(reduceMotion ? .opacity : .move(edge: .leading).combined(with: .opacity))
            case .settings:
                SettingsPage()
                    .transition(reduceMotion ? .opacity : .move(edge: .trailing).combined(with: .opacity))
            }
        }
        .frame(width: panelWidth)
        .fixedSize(horizontal: false, vertical: true)
        .background {
            // Keep ⌘Q active on both FirstPage and SettingsPage since the app has no main menu bar.
            Button("Quit Turnscope") { NSApp.terminate(nil) }
                .keyboardShortcut("q", modifiers: .command)
                .hiddenShortcut()
        }
    }
}

/// Keeps `ScrollView` permanently in the tree sized to `min(contentHeight, maxHeight)` so crossing
/// the overflow threshold never tears down child state, `@FocusState`, or bar animations.
struct OverflowScroll<Content: View>: View {
    var maxHeight: CGFloat
    @ViewBuilder var content: () -> Content
    @Environment(\.isSnapshot) private var isSnapshot
    @State private var contentHeight: CGFloat?

    var body: some View {
        if isSnapshot {
            inner
        } else {
            let scrolling = (contentHeight ?? 0) > maxHeight
            ScrollView(.vertical) {
                inner
            }
            .scrollIndicators(scrolling ? .automatic : .hidden)
            .scrollDisabled(!scrolling)
            .scrollBounceBehavior(.basedOnSize)
            .frame(height: contentHeight.map { min($0, maxHeight) })
        }
    }

    private var inner: some View {
        content()
            .frame(maxWidth: .infinity, alignment: .leading)
            .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { height in
                if !isSnapshot {
                    if contentHeight != nil {
                        withMotion { contentHeight = height }
                    } else {
                        contentHeight = height
                    }
                }
            }
    }
}
