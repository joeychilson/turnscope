// One account as a row, the same everywhere it's listed.
//
// Closed, it shows the account's logo and name, a line under the name saying
// which limit decides it and what happens next, and on the right what's left
// of that limit ("—" when nothing is known).
//
// It opens only when it has more to show: its limits, or a note on what they
// can't say. Opened, it shows each limit as its own line, all laid out alike:
// the limit's name and times (when it runs out, when it resets, or when it's
// back), what's left, and a bar. Below them come the note, and the sessions
// that used it most, where any did.
//
// Hovering a row shows its Hide button, and right-clicking offers Hide too.

import SwiftUI
import TurnscopeKit

struct AccountRow: View {
    @Environment(Store.self) private var store
    @Environment(Navigation.self) private var navigation
    var account: AccountStatus
    /// Whether the headline is about this account. If the account has no
    /// limits, the headline already says what its row would.
    var headlined = false
    @State private var hover = false

    var body: some View {
        // As of the panel's minute, so countdowns redraw with it.
        let words = store.words(now: navigation.now)
        let note = words.opened(account, headlined: headlined)
        let openable = !account.limits.isEmpty || note != nil
        let open = openable && navigation.open == account.id
        // Whether its limits show as lines. They say everything the line
        // under the name would.
        let lines = open && !account.limits.isEmpty
        let usedMost = store.usedMost(account)
        let used = open && !usedMost.isEmpty
        let deciding = account.decidingLimit
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .center, spacing: Row.gap) {
                Logo(account: account, size: Row.logo)
                VStack(alignment: .leading, spacing: 1) {
                    AccountTitle(account: account, labelled: account.twinned(among: store.all))
                    if !lines {
                        Text(words.status(account))
                            .font(.small)
                            .foregroundStyle(deciding?.standing.ink(lasting: .secondary) ?? .secondary)
                            .lineLimit(1)
                            .transition(appears)
                    }
                }
                // The name and the line under it take the space they need
                // before the space between them and the figure is shared.
                .layoutPriority(1)
                Spacer(minLength: 8)
                HideButton(account: account, shown: hover)
                // Once the limit lines show, they say what the figure does,
                // so it fades as they open, like the line under the name.
                if !lines {
                    // The text after the figure says what it is, so they
                    // read as one: "$10 of $30", "$84 this month".
                    let said = words.rowFigure(account)
                    let left = Text(said.figure).foregroundStyle(deciding?.standing.ink(lasting: .primary) ?? .primary)
                    let then = Text(said.then.map { " \($0)" } ?? "")
                        .font(.name).foregroundStyle(.secondary)
                    Text("\(left)\(then)")
                        .font(.figure)
                        .contentTransition(.numericText())
                        .fixedSize()
                        .transition(appears)
                }
            }
            // VoiceOver reads the row as one button with its name and line.
            // Once it's open, each limit and session follows on its own.
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(openable ? .isButton : [])
            .accessibilityHint(openable ? (open ? "Closes it" : "Opens it") : "")
            .accessibilityAction(.default) { if openable { toggle() } }
            .accessibilityAction(named: "Hide") { withAnimation(spring) { store.setHidden(true, for: account.id) } }
            if open {
                VStack(alignment: .leading, spacing: 10) {
                    if lines { LimitLines(limits: account.limits, last: note == nil && !used) }
                    if let note {
                        Text(note)
                            .font(.line).foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    // They can come after the row opened. If so, they wait
                    // for their space like the rest of it did.
                    if used { UsedMostList(account: account, sessions: usedMost).transition(appears) }
                }
                .transition(appears)
            }
        }
        .padding(Row.padding)
        .rowBackground(open || hover)
        .onHover { over in withAnimation(.easeOut(duration: 0.12)) { hover = over } }
        // Not a button, since its Hide button sits inside it. It opens on a
        // click, or on Space or Return once it has keyboard focus.
        .onTapGesture { if openable { toggle() } }
        .focusable(openable, interactions: .activate)
        .onKeyPress(keys: [.space, .return]) { _ in
            guard openable else { return .ignored }
            toggle()
            return .handled
        }
        .contextMenu {
            Button("Hide \(account.title)") { withAnimation(spring) { store.setHidden(true, for: account.id) } }
        }
    }

    /// Open it in place, closing any other, or close it if it's open.
    private func toggle() {
        withAnimation(spring) { navigation.toggle(account.id) }
    }
}

