// Settings → Agents: each installed agent that can use Turnscope's MCP server,
// with a switch that connects or disconnects it.

import SwiftUI
import TurnscopeKit

struct AgentsTab: View {
    @Environment(Store.self) private var store

    var body: some View {
        if store.agents.isEmpty {
            Text("No agent Turnscope serves is installed.")
                .font(.line).foregroundStyle(.secondary).padding(.horizontal, 12)
        } else {
            SettingGroup(title: "Let these agents check their limits") {
                ForEach(store.agents) { AgentRow(agent: $0) }
            }
        }
    }
}

/// An agent, in a row like an account's: its logo, its name, and a switch
/// that connects it. A switch says on or off by itself, so the line under
/// the name shows only what it can't: that the agent uses another copy of
/// Turnscope, which turning it on fixes, or why connecting or disconnecting
/// failed.
private struct AgentRow: View {
    @Environment(Store.self) private var store
    var agent: AgentStatus

    var body: some View {
        let changing = store.changingAgents[agent.id]
        let note = store.agentErrors[agent.id] ?? store.words().connection(agent)
        HStack(spacing: Row.gap) {
            Logo(agent: agent, size: Row.logo)
            VStack(alignment: .leading, spacing: 1) {
                Text(agent.name).font(.name)
                if let note {
                    Text(note).font(.small).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                }
            }
            Spacer(minLength: 8)
            // Connecting runs the agent's own command, which can take a
            // while. Meanwhile the switch shows where it's going, and what's
            // happening is said beside it rather than under the name, so the
            // row doesn't grow and shrink.
            if let changing {
                Text(changing == .connected ? "Connecting…" : "Disconnecting…")
                    .font(.small).foregroundStyle(.secondary)
                    .transition(appears)
            }
            Switch(label: "Connect \(agent.name)",
                   on: Binding(get: { (changing ?? agent.connection) == .connected },
                               set: { on in
                                   withAnimation(spring) { on ? store.connect(agent.id) : store.disconnect(agent.id) }
                               }))
                .disabled(changing != nil)
        }
        .padding(Row.padding)
        .frame(minHeight: Row.setting)
    }
}
