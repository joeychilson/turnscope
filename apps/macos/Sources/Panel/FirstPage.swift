// The panel's first page (docs/app.md#the-panel).

import SwiftUI

struct FirstPage: View {
    @Environment(Bridge.self) private var bridge
    @Environment(UIState.self) private var ui
    @FocusState private var focus: PanelFocus?

    var body: some View {
        let panel = bridge.panel
        VStack(alignment: .leading, spacing: 0) {
            if let waiting = panel.waiting {
                Waiting(model: waiting)
            } else {
                if let verdict = panel.verdict {
                    Verdict(model: verdict)
                        .padding(.horizontal, 20)
                        .padding(.top, 18)
                        .padding(.bottom, 14)
                }
                OverflowScroll(maxHeight: max(180, bridge.screenHeight - 230)) {
                    VStack(alignment: .leading, spacing: 0) {
                        if !panel.inUse.isEmpty {
                            VStack(spacing: Row.spacing) {
                                ForEach(panel.inUse) {
                                    AccountRow(row: $0, focus: $focus)
                                }
                            }
                        }
                        if !panel.recent.isEmpty {
                            VStack(alignment: .leading, spacing: Row.spacing) {
                                SectionTitle(text: "Used this week")
                                ForEach(panel.recent) {
                                    AccountRow(row: $0, focus: $focus)
                                }
                            }
                            .padding(.top, panel.inUse.isEmpty ? 0 : Row.groups)
                        }
                        if let label = panel.unusedFoldLabel {
                            UnusedAccounts(label: label, focus: $focus)
                                .padding(.top, Row.spacing)
                        }
                        if let offer = panel.connectOffer {
                            ConnectNudge(offer: offer)
                                .padding(.top, Row.groups)
                        }
                    }
                    .padding(.horizontal, 8)
                }
            }
            Footer()
                .padding(8)
        }
        .focusable()
        .focusEffectDisabled()
        .focused($focus, equals: .page)
        .onChange(of: ui.focusNonce, initial: true) { focus = .page }
        .onKeyPress(.downArrow) { stepFocus(1, panel: panel) }
        .onKeyPress(.upArrow) { stepFocus(-1, panel: panel) }
    }

    private func focusableItems(_ panel: PanelModel) -> [PanelFocus] {
        var items: [PanelFocus] = []
        items.append(contentsOf: panel.inUse.filter(\.openable).map { .account($0.id) })
        items.append(contentsOf: panel.recent.filter(\.openable).map { .account($0.id) })
        if panel.unusedFoldLabel != nil {
            items.append(.unusedFold)
        }
        if ui.unusedOpen {
            items.append(contentsOf: panel.unused.filter(\.openable).map { .account($0.id) })
            if panel.hiddenFoldLabel != nil {
                items.append(.hiddenFold)
            }
        }
        return items
    }

    private func stepFocus(_ delta: Int, panel: PanelModel) -> KeyPress.Result {
        let items = focusableItems(panel)
        guard !items.isEmpty else { return .ignored }
        let currentFocus = (focus == .page) ? nil : focus
        let anchor = currentFocus ?? ui.openAccount.map(PanelFocus.account)
        if let current = anchor, let index = items.firstIndex(of: current) {
            let next = max(0, min(items.count - 1, index + delta))
            focus = items[next]
        } else {
            focus = delta > 0 ? items.first : items.last
        }
        return .handled
    }
}

struct Waiting: View {
    var model: WaitingModel

    var body: some View {
        if let problem = model.problem {
            Message(
                headline: "Turnscope can't read its data",
                detail: problem,
                footnote: model.retries ? "It tries again on its own." : nil
            )
        } else {
            VStack(spacing: 8) {
                Spinner()
                Text(model.reading)
                    .font(.line)
                    .foregroundStyle(.secondary)
            }
            .padding(24)
            .frame(maxWidth: .infinity, minHeight: 140)
        }
    }
}

private struct Verdict: View {
    var model: VerdictModel

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(model.headline)
                .font(.system(size: 20, weight: .semibold))
                .foregroundStyle(model.standing.color)
                .fixedSize(horizontal: false, vertical: true)
                .contentTransition(.opacity)
                .accessibilityAddTraits(.isHeader)
            if !model.detail.isEmpty {
                Text(model.detail)
                    .font(.name)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .textSelection(.enabled)
    }
}

private struct ConnectNudge: View {
    @Environment(Bridge.self) private var bridge
    var offer: ConnectOfferModel

    var body: some View {
        HStack(spacing: Row.gap) {
            Logo(agent: offer.id, size: Row.logo)
            AgentLabel(name: offer.name, note: offer.note, configured: offer.configured)
            if offer.changing {
                Spinner()
                    .accessibilityElement()
                    .accessibilityLabel("Connecting \(offer.name)")
            } else {
                Button(offer.verb) {
                    withAnimation(spring) {
                        bridge.send(.setConnected(id: offer.id, connecting: true))
                    }
                }
                .buttonStyle(PillButton())
                .disabled(bridge.panel.stopped)
                .accessibilityLabel("\(offer.verb) \(offer.name)")
                .help(offer.help)
            }
            Button {
                withAnimation(spring) {
                    bridge.send(.dismissConnect(id: offer.id))
                }
            } label: {
                Image(systemName: "xmark")
            }
            .buttonStyle(RoundButton())
            .help("Don't suggest this again")
            .accessibilityLabel("Don't suggest connecting \(offer.name) again")
        }
        .padding(Row.padding)
        .transition(appears)
    }
}

private struct Footer: View {
    @Environment(Bridge.self) private var bridge
    @Environment(UIState.self) private var ui
    @Environment(Updater.self) private var updater
    @Environment(\.closePanel) private var closePanel

    var body: some View {
        HStack(spacing: 2) {
            if let note = bridge.panel.footerNote {
                HStack(spacing: 6) {
                    if note.busy {
                        Spinner()
                    }
                    Text(note.text)
                        .font(.small)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                .help(note.help ?? "")
                .accessibilityElement(children: .combine)
                .accessibilityLabel(note.spoken)
                .padding(.leading, 12)
                .id(note.text)
                .transition(appears)
            }
            Spacer()
            Button("Close", action: closePanel)
                .keyboardShortcut(.cancelAction)
                .hiddenShortcut()
            if bridge.panel.canRefresh {
                Button("Refresh Limits") {
                    withAnimation(spring) {
                        bridge.send(.refreshNow)
                    }
                }
                .keyboardShortcut("r", modifiers: .command)
                .hiddenShortcut()
            }
            if updater.waiting {
                Button("Update available") {
                    closePanel()
                    updater.check()
                }
                .buttonStyle(PillButton())
                .help("Shows what's new, and asks before installing it")
                .padding(.trailing, 4)
                .transition(appears)
            }
            Button { ui.go(.settings) } label: { Image(systemName: "gearshape") }
                .buttonStyle(IconButton())
                .keyboardShortcut(",", modifiers: .command)
                .help("Settings")
                .accessibilityLabel("Settings")
            Button { NSApp.terminate(nil) } label: { Image(systemName: "power") }
                .buttonStyle(IconButton())
                .help("Quit Turnscope")
                .accessibilityLabel("Quit Turnscope")
        }
    }
}
