import AppKit
import SwiftUI

enum PanelFocus: Hashable {
    case page
    case account(String)
    case unusedFold
    case hiddenFold
}

struct AccountRow: View {
    @Environment(Bridge.self) private var bridge
    @Environment(UIState.self) private var ui
    var row: AccountRowModel
    var focus: FocusState<PanelFocus?>.Binding
    @State private var hover = false

    var body: some View {
        let open = row.openable && ui.openAccount == row.id
        let lines = open && !row.limits.isEmpty
        let used = open && !row.usedMost.isEmpty
        let stopped = bridge.panel.stopped
        let focused = focus.wrappedValue == .account(row.id)

        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .center, spacing: Row.gap) {
                Logo(provider: row.provider, size: Row.logo)
                    .allowsHitTesting(false)
                    .accessibilityHidden(row.openable)
                VStack(alignment: .leading, spacing: 1) {
                    HStack(alignment: .center, spacing: 6) {
                        AccountTitle(title: row.title, label: row.label, labelStyle: .secondary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .allowsHitTesting(false)
                            .accessibilityHidden(row.openable)
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
                            let left = Text(row.figure).foregroundStyle(row.standing.color)
                            let then = Text(row.figureThen.map { " \($0)" } ?? "")
                                .font(.name)
                                .foregroundStyle(.secondary)
                            Text("\(left)\(then)")
                                .font(.figure)
                                .contentTransition(.numericText())
                                .fixedSize()
                                .transition(quickFade)
                                .allowsHitTesting(false)
                                .accessibilityHidden(row.openable || row.figureSpoken.isEmpty)
                                .accessibilityLabel(row.figureSpoken)
                        }
                    }
                    if !lines {
                        Text(row.status)
                            .font(.small)
                            .foregroundStyle(Urgency(standing: row.standing, lasting: .secondary))
                            .fixedSize(horizontal: false, vertical: true)
                            .transition(quickFade)
                            .allowsHitTesting(false)
                            .accessibilityHidden(row.openable)
                    }
                }
            }

            if open {
                VStack(alignment: .leading, spacing: 10) {
                    if lines {
                        ForEach(row.limits) { limit in
                            LimitLine(
                                limit: limit,
                                last: row.note == nil && !used && limit.id == row.limits.last?.id
                            )
                        }
                        .allowsHitTesting(false)
                    }
                    if let note = row.note {
                        Text(note)
                            .font(.line)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                            .allowsHitTesting(false)
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
        .background {
            // A button behind the whole padded row, so all of it opens the row.
            if row.openable {
                Button(action: toggle) {
                    Color.clear
                }
                .buttonStyle(RowSurfaceButtonStyle(open: open, hover: hover, focused: focused))
                .focused(focus, equals: .account(row.id))
                .onKeyPress(.return) {
                    toggle()
                    return .handled
                }
                .accessibilityLabel(lines ? row.called : row.collapsedSpoken)
                .accessibilityHint(open ? "Closes it" : "Opens it")
                .accessibilityAction(named: "Hide") { if !stopped { hide() } }
                .help(row.label != nil ? row.called : "")
            } else {
                RowSurface(active: hover)
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: Row.radius, style: .continuous))
        .onHover { over in withAnimation(.easeOut(duration: 0.10)) { hover = over } }
        .contextMenu {
            Button("Hide \(row.called)", action: hide)
                .disabled(stopped)
        }
    }

    private func hide() {
        withAnimation(spring) {
            ui.didHide(row.id)
            bridge.send(.setHidden(id: row.id, hidden: true))
        }
    }

    private func toggle() {
        ui.toggleAccount(row.id)
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
            .foregroundStyle(limit.standing.color)
        let size = Text(limit.size.map { " \($0)" } ?? "").foregroundStyle(.secondary)

        VStack(alignment: .leading, spacing: 5) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                ViewThatFits(in: .horizontal) {
                    line([ahead, window].compactMap { $0 }).lineLimit(1)
                    line([ahead ?? window].compactMap { $0 }).lineLimit(1)
                    line([ahead ?? window].compactMap { $0 }).fixedSize(horizontal: false, vertical: true)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                Text("\(left)\(size)")
                    .monospacedDigit()
                    .contentTransition(.numericText())
                    .fixedSize()
                    .accessibilityLabel(limit.spoken)
            }
            Bar(
                leftPercent: limit.leftPercent,
                timeLeftFraction: limit.timeLeftFraction,
                standing: limit.standing
            )
        }
        .padding(.bottom, last ? Row.underBar : 0)
        .font(.small)
    }
}

struct Bar: View {
    var leftPercent: UInt8
    var timeLeftFraction: Double?
    var standing: Standing
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.colorSchemeContrast) private var contrast
    @Environment(\.isSnapshot) private var isSnapshot
    @State private var grown = false

