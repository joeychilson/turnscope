import SwiftUI

struct AccountsTab: View {
    @Environment(Bridge.self) private var bridge

    var body: some View {
        let settings = bridge.panel.settings
        // With no account, the headline says why, as the panel's does.
        if settings.subscriptions.isEmpty && settings.apiKeys.isEmpty, let verdict = bridge.panel.verdict {
            Message(headline: verdict.headline, detail: verdict.detail)
        } else {
            VStack(alignment: .leading, spacing: 14) {
                if !settings.subscriptions.isEmpty {
                    SettingGroup(title: "Subscriptions") {
                        ForEach(settings.subscriptions) { AccountSwitch(account: $0) }
                    }
                }
                if !settings.apiKeys.isEmpty {
                    SettingGroup(title: "API keys") {
                        ForEach(settings.apiKeys) { AccountSwitch(account: $0) }
                    }
                }
            }
        }
    }
}

private struct AccountSwitch: View {
    @Environment(Bridge.self) private var bridge
    @Environment(UIState.self) private var ui
    var account: AccountModel

    var body: some View {
        HStack(spacing: Row.gap) {
            Logo(provider: account.provider, size: Row.logo)
            AccountName(title: account.title, label: account.label, via: account.via)
                .frame(maxWidth: .infinity, alignment: .leading)
            Switch(
                label: "Show \(account.called)",
                on: Binding(
                    get: { account.shown },
                    set: { shown in
                        withAnimation(spring) {
                            if !shown { ui.didHide(account.id) }
                            bridge.send(.setHidden(id: account.id, hidden: !shown))
                        }
                    }
                )
            )
            .disabled(bridge.panel.stopped)
        }
        .padding(Row.padding)
        .frame(minHeight: Row.setting)
    }
}
