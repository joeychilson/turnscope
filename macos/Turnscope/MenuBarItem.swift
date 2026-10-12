// The menu bar item: the logo of each account in use, most urgent first, and
// beside it what the menu bar says of it (`Words.figure`):
//
// - what's left, "68%", in amber once it's running out;
// - how long it has, "1h 40m", within three hours of running out;
// - when it's back, "Back 5:40 AM", in red once used up;
// - for a key with no limit, what it cost this month, "$84".
//
// With nothing in use, it shows Turnscope's logo alone. A setting can limit
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
        let spoken = words.spokenItem(shown, outOfDate: outOfDate)
        let urgent = MenuBarItem.urgent(accounts)
        let showing = Drawn(accounts: shown.map { [$0.provider, "\($0.standing)"] }, figures: figures,
                            spoken: spoken, dark: urgent && dark, outOfDate: outOfDate)
        guard showing != drawn else { return nil }
        drawn = showing
        let renderer = ImageRenderer(content: ItemLabel(accounts: shown, figures: figures, template: !urgent)
            .opacity(outOfDate ? 0.45 : 1)
            .environment(\.colorScheme, dark ? .dark : .light))
        renderer.scale = 2
        let image = renderer.nsImage ?? NSImage()
        image.isTemplate = !urgent
        image.accessibilityDescription = spoken
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

    /// What an image shows: the accounts' logos and standings, their
    /// figures, what VoiceOver reads, which can change while the figures
    /// don't, the appearance when it's drawn in color, and whether it's out
    /// of date.
    private struct Drawn: Equatable {
        var accounts: [[String]]
        var figures: [String]
        var spoken: String
        var dark: Bool
        var outOfDate: Bool
    }

    private struct ItemLabel: View {
        var accounts: [AccountStatus]
        var figures: [String]
        var template: Bool

        var body: some View {
            HStack(spacing: 10) {
                // A little under other apps' icons, which are 16 points
                // (measured beside them in October 2026): at 16, its filled
                // ring looked bigger than they do.
                if accounts.isEmpty {
                    Mark(size: 14).foregroundStyle(template ? Color.black : .primary)
                }
                // A mark and its figure read as one item. A provider's mark
                // has room around it in its frame (`macos/Logos/README.md`),
                // so at 21 points Anthropic's is 16 by 11.5 and OpenAI's 15.5
                // square. At 17 points (Anthropic's 9 tall) the item looked
                // small beside other apps' icons. At 22 (12 tall) the mark
                // outweighed the figure, so the figure is semibold too.
                ForEach(Array(zip(accounts, figures)), id: \.0.id) { account, figure in
                    HStack(spacing: 4) {
                        Logo(account: account, size: 21)
                            .foregroundStyle(template ? Color.black : .primary)
                        // In the menu bar's own size, as the clock is, and
                        // raised a point onto the clock's baseline: centered
                        // in the item, it sat lower (measured October 2026).
                        Text(figure)
                            .font(.system(size: NSFont.menuBarFont(ofSize: 0).pointSize, weight: .semibold)
                                .monospacedDigit())
                            .foregroundStyle(template ? AnyShapeStyle(Color.black)
                                                      : AnyShapeStyle(account.standing.color))
                            .offset(y: -1)
                    }
                }
            }
            .padding(.horizontal, 1)
            .frame(height: 22)
        }
    }

    /// Turnscope's logo, as its icon draws it (`Turnscope.icon`): a ring
    /// around a slice, like a limit with some of it used. The radii are the
    /// icon's, in its units, 530 across the ring.
    private struct Mark: View {
        var size: CGFloat

        var body: some View {
            Canvas { context, canvas in
                let unit = canvas.width / 530
                let center = CGPoint(x: canvas.width / 2, y: canvas.height / 2)
                let circle = { (radius: CGFloat) in
                    Path(ellipseIn: CGRect(x: center.x - radius * unit, y: center.y - radius * unit,
                                           width: radius * unit * 2, height: radius * unit * 2))
                }
                var ring = circle(265)
                ring.addPath(circle(207))
                context.fill(ring, with: .foreground, style: FillStyle(eoFill: true))
                // From twelve o'clock, clockwise to 235°.
                var slice = Path()
                slice.move(to: center)
                slice.addArc(center: center, radius: 169 * unit,
                             startAngle: .degrees(-90), endAngle: .degrees(145), clockwise: false)
                slice.closeSubpath()
                context.fill(slice, with: .foreground)
            }
            .frame(width: size, height: size)
        }
    }
}
