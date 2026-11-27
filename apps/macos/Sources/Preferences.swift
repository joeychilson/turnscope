// The app's own preferences, in UserDefaults. Their keys stay as they are, or an
// install loses what was set.

import Foundation

struct Preferences {
    private static let dismissedConnectKey = "dismissedConnect"

    var dismissedConnect: [String]

    static func read() -> Preferences {
        let dismissed = (UserDefaults.standard.string(forKey: dismissedConnectKey) ?? "")
            .split(separator: ",")
            .map(String.init)
        return Preferences(dismissedConnect: dismissed)
    }

    static func save(dismissedConnect: [String]) {
        UserDefaults.standard.set(dismissedConnect.joined(separator: ","), forKey: dismissedConnectKey)
    }
}
