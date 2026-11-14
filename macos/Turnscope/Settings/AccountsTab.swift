// Settings → Accounts: every account found, subscriptions first and API keys
// after, each with a switch to show or hide it.

import SwiftUI
import TurnscopeKit

/// Every account found, with subscriptions and API keys in separate groups.
/// Each has a switch to leave it out of the menu bar and the panel.
struct AccountsTab: View {
    @Environment(Store.self) private var store

    var body: some View {
        if store.all.isEmpty {
            // With no account to list, what the panel's headline says, as
            // "Getting your limits" on a first launch, over an empty tab.
            let said = store.verdict(in: store.words())
            VStack(spacing: 8) {
                Text(said.headline).font(.system(size: 13, weight: .semibold))
                Text(said.detail).font(.line).foregroundStyle(.secondary)
                    .multilineTextAlignment(.center).fixedSize(horizontal: false, vertical: true)
            }
            .padding(24)
            .frame(maxWidth: .infinity, minHeight: 140)
        } else {
            VStack(alignment: .leading, spacing: 14) {
                group("Subscriptions", store.all.filter { !$0.apiKey })
                group("API keys", store.all.filter(\.apiKey))
            }
        }
    }

    @ViewBuilder private func group(_ title: String, _ accounts: [AccountStatus]) -> some View {
        if !accounts.isEmpty {
            SettingGroup(title: title) {
                ForEach(accounts) { AccountSwitch(account: $0) }
            }
        }
    }
}

private struct AccountSwitch: View {
    @Environment(Store.self) private var store
    var account: AccountStatus

    var body: some View {
        HStack(spacing: Row.gap) {
            Logo(account: account, size: Row.logo)
            AccountName(account: account)
            Spacer(minLength: 8)
            Switch(label: "Show \(store.words().called(account, labelled: true))",
                   on: Binding(get: { !account.hidden },
                               set: { shown in withAnimation(spring) { store.setHidden(!shown, for: account.id) } }))
                // While serve is away, a change would go back.
                .disabled(store.failure != nil)
        }
        .padding(Row.padding)
        .frame(minHeight: Row.setting)
    }
}
