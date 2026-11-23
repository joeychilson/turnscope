// Accounts not used this week folded into one line ("2 more accounts"),
// with hidden accounts in a fold of their own at the end ("1 hidden").

import SwiftUI

struct UnusedAccounts: View {
    @Environment(Bridge.self) private var bridge

    var body: some View {
        let panel = bridge.panel
        if let label = panel.unusedFoldLabel {
            VStack(alignment: .leading, spacing: Row.spacing) {
                Fold(text: label, open: panel.unusedOpen) {
                    bridge.send(.toggleUnused)
                }
                if panel.unusedOpen {
                    VStack(spacing: Row.spacing) {
                        ForEach(panel.unused) { AccountRow(row: $0) }
                        if let hiddenLabel = panel.hiddenFoldLabel {
                            Fold(text: hiddenLabel, open: panel.hiddenShown) {
                                bridge.send(.toggleHidden)
                            }
                        }
                        if !panel.hidden.isEmpty && (panel.hiddenShown || panel.unused.isEmpty) {
                            ForEach(panel.hidden) { HiddenRow(account: $0) }
                                .transition(appears)
                        }
                    }
                    .transition(appears)
                }
            }
        }
    }
}

private struct Fold: View {
    var text: String
    var open: Bool
    var toggle: () -> Void

    var body: some View {
        Button { withAnimation(spring, toggle) } label: {
            Hover { hover in
                HStack(spacing: 8) {
                    Text(text).font(.line).foregroundStyle(.secondary)
                    Spacer(minLength: 6)
                    Image(systemName: "chevron.right")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(.tertiary)
                        .rotationEffect(.degrees(open ? 90 : 0))
                }
                .padding(Row.padding)
                .rowBackground(hover)
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(text)
        .accessibilityHint(open ? "Folds them" : "Lists them")
    }
}

private struct HiddenRow: View {
    @Environment(Bridge.self) private var bridge
    var account: HiddenAccountModel

    var body: some View {
        HStack(spacing: Row.gap) {
            Logo(provider: account.provider, size: Row.logo).foregroundStyle(.tertiary)
            AccountName(
                title: account.title,
                label: account.label,
                via: account.via,
                titleStyle: .secondary,
                subtitleStyle: .tertiary
            )
            Spacer(minLength: 8)
            Button("Show") {
                withAnimation(spring) {
                    bridge.send(.setHidden(id: account.id, hidden: false))
                }
            }
            .buttonStyle(PillButton())
            .disabled(bridge.panel.stopped)
            .accessibilityLabel("Show \(account.called)")
        }
        .padding(Row.padding)
    }
}