/// Every limit, one line each, across the full width of a row: its name, its
/// times and what's left, above its bar. Every limit of every account uses
/// the same bar.
struct LimitLines: View {
    var limits: [LimitStatus]
    /// Whether they're the last thing in their row.
    var last: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(limits) { LimitLine(limit: $0, last: last && $0.id == limits.last?.id) }
        }
    }
}

private struct LimitLine: View {
    @Environment(Store.self) private var store
    @Environment(Navigation.self) private var navigation
    var limit: LimitStatus
    /// Whether it's the last thing in its row. If so, since it ends in its
    /// bar, it adds the space a line of text keeps above its letters
    /// (`Row.underBar`), so the row looks as deep below as above.
    var last: Bool

    var body: some View {
        let words = store.words(now: navigation.now)
        let name = Text(words.name(limit)).fontWeight(.medium).foregroundStyle(.secondary)
        // When it runs out, in amber; then when it resets, or when it's back
        // if it's used up, in red. Only the times are colored, not the dots.
        let ahead = words.ahead(limit).map { Text($0).foregroundStyle(limit.standing.color) }
        let usedUp = limit.standing == .usedUp
        let window = words.window(limit).map {
            Text($0).foregroundStyle(usedUp ? limit.standing.color : .secondary)
        }
        let line = { (parts: [Text]) in
            parts.reduce(name) { said, part in Text("\(said)\(Text(" · ").foregroundStyle(.secondary))\(part)") }
        }
        let left = Text(words.left(limit))
            .fontWeight(.medium).foregroundStyle(limit.standing.ink(lasting: .primary))
        let size = Text(words.size(limit).map { " \($0)" } ?? "").foregroundStyle(.secondary)
        VStack(alignment: .leading, spacing: 5) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                // If both times don't fit, show only when it runs out, or
                // else the other time.
                ViewThatFits(in: .horizontal) {
                    line([ahead, window].compactMap { $0 }).lineLimit(1)
                    line([ahead ?? window].compactMap { $0 }).lineLimit(1).truncationMode(.tail)
                }
                Spacer(minLength: 8)
                Text("\(left)\(size)").monospacedDigit().fixedSize()
            }
            Bar(limit: limit)
        }
        .padding(.bottom, last ? Row.underBar : 0)
        .font(.small)
    }
}

/// A limit's level: what's left now. It's gray while the limit lasts, amber
/// while it's running out, and red when it's used up. It's full once the
/// limit has reset since it was read, as serve says. This is the only bar
/// the app draws.
struct Bar: View {
    var limit: LimitStatus
    @State private var grown = false

    var body: some View {
        let level = Double(limit.leftPercent) / 100
        GeometryReader { geometry in
            ZStack(alignment: .leading) {
                Capsule().fill(.quaternary)
                Capsule()
                    .fill(limit.standing == .lasts ? AnyShapeStyle(.secondary) : AnyShapeStyle(limit.standing.color))
                    .frame(width: max(level > 0 ? 5 : 0, geometry.size.width * (grown ? level : 0)))
            }
        }
        .frame(height: 5)
        .accessibilityHidden(true)
        .onAppear { withAnimation(spring.delay(0.05)) { grown = true } }
    }
}

/// What used the limit that matters: the sessions that used the most, if
/// any. A session's share goes under its title, at the start of the line, so
/// the figures on the right are always what's left.
private struct UsedMostList: View {
    @Environment(Store.self) private var store
    @Environment(Navigation.self) private var navigation
    var account: AccountStatus
    var sessions: [UsedMost]

    var body: some View {
        let words = store.words(now: navigation.now)
        // The agent is named only if the sessions ran in more than one.
        let agents = Set(sessions.map(\.agent)).count > 1
        VStack(alignment: .leading, spacing: 8) {
            Text("Used most \(account.decidingLimit.map(words.of) ?? "")")
                .font(.small.weight(.semibold))
                .foregroundStyle(.secondary)
            ForEach(sessions) { used in
                VStack(alignment: .leading, spacing: 1) {
                    HStack(spacing: 5) {
                        Text(used.title ?? "Untitled session").font(.line).lineLimit(1)
                        if used.running {
                            Circle().fill(Color.active).frame(width: 6, height: 6)
                                .help("Active now")
                                .accessibilityLabel("Active now")
                        }
                    }
                    Text(words.used(used, agents: agents))
                        .font(.small.monospacedDigit()).foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }
}
