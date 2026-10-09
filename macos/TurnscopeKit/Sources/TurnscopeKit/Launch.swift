// The options the app was started with:
//
// - `--data <dir>` and `--home <dir>` are passed to the engine, to keep test
//   data apart;
// - `--open` opens the panel once the first status arrives, and
//   `--open <what>` also opens something in it;
// - `--fixture <status.json>` shows a status from a file with no engine, such
//   as `contract/status.notification.json`;
// - `--snapshot <folder>` draws the panel as PNGs and quits.

import Foundation

public struct Launch: Equatable, Sendable {
    /// The status to show instead of an engine's.
    public var fixture: String?
    /// Where to draw the panel before quitting.
    public var snapshot: String?
    /// What to open once the first status arrives, or nil to open nothing.
    public var opening: Opening?
    /// What to pass on to the engine: `--data` and `--home`, each with its
    /// directory.
    public var passed: [String]

    /// The options in `arguments`, as the command line gives them, with the
    /// program first.
    public init(_ arguments: [String]) {
        /// The value given after `option`, or nil if it's last or another
        /// option follows it.
        func value(_ option: String) -> String? {
            guard let index = arguments.firstIndex(of: option), index + 1 < arguments.count else { return nil }
            let value = arguments[index + 1]
            return value.hasPrefix("--") ? nil : value
        }
        fixture = value("--fixture")
        snapshot = value("--snapshot")
        opening = arguments.contains("--open") ? Opening(value("--open") ?? "") : nil
        passed = ["--data", "--home"].flatMap { option in value(option).map { [option, $0] } ?? [] }
    }
}

/// What `--open` opens in the panel: names joined by commas, such as
/// `account,unused`. An unknown name opens nothing.
public struct Opening: Equatable, Sendable {
    /// The first account in use, opened in place.
    public var account = false
    /// The accounts not used this week, unfolded.
    public var unused = false
    /// Settings, opened at this tab.
    public var settings: SettingsTab?

    public init(_ names: String) {
        for name in names.split(separator: ",") {
            switch name {
            case "account": account = true
            case "unused": unused = true
            case "settings": settings = .general
            case "agents": settings = .agents
            case "accounts": settings = .accounts
            default: break
            }
        }
    }
}

/// The tabs in Settings, as the panel shows them.
public enum SettingsTab: String, CaseIterable, Identifiable, Sendable {
    case general, agents, accounts

    public var id: Self { self }

    /// The name shown on its tab, such as "General".
    public var title: String { rawValue.capitalized }
}
