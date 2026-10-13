// Pieces shared by account rows, in the panel and in Settings: an account's
// title, and its name with the agents that use it.

import SwiftUI
import TurnscopeKit

/// An account's title as every row shows it, in the gray `title`. If
/// `labelled`, its label is beside it, to tell it apart from another account
/// with the same name. The label is cut short in the middle before the title
/// is, so a row never grows wider than the panel.
struct AccountTitle: View {
    var account: AccountStatus
    var labelled: Bool
    var title: HierarchicalShapeStyle = .primary

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Text(account.title).font(.name).foregroundStyle(title)
                .lineLimit(1).truncationMode(.tail)
                .layoutPriority(1)
                .help(account.title)
            if labelled, let label = account.label {
                Text(label).font(.small).foregroundStyle(.tertiary)
                    .lineLimit(1).truncationMode(.middle)
            }
        }
    }
}

/// An account where it's listed to show or hide: its title and label, and
/// under them the agents that use it, in the grays `title` and `subtitle`.
struct AccountName: View {
    @Environment(Store.self) private var store
    var account: AccountStatus
    var title: HierarchicalShapeStyle = .primary
    var subtitle: HierarchicalShapeStyle = .secondary

    var body: some View {
        VStack(alignment: .leading, spacing: 1) {
            AccountTitle(account: account, labelled: true, title: title)
            if let via = store.words().via(account) {
                Text(via).font(.small).foregroundStyle(subtitle)
                    .lineLimit(1).truncationMode(.tail)
            }
        }
    }
}
