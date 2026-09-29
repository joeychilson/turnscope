// The menu bar item: each account in use, the most urgent first, as its logo
// and what the menu bar says of it ([`Words.figure`]): what is left, "68%",
// in amber once it runs out; how long it has, "1h 40m", within three hours of
// that; when it is back, "Back 5:40 AM", in red once used up. With none in use, Turnscope's mark
// alone. The person can have it show only the most urgent.
//
// It is one image, drawn by SwiftUI: while every account is fine a template,
// which macOS draws in the menu bar's own color; once one needs the person, in
// color, the rest in the ink the menu bar's appearance has. It is drawn again
// only when what it shows has changed, as each feed and each minute's tick
// ask for it.

import AppKit
import SwiftUI
import TurnscopeKit

@MainActor
enum MenuBarItem {
    /// What the image last drawn showed.
    private static var drawn: Shown?

    /// The item's image for `accounts` in use, as of `now`; nil when it
    /// would show what the last did.
    static func image(_ accounts: [Account], now: Date = .now, dark: Bool) -> NSImage? {
        let words = Words(now: now)
        let shown = Array(accounts.prefix(most))
        let figures = shown.map(words.figure)
        let urgent = shown.contains { $0.standing != .lasts }
        let showing = Shown(accounts: shown.map { [$0.logo, $0.standing.rawValue] }, figures: figures,
                            dark: urgent && dark)
        guard showing != drawn else { return nil }
        drawn = showing
        let renderer = ImageRenderer(content: Label(accounts: shown, figures: figures, template: !urgent)
            .environment(\.colorScheme, dark ? .dark : .light))
        renderer.scale = 2
        let image = renderer.nsImage ?? NSImage()
        image.isTemplate = !urgent
        image.accessibilityDescription = shown.isEmpty
            ? "Turnscope"
            : zip(shown, figures).map { "\($0.title) \($1)" }.joined(separator: ", ")
        return image
    }

    /// Whether the item counts down with the clock for `accounts` in use: a
    /// figure it shows is a time, or becomes one as running out nears.
    static func counts(_ accounts: [Account]) -> Bool {
        accounts.prefix(most).contains { $0.decidingLimit?.standing ?? .lasts != .lasts }
    }

    /// How many accounts it shows, as the person chose.
    private static var most: Int {
        UserDefaults.standard.string(forKey: Preference.menuShows) == "one" ? 1 : 2
    }

    /// What an image shows: the accounts' logos and standings, their
    /// figures, and, drawn in color, the appearance.
    private struct Shown: Equatable {
        var accounts: [[String]]
        var figures: [String]
        var dark: Bool
    }

    private struct Label: View {
        var accounts: [Account]
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
                            .foregroundStyle(template ? Color.black : account.standing == .lasts ? .primary : account.standing.color)
                    }
                }
            }
            .padding(.horizontal, 1)
            .frame(height: 22)
        }
    }
}
