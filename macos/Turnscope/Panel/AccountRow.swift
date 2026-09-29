// An account in use: one compact row, its logo, name and what is left of the
// limit that matters, over a line saying how it stands and its bar. Opened in
// place, it shows every limit with its sentence, and what used it: the
// sessions that took most, and one piece of advice. The only account in use
// shows its limits without being opened.

import SwiftUI
import TurnscopeKit

struct AccountRow: View {
    @Environment(Store.self) private var store
    var account: Account
    var open: Bool
    /// The only account in use: its limits show unopened.
    var alone: Bool
    /// Another account in use is called the same, so its label tells them apart.
    var twin: Bool
    var toggle: () -> Void

    var body: some View {
        let words = Words()
        let deciding = account.decidingLimit
        let details = alone || open
        Button(action: toggle) {
            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .center, spacing: 8) {
                    Logo(account: account)
                    VStack(alignment: .leading, spacing: 2) {
                        HStack(alignment: .firstTextBaseline, spacing: 6) {
                            Text(account.title).font(.system(size: 13, weight: .semibold)).fixedSize()
                            if twin, let label = account.label {
                                Text(label).font(.system(size: 12)).foregroundStyle(.secondary)
                                    .lineLimit(1).truncationMode(.middle)
                            }
                        }
                        if !details, let deciding {
                            Text(words.short(deciding))
                                .font(.system(size: 12))
                                .foregroundStyle(deciding.standing == .lasts ? Color.secondary : deciding.standing.color)
                                .lineLimit(1)
                                .transition(.opacity)
                        }
                    }
                    Spacer(minLength: 8)
                    Text(words.percent(deciding?.left))
                        .font(.system(size: 20, weight: .semibold, design: .rounded).monospacedDigit())
                        .foregroundStyle(deciding?.standing.color ?? .primary)
                        .contentTransition(.numericText())
                        .fixedSize()
                }
                if details {
                    LimitRows(limits: account.limits).transition(appears)
                } else if let deciding {
                    Level(limit: deciding).padding(.leading, 24)
                }
                if let trouble = words.trouble(account) {
                    Text(trouble)
                        .font(.system(size: 12)).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.leading, 24)
                }
                if open {
                    UsedMost(account: account).transition(appears)
                }
            }
            .padding(12)
            .rowBackground(on: open, radius: 12)
        }
        .buttonStyle(.plain)
        .contextMenu {
            Button("Hide \(account.title)") { withAnimation(spring) { store.setHidden(account.id, true) } }
        }
        .accessibilityElement(children: .combine)
        .accessibilityHint(open ? "Closes what used it" : "Opens what used it")
        .accessibilityAction(named: "Hide") { store.setHidden(account.id, true) }
    }
}

/// Every limit: its name, its bar and what is left, and its sentence under
/// the bar. The names take the room the longest needs, "Opus week", so every
/// bar starts at one edge.
struct LimitRows: View {
    var limits: [Limit]

    var body: some View {
        let words = Words()
        VStack(alignment: .leading, spacing: 9) {
            ForEach(limits) { limit in
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    // Every name, only this one seen, so each is as wide as
                    // the widest.
                    ZStack(alignment: .leading) {
                        ForEach(limits) { other in
                            Text(words.name(other)).opacity(other.id == limit.id ? 1 : 0)
                                .accessibilityHidden(other.id != limit.id)
                        }
                    }
                    .font(.system(size: 11, weight: .medium)).foregroundStyle(.secondary)
                    .fixedSize()
                    .frame(minWidth: 44, alignment: .leading)
                    VStack(alignment: .leading, spacing: 5) {
                        HStack(spacing: 6) {
                            Level(limit: limit)
                            Text(words.percent(limit.left))
                                .font(.system(size: 11, weight: .medium).monospacedDigit())
                                .foregroundStyle(.secondary)
                                .fixedSize()
                                .frame(minWidth: 34, alignment: .trailing)
                        }
                        Text(words.sentence(limit))
                            .font(.system(size: 12))
                            .foregroundStyle(limit.standing == .lasts ? Color.secondary : limit.standing.color)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
    }
}

/// A level, as a battery shows one: what is left now, and within it, solid,
/// what will still be left when it resets at this pace.
struct Level: View {
    var limit: Limit
    @State private var grown = false

    var body: some View {
        let level = (limit.left ?? 0) / 100
        let atReset = limit.standing == .lasts ? min(level, (limit.leftAtReset ?? limit.left ?? 0) / 100) : 0
        GeometryReader { geometry in
            let width = geometry.size.width
            ZStack(alignment: .leading) {
                Capsule().fill(.quaternary)
                Capsule()
                    .fill(limit.standing == .lasts ? AnyShapeStyle(.tertiary) : AnyShapeStyle(limit.standing.color.opacity(0.9)))
                    .frame(width: max(level > 0 ? 6 : 0, width * (grown ? level : 0)))
                if limit.standing == .lasts {
                    Capsule()
                        .fill(.secondary)
                        .frame(width: max(atReset > 0 ? 6 : 0, width * (grown ? atReset : 0)))
                }
            }
        }
        .frame(height: 6)
        .accessibilityHidden(true)
        .onAppear { withAnimation(.spring(response: 0.6, dampingFraction: 0.8).delay(0.05)) { grown = true } }
    }
}

/// What used the limit that matters, and one thing to do about it.
private struct UsedMost: View {
    var account: Account

    var body: some View {
        let words = Words()
        VStack(alignment: .leading, spacing: 8) {
            if !account.usedMost.isEmpty {
                Text("Used most \(account.decidingLimit.map(words.of) ?? "")")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(.secondary)
                    .padding(.top, 4)
                ForEach(account.usedMost) { used in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        VStack(alignment: .leading, spacing: 1) {
                            HStack(spacing: 5) {
                                Text(used.title ?? "Untitled session").font(.system(size: 12)).lineLimit(1)
                                if used.active {
                                    Circle().fill(.primary).frame(width: 5, height: 5)
                                        .accessibilityLabel("Active now")
                                }
                            }
                            Text([used.project, words.agentName(used.agent)].compactMap { $0 }.joined(separator: " · "))
                                .font(.system(size: 11)).foregroundStyle(.secondary)
                        }
                        Spacer(minLength: 8)
                        Text(String(format: "%.1f%%", used.share))
                            .font(.system(size: 12).monospacedDigit()).foregroundStyle(.secondary)
                    }
                }
            }
            if let advice = account.advice {
                let said = words.advice(advice)
                HStack(spacing: 6) {
                    Image(systemName: "lightbulb").font(.system(size: 11)).accessibilityHidden(true)
                    Text(said.short).font(.system(size: 12)).lineLimit(1)
                }
                .foregroundStyle(.secondary)
                .padding(.top, 2)
                .help(said.whole)
                .accessibilityLabel(said.whole)
            }
        }
    }
}
