// One account as a row, expanded in place on click, Space, or Return,
// plus shared account title/name views and limit progress bars.

import SwiftUI

struct AccountRow: View {
    @Environment(Bridge.self) private var bridge
    var row: AccountRowModel
    @State private var hover = false

    var body: some View {
        let open = row.openable && bridge.panel.openAccount == row.id
        let lines = open && !row.limits.isEmpty
        let used = open && !row.usedMost.isEmpty
        let stopped = bridge.panel.stopped

        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .center, spacing: Row.gap) {
                Logo(provider: row.provider, size: Row.logo)
                VStack(alignment: .leading, spacing: 1) {
                    AccountTitle(title: row.title, label: row.label, labelStyle: .secondary)
                    if !lines {
                        Text(row.status)
                            .font(.small)
                            .foregroundStyle(row.standing.ink(lasting: .secondary))
                            .lineLimit(1)
                            .transition(appears)
                    }
                }
                .layoutPriority(1)
                Spacer(minLength: 8)
                // Always given its layout slot so showing it on hover never shifts text.
                Button(action: hide) { Image(systemName: "eye.slash") }
                    .buttonStyle(IconButton(width: 22, height: 16))
                    .disabled(stopped)
                    .opacity(hover ? 1 : 0)
                    .allowsHitTesting(hover)
                    .accessibilityHidden(!hover)
                    .help("Hide \(row.called) from the menu bar and the panel")
                    .accessibilityLabel("Hide \(row.called)")
                if !lines {
                    let left = Text(row.figure).foregroundStyle(row.standing.ink(lasting: .primary))
                    let then = Text(row.figureThen.map { " \($0)" } ?? "")
                        .font(.name)
                        .foregroundStyle(.secondary)
                    Text("\(left)\(then)")
                        .font(.figure)
                        .contentTransition(.numericText())
                        .fixedSize()
                        .transition(appears)
                        .accessibilityLabel(row.figureSpoken)
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel(row.headerSpoken)
            .accessibilityAddTraits(row.openable ? .isButton : [])
            .accessibilityHint(row.openable ? (open ? "Closes it" : "Opens it") : "")
            .accessibilityAction(.default) { if row.openable { toggle() } }
            .accessibilityAction(named: "Hide") { if !stopped { hide() } }

            if open {
                VStack(alignment: .leading, spacing: 10) {
                    if lines {
                        ForEach(row.limits) { limit in
                            LimitLine(
                                limit: limit,
                                last: row.note == nil && !used && limit.id == row.limits.last?.id
                            )
                        }
                    }
                    if let note = row.note {
                        Text(note)
                            .font(.line)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    if used, let heading = row.usedMostHeading {
                        UsedMostList(heading: heading, sessions: row.usedMost)
                            .transition(appears)
                    }
                }
                .padding(.top, 10)
                .transition(appears)
            }
        }
        .padding(Row.padding)
        .rowBackground(open || hover)
        .clipShape(RoundedRectangle(cornerRadius: Row.radius, style: .continuous))
        .onHover { over in withAnimation(.easeOut(duration: 0.12)) { hover = over } }
        .onTapGesture { if row.openable { toggle() } }
        .focusable(row.openable, interactions: .activate)
        .onKeyPress(keys: [.space, .return]) { _ in
            guard row.openable else { return .ignored }
            toggle()
            return .handled
        }
        .contextMenu {
            Button("Hide \(row.called)", action: hide)
                .disabled(stopped)
        }
    }

    private func hide() {
        withAnimation(spring) {
            bridge.send(.setHidden(id: row.id, hidden: true))
        }
    }

    private func toggle() {
        withAnimation(spring) {
            bridge.send(.toggleAccount(id: row.id))
        }
    }
}

private struct LimitLine: View {
    var limit: LimitLineModel
    var last: Bool

