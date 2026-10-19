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
        /// What happened: "Weekly runs out Thursday 9 AM".
        public var subtitle: String
        /// When it's back, or what to do. Empty when nothing more is known.
        public var body: String
    }

    /// What an alert says. `accounts` is every account in the status. It's
    /// used to name the account and the limit as the panel does (`called`),
    /// and to say which agent to sign in with. An alert's own title, as it
    /// was raised, is only for an account no longer in the status. Times are
    /// clock times, since a notification stays on screen.
    public func note(_ alert: Alert, accounts: [AccountStatus] = []) -> Note {
        let account = accounts.first { $0.id == alert.account }
        let title = account.map { called($0, among: accounts) } ?? alert.accountTitle
        let limit = account?.limits.first { $0.key == alert.limit }
        let name = limit.map(name) ?? alert.limitName ?? "A limit"
        let (runs, be) = limit.map(plural) == true ? ("run", "are") : ("runs", "is")
        let back = alert.resets.map { "Back \(clock($0, full: true))." } ?? ""
        switch alert.kind {
        case .signIn:
            let agent = account?.agents.first.map(self.agent)
            return Note(title: title, subtitle: "Sign in again",
                        body: agent.map { "Open \($0) to sign in." } ?? "")
        case .runningOut:
            let out = alert.runsOut.map { clock($0, full: true) } ?? "soon"
            return Note(title: title, subtitle: "\(name) \(runs) out \(out)", body: back)
        case .usedUp:
            return Note(title: title, subtitle: "\(name) \(be) used up", body: back)
        case .reset:
            let room = limit.map { "\(left($0))\(size($0).map { " \($0)" } ?? "") left." }
            return Note(title: title, subtitle: "\(name) \(be) back", body: room ?? "")
        }
    }

    /// What a notification calls `account`, among `accounts`: its title,
    /// and its label too when another shown account in use has the same
    /// title, "Claude Max (me@example.com)". A notification stands alone, so
    /// unlike the headline it can't point to a row. Only an account in use
    /// counts: an idle account with the same title can't be taken for one in
    /// use.
    func called(_ account: AccountStatus, among accounts: [AccountStatus]) -> String {
        called(account, labelled: account.twinned(among: accounts.filter(\.inUse)))
    }

    /// What Hide and Show call `account`: its title, and its label too where
    /// its row shows it (`labelled`), "Claude Max (me@example.com)", so two
    /// accounts with the same title are told apart as their rows are.
    public func called(_ account: AccountStatus, labelled: Bool) -> String {
        guard labelled, let label = account.label else { return account.title }
        return "\(account.title) (\(label))"
    }
}
