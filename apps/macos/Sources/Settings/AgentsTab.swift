// Settings → Agents: installed agents and MCP connection switches.

import SwiftUI

struct AgentsTab: View {
    @Environment(Bridge.self) private var bridge

    var body: some View {
        let settings = bridge.panel.settings
        if let empty = settings.agentsEmpty {
            EmptyTabMessage(message: empty)
        } else {
            let extraAgents = settings.agents.filter { !$0.snippets.isEmpty }
            VStack(alignment: .leading, spacing: 14) {
                SettingGroup(title: settings.agentsTitle) {
                    ForEach(settings.agents) { AgentRow(agent: $0) }
                }
                ForEach(extraAgents) { agent in
                    AgentSnippets(agent: agent)
                }
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
                        .help(agent.configured.map { "Configured to run \($0)" } ?? "")
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
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

private struct AgentSnippets: View {
    @Environment(UIState.self) private var ui
    var agent: AgentRowModel
    @State private var hover = false

    var body: some View {
        let open = ui.openSnippets.contains(agent.id)
        VStack(alignment: .leading, spacing: Row.spacing) {
            Button { ui.toggleSnippets(agent.id) } label: {
                HStack(spacing: 8) {
                    Text("\(agent.name) status line & guard")
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
            .buttonStyle(RowSurfaceButtonStyle(hover: hover))
            .onHover { over in withAnimation(.easeOut(duration: 0.10)) { hover = over } }
            .accessibilityHint(open ? "Folds them" : "Lists copyable JSON snippets")

            if open {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(agent.snippets) { snippet in
                        SnippetRow(snippet: snippet)
                    }
                    if let file = agent.snippets.first?.file {
                        Text("Paste into \(file). Turnscope never edits it.")
                            .font(.small)
                            .foregroundStyle(.tertiary)
                            .padding(.horizontal, 12)
                            .padding(.top, 2)
                            .padding(.bottom, 6)
                    }
                }
                .padding(.vertical, 3)
                .background(
                    RoundedRectangle(cornerRadius: Row.radius, style: .continuous)
                        .fill(Color.primary.opacity(0.05))
                )
                .transition(appears)
            }
        }
    }
}

private struct SnippetRow: View {
    var snippet: SnippetModel
    @State private var copied = false
    @State private var clearCopy: Task<Void, Never>?

    var body: some View {
        HStack(spacing: Row.gap) {
            VStack(alignment: .leading, spacing: 1) {
                Text(snippet.title).font(.name)
                Text(snippet.detail)
                    .font(.small)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            Button(copied ? "Copied" : "Copy JSON") {
                copyToPasteboard(snippet.json)
                clearCopy?.cancel()
                withAnimation(.easeOut(duration: 0.12)) { copied = true }
                clearCopy = Task { @MainActor in
                    try? await Task.sleep(for: .seconds(1.5))
                    guard !Task.isCancelled else { return }
                    withAnimation(.easeOut(duration: 0.16)) { copied = false }
                }
            }
            .buttonStyle(PillButton())
            .help("Copies JSON snippet for \(snippet.file)")
            .accessibilityLabel("Copy \(snippet.title) JSON")
        }
        .padding(Row.padding)
        .frame(minHeight: Row.setting)
    }
}
