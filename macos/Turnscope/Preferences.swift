// The app's own preferences, kept in its defaults. Which notifications are
// sent is the engine's setting instead, so the command line and the app agree.

import Foundation

enum Preference {
    /// What the menu bar shows, as a `MenuShows`.
    static let menuShows = "menuShows"
    /// Agents in use for which the line asking to connect them was dismissed.
    static let dismissedConnect = "dismissedConnect"

    static func register() {
        UserDefaults.standard.register(defaults: [menuShows: MenuShows.all.rawValue])
    }
}

/// What the menu bar shows: every account in use, or only the most urgent.
enum MenuShows: String {
    case all, one
}
