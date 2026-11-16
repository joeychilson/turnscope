// Posts serve's alerts as notifications, each under the id serve gives it.
// A sign-in notification is removed once its account can be read.

import AppKit
import TurnscopeKit
import UserNotifications

@MainActor
final class Notifier: NSObject, UNUserNotificationCenterDelegate {
    /// What clicking a notification does, given the account it's about.
    var onOpen: (String?) -> Void = { _ in }
    /// The accounts as of the last status. A sign-in notification can be
    /// posted after the status that says its account can be read, so it's
    /// checked against them once it's posted.
    private var latest: [AccountStatus] = []

    func start() {
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        Task {
            // If permission is refused, nothing shows, and Settings says so.
            _ = try? await center.requestAuthorization(options: [.alert, .sound])
        }
    }

    /// Remove the sign-in notification of each of `accounts` that can be
    /// read, also one left from a previous run. Serve names it
    /// `account:<id>` in `Alert.replaces`.
    func renewed(_ accounts: [AccountStatus]) {
        latest = accounts
        UNUserNotificationCenter.current()
            .removeDeliveredNotifications(withIdentifiers: accounts.filter { !$0.refused }.map { "account:\($0.id)" })
    }

    /// Post `alert` in `words`, with `accounts` as of the last status.
    /// Returns whether macOS accepted it, which it doesn't while Turnscope's
    /// notifications are off.
    func tell(_ alert: Alert, accounts: [AccountStatus], words: Words) async -> Bool {
        let note = words.note(alert, accounts: accounts)
        let content = UNMutableNotificationContent()
        content.title = note.title
        content.subtitle = note.subtitle
        content.body = note.body
        content.sound = .default
        content.threadIdentifier = alert.account
        content.userInfo = ["account": alert.account]
        do {
            try await UNUserNotificationCenter.current()
                .add(UNNotificationRequest(identifier: alert.replaces, content: content, trigger: nil))
        } catch {
            return false
        }
        // The last status may have come while it was posted.
        if alert.kind == .signIn { renewed(latest) }
        return true
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                            willPresent notification: UNNotification) async
        -> UNNotificationPresentationOptions {
        [.banner, .sound]
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter,
                                            didReceive response: UNNotificationResponse) async {
        let account = response.notification.request.content.userInfo["account"] as? String
        await MainActor.run { onOpen(account) }
    }
}
