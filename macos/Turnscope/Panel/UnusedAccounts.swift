// The accounts not used this week, folded into one line, "3 more accounts",
// that opens into a row for each. They're listed as serve orders them: those
// with the most left first, then those to sign in again, then keys with no
// limit, by what they cost this month.
//
// Hidden accounts gather at the end in a line of their own, "2 hidden",
// which lists them, each with a Show button.

import SwiftUI
import TurnscopeKit

struct UnusedAccounts: View {
    @Environment(Store.self) private var store
    @Environment(Navigation.self) private var navigation
    var accounts: [AccountStatus]
    var hidden: [AccountStatus]
    /// Whether accounts are shown above these.
    var besideOthers: Bool

    var body: some View {
        let open = navigation.unusedOpen
        let words = store.words()
        let label = accounts.isEmpty
            ? words.hidden(hidden.count)
            : words.unused(accounts.count, besideOthers: besideOthers)
        VStack(alignment: .leading, spacing: Row.spacing) {
            Fold(text: label, open: open) {
                navigation.unusedOpen.toggle()
                if !navigation.unusedOpen {
                    if accounts.contains(where: { $0.id == navigation.open }) { navigation.open = nil }
                    navigation.hiddenShown = false
                }
            }
            if open {
                VStack(spacing: Row.spacing) {
                    ForEach(accounts) { AccountRow(account: $0) }
                    if !hidden.isEmpty && !accounts.isEmpty {
                        Fold(text: words.hidden(hidden.count), open: navigation.hiddenShown) {
                            navigation.hiddenShown.toggle()
                        }
                    }
                    if navigation.hiddenShown || accounts.isEmpty {
                        ForEach(hidden) { HiddenRow(account: $0) }
                            .transition(appears)
                    }
                }
                .transition(appears)
            }
        }
    }
}

/// A line that folds rows away, showing what it holds and a chevron. A click
/// runs `toggle`, animated.
private struct Fold: View {
    var text: String
    var open: Bool
    var toggle: () -> Void

    var body: some View {
        Button { withAnimation(spring, toggle) } label: {
            Hover { hover in
                HStack(spacing: 8) {
                    Text(text).font(.line).foregroundStyle(.secondary)
                    Spacer(minLength: 6)
                    Image(systemName: "chevron.right")
                        .font(.system(size: 10, weight: .semibold))
                        .foregroundStyle(.tertiary)
                        .rotationEffect(.degrees(open ? 90 : 0))
                }
                .padding(Row.padding)
                .rowBackground(hover)
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(text)
        .accessibilityHint(open ? "Folds them" : "Lists them")
    }
}

private struct HiddenRow: View {
    @Environment(Store.self) private var store
    var account: AccountStatus

    var body: some View {
        HStack(spacing: Row.gap) {
            Logo(account: account, size: Row.logo).foregroundStyle(.tertiary)
            AccountName(account: account, title: .secondary, subtitle: .tertiary)
            Spacer(minLength: 8)
            Button("Show") { withAnimation(spring) { store.setHidden(false, for: account.id) } }
                .buttonStyle(PillButton())
                .accessibilityLabel("Show \(account.title)")
        }
        .padding(Row.padding)
    }
}
