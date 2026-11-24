import SwiftUI

struct AgentsTab: View {
    @Environment(Bridge.self) private var bridge

    var body: some View {
        let settings = bridge.panel.settings
        if let empty = settings.agentsEmpty {
            Message(headline: empty.headline, detail: empty.detail)
        } else {
            SettingGroup(title: settings.agentsTitle) {
                ForEach(settings.agents) { AgentRow(agent: $0) }
            }
        }
    }
}

private struct AgentRow: View {
    @Environment(Bridge.self) private var bridge
    var agent: AgentRowModel

    var body: some View {
        HStack(spacing: Row.gap) {
            Logo(agent: agent.id, size: Row.logo)
            AgentLabel(name: agent.name, note: agent.note, configured: agent.configured)
            if let changing = agent.changing {
                Text(changing)
                    .font(.small)
                    .foregroundStyle(.secondary)
                    .transition(appears)
            }
            Switch(
                label: "Connect \(agent.name)",
                on: Binding(
                    get: { agent.connected },
                    set: { on in
                        withAnimation(spring) {
                            bridge.send(.setConnected(id: agent.id, connecting: on))
                        }
                    }
                )
            )
            .disabled(!agent.enabled)
        }
        .padding(Row.padding)
        .frame(minHeight: Row.setting)
    }
}

/// An agent's name, and why it isn't connected, with the path it's configured to run on hover.
struct AgentLabel: View {
    var name: String
    var note: String?
    var configured: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            Text(name).font(.name)
            if let note {
                Text(note)
                    .font(.small)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                    .help(configured.map { "Configured to run \($0)" } ?? "")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
