// The app's look, shared by every view: its springs, sizes, fonts, colors of
// urgency, and the few controls it draws itself. It is grayscale until
// something needs the person: amber for a limit that at this pace runs out
// before it resets, red for one used up. A switch that's on keeps the
// system's accent, as every Mac app's does.

import SwiftUI
import TurnscopeKit

/// How things move: one spring for the whole app.
let spring = Animation.spring(response: 0.34, dampingFraction: 0.9)

/// New content fades in once its space has opened, and fades out at once, so
/// nothing moving passes over text.
@MainActor let appears = AnyTransition.asymmetric(
    insertion: .opacity.animation(.easeOut(duration: 0.16).delay(0.16)),
    removal: .opacity.animation(.easeOut(duration: 0.06))
)

/// The sizes every row uses, in the panel and in Settings, whether it's an
/// account, an agent or a setting. That way rows of every kind line up and
/// read alike.
enum Row {
    /// A logo's size, beside a row's name. A mark has room around it in its
    /// frame (`macos/Logos/README.md`): at 16 points Anthropic's was 9 tall
    /// and looked small beside a name with a line under it, and at 20
    /// OpenRouter's looked heavy.
    static let logo: CGFloat = 18
    /// The space from a logo to the name beside it.
    static let gap: CGFloat = 8
    /// The padding around a row's content.
    static let padding = EdgeInsets(top: 7, leading: 12, bottom: 7, trailing: 12)
    /// Extra space under a row that ends in a bar. It matches the space a
    /// line of text keeps above its letters, so the row looks as deep below
    /// as above, like a row that ends in text.
    static let underBar: CGFloat = 3
    /// The space between rows, so an open or hovered row doesn't touch the
    /// next.
    static let spacing: CGFloat = 4
    /// The space between the accounts in use and those used this week.
    static let groups: CGFloat = 12
    /// The corner radius of a row when it's filled, and of a card in
    /// Settings.
    static let radius: CGFloat = 10
    /// The height of a row in Settings with one line, whether it ends in a
    /// switch, a pill or nothing.
    static let setting: CGFloat = 36
}

extension Font {
    /// A row's name: an account, an agent or a setting.
    static let name = Font.system(size: 13)
    /// The figure on a row's right: what's left, or what it cost.
    static let figure = Font.system(size: 13, weight: .semibold).monospacedDigit()
    /// A line of text, such as a sentence or what happens to a limit next.
    static let line = Font.system(size: 12)
    /// Small text: the line under a name, a limit's name and figure, or a
    /// heading.
    static let small = Font.system(size: 11)
}

extension Color {
    /// The color of something at work right now, such as an active session.
    /// It's the only color in a calm panel, since it needs nothing from you.
    /// It's as bright as the dark appearance's amber, and as muted.
    static let active = Color(red: 0.27, green: 0.72, blue: 0.43)

    /// The color `0xRRGGBB`, in sRGB.
    fileprivate init(hex: Int) {
        self.init(red: Double(hex >> 16 & 0xFF) / 255, green: Double(hex >> 8 & 0xFF) / 255,
                  blue: Double(hex & 0xFF) / 255)
    }
}

extension Standing {
    /// The color for something with this standing, in the appearance it's
    /// drawn in.
    var color: Urgency { Urgency(standing: self) }

    /// The color for text about something with this standing: amber or red
    /// when it needs attention, otherwise the gray `lasting`.
    func ink(lasting: Color) -> AnyShapeStyle {
        self == .lasts ? AnyShapeStyle(lasting) : AnyShapeStyle(color)
    }
}

/// A standing's color: the text's own color while it lasts, amber while it's
/// running out, red once it's used up.
///
/// Amber and red are text as small as 11 points, so each reaches 4.5:1
/// against the darkest background it's on: an open row on the panel's
/// glass, about #DDDDDD in light mode and #3B3B3B in dark. With Increase
/// Contrast, which also makes the glass opaque, each reaches 7:1 against an
/// open row on the window's background, #F2F2F2 and #292929 as `--snapshot`
/// draws them. Each keeps its hue, and as much of its chroma as sRGB holds.
/// One amber and one red for every appearance were 1.7:1 and 2.9:1 on a
/// light open row on the glass, and the red 2.9:1 on a dark one (October
/// 2026).
///
/// | | Light | Dark | Light, Increase Contrast | Dark, Increase Contrast |
/// |---|---|---|---|---|
/// | Amber | #8A5303, 4.7:1 | #ED941A, 4.7:1 | #754501, 7.2:1 | #FCA231, 7.2:1 |
/// | Red | #C1070E, 4.7:1 | #FE8375, 4.7:1 | #A5040A, 7.2:1 | #FF9C8F, 7.2:1 |
///
/// SwiftUI resolves it from the environment. A dynamic `NSColor` would never
/// see Increase Contrast: SwiftUI asks it only for light or dark.
struct Urgency: ShapeStyle {
    var standing: Standing

