// The app's own preferences, in UserDefaults. Their keys stay as they are, or an
// install loses what was set.

import Foundation

struct Preferences {
    private static let menuShowsKey = "menuShows"
    private static let dismissedConnectKey = "dismissedConnect"
    private static let dismissedOpenAtLoginKey = "dismissedOpenAtLogin"

    var menuShows: MenuShows
    var dismissedConnect: [String]
    var dismissedOpenAtLogin: Bool

    static func read() -> Preferences {
        let defaults = UserDefaults.standard
        let shows: MenuShows = defaults.string(forKey: menuShowsKey) == MenuShows.one.rawValue ? .one : .all
        let dismissed = (defaults.string(forKey: dismissedConnectKey) ?? "")
            .split(separator: ",")
            .map(String.init)
        return Preferences(
            menuShows: shows,
            dismissedConnect: dismissed,
            dismissedOpenAtLogin: defaults.bool(forKey: dismissedOpenAtLoginKey)
        )
    }

    static func save(menuShows: MenuShows, dismissedConnect: [String], dismissedOpenAtLogin: Bool) {
        let defaults = UserDefaults.standard
        defaults.set(menuShows.rawValue, forKey: menuShowsKey)
        defaults.set(dismissedConnect.joined(separator: ","), forKey: dismissedConnectKey)
        defaults.set(dismissedOpenAtLogin, forKey: dismissedOpenAtLoginKey)
    }
}
