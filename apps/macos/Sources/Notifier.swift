// Posts Rust-worded alerts as notifications, each under `AlertNote.replaces`.
// A sign-in notification is removed once its account can be read again.

import AppKit
import Observation
import UserNotifications

@MainActor
@Observable
final class Notifier: NSObject, UNUserNotificationCenterDelegate {
    private(set) var denied = false
    var onOpen: (String?) -> Void = { _ in }
    var onAcknowledged: (Int64, String?) -> Void = { _, _ in }

    private var center: UNUserNotificationCenter { .current() }

    func start() {
        center.delegate = self
        Task {
            _ = try? await center.requestAuthorization(options: [.alert, .sound])
            await refreshDenied()
        }
    }

    func check() {
        Task { await refreshDenied() }
    }

    private func refreshDenied() async {
        let settings = await center.notificationSettings()
        denied = settings.authorizationStatus == .denied
    }

    func post(_ alerts: [AlertNote]) {
        for alert in alerts {
            let content = UNMutableNotificationContent()
            content.title = alert.title
            content.subtitle = alert.subtitle
            content.body = alert.body
            content.sound = .default
            content.threadIdentifier = alert.account
            content.userInfo = ["account": alert.account]
            let request = UNNotificationRequest(identifier: alert.replaces, content: content, trigger: nil)
            Task {
                do {
                    try await center.add(request)
                    onAcknowledged(alert.id, alert.signIn ? alert.account : nil)
                } catch {}
            }
        }
    }

    func remove(_ identifiers: [String]) {
        center.removeDeliveredNotifications(withIdentifiers: identifiers)
    }

    func openSettings() {
        let id = Bundle.main.bundleIdentifier ?? ""
        if let url = URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=\(id)") {
            NSWorkspace.shared.open(url)
        }
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .sound]
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        let account = response.notification.request.content.userInfo["account"] as? String
        await MainActor.run { onOpen(account) }
    }
}
