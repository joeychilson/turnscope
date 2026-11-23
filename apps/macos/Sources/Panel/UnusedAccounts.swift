// Accounts not used this week folded into one line ("2 more accounts"),
// with hidden accounts in a fold of their own at the end ("1 hidden").

import SwiftUI

struct UnusedAccounts: View {
    @Environment(Bridge.self) private var bridge
    @Environment(UIState.self) private var ui
    var focus: FocusState<PanelFocus?>.Binding

    var body: some View {
        let panel = bridge.panel
        let openAccount = ui.openAccount
        if let label = panel.unusedFoldLabel {
            VStack(alignment: .leading, spacing: Row.spacing) {
                Fold(text: label, open: ui.unusedOpen, focus: focus, target: .unusedFold) {
                    ui.toggleUnused(panel: panel)
                }
                if ui.unusedOpen {
                    VStack(spacing: Row.spacing) {
                        ForEach(panel.unused) {
                            AccountRow(row: $0, isOpen: openAccount == $0.id, focus: focus)
                        }
                        if let hiddenLabel = panel.hiddenFoldLabel {
                            Fold(text: hiddenLabel, open: ui.hiddenShown, focus: focus, target: .hiddenFold) {
                                ui.toggleHidden()
                            }
                        }
                        if !panel.hidden.isEmpty && (ui.hiddenShown || panel.unused.isEmpty) {
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
    var focus: FocusState<PanelFocus?>.Binding
    var target: PanelFocus
    var toggle: () -> Void
    @State private var hover = false

    var body: some View {
        let focused = focus.wrappedValue == target
        Button(action: toggle) {
            HStack(spacing: 8) {
                Text(text)
                    .font(.line)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                Image(systemName: "chevron.right")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(.tertiary)
                    .rotationEffect(.degrees(open ? 90 : 0))
            }
            .padding(Row.padding)
        }
        .buttonStyle(RowSurfaceButtonStyle(hover: hover, focused: focused))
        .onHover { over in withAnimation(.easeOut(duration: 0.10)) { hover = over } }
        .focused(focus, equals: target)
        .onKeyPress(.return) {
            toggle()
            return .handled
        }
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
            .frame(maxWidth: .infinity, alignment: .leading)
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
