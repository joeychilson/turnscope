// The panel's first page (docs/app.md#the-panel).

import SwiftUI

struct FirstPage: View {
    @Environment(Bridge.self) private var bridge

    var body: some View {
        let panel = bridge.panel
        VStack(alignment: .leading, spacing: 0) {
            if !panel.hasStatus {
                Waiting(model: panel.waiting)
            } else {
                if let verdict = panel.verdict {
                    Verdict(model: verdict)
                        .padding(.horizontal, 20)
                        .padding(.top, 18)
                        .padding(.bottom, 14)
                }
                VStack(alignment: .leading, spacing: 0) {
                    if !panel.inUse.isEmpty {
                        VStack(spacing: Row.spacing) {
                            ForEach(panel.inUse) { AccountRow(row: $0) }
                        }
                    }
                    if !panel.recent.isEmpty {
                        VStack(alignment: .leading, spacing: Row.spacing) {
                            SectionTitle(text: "Used this week")
                            ForEach(panel.recent) { AccountRow(row: $0) }
                        }
                        .padding(.top, panel.inUse.isEmpty ? 0 : Row.groups)
                    }
                    if panel.unusedFoldLabel != nil {
                        UnusedAccounts()
                            .padding(.top, Row.spacing)
                    }
                    if let offer = panel.connectOffer {
                        ConnectNudge(offer: offer)
                            .padding(.top, Row.groups)
                    }
                }
                .padding(.horizontal, 8)
            }
            Footer()
                .padding(8)
        }
    }
}

struct Waiting: View {
    var model: WaitingModel?

    var body: some View {
        VStack(spacing: 8) {
            if let problem = model?.problem {
                Text("Turnscope can't read its data")
                    .font(.system(size: 13, weight: .semibold))
                Text(problem)
                    .font(.line)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                if model?.retries == true {
                    Text("It tries again on its own.")
                        .font(.small)
                        .foregroundStyle(.tertiary)
                }
            } else {
                Spinner()
                Text("Reading your agents' history…")
                    .font(.line)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(24)
        .frame(maxWidth: .infinity, minHeight: 140)
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
            VStack(alignment: .leading, spacing: 1) {
                Text(offer.name).font(.name)
                if let note = offer.note {
                    Text(note)
                        .font(.small)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
            }
            Spacer(minLength: 8)
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
    @Environment(Updater.self) private var updater
    @Environment(\.closePanel) private var closePanel

    var body: some View {
        HStack(spacing: 2) {
            if let note = bridge.panel.footerNote {
                switch note {
                case let .stale(text, help, spoken):
                    Text(text)
                        .font(.small)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .help(help)
                        .accessibilityLabel(spoken)
                        .padding(.leading, 12)
                        .transition(appears)
                case let .reading(text, help, spoken):
                    HStack(spacing: 6) {
                        Spinner()
                        Text(text)
                            .font(.small)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.tail)
                    }
                    .help(help)
                    .accessibilityElement(children: .combine)
                    .accessibilityLabel(spoken)
                    .padding(.leading, 12)
                    .transition(appears)
                }
            }
            Spacer()
            Button("Close", action: closePanel)
                .keyboardShortcut(.cancelAction)
                .frame(width: 0, height: 0)
                .opacity(0)
                .accessibilityHidden(true)
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
            Button { bridge.go(.settings) } label: { Image(systemName: "gearshape") }
                .buttonStyle(IconButton())
                .keyboardShortcut(",", modifiers: .command)
                .help("Settings")
                .accessibilityLabel("Settings")
            Button { NSApp.terminate(nil) } label: { Image(systemName: "power") }
                .buttonStyle(IconButton())
                .keyboardShortcut("q", modifiers: .command)
                .help("Quit Turnscope")
                .accessibilityLabel("Quit Turnscope")
        }
    }
}
