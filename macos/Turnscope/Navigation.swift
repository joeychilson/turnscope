// Where the panel is: which page, what's open on it, and which way the pages
// slide. Like a menu, the panel always opens on its first page.

import Observation
import SwiftUI
import TurnscopeKit

@MainActor
@Observable
final class Navigation {
    /// The panel's pages, left to right. Going deeper, the new page comes in
    /// from the right; going back, from the left.
    enum Page: Int { case main, settings }

    private(set) var page: Page = .main
    /// Whether the last move went deeper.
    private(set) var forward = true
    /// The tab shown in Settings.
    var tab: SettingsTab = .general
    /// The id of the account open in place. Only one is open at a time,
    /// wherever it's listed.
    var open: String?
    /// Whether the accounts not used this week are unfolded.
    var unusedOpen = false
    /// Whether hidden accounts are listed.
    var hiddenShown = false
    /// The time the panel's countdowns run from. It's set when the panel
    /// opens and each minute while it's open, so a closed panel never wakes.
    var now = Date.now

    /// Open `account` in place, closing any other, or close it if it's open.
    func toggle(_ account: String) {
        open = open == account ? nil : account
    }

    /// Go to `page`. The direction is set a frame ahead, so the page that's
    /// leaving uses it too.
    func go(_ to: Page, tab: SettingsTab? = nil) {
        if let tab { self.tab = tab }
        forward = to.rawValue > page.rawValue
        Task { withAnimation(spring) { page = to } }
    }

    /// Open what `opening` names, as `--open` does: the first account in use
    /// (`firstInUse`) and the accounts not in use. Returns the Settings tab
    /// to go to, if any.
    func apply(_ opening: Opening, firstInUse: String?) -> SettingsTab? {
        if opening.account { open = firstInUse }
        if opening.unused { unusedOpen = true }
        return opening.settings
    }

    /// Go back to where the panel opens, with nothing open, for next time.
    func reset() {
        page = .main
        tab = .general
        open = nil
        unusedOpen = false
        hiddenShown = false
    }
}
