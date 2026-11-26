import AppKit
import SwiftUI

func copyToPasteboard(_ text: String) {
    let pasteboard = NSPasteboard.general
    pasteboard.clearContents()
    pasteboard.setString(text, forType: .string)
}

extension View {
    /// Sets `copied` for a moment each time `copies` grows.
    func flashesCopied(_ copied: Binding<Bool>, copies: Int) -> some View {
        task(id: copies) {
            guard copies > 0 else { return }
            withAnimation(.easeOut(duration: 0.12)) { copied.wrappedValue = true }
            try? await Task.sleep(for: .seconds(1.5))
            guard !Task.isCancelled else { return }
            withAnimation(.easeOut(duration: 0.16)) { copied.wrappedValue = false }
        }
    }

    /// Attaches a tooltip only when non-nil, so an empty string never shadows an ancestor's help.
    @ViewBuilder
    func optionalHelp(_ text: String?) -> some View {
        if let text, !text.isEmpty {
            help(text)
        } else {
            self
        }
    }
}

/// Just under critically damped: it overshoots by under 0.1%, so nothing visibly wobbles.
let spring = Animation.spring(response: 0.25, dampingFraction: 0.92)

/// Uses `spring` normally; under Reduce Motion, suppresses movement (`nil`) or uses a short
/// fade for opacity-only page/tab transitions (`docs/app.md#the-panel`).
@MainActor
func withMotion<Result>(fade: Bool = false, _ body: () throws -> Result) rethrows -> Result {
    let animation: Animation? = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        ? (fade ? .easeOut(duration: 0.12) : nil)
        : spring
    return try withAnimation(animation, body)
}

/// Expanded content fades in right as layout opens, and fades out at once.
@MainActor let appears = AnyTransition.asymmetric(
    insertion: .opacity.animation(.easeOut(duration: 0.14).delay(0.05)),
    removal: .opacity.animation(.easeOut(duration: 0.05))
)

/// Collapsed row content returns immediately so closing rows never pause blank.
@MainActor let quickFade = AnyTransition.opacity.animation(.easeOut(duration: 0.12))

enum Row {
    /// At 16pt Anthropic's mark was 9pt tall and looked small; at 20pt OpenRouter's looked heavy.
    static let logo: CGFloat = 18
    static let gap: CGFloat = 8
    static let padding = EdgeInsets(top: 7, leading: 12, bottom: 7, trailing: 12)
    /// Balances visual space below a bar with the line-height leading above text.
    static let underBar: CGFloat = 3
    static let spacing: CGFloat = 4
    static let groups: CGFloat = 12
    static let radius: CGFloat = 10
    static let setting: CGFloat = 36
}

extension Font {
    static let name = Font.system(size: 13)
    static let figure = Font.system(size: 13, weight: .semibold).monospacedDigit()
    static let line = Font.system(size: 12)
    static let small = Font.system(size: 11)
}

extension Color {
    init(hex: UInt32) {
        self.init(
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255
        )
    }
}

/// Amber and red in Light, Dark, Light + Increase Contrast, Dark + Increase Contrast.
/// 4.7:1 against an open row on glass (#DDDDDD / #3B3B3B); 7.2:1 with Increase Contrast
/// against an open row on the opaque background (#E6E6E6 / #343434).
enum UrgencyHex {
    static let amber: [UInt32] = [0x8A5303, 0xED941A, 0x6B3E00, 0xFFB76B]
    static let red: [UInt32] = [0xC1070E, 0xFE8375, 0x970007, 0xFFB3A8]
}

extension Standing {
    var color: Urgency { Urgency(standing: self) }
}

struct Urgency: ShapeStyle {
    var standing: Standing
    var lasting: Color = .primary

    func resolve(in environment: EnvironmentValues) -> Color {
        let index = (environment.colorScheme == .dark ? 1 : 0)
            + (environment.colorSchemeContrast == .increased ? 2 : 0)
        return switch standing {
        case .lasts: lasting
        case .runningOut: Color(hex: UrgencyHex.amber[index])
        case .usedUp: Color(hex: UrgencyHex.red[index])
        }
    }
}

struct SectionTitle: View {
    var text: String

