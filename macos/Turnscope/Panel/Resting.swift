// The accounts not in use, folded into one line that names the one with most
// room, and opens in place into a row each: those with room, the most first,
// with a bar; then those to sign in again; then keys with no limit. A row
// opens in place into its limits.
//
// An account is hidden where it is seen: hovering a row shows its Hide
// button, and a right-click offers it too. Those hidden gather in one line at
// the end, "2 hidden", which lists them, each to show again.

import SwiftUI
import TurnscopeKit

struct Resting: View {
    @Environment(Navigation.self) private var navigation
    var accounts: [Account]
    var hidden: [Account]

    var body: some View {
        let words = Words()
        let open = navigation.restingOpen
        // The feed lists those with room first, the most first.
        let best = accounts.first { $0.problem == nil && $0.decidingLimit?.left != nil }
        let label = accounts.isEmpty ? "\(hidden.count) hidden" : "\(accounts.count) not in use"
        // Rows a few points apart, so one open, or hovered, doesn't sit on
        // the next.
        VStack(alignment: .leading, spacing: 4) {
            Button {
                withAnimation(spring) {
                    navigation.restingOpen.toggle()
                    if !navigation.restingOpen {
                        navigation.restingChosen = nil
                        navigation.hiddenShown = false
                    }
                }
            } label: {
                HStack(spacing: 8) {
                    Text(label).font(.system(size: 12)).foregroundStyle(.secondary)
                    Spacer(minLength: 6)
                    if let best, !open {
                        Text("\(best.title) \(words.percent(best.decidingLimit?.left))")
                            .font(.system(size: 12).monospacedDigit()).foregroundStyle(.tertiary).lineLimit(1)
                            .transition(.opacity)
                    }
                    Chevron(open: open)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .rowBackground()
            }
            .buttonStyle(.plain)
            .accessibilityLabel(label)
            .accessibilityHint(open ? "Folds them" : "Lists them")
            if open {
                VStack(spacing: 4) {
                    ForEach(accounts) { account in
                        RestingRow(account: account, open: navigation.restingChosen == account.id) {
                            withAnimation(spring) {
                                navigation.restingChosen = navigation.restingChosen == account.id ? nil : account.id
                            }
                        }
                    }
                    if !hidden.isEmpty && !accounts.isEmpty {
                        Button {
                            withAnimation(spring) { navigation.hiddenShown.toggle() }
                        } label: {
                            HStack(spacing: 8) {
                                Text("\(hidden.count) hidden").font(.system(size: 12)).foregroundStyle(.tertiary)
                                Spacer()
                                Chevron(open: navigation.hiddenShown)
                            }
                            .padding(.horizontal, 12)
                            .padding(.vertical, 7)
                            .rowBackground()
                        }
                        .buttonStyle(.plain)
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

private struct Chevron: View {
    var open: Bool

    var body: some View {
        Image(systemName: "chevron.right")
            .font(.system(size: 10, weight: .semibold))
            .foregroundStyle(.tertiary)
            .rotationEffect(.degrees(open ? 90 : 0))
    }
}

private struct RestingRow: View {
    var account: Account
    var open: Bool
    var toggle: () -> Void
    @State private var hover = false

    var body: some View {
        let words = Words()
        let usable = account.problem == nil && account.decidingLimit?.left != nil
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Logo(account: account, size: 14).foregroundStyle(usable ? .primary : .secondary)
                AccountName(account: account, title: usable ? .primary : .secondary)
                Spacer(minLength: 8)
                if hover {
                    HideButton(account: account).transition(.opacity)
                }
                if account.problem == .signIn {
                    Text("Sign in again").font(.system(size: 12)).foregroundStyle(.secondary)
                } else if let limit = account.decidingLimit, limit.left != nil {
                    Level(limit: limit, even: false).frame(width: 44)
                    Text(words.percent(limit.left))
                        .font(.system(size: 12, weight: .medium).monospacedDigit())
                        .fixedSize()
                        .frame(minWidth: 36, alignment: .trailing)
                } else {
                    Text("No limit").font(.system(size: 12)).foregroundStyle(.tertiary)
                }
            }
            if open {
                // Its limits, and what stands in the way of reading them, as
                // an account in use says it; a key with neither carries none.
                VStack(alignment: .leading, spacing: 10) {
                    if !account.limits.isEmpty {
                        LimitRows(limits: account.limits)
                    }
                    if let trouble = words.trouble(account) {
                        Text(trouble)
                    } else if account.limits.isEmpty && account.apiKey {
                        Text("This key carries no limit to watch.")
                    }
                }
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
                .transition(appears)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 7)
        .rowBackground(on: open)
        .onHover { over in withAnimation(.easeOut(duration: 0.12)) { hover = over } }
        // Not a button, as its Hide button sits within it: it opens on a
        // click, and on Space or Return once the keyboard has brought focus.
        .onTapGesture(perform: toggle)
        .focusable(interactions: .activate)
        .onKeyPress(keys: [.space, .return]) { _ in
            toggle()
            return .handled
        }
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
        .accessibilityHint(open ? "Closes its limits" : "Opens its limits")
        .accessibilityAction(.default, toggle)
        .hideable(account)
    }
}

/// Hide an account, from its row.
private struct HideButton: View {
    @Environment(Store.self) private var store
    var account: Account

    var body: some View {
        Button {
            hide(account, in: store)
        } label: {
            Image(systemName: "eye.slash")
        }
        .buttonStyle(IconButton())
        .help("Hide \(account.title) from the menu bar and the panel")
        .accessibilityLabel("Hide \(account.title)")
    }
}

private struct HiddenRow: View {
    @Environment(Store.self) private var store
    var account: Account

    var body: some View {
        HStack(spacing: 8) {
            Logo(account: account, size: 14).foregroundStyle(.tertiary)
            AccountName(account: account, title: .secondary)
            Spacer(minLength: 8)
            Button("Show") { withAnimation(spring) { store.setHidden(account.id, false) } }
                .buttonStyle(PillButton())
                .accessibilityLabel("Show \(account.title)")
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 5)
    }
}
