// The panel: its floating frame, and its two pages.

import AppKit
import SwiftUI

let panelWidth: CGFloat = 340

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

    /// Keeps a button for its keyboard shortcut, unseen and unread by VoiceOver.
    func hiddenShortcut() -> some View {
        frame(width: 0, height: 0).opacity(0).accessibilityHidden(true)
    }
}

struct Floating: View {
    static let gutter: CGFloat = 28

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: 14, style: .continuous)
        Panel()
            .clipShape(shape)
            .background {
                // Keep hit-testing on the material so WindowServer's event shape covers the whole
                // panel; disabling it makes closed rows and row gaps click-through to windows behind.
                shape
                    .fill(.regularMaterial)
                    .shadow(color: .black.opacity(0.28), radius: 18, y: 8)
            }
            .overlay {
                shape
                    .strokeBorder(Color(nsColor: .separatorColor), lineWidth: 0.5)
                    .allowsHitTesting(false)
            }
            .materialActiveAppearance(.active)
            .padding(.horizontal, Floating.gutter)
            .padding(.bottom, Floating.gutter * 2)
            .frame(maxHeight: .infinity, alignment: .top)
    }
}

extension View {
    func panelFrame(_ background: some View) -> some View {
        let shape = RoundedRectangle(cornerRadius: 14, style: .continuous)
        return self.background(background)
            .clipShape(shape)
            .overlay(shape.strokeBorder(Color(nsColor: .separatorColor), lineWidth: 0.5))
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

/// Renders content unwrapped for single-spring animations, activating vertical scroll only
/// when content actually exceeds `maxHeight` (e.g. a very long account list on a small screen).
struct OverflowScroll<Content: View>: View {
    var maxHeight: CGFloat
    @ViewBuilder var content: () -> Content
    @Environment(\.isSnapshot) private var isSnapshot
    @State private var overflow = false

    var body: some View {
        if overflow && !isSnapshot {
            ScrollView(.vertical) {
                measured
            }
            .scrollBounceBehavior(.basedOnSize)
            .frame(height: maxHeight)
        } else {
            measured
        }
    }

    private var measured: some View {
        content()
            .frame(maxWidth: .infinity, alignment: .leading)
            .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { height in
                guard !isSnapshot else { return }
                if !overflow && height > maxHeight {
                    overflow = true
                } else if overflow && height < maxHeight - 40 {
                    overflow = false
                }
            }
    }
}
