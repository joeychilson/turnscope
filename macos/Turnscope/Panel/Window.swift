// The panel's shell: the floating frame with its own glass, edge and shadow,
// and the pages it slides between, the first page (`FirstPage.swift`) and
// Settings.
//
// It sits in a transparent window that doesn't resize and draws its own
// glass, edge and shadow, so its height animates with its content and nothing
// jumps (`App.swift`).

import SwiftUI
import TurnscopeKit

let panelWidth: CGFloat = 340

/// What the panel needs the app around it to do.
struct PanelActions {
    var quit: () -> Void = {}
    var close: () -> Void = {}
    /// Whether Turnscope's notifications are turned off in System Settings.
    var notificationsOff: () async -> Bool = { false }
}

extension EnvironmentValues {
    @Entry var panelActions = PanelActions()
}

/// The floating panel, with its own glass, edge and shadow.
struct Floating: View {
    @Environment(Store.self) private var store
    @Environment(Navigation.self) private var navigation
    /// Space around the panel in its window, for its shadow.
    static let gutter: CGFloat = 28

    var body: some View {
        let open = store.all.first { $0.id == navigation.open }
        Panel()
            .panelFrame(Glass())
            // What used the open account can come after it opened, or change
            // while it's open: the panel moves with it as when it opened.
            .animation(spring, value: open.map(store.usedMost))
            .shadow(color: .black.opacity(0.28), radius: 18, y: 8)
            .padding(.horizontal, Floating.gutter)
            .padding(.bottom, Floating.gutter * 2)
            .frame(maxHeight: .infinity, alignment: .top)
    }
}

extension View {
    /// The panel's shape over `background`: its corners and its edge.
    func panelFrame(_ background: some View) -> some View {
        let shape = RoundedRectangle(cornerRadius: 14, style: .continuous)
        return self.background(background)
            .clipShape(shape)
            .overlay(shape.strokeBorder(Color.primary.opacity(0.12), lineWidth: 0.5))
    }
}

/// The system's popover material, behind the window.
struct Glass: NSViewRepresentable {
    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = .popover
        view.blendingMode = .behindWindow
        view.state = .active
        return view
    }

    func updateNSView(_ view: NSVisualEffectView, context: Context) {}
}

/// The page shown, sliding in from the side as it changes. It's as tall as
/// its page.
struct Panel: View {
    @Environment(Navigation.self) private var navigation

    var body: some View {
        ZStack(alignment: .top) {
            switch navigation.page {
            case .main: FirstPage().transition(slide)
            case .settings: SettingsPage().transition(slide)
            }
        }
        .frame(width: panelWidth)
        .fixedSize(horizontal: false, vertical: true)
        .clipped()
    }

    private var slide: AnyTransition {
        let forward = navigation.forward
        return .asymmetric(insertion: .move(edge: forward ? .trailing : .leading).combined(with: .opacity),
                           removal: .move(edge: forward ? .leading : .trailing).combined(with: .opacity))
    }
}
