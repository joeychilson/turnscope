// Settings → Accounts: every account found, subscriptions first and API keys
// after, each with a switch to show or hide it.

import SwiftUI
import TurnscopeKit

/// Every account found, with subscriptions and API keys in separate groups.
/// Each has a switch to leave it out of the menu bar and the panel.
struct AccountsTab: View {
    @Environment(Store.self) private var store

    var body: some View {
        let keys = store.all.filter(\.apiKey)
        let subscriptions = store.all.filter { !$0.apiKey }
        VStack(alignment: .leading, spacing: 18) {
            group("Subscriptions", subscriptions)
            group("API keys", keys)
        }
    }

    @ViewBuilder private func group(_ title: String, _ accounts: [AccountStatus]) -> some View {
        if !accounts.isEmpty {
            VStack(alignment: .leading, spacing: 0) {
                SectionTitle(text: title)
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
            Switch(label: "Show \([account.title, account.label].compactMap { $0 }.joined(separator: ", "))",
                   on: Binding(get: { !account.hidden },
                               set: { shown in withAnimation(spring) { store.setHidden(!shown, for: account.id) } }))
        }
        .padding(Row.padding)
    }
}