    var body: some View {
        Text(text)
            .font(.small.weight(.semibold))
            .foregroundStyle(.secondary)
            .accessibilityAddTraits(.isHeader)
            .padding(.horizontal, 12)
            .padding(.bottom, 4)
    }
}

struct IconButton: ButtonStyle {
    var width: CGFloat = 28
    var height: CGFloat = 28
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        Hover { hover in
            let active = isEnabled && hover
            configuration.label
                .font(.system(size: height > 24 ? 13 : 11, weight: .medium))
                .foregroundStyle(active ? .primary : .secondary)
                .frame(width: width, height: height)
                .background(
                    RoundedRectangle(cornerRadius: min(width, height) / 4, style: .continuous)
                        .fill(Color.primary.opacity(
                            !isEnabled ? 0 : configuration.isPressed ? 0.14 : hover ? 0.08 : 0
                        ))
                )
                .opacity(isEnabled ? 1 : 0.5)
        }
    }
}

struct PillButton: ButtonStyle {
    var filled = true
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        Hover { hover in
            let active = isEnabled && (filled || hover)
            configuration.label
                .font(.system(size: 12, weight: .medium))
                .lineLimit(1)
                .fixedSize(horizontal: true, vertical: false)
                .foregroundStyle(active ? .primary : .secondary)
                .padding(.horizontal, 10)
                .frame(height: 22)
                .background(
                    Capsule().fill(Color.primary.opacity(
                        !isEnabled ? (filled ? 0.08 : 0)
                            : configuration.isPressed ? 0.20 : hover ? 0.14 : filled ? 0.08 : 0
                    ))
                )
                .opacity(isEnabled ? 1 : 0.5)
        }
    }
}

/// Circular symbol button the height of a pill, used beside pills (e.g. dismiss connect offer).
struct RoundButton: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        Hover { hover in
            configuration.label
                .font(.system(size: 10, weight: .semibold))
                .foregroundStyle(hover ? .primary : .secondary)
                .frame(width: 22, height: 22)
                .background(
                    Circle().fill(Color.primary.opacity(
                        configuration.isPressed ? 0.20 : hover ? 0.14 : 0.08
                    ))
                )
        }
    }
}

private struct SegmentButton: ButtonStyle {
    var selected: Bool
    @Environment(\.colorScheme) private var scheme
    @Environment(\.colorSchemeContrast) private var contrast
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        Hover { hover in
            let active = isEnabled && (selected || hover)
            configuration.label
                .font(.system(size: 12, weight: .medium))
                .lineLimit(1)
                .fixedSize(horizontal: true, vertical: false)
                .foregroundStyle(active ? .primary : .secondary)
                .padding(.horizontal, 9)
                .frame(height: 20)
                .background {
                    if selected {
                        Capsule()
                            .fill(scheme == .dark
                                ? Color.white.opacity(contrast == .increased ? 0.24 : 0.16)
                                : Color.white.opacity(contrast == .increased ? 1.0 : 0.92))
                            .shadow(color: .black.opacity(scheme == .dark ? 0.22 : 0.10), radius: 1, y: 0.5)
                    } else if hover || configuration.isPressed {
                        Capsule()
                            .fill(Color.primary.opacity(configuration.isPressed ? 0.12 : 0.06))
                    }
                }
                .opacity(isEnabled ? 1 : 0.5)
        }
    }
}

struct Choice<Value: Hashable>: View {
    var label: String
    var options: [(value: Value, title: String)]
    var selection: Value
    var onSelect: (Value) -> Void
    @Environment(\.colorSchemeContrast) private var contrast

    var body: some View {
        HStack(spacing: 2) {
            ForEach(options.indices, id: \.self) { index in
                let (value, title) = options[index]
                Button(title) { onSelect(value) }
                    .buttonStyle(SegmentButton(selected: value == selection))
                    .accessibilityAddTraits(value == selection ? .isSelected : [])
            }
        }
        .padding(2)
        .background(
            Capsule().fill(Color.primary.opacity(contrast == .increased ? 0.10 : 0.06))
        )
        .accessibilityElement(children: .contain)
        .accessibilityLabel(label)
    }
}

struct Hover<Content: View>: View {
    @ViewBuilder var content: (Bool) -> Content
    @State private var hover = false

    var body: some View {
        content(hover)
            .contentShape(Rectangle())
            .onHover { over in withAnimation(.easeOut(duration: 0.12)) { hover = over } }
    }
}

