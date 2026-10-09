// Settings → Agents: each installed agent that can use Turnscope's MCP server,
// with one button: Connect, Update or Disconnect.

import SwiftUI
import TurnscopeKit

struct AgentsTab: View {
    @Environment(Store.self) private var store

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if store.agents.isEmpty {
                Text("No agent Turnscope serves is installed.")
                    .font(.line).foregroundStyle(.secondary).padding(.horizontal, 12)
            }
            ForEach(store.agents) { AgentRow(agent: $0) }
        }
    }
}

/// An agent, in a row like an account's. It shows the agent's name, and
/// under it whether it has Turnscope (connected, not connected, or pointed
/// at another copy) or why connecting or disconnecting failed. On the right
/// is the one thing to do: Connect, Update or Disconnect.
private struct AgentRow: View {
    @Environment(Store.self) private var store
    var agent: AgentStatus

    var body: some View {
        HStack(spacing: Row.gap) {
            Logo(agent: agent, size: Row.logo)
            VStack(alignment: .leading, spacing: 1) {
                Text(agent.name).font(.name)
                if let error = store.agentErrors[agent.id] {
                    Text(error).font(.small).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                } else {
                    Text(store.words().connection(agent)).font(.small).foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: 8)
            if agent.connection != .connected {
                ConnectButton(agent: agent)
            } else if store.changingAgents.contains(agent.id) {
                ProgressView().controlSize(.mini)
            } else {
                Button("Disconnect") { store.disconnect(agent.id) }
                    .buttonStyle(PillButton())
                    .accessibilityLabel("Disconnect \(agent.name)")
                    .help("Removes Turnscope from \(agent.name)'s MCP servers")
            }
        }
        .padding(Row.padding)
    }
}
