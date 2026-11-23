// Settings → Agents: installed agents and MCP connection switches.

import SwiftUI

struct AgentsTab: View {
    @Environment(Bridge.self) private var bridge

    var body: some View {
        let settings = bridge.panel.settings
        if let empty = settings.agentsEmpty {
            EmptyTabMessage(message: empty)
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
            VStack(alignment: .leading, spacing: 1) {
                Text(agent.name).font(.name)
                if let note = agent.note {
                    Text(note)
                        .font(.small)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
            }
            Spacer(minLength: 8)
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