struct RowSurface: View {
    var active: Bool
    var pressed = false
    @Environment(\.colorSchemeContrast) private var contrast

    var body: some View {
        let base = contrast == .increased ? 0.10 : 0.06
        let opacity = pressed ? base + 0.06 : active ? base : 0
        RoundedRectangle(cornerRadius: Row.radius, style: .continuous)
            .fill(opacity > 0 ? Color.primary.opacity(opacity) : .clear)
    }
}

struct RowSurfaceButtonStyle: ButtonStyle {
    var open = false
    var hover: Bool
    var focused = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(RowSurface(active: open || hover || focused, pressed: !open && configuration.isPressed))
            .contentShape(Rectangle())
    }
}

/// Custom accent switch so it animates smoothly inside the panel and renders cleanly in snapshots.
struct Switch: View {
    var label: String
    @Binding var on: Bool
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        Button { on.toggle() } label: {
            Capsule()
                .fill(on ? Color.accentColor : Color.primary.opacity(0.16))
                .frame(width: 30, height: 18)
                .overlay(alignment: on ? .trailing : .leading) {
                    Circle()
                        .fill(.white)
                        .shadow(color: .black.opacity(0.18), radius: 1, y: 0.5)
                        .padding(2)
                }
                .animation(reduceMotion ? nil : spring, value: on)
                .opacity(isEnabled ? 1 : 0.5)
        }
        .buttonStyle(.plain)
        .accessibilityRepresentation { Toggle(label, isOn: $on) }
    }
}

struct Spinner: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        Group {
            if reduceMotion || isSnapshot {
                Circle()
                    .trim(from: 0, to: 0.7)
                    .stroke(.secondary, style: StrokeStyle(lineWidth: 1.5, lineCap: .round))
            } else {
                TurningArc()
            }
        }
        .frame(width: 9, height: 9)
        .accessibilityHidden(true)
    }
}

/// The spinner's arc, turned by Core Animation in the render server, and only while its window is
/// on screen. SwiftUI's `repeatForever` rotation cost the app 4–5% of a core, on screen or ordered
/// out; this, nothing measurable (measured 2026-11-26).
private struct TurningArc: NSViewRepresentable {
    func makeNSView(context: Context) -> TurningArcView { TurningArcView() }
    func updateNSView(_ view: TurningArcView, context: Context) {}
}

private final class TurningArcView: NSView {
    private let arc = CAShapeLayer()
    private var occlusion: NSObjectProtocol?

    override init(frame: NSRect) {
        super.init(frame: frame)
        wantsLayer = true
        arc.fillColor = nil
        arc.lineWidth = 1.5
        arc.lineCap = .round
        arc.strokeEnd = 0.7
        arc.actions = ["bounds": NSNull(), "position": NSNull(), "path": NSNull(), "strokeColor": NSNull()]
        layer?.addSublayer(arc)
    }

    required init?(coder: NSCoder) { nil }

    // As SwiftUI draws the arc: from 3 o'clock, clockwise.
    override var isFlipped: Bool { true }

    // Clicks and the footer's tooltip go to SwiftUI, as over the arc SwiftUI drew.
    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func layout() {
        super.layout()
        arc.frame = bounds
        arc.path = CGPath(ellipseIn: bounds, transform: nil)
    }

    override func updateLayer() {
        arc.strokeColor = NSColor.secondaryLabelColor.cgColor
    }

    override var wantsUpdateLayer: Bool { true }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if let occlusion { NotificationCenter.default.removeObserver(occlusion) }
        occlusion = window.map { window in
            NotificationCenter.default.addObserver(
                forName: NSWindow.didChangeOcclusionStateNotification, object: window, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.turn() }
            }
        }
        turn()
    }

    private func turn() {
        guard window?.occlusionState.contains(.visible) == true else {
            arc.removeAnimation(forKey: "turn")
            return
        }
        guard arc.animation(forKey: "turn") == nil else { return }
        let turn = CABasicAnimation(keyPath: "transform.rotation.z")
        turn.byValue = 2 * Double.pi
        turn.duration = 0.9
        turn.repeatCount = .infinity
        arc.add(turn, forKey: "turn")
    }
}
