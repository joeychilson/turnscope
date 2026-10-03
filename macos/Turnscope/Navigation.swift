// Where the panel is: its page, what is open on it, and which way pages move.
// The panel always opens on its first page, as a menu does.

import Observation
import SwiftUI

@MainActor
@Observable
final class Navigation {
    /// The panel's pages, left to right: going deeper, a page comes from the
    /// right, and going back, from the left.
    enum Page: Int { case main, settings }

    /// Settings' tabs.
    enum Tab: String, CaseIterable, Identifiable {
        case general = "General", agents = "Agents", accounts = "Accounts"
        var id: String { rawValue }
    }

    private(set) var page: Page = .main
    /// Whether the last move went deeper.
    private(set) var forward = true
    /// The tab Settings shows.
    var tab: Tab = .general
    /// The account opened in place, by id.
    var open: String?
    /// Whether the accounts not in use are unfolded.
    var restingOpen = false
    /// The account not in use opened in place, by id.
    var restingChosen: String?
    /// Whether the accounts hidden are listed.
    var hiddenShown = false
    /// The time the panel's countdowns are as of: set as it opens, and each
    /// minute while it is open, so a closed panel never wakes.
    var now = Date.now

    /// Go to `page`. The direction is set a frame ahead, so the page leaving
    /// takes it too.
    func go(_ to: Page, tab: Tab? = nil) {
        if let tab { self.tab = tab }
        forward = to.rawValue > page.rawValue
        Task { @MainActor in withAnimation(spring) { self.page = to } }
    }

    /// Open what `--open` names, to look at: `account`, the first account in
    /// use, `firstInUse`; `resting`, the accounts not in use; or `settings`,
    /// `agents` or `accounts`, the tab of Settings it gives back to go to.
    func opening(_ name: String, firstInUse: String?) -> Tab? {
        switch name {
        case "account": open = firstInUse
        case "resting": restingOpen = true
        case "settings": return .general
        case "agents": return .agents
        case "accounts": return .accounts
        default: break
        }
        return nil
    }

    /// Back where it opens, with nothing open, for the next time.
    func reset() {
        page = .main
        tab = .general
        open = nil
        restingOpen = false
        restingChosen = nil
        hiddenShown = false
    }
}
