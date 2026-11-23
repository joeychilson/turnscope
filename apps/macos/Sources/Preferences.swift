// The app's own preferences, kept in UserDefaults under the keys 0.2 and 0.3 used,
// so existing installs carry over seamlessly.

import Foundation

struct Preferences {
    private static let menuShowsKey = "menuShows"
    private static let dismissedConnectKey = "dismissedConnect"

    var menuShows: MenuShows
    var dismissedConnect: [String]

    static func read() -> Preferences {
        let defaults = UserDefaults.standard
        let shows: MenuShows = defaults.string(forKey: menuShowsKey) == MenuShows.one.rawValue ? .one : .all
        let dismissed = (defaults.string(forKey: dismissedConnectKey) ?? "")
            .split(separator: ",")
            .map(String.init)
            .filter { !$0.isEmpty }
        return Preferences(menuShows: shows, dismissedConnect: dismissed)
    }

    static func save(menuShows: MenuShows, dismissedConnect: [String]) {
        let defaults = UserDefaults.standard
        defaults.set(menuShows.rawValue, forKey: menuShowsKey)
        defaults.set(dismissedConnect.joined(separator: ","), forKey: dismissedConnectKey)
    }
}
