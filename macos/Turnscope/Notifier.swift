// Posts serve's alerts as notifications, in the words of `Words.note`. Serve
// decides which alerts there are and drops the kinds turned off in Settings,
// so every alert that arrives is posted.
//
// Each is posted under the id serve gives it: one per limit, or one per
// account for signing in. A newer one replaces the older, so "back" replaces
// "used up". A sign-in notification is removed once the account can be read
// again. Clicking a notification opens the panel with its account open.
//
// macOS asks for permission the first time.

import AppKit
import TurnscopeKit
import UserNotifications

@MainActor
final class Notifier: NSObject, UNUserNotificationCenterDelegate {
    /// What clicking a notification does, given the account it's about.
    var onOpen: (String?) -> Void = { _ in }
    /// Accounts that may still have a notification asking to sign in again.
    private var signingIn: Set<String> = []

    func start() {
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        Task {
            // If permission is refused, nothing shows, and Settings says so.
            _ = try? await center.requestAuthorization(options: [.alert, .sound])
            // Notifications from before this run that ask to sign in again,
            // to remove once the account can be read.
            let shown = await center.deliveredNotifications().map(\.request.identifier)
            signingIn.formUnion(shown.compactMap { $0.hasPrefix("account:") ? String($0.dropFirst(8)) : nil })
        }
    }

    /// Remove the notifications asking to sign in again for each of
    /// `accounts` that can be read again.
    func renewed(_ accounts: [AccountStatus]) {
        let read = signingIn.filter { id in accounts.contains { $0.id == id && !$0.refused } }
        guard !read.isEmpty else { return }
        signingIn.subtract(read)
        UNUserNotificationCenter.current().removeDeliveredNotifications(withIdentifiers: read.map { "account:\($0)" })
    }

    /// Whether Turnscope's notifications are turned off in System Settings,
    /// so none can show.
    func off() async -> Bool {
        await UNUserNotificationCenter.current().notificationSettings().authorizationStatus == .denied
    }

    /// Post `alert` in `words`, with `accounts` as of the last status.
    /// Returns whether macOS accepted it.
    func tell(_ alert: Alert, accounts: [AccountStatus], words: Words) async -> Bool {
        if alert.kind == .signIn { signingIn.insert(alert.account) }
        return await post(words.note(alert, accounts: accounts), id: alert.replaces, account: alert.account)
    }

    /// Post `note`. Returns whether macOS accepted it, which it doesn't while
    /// Turnscope's notifications are off.
    private func post(_ note: Words.Note, id: String, account: String) async -> Bool {
        do {
            try await UNUserNotificationCenter.current().add(request(note, id: id, account: account))
            return true
        } catch {
            return false
        }
    }

    private func request(_ note: Words.Note, id: String, account: String) -> UNNotificationRequest {
        let content = UNMutableNotificationContent()
        content.title = note.title
        content.subtitle = note.subtitle
        content.body = note.body
        content.sound = .default
        content.threadIdentifier = account
        content.userInfo = ["account": account]
        return UNNotificationRequest(identifier: id, content: content, trigger: nil)
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