    func resolve(in environment: EnvironmentValues) -> Color {
        let dark = environment.colorScheme == .dark
        let more = environment.colorSchemeContrast == .increased
        return switch (standing, dark, more) {
        case (.lasts, _, _): .primary
        case (.runningOut, false, false): Color(hex: 0x8A5303)
        case (.runningOut, true, false): Color(hex: 0xED941A)
        case (.runningOut, false, true): Color(hex: 0x754501)
        case (.runningOut, true, true): Color(hex: 0xFCA231)
        case (.usedUp, false, false): Color(hex: 0xC1070E)
        case (.usedUp, true, false): Color(hex: 0xFE8375)
        case (.usedUp, false, true): Color(hex: 0xA5040A)
        case (.usedUp, true, true): Color(hex: 0xFF9C8F)
        }
    }
}

/// A section's heading, in both Settings and the panel.
struct SectionTitle: View {
    var text: String

    var body: some View {
        Text(text).font(.small.weight(.semibold)).foregroundStyle(.secondary)
            .accessibilityAddTraits(.isHeader)
            .padding(.horizontal, 12)
            .padding(.bottom, 4)
    }
}

/// A small icon button that fills on hover and darkens when pressed, like
/// the system's toolbar buttons.
struct IconButton: ButtonStyle {
    var width: CGFloat = 28
    /// How tall it is. Within a row, it's no taller than the line it sits
    /// on, so showing it never moves the row.
    var height: CGFloat = 28

    func makeBody(configuration: Configuration) -> some View {
        Hover { hover in
            configuration.label
                .font(.system(size: height > 24 ? 13 : 11, weight: .medium))
                .foregroundStyle(hover ? .primary : .secondary)
                .frame(width: width, height: height)
                .background(RoundedRectangle(cornerRadius: min(width, height) / 4, style: .continuous)
                    .fill(Color.primary.opacity(configuration.isPressed ? 0.14 : hover ? 0.08 : 0)))
        }
    }
}

/// The one action on a row: quiet, and filling on hover.
struct PillButton: ButtonStyle {
    /// Whether it's filled before it's hovered. In a `Choice`, only the
    /// chosen one is, and the rest are gray.
    var filled = true

    func makeBody(configuration: Configuration) -> some View {
        Hover { hover in
            configuration.label
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(filled || hover ? .primary : .secondary)
                .padding(.horizontal, 10)
                .frame(height: 22)
                .background(Capsule().fill(Color.primary.opacity(
                    configuration.isPressed ? 0.2 : hover ? 0.14 : filled ? 0.08 : 0)))
        }
    }
}

/// One of a few options, as pills in a row with the chosen one filled. It
/// stands in for the system's segmented control, whose gray bar was the one
/// part of the panel the system drew.
struct Choice<Value: Hashable>: View {
    var label: String
    var options: [(value: Value, title: String)]
    @Binding var selection: Value

    var body: some View {
        HStack(spacing: 2) {
            ForEach(options.indices, id: \.self) { index in
                let (value, title) = options[index]
                Button(title) { selection = value }
                    .buttonStyle(PillButton(filled: value == selection))
                    .accessibilityAddTraits(value == selection ? .isSelected : [])
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(label)
    }
}

/// Tells `content` whether the pointer is over it, animated.
struct Hover<Content: View>: View {
    @ViewBuilder var content: (Bool) -> Content
    @State private var hover = false

    var body: some View {
        content(hover)
            .contentShape(Rectangle())
            .onHover { over in withAnimation(.easeOut(duration: 0.1)) { hover = over } }
    }
}

extension View {
    /// Fills a row softly while it's `on`: hovered, or open. The row tracks
    /// its own hover, since it shows more than this on hover.
    func rowBackground(_ on: Bool) -> some View {
        background(RoundedRectangle(cornerRadius: Row.radius, style: .continuous)
            .fill(on ? Color.primary.opacity(0.06) : .clear))
            .contentShape(Rectangle())
    }
}
