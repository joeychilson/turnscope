// Notifications in words: the account as the title, what happened as the
// subtitle, and when it's back or what to do as the body. Times are clock
// times, since a notification stays on screen.

import Foundation

extension Words {
    /// A notification's words. The title is the account. The subtitle is
    /// what happened, worded like the limit's line in the panel. The body is
    /// when it's back, or what to do.
    public struct Note: Equatable, Sendable {
        public var title: String
        /// What happened: "Weekly runs out Thu 9 AM".
        public var subtitle: String
        /// When it's back, or what to do. Empty when nothing more is known.
        public var body: String
    }

    /// What an alert says. `accounts` is every account in the status. It's
    /// used to name the limit as the panel does, to tell the account apart
    /// from another with the same name, and to say which agent to sign in
    /// with. Times are clock times, since a notification stays on screen.
    public func note(_ alert: Alert, accounts: [AccountStatus] = []) -> Note {
        let account = accounts.first { $0.id == alert.account }
        let title = account.map { called($0, among: accounts) } ?? alert.accountTitle
        let limit = account?.limits.first { $0.key == alert.limit }
        let name = limit.map(name) ?? alert.limitName ?? "A limit"
        let (runs, be) = limit.map(plural) == true ? ("run", "are") : ("runs", "is")
        let back = alert.resets.map { "Back \(clock($0))." } ?? ""
        switch alert.kind {
        case .signIn:
            let agent = account?.agents.first.map(self.agent)
            return Note(title: title, subtitle: "Sign in again",
                        body: agent.map { "Open \($0) to sign in." } ?? "")
        case .runningOut:
            let out = alert.runsOut.map(clock) ?? "soon"
            return Note(title: title, subtitle: "\(name) \(runs) out \(out)", body: back)
        case .usedUp:
            return Note(title: title, subtitle: "\(name) \(be) used up", body: back)
        case .reset:
            let room = limit.map { "\(left($0))\(size($0).map { " \($0)" } ?? "") left." }
            return Note(title: title, subtitle: "\(name) \(be) back", body: room ?? "")
        }
    }

    /// What a notification calls `account`, among `accounts`. It's the
    /// title, and if another account has the same title (`twinned`), the
    /// part of the label before the "@" too: "Claude Max · joey". If the
    /// other's label has the same part before the "@", the whole label is
    /// used instead.
    func called(_ account: AccountStatus, among accounts: [AccountStatus]) -> String {
        let title = account.title
        guard let label = account.label, account.twinned(among: accounts) else { return title }
        let others = accounts.filter { $0.title == title && $0.id != account.id && !$0.hidden }.map(\.label)
        let name = { (label: String) -> String in
            guard let at = label.firstIndex(of: "@"), at > label.startIndex else { return label }
            return String(label[..<at])
        }
        let alike = others.contains { $0.map(name) == name(label) }
        return "\(title) · \(alike ? label : name(label))"
    }
}