    var body: some View {
        let level = Double(min(leftPercent, 100)) / 100
        let shown = grown || reduceMotion || isSnapshot
        Capsule()
            .fill(.quaternary)
            .overlay(
                BarFill(fraction: shown ? level : 0, nonZero: level > 0)
                    .fill(Urgency(standing: standing, lasting: .secondary))
            )
            .overlay {
                if let tick = timeLeftFraction {
                    BarTick(fraction: tick)
                        .fill(Color.primary.opacity(contrast == .increased ? 0.55 : 0.36))
                }
            }
            .frame(height: 5)
            .accessibilityHidden(true)
            .onAppear {
                if !isSnapshot && !reduceMotion {
                    withAnimation(spring.delay(0.08)) { grown = true }
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

private struct BarTick: Shape {
    var fraction: Double

    func path(in rect: CGRect) -> Path {
        let width: CGFloat = 1.5
        let x = max(0, min(rect.width - width, rect.width * fraction - width / 2))
        return Path(
            roundedRect: CGRect(x: x, y: -1.5, width: width, height: rect.height + 3),
            cornerRadius: width / 2
        )
    }
}

private struct UsedMostList: View {
    var heading: String
    var sessions: [UsedMostModel]

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(heading)
                .font(.small.weight(.semibold))
                .foregroundStyle(.secondary)
                .padding(.bottom, 2)
            ForEach(sessions) { used in
                UsedMostRow(used: used)
            }
        }
    }
}

private struct UsedMostRow: View {
    @Environment(\.closePanel) private var closePanel
    var used: UsedMostModel
    @State private var hover = false
    @State private var copied = false
    @State private var copies = 0

    var body: some View {
        Button(action: copyPrimary) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                VStack(alignment: .leading, spacing: 1) {
                    Text(used.title).font(.line).lineLimit(1)
                    Text(used.detail)
                        .font(.small.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                if copied {
                    Text("Copied")
                        .font(.small)
                        .foregroundStyle(.secondary)
                        .transition(.opacity)
                } else if hover {
                    Image(systemName: "doc.on.doc")
                        .font(.system(size: 10))
                        .foregroundStyle(.tertiary)
                        .transition(.opacity)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 6)
            .padding(.vertical, 3)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(hover ? Color.primary.opacity(0.06) : .clear)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.horizontal, -6)
        .onHover { over in withAnimation(.easeOut(duration: 0.12)) { hover = over } }
        .help(used.projectPath != nil ? "Click to copy project path" : "Click to copy session title")
        .contextMenu {
            if let path = used.projectPath {
                if FileManager.default.fileExists(atPath: path) {
                    Button("Open Project in Finder") {
                        closePanel()
                        NSWorkspace.shared.open(URL(fileURLWithPath: path))
                    }
                }
                Button("Copy Project Path") { copy(path) }
            }
            Button("Copy Session ID") { copy(used.sessionId) }
        }
        .flashesCopied($copied, copies: copies)
    }

    private func copyPrimary() {
        copy(used.projectPath ?? used.title)
    }

    private func copy(_ text: String) {
        copyToPasteboard(text)
        copies += 1
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
                .layoutPriority(1)
                .help(title)
            if let label {
                Text(label)
                    .font(.small)
                    .foregroundStyle(labelStyle)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help(label)
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
            }
        }
    }
}
