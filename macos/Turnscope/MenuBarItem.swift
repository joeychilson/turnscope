// The menu bar item: the logo of each account in use, most urgent first, and
// beside it what the menu bar says of it (`Words.figure`):
//
// - what's left, "68%", in amber once it's running out;
// - how long it has, "1h 40m", within three hours of running out;
// - when it's back, "Back 5:40 AM", in red once used up;
// - for a key with no limit, what it cost this month, "$84".
//
// With nothing in use, it shows a gauge symbol alone. A setting can limit
// it to the most urgent account.
//
// It's one image drawn by SwiftUI. While every account is fine it's a
// template, which macOS draws in the menu bar's own color; once one needs
// attention it's drawn in color. While serve is away, it's dimmed, since what
// it shows is as of the last status. It's redrawn only when what it shows
// changes.

import AppKit
import SwiftUI
import TurnscopeKit

@MainActor
final class MenuBarItem {
    /// What the last image drawn showed.
    private var drawn: Drawn?

    /// The item's image for `accounts` in use, in `words`, or nil if it
    /// would show the same as the last one. If it's `outOfDate`, it's dimmed
    /// and VoiceOver says so.
    func image(_ accounts: [AccountStatus], words: Words, dark: Bool, outOfDate: Bool) -> NSImage? {
        let shown = Array(accounts.prefix(MenuBarItem.most))
        let figures = shown.map(words.figure)
        let urgent = MenuBarItem.urgent(accounts)
        let showing = Drawn(accounts: shown.map { [$0.title, $0.provider, "\($0.standing)"] },
                            figures: figures, dark: urgent && dark, outOfDate: outOfDate)
        guard showing != drawn else { return nil }
        drawn = showing
        let renderer = ImageRenderer(content: ItemLabel(accounts: shown, figures: figures, template: !urgent)
            .opacity(outOfDate ? 0.45 : 1)
            .environment(\.colorScheme, dark ? .dark : .light))
        renderer.scale = 2
        let image = renderer.nsImage ?? NSImage()
        image.isTemplate = !urgent
        let said = shown.isEmpty
            ? "Turnscope"
            : shown.map { "\($0.title), \(words.spokenFigure($0))" }.joined(separator: "; ")
        image.accessibilityDescription = outOfDate ? "\(said), not up to date" : said
        return image
    }

    /// Whether an account the item shows, of `accounts` in use, needs
    /// attention because its deciding limit does. If so, the item is in
    /// color and counts down with the clock, since a figure it shows is a
    /// time, or becomes one as running out nears.
    static func urgent(_ accounts: [AccountStatus]) -> Bool {
        accounts.prefix(most).contains { $0.standing != .lasts }
    }

    /// How many accounts to show: every one in use, or only the most
    /// urgent.
    private static var most: Int {
        UserDefaults.standard.string(forKey: Preference.menuShows) == MenuShows.one.rawValue ? 1 : .max
    }

    /// What an image shows: the accounts' names (VoiceOver reads these),
    /// logos and standings, their figures, the appearance when it's drawn in
    /// color, and whether it's out of date.
    private struct Drawn: Equatable {
        var accounts: [[String]]
        var figures: [String]
        var dark: Bool
        var outOfDate: Bool
    }

    private struct ItemLabel: View {
        var accounts: [AccountStatus]
        var figures: [String]
        var template: Bool

        var body: some View {
            HStack(spacing: 10) {
                if accounts.isEmpty {
                    Image(systemName: "gauge.with.dots.needle.50percent")
                        .font(.system(size: 15, weight: .medium))
                        .foregroundStyle(template ? Color.black : .primary)
                }
                ForEach(Array(zip(accounts, figures)), id: \.0.id) { account, figure in
                    HStack(spacing: 4) {
                        Logo(account: account, size: 17)
                            .foregroundStyle(template ? Color.black : .primary)
                        Text(figure)
                            .font(.system(size: 14, weight: .medium).monospacedDigit())
                            .foregroundStyle(template ? Color.black : account.standing.color)
                    }
                }
            }
            .padding(.horizontal, 1)
            .frame(height: 22)
        }
    }
}
