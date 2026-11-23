// The panel's floating frame with native popover glass, border, shadow,
// and horizontal page transitions between FirstPage and SettingsPage.

import AppKit
import SwiftUI

let panelWidth: CGFloat = 340

extension EnvironmentValues {
    @Entry var closePanel: () -> Void = {}
    @Entry var isSnapshot = false
}

struct Floating: View {
    @Environment(Bridge.self) private var bridge
    static let gutter: CGFloat = 28

    var body: some View {
        AnimatablePanel(active: bridge.panelVisible, maxHeight: max(280, bridge.screenHeight - 80)) {
            Panel()
        }
        .padding(.horizontal, Floating.gutter)
        .padding(.bottom, Floating.gutter * 2)
        .frame(maxHeight: .infinity, alignment: .top)
    }
}

/// Interpolates the panel's frame height (`NSVisualEffectView`, clip mask, border, and shadow)
/// on every spring frame so AppKit's backdrop never snaps ahead of expanding or collapsing rows.
private struct AnimatablePanel<Content: View>: View {
    var active: Bool
    var maxHeight: CGFloat
    @ViewBuilder var content: () -> Content
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var targetHeight: CGFloat = 0
    @State private var measured = false

    var body: some View {
        content()
            .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { newHeight in
                guard newHeight > 0 else { return }
                let clamped = min(newHeight, maxHeight)
                if measured && active && !reduceMotion {
                    withAnimation(spring) {
                        targetHeight = clamped
                    }
                } else {
                    targetHeight = clamped
                    if active { measured = true }
                }
            }
            .onChange(of: active) { _, isActive in
                if !isActive {
                    measured = false
                } else if targetHeight > 0 {
                    measured = true
                }
            }
            .modifier(AnimatedPanelFrame(height: targetHeight))
    }
}

private struct AnimatedPanelFrame: ViewModifier, Animatable {
    nonisolated var height: CGFloat

    nonisolated var animatableData: CGFloat {
        get { height }
        set { height = newValue }
    }

    func body(content: Content) -> some View {
        let shape = RoundedRectangle(cornerRadius: 14, style: .continuous)
        content
            .frame(width: panelWidth, height: height > 0 ? height : nil, alignment: .top)
            .background(Glass())
            .clipShape(shape)
            .overlay(shape.strokeBorder(Color(nsColor: .separatorColor), lineWidth: 0.5))
            .shadow(color: .black.opacity(0.28), radius: 18, y: 8)
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

private final class BackdropView: NSVisualEffectView {
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

struct Glass: NSViewRepresentable {
    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = BackdropView()
        view.material = .popover
        view.blendingMode = .behindWindow
        view.state = .active
        return view
    }

    func updateNSView(_ view: NSVisualEffectView, context: Context) {}
}

struct Panel: View {
    @Environment(Bridge.self) private var bridge
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        ZStack(alignment: .top) {
            switch bridge.panel.page {
            case .main: FirstPage().transition(slide)
            case .settings: SettingsPage().transition(slide)
            }
        }
        .frame(width: panelWidth)
        .fixedSize(horizontal: false, vertical: true)
        .clipped()
    }

    private var slide: AnyTransition {
        guard !reduceMotion else { return .opacity }
        let forward = bridge.forward
        return .asymmetric(
            insertion: .move(edge: forward ? .trailing : .leading).combined(with: .opacity),
            removal: .move(edge: forward ? .leading : .trailing).combined(with: .opacity)
        )
    }
}

/// Scroll container used only by Settings tabs taller than the screen.
/// Animates height smoothly on tab switches and renders unwrapped in `--snapshot`.
struct SettingsScrollView<Content: View>: View {
    var maxHeight: CGFloat
    @ViewBuilder var content: () -> Content
    @Environment(\.isSnapshot) private var isSnapshot
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var contentHeight: CGFloat = 0
    @State private var measured = false

    var body: some View {
        if isSnapshot {
            content()
                .frame(maxWidth: .infinity, alignment: .leading)
        } else {
            ScrollView(.vertical) {
                content()
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .onGeometryChange(for: CGFloat.self, of: { $0.size.height }) { newHeight in
                        guard newHeight > 0 else { return }
                        if measured && !reduceMotion {
                            withAnimation(spring) {
                                contentHeight = newHeight
                            }
                        } else {
                            contentHeight = newHeight
                            measured = true
                        }
                    }
            }
            .scrollDisabled(contentHeight <= maxHeight)
            .scrollIndicators(contentHeight > maxHeight ? .automatic : .never)
            .scrollBounceBehavior(.basedOnSize)
            .frame(height: contentHeight > 0 ? min(contentHeight, maxHeight) : nil)
        }
    }
}
