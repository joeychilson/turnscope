// Notifications, as the engine's alerts and recaps call for them, each only
// if the person wants it (Settings), in the words `Words.note` gives them.
// An account's notifications gather in one group, and a newer one about a
// limit takes the place of the last: once a limit is back, what said it ran
// out is taken away. An account in use whose sign-in was refused is said
// once, even across launches, until it is renewed. Clicking one opens the
// panel, with its account open.
//
// macOS asks the person the first time; only an app in a folder macOS keeps
// apps in, as Applications, can post them, so one run from a build folder
// posts none.

import AppKit
import TurnscopeKit
import UserNotifications

@MainActor
final class Notifier: NSObject, UNUserNotificationCenterDelegate {
    /// What clicking a notification does, with the account it is about.
    var onOpen: (String?) -> Void = { _ in }
    /// The accounts whose refused sign-in was said, kept in the defaults.
    private static let toldSignIn = "toldSignIn"

    func start() {
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        center.requestAuthorization(options: [.alert, .sound]) { _, _ in }
    }

    /// Tell `alert`, if the person wants its kind.
    func tell(_ alert: Alert) {
        let wanted: String = switch alert.kind {
        case .runningOut: Preference.notifyRunningOut
        case .usedUp: Preference.notifyUsedUp
        case .back: Preference.notifyBack
        case .unused, .threeQuartersLeft, .halfLeft, .quarterLeft: Preference.notifyMilestones
        }
        let limit = "\(alert.account):\(alert.limit)"
        if alert.kind == .back {
            UNUserNotificationCenter.current().removeDeliveredNotifications(
                withIdentifiers: [Alert.Kind.runningOut, .usedUp].map { "\(limit):\($0.rawValue)" })
        }
        guard UserDefaults.standard.bool(forKey: wanted) else { return }
        post(Words().note(alert), id: "\(limit):\(alert.kind.rawValue)", account: alert.account)
    }

    func tell(_ weeks: [Week]) {
        guard UserDefaults.standard.bool(forKey: Preference.notifyRecap), !weeks.isEmpty else { return }
        post(Words().recap(weeks), id: "recap", account: nil)
    }

    /// Say once of each account in use whose sign-in was refused, until it is renewed.
    func follow(_ feed: Feed) {
        let defaults = UserDefaults.standard
        let told = Set(defaults.stringArray(forKey: Notifier.toldSignIn) ?? [])
        let refused = feed.accounts.filter { $0.problem == .signIn && $0.inUse && !$0.hidden }
        for account in refused where !told.contains(account.id) {
            post(Words().signIn(account), id: "sign-in:\(account.id)", account: account.id)
        }
        // An account renewed, or no longer in use, can be said again.
        let now = Set(refused.map(\.id))
        if now != told { defaults.set(now.sorted(), forKey: Notifier.toldSignIn) }
    }

    /// A notification as a limit running out would send, over the first
    /// account in use, or the first there is.
    func test(_ feed: Feed?) {
        let account = feed?.accounts.first { $0.inUse } ?? feed?.accounts.first
        let words = Words()
        let out = Date.now.addingTimeInterval(52 * 3600)
        let alert = Alert(account: account?.id ?? "test", title: account?.title ?? "Claude Max",
                          label: account?.label, limit: "Weekly", scope: nil,
                          kind: .runningOut, at: out, left: 20)
        post(words.note(alert), id: "test", account: account?.id)
    }

    private func post(_ note: Words.Note, id: String, account: String?) {
        let content = UNMutableNotificationContent()
        content.title = note.title
        content.body = note.body
        content.sound = .default
        content.interruptionLevel = note.urgent ? .timeSensitive : .active
        content.threadIdentifier = account ?? id
        if let account { content.userInfo = ["account": account] }
        UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: id, content: content, trigger: nil))
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
