// The app's look, shared by every view: its springs, sizes, fonts, colors of
// urgency, and the few controls it draws itself. It is grayscale until
// something needs the person: amber for a limit that at this pace runs out
// before it resets, red for one used up. Native controls, switches and
// pickers, keep the system's accent, as every Mac app's do.

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
    /// A logo's size, beside a row's name.
    static let logo: CGFloat = 16
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
    /// The corner radius of a row when it's filled.
    static let radius: CGFloat = 10
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
    /// It's as bright as the amber beside it, and as muted.
    static let active = Color(red: 0.27, green: 0.72, blue: 0.43)
}

extension Standing {
    /// The color for something with this standing.
    var color: Color {
        switch self {
        case .lasts: .primary
        case .runningOut: Color(red: 0.93, green: 0.58, blue: 0.10)
        case .usedUp: Color(red: 0.92, green: 0.26, blue: 0.22)
        }
    }

    /// The color for text about something with this standing: amber or red
    /// when it needs attention, otherwise the gray `lasting`.
    func ink(lasting: Color) -> Color {
        self == .lasts ? lasting : color
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
