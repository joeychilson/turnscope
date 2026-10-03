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

/// An account's title, and under it what tells it apart, as a list of
/// accounts names each: at `size`, in the grays `title` and `subtitle`.
struct AccountName: View {
    var account: Account
    var size: CGFloat = 12
    var title: HierarchicalShapeStyle = .primary
    var subtitle: HierarchicalShapeStyle = .tertiary

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(account.title).font(.system(size: size)).foregroundStyle(title)
            if let line = Words().subtitle(account) {
                Text(line).font(.system(size: 11)).foregroundStyle(subtitle)
                    .lineLimit(1).truncationMode(.middle)
            }
        }
    }
}
