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

        /// The tab `--open` names: `settings`, `agents` or `accounts`.
        init?(opening: String) {
            switch opening {
            case "settings": self = .general
            case "agents": self = .agents
            case "accounts": self = .accounts
            default: return nil
            }
        }
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

    /// Back where it opens, with nothing open, for the next time.
    func reset() {
        page = .main
        tab = .general
        open = nil
        restingOpen = false
    }
}

/// What the person chose, kept in the app's defaults.
enum Preference {
    /// What the menu bar shows: every account in use, or the most urgent.
    static let menuShows = "menuShows"
    static let notifyRunningOut = "notifyRunningOut"
    static let notifyUsedUp = "notifyUsedUp"
    static let notifyBack = "notifyBack"
    static let notifyMilestones = "notifyMilestones"
    static let notifyRecap = "notifyRecap"

    /// Each notification's default: on for what asks something of the person.
    static func register() {
        UserDefaults.standard.register(defaults: [
            menuShows: "all",
            notifyRunningOut: true,
            notifyUsedUp: true,
            notifyBack: true,
            notifyMilestones: false,
            notifyRecap: false,
        ])
    }
}
