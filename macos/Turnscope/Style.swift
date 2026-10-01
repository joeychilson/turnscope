// The app's look, shared by every view: its springs, its colors of urgency,
// and the few controls it draws itself. It is grayscale until something needs
// the person: amber for a limit that at this pace runs out before it resets,
// red for one used up. Native controls, switches and pickers, keep the system's
// accent, as every Mac app's do.

import SwiftUI
import TurnscopeKit

/// How what moves, moves: one spring for the whole app.
let spring = Animation.spring(response: 0.34, dampingFraction: 0.9)

/// New content fades in once the room for it has opened, and out at once, so
/// nothing moving passes over text.
@MainActor let appears = AnyTransition.asymmetric(
    insertion: .opacity.animation(.easeOut(duration: 0.16).delay(0.16)),
    removal: .opacity.animation(.easeOut(duration: 0.06))
)

/// The color of something at work this moment, as a session active now: the
/// one color a calm panel carries, since it asks nothing of the person. As
/// bright as the amber beside it, and muted as it is.
let active = Color(red: 0.27, green: 0.72, blue: 0.43)

extension Standing {
    /// The color what stands so is written in.
    var color: Color {
        switch self {
        case .lasts: .primary
        case .runningOut: Color(red: 0.93, green: 0.58, blue: 0.10)
        case .usedUp: Color(red: 0.92, green: 0.26, blue: 0.22)
        }
    }
}

/// A small icon button that fills on hover and darker when pressed, as the
/// system's toolbar buttons do.
struct IconButton: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        Hover { hover in
            configuration.label
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(hover ? .primary : .secondary)
                .frame(width: 28, height: 28)
                .background(RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(Color.primary.opacity(configuration.isPressed ? 0.14 : hover ? 0.08 : 0)))
        }
    }
}

/// A row's one action: quiet, filling on hover.
struct PillButton: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        Hover { hover in
            configuration.label
                .font(.system(size: 12, weight: .medium))
                .padding(.horizontal, 10)
                .frame(height: 22)
                .background(Capsule().fill(Color.primary.opacity(configuration.isPressed ? 0.2 : hover ? 0.14 : 0.08)))
        }
    }
}

/// Whether the pointer is over `content`, animated.
struct Hover<Content: View>: View {
    @ViewBuilder var content: (Bool) -> Content
    @State private var hover = false

    var body: some View {
        content(hover)
            .contentShape(Rectangle())
            .onHover { over in withAnimation(.easeOut(duration: 0.1)) { hover = over } }
    }
}

/// A row that fills softly on hover, and while `on`.
struct RowBackground: ViewModifier {
    var on = false
    var radius: CGFloat = 10
    @State private var hover = false

    func body(content: Content) -> some View {
        content
            .background(RoundedRectangle(cornerRadius: radius, style: .continuous)
                .fill(hover || on ? Color.primary.opacity(0.06) : .clear))
            .contentShape(Rectangle())
            .onHover { over in withAnimation(.easeOut(duration: 0.12)) { hover = over } }
    }
}

extension View {
    func rowBackground(on: Bool = false, radius: CGFloat = 10) -> some View {
        modifier(RowBackground(on: on, radius: radius))
    }
}

/// A section's heading, and what it is for under it.
struct SectionTitle: View {
    var text: String
    var note: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(text).font(.system(size: 11, weight: .semibold)).foregroundStyle(.secondary)
                .accessibilityAddTraits(.isHeader)
            if let note {
                Text(note).font(.system(size: 11)).foregroundStyle(.tertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.horizontal, 12)
        .padding(.bottom, 4)
    }
}

/// A setting: its name, what it means under it, and its control at its end.
struct SettingRow<Control: View>: View {
    var title: String
    var detail: String?
    @ViewBuilder var control: () -> Control

    var body: some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 1) {
                Text(title).font(.system(size: 13))
                if let detail {
                    Text(detail).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            Spacer(minLength: 8)
            control()
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 5)
        .accessibilityElement(children: .combine)
    }
}

/// A switch, as small as the rows it sits in.
struct Switch: View {
    var label: String
    @Binding var on: Bool

    var body: some View {
        Toggle(label, isOn: $on).toggleStyle(.switch).controlSize(.mini).labelsHidden()
    }
}

/// A page's title with the way back beside it.
struct PageTitle: View {
    var title: String
    var back: () -> Void

    var body: some View {
        HStack(spacing: 4) {
            Button(action: back) { Image(systemName: "chevron.left") }
                .buttonStyle(IconButton())
                .keyboardShortcut(.cancelAction)
                .help("Back")
                .accessibilityLabel("Back")
            Text(title).font(.system(size: 15, weight: .semibold)).fixedSize().accessibilityAddTraits(.isHeader)
            Spacer()
        }
        .padding(.horizontal, 10)
        .padding(.top, 10)
    }
}
