// Pieces shared by account and agent rows, in the panel and in Settings: an
// account's title, its name with the agents that use it, its Hide button, and
// an agent's Connect button.

import SwiftUI
import TurnscopeKit

/// Connects `agent`, or points it back at this copy, depending on its
/// status. Its help says what went wrong if connecting failed. A spinner
/// shows while it works.
struct ConnectButton: View {
    @Environment(Store.self) private var store
    var agent: AgentStatus

    var body: some View {
        if store.changingAgents.contains(agent.id) {
            ProgressView().controlSize(.mini)
        } else {
            let outdated = agent.connection == .outdated
            Button(outdated ? "Update" : "Connect") { store.connect(agent.id) }
                .buttonStyle(PillButton())
                .accessibilityLabel("\(outdated ? "Update" : "Connect") \(agent.name)")
                .help(store.agentErrors[agent.id] ?? (outdated
                    ? "Points \(agent.name) at this copy of Turnscope"
                    : "Adds Turnscope to \(agent.name)'s MCP servers, so it can pace itself"))
        }
    }
}

/// An account's title as every row shows it, in the gray `title`. If
/// `labelled`, its label is beside it, to tell it apart from another account
/// with the same name. The label is cut short in the middle before the title
/// is, so a row never grows wider than the panel.
struct AccountTitle: View {
    var account: AccountStatus
    var labelled: Bool
    var title: HierarchicalShapeStyle = .primary

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Text(account.title).font(.name).foregroundStyle(title)
                .lineLimit(1).truncationMode(.tail)
                .layoutPriority(1)
                .help(account.title)
            if labelled, let label = account.label {
                Text(label).font(.small).foregroundStyle(.tertiary)
                    .lineLimit(1).truncationMode(.middle)
            }
        }
    }
}

/// An account where it's listed to show or hide: its title and label, and
/// under them the agents that use it, in the grays `title` and `subtitle`.
struct AccountName: View {
    @Environment(Store.self) private var store
    var account: AccountStatus
    var title: HierarchicalShapeStyle = .primary
    var subtitle: HierarchicalShapeStyle = .secondary

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            AccountTitle(account: account, labelled: true, title: title)
            if let via = store.words().via(account) {
                Text(via).font(.small).foregroundStyle(subtitle)
                    .lineLimit(1).truncationMode(.tail)
            }
        }
    }
}

/// A button on an account's row that hides the account. It's sized to a
/// name's line and the row always keeps room for it. It shows only while
/// `shown`, so nothing moves when it appears.
struct HideButton: View {
    @Environment(Store.self) private var store
    var account: AccountStatus
    var shown: Bool

    var body: some View {
        Button {
            withAnimation(spring) { store.setHidden(true, for: account.id) }
        } label: {
            Image(systemName: "eye.slash")
        }
        .buttonStyle(IconButton(width: 22, height: 16))
        .opacity(shown ? 1 : 0)
        .allowsHitTesting(shown)
        .accessibilityHidden(!shown)
        .help("Hide \(account.title) from the menu bar and the panel")
        .accessibilityLabel("Hide \(account.title)")
    }
}