    var body: some View {
        let name = Text(limit.name).fontWeight(.medium).foregroundStyle(.secondary)
        let ahead = limit.ahead.map { Text($0).foregroundStyle(limit.standing.color) }
        let usedUp = limit.standing == .usedUp
        let window = limit.window.map {
            Text($0).foregroundStyle(usedUp ? AnyShapeStyle(limit.standing.color) : AnyShapeStyle(.secondary))
        }
        let line: ([Text]) -> Text = { parts in
            parts.reduce(name) { said, part in
                Text("\(said)\(Text(" · ").foregroundStyle(.secondary))\(part)")
            }
        }
        let left = Text(limit.left)
            .fontWeight(.medium)
            .foregroundStyle(limit.standing.ink(lasting: .primary))
        let size = Text(limit.size.map { " \($0)" } ?? "").foregroundStyle(.secondary)

        VStack(alignment: .leading, spacing: 5) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                ViewThatFits(in: .horizontal) {
                    line([ahead, window].compactMap { $0 }).lineLimit(1)
                    line([ahead ?? window].compactMap { $0 }).lineLimit(1).truncationMode(.tail)
                }
                Spacer(minLength: 8)
                Text("\(left)\(size)")
                    .monospacedDigit()
                    .contentTransition(.numericText())
                    .fixedSize()
                    .accessibilityLabel(limit.spoken)
            }
            Bar(leftPercent: limit.leftPercent, standing: limit.standing)
        }
        .padding(.bottom, last ? Row.underBar : 0)
        .font(.small)
    }
}

struct Bar: View {
    var leftPercent: UInt8
    var standing: Standing
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.isSnapshot) private var isSnapshot
    @State private var grown = false

    var body: some View {
        let level = Double(min(leftPercent, 100)) / 100
        let shown = grown || reduceMotion || isSnapshot
        Capsule()
            .fill(.quaternary)
            .overlay(
                BarFill(fraction: shown ? level : 0, nonZero: level > 0)
                    .fill(standing.ink(lasting: .secondary))
            )
            .frame(height: 5)
            .accessibilityHidden(true)
            .onAppear {
                if !isSnapshot && !reduceMotion {
                    withAnimation(spring.delay(0.16)) { grown = true }
                }
            }
    }
}

private struct BarFill: Shape {
    var fraction: Double
    var nonZero: Bool

    var animatableData: Double {
        get { fraction }
        set { fraction = newValue }
    }

    func path(in rect: CGRect) -> Path {
        let target = rect.width * fraction
        let width = nonZero ? max(rect.height, target) : target
        guard width > 0 else { return Path() }
        return Path(
            roundedRect: CGRect(x: 0, y: 0, width: min(width, rect.width), height: rect.height),
            cornerRadius: rect.height / 2
        )
    }
}

private struct UsedMostList: View {
    var heading: String
    var sessions: [UsedMostModel]

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(heading)
                .font(.small.weight(.semibold))
                .foregroundStyle(.secondary)
            ForEach(sessions) { used in
                VStack(alignment: .leading, spacing: 1) {
                    Text(used.title).font(.line).lineLimit(1)
                    Text(used.detail)
                        .font(.small.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
    }
}

struct AccountTitle: View {
    var title: String
    var label: String?
    var titleStyle: HierarchicalShapeStyle = .primary
    var labelStyle: HierarchicalShapeStyle = .tertiary

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Text(title)
                .font(.name)
                .foregroundStyle(titleStyle)
                .lineLimit(1)
                .truncationMode(.tail)
                .layoutPriority(1)
                .help(title)
            if let label {
                Text(label)
                    .font(.small)
                    .foregroundStyle(labelStyle)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
        }
    }
}

struct AccountName: View {
    var title: String
    var label: String?
    var via: String?
    var titleStyle: HierarchicalShapeStyle = .primary
    var subtitleStyle: HierarchicalShapeStyle = .secondary

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            AccountTitle(title: title, label: label, titleStyle: titleStyle, labelStyle: .tertiary)
            if let via {
                Text(via)
                    .font(.small)
                    .foregroundStyle(subtitleStyle)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
        }
    }
}
