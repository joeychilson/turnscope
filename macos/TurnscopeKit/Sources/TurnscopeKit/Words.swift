// Everything a person reads goes through here, so a limit reads the same in
// the menu bar, the panel and a notification. Figures and times are here,
// limits and the panel in `Words+Limits.swift`, notifications in
// `Words+Notes.swift`.
//
// Words decides nothing the engine decides. Whether a limit lasts, runs out
// or is used up is its outlook. What's left is `leftPercent`, rounded down so
// it's never overstated, and full once its window has reset since it was read
// (`resetSince`). The limit that speaks for an account is its `deciding`. A
// limit is called what its provider calls it.
//
// A time is written for how far off it is: within three hours, "in 2h 10m";
// later today, "3:40 PM"; this week, "Thu 7:50 PM" or "tomorrow 9 AM"; later,
// "Oct 18". Durations read "1h 30m", or "2d 4h" past a day.

import Foundation

/// Words as of `now`, in `calendar`'s time zone, with agents named as in
/// their status (`Store.words`).
public struct Words: Sendable {
    var now: Date
    var calendar: Calendar
    /// Every agent's name, by id.
    var names: [String: String]

    init(now: Date = .now, calendar: Calendar = .autoupdatingCurrent, names: [String: String] = [:]) {
        (self.now, self.calendar, self.names) = (now, calendar, names)
    }

    /// The name of the agent `id`, such as "Claude Code", or its id if the
    /// status doesn't name it.
    func agent(_ id: String) -> String {
        names[id] ?? id
    }

    // MARK: Figures

    /// A percent, "67%".
    func percent(_ value: Int) -> String {
        "\(max(0, min(100, value)))%"
    }

    /// An amount of dollars, written for reading: "$9.96" under ten dollars,
    /// "$84" and "$1,250" from ten up, and "$0" for none. With `down`, it's
    /// rounded down, so what's left is never overstated. Rounding down
    /// allows a hair over for an amount in cents that binary can't hold
    /// exactly, such as 9.95, which is 9.9499…, so it doesn't lose a cent.
    func dollars(_ value: Double, down: Bool = false) -> String {
        let value = max(0, value)
        if value == 0 { return "$0" }
        let hair = 1e-9
        // The ten-dollar cutoff is after rounding: $9.996 is "$10", written
        // like amounts from ten up.
        let cents = down ? (value * 100 + hair).rounded(.down) : (value * 100).rounded()
        if cents < 1000 { return "$" + String(format: "%.2f", cents / 100) }
        let whole = Int(down ? (value + hair).rounded(.down) : value.rounded())
        let digits = String(whole)
        var grouped = ""
        for (index, digit) in digits.enumerated() {
            if index > 0 && (digits.count - index) % 3 == 0 { grouped.append(",") }
            grouped.append(digit)
        }
        return "$" + grouped
    }

    /// What's left of a limit, as its figure: "67%", or "$9.96" for a limit
    /// of money. A limit that reset since it was read is full, "100%", as
    /// far as this Mac knows, as serve says.
    public func left(_ limit: LimitStatus) -> String {
        if let money = limit.money { return dollars(money.leftUsd, down: true) }
        return percent(limit.leftPercent)
    }

    /// Whether a month's cost is unknown because none of the usage had a
    /// price.
    func unknown(_ spend: Spend) -> Bool {
        spend.partial && spend.monthUsd == 0
    }

    /// What an API-key account cost this month, in short for a row: "$84
    /// this month", "At least $84 this month" when some usage had no price,
    /// and "Cost unknown" when none of it had.
    func spent(_ spend: Spend) -> String {
        if unknown(spend) { return "Cost unknown" }
        return (spend.partial ? "At least " : "") + "\(dollars(spend.monthUsd)) this month"
    }

    /// What an API-key account cost this month, as its figure: "$84", or "—"
    /// (never "$0") when none of its usage had a price.
    func spentFigure(_ spend: Spend) -> String {
        unknown(spend) ? "—" : dollars(spend.monthUsd)
    }

    /// The line under the name of a key with no limit. It says what the
    /// figure is not: "No limit", "No limit · some unpriced", or "No limit ·
    /// cost unknown".
    func spentLine(_ spend: Spend) -> String {
        if unknown(spend) { return "No limit · cost unknown" }
        return spend.partial ? "No limit · some unpriced" : "No limit"
    }

    /// Where an API-key account's cost for the month is headed at its pace,
    /// as a sentence: "On pace for about $210 this month." Nil if there's no
    /// forecast yet, or the forecast is the same as the cost so far.
    func pace(_ spend: Spend) -> String? {
        guard !unknown(spend), let forecast = spend.forecastUsd,
              dollars(forecast) != dollars(spend.monthUsd) else { return nil }
        return "On pace for about \(dollars(forecast)) this month."
    }

    // MARK: Time

    /// How long `seconds` is: "45m", "1h 30m", "2d 4h".
    func stretch(_ seconds: TimeInterval) -> String {
        let minutes = max(0, Int(seconds / 60))
        let (hours, rest) = (minutes / 60, minutes % 60)
        if hours >= 24 {
            return hours % 24 == 0 ? "\(hours / 24)d" : "\(hours / 24)d \(hours % 24)h"
        }
        if hours > 0 { return rest > 0 ? "\(hours)h \(rest)m" : "\(hours)h" }
        return "\(rest)m"
    }

    /// When `moment` is, worded for its horizon: "in 2h 10m", "3:40 PM",
    /// "tomorrow 9 AM", "Thu 7:50 PM", "Oct 18".
    func at(_ moment: Moment) -> String {
        at(moment.at, moment.horizon)
    }

    /// When `date` is, worded for `horizon`.
    func at(_ date: Date, _ horizon: Horizon) -> String {
        switch horizon {
        case .soon: "in \(stretch(date.timeIntervalSince(now)))"
        case .today: time(date)
        case .thisWeek: "\(tomorrow(date) ? "tomorrow" : weekday(date)) \(time(date))"
        case .later: monthDay(date)
        }
    }

    /// When `moment` is, as a clock time whatever its horizon. It's for text
    /// that stays on screen while the clock moves, such as a notification:
    /// "3:40 PM", "tomorrow 9 AM", "Thu 7:50 PM", "Oct 18". A time that's
    /// soon is given as the time of day it falls on.
    func clock(_ moment: Moment) -> String {
        guard moment.horizon == .soon else { return at(moment) }
        return at(moment.at, calendar.isDate(moment.at, inSameDayAs: now) ? .today : .thisWeek)
    }

    /// When a past `date` was: "3:40 AM" today, "yesterday 9 AM", "Mon 12 AM"
    /// within the week, "Sep 22" before that.
    func ago(_ date: Date) -> String {
        let time = self.time(date)
        if calendar.isDate(date, inSameDayAs: now) { return time }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: now),
           calendar.isDate(date, inSameDayAs: yesterday) {
            return "yesterday \(time)"
        }
        if date < now, now.timeIntervalSince(date) < 6 * 86_400 {
            return "\(weekday(date)) \(time)"
        }
        return monthDay(date)
    }

    /// Whether `date` falls tomorrow.
    private func tomorrow(_ date: Date) -> Bool {
        calendar.date(byAdding: .day, value: 1, to: now).map { calendar.isDate(date, inSameDayAs: $0) } ?? false
    }

    /// The day `date` falls on, as a headline says it: "today", "tomorrow",
    /// "Thursday".
    func day(_ date: Date) -> String {
        if calendar.isDate(date, inSameDayAs: now) { return "today" }
        if tomorrow(date) { return "tomorrow" }
        return format(date, "\(weekday: .wide)")
    }

    /// Roughly when `date` is, as a headline words a forecast: "this
    /// evening", "tonight", "tomorrow morning", "Saturday night", "Wednesday
    /// around noon". The small hours of a later day count as the night
    /// before, the way people call 12:40 AM tomorrow "tonight".
    func when(_ date: Date) -> String {
        let part = partOfDay(date)
        var day = date
        if part == "night", calendar.component(.hour, from: date) < 5,
           !calendar.isDate(date, inSameDayAs: now),
           let before = calendar.date(byAdding: .day, value: -1, to: date) {
            day = before
        }
        let today = calendar.isDate(day, inSameDayAs: now)
        if part == "noon" { return today ? "around noon" : "\(self.day(day)) around noon" }
        guard today else { return "\(self.day(day)) \(part)" }
        return part == "night" ? "tonight" : "this \(part)"
    }

    /// The part of the day `date` falls in: "morning", "noon" until half
    /// past twelve, "afternoon", "evening", "night".
    func partOfDay(_ date: Date) -> String {
        switch calendar.component(.hour, from: date) {
        case 12 where calendar.component(.minute, from: date) < 30: "noon"
        case 5..<12: "morning"
        case 12..<17: "afternoon"
        case 17..<22: "evening"
        default: "night"
        }
    }

    /// The time of day of `date`: "3:40 AM", or "9 AM" on the hour.
    private func time(_ date: Date) -> String {
        let hour = Date.FormatStyle.Symbol.VerbatimHour.defaultDigits(clock: .twelveHour, hourCycle: .oneBased)
        return calendar.component(.minute, from: date) == 0
            ? format(date, "\(hour: hour) \(dayPeriod: .standard(.abbreviated))")
            : format(date, "\(hour: hour):\(minute: .twoDigits) \(dayPeriod: .standard(.abbreviated))")
    }

    /// The day of the week `date` falls on, abbreviated: "Thu".
    private func weekday(_ date: Date) -> String {
        format(date, "\(weekday: .abbreviated)")
    }

    /// The month and day `date` falls on: "Oct 18".
    private func monthDay(_ date: Date) -> String {
        format(date, "\(month: .abbreviated) \(day: .defaultDigits)")
    }

    /// `count` of `word`, as said aloud: "1 hour", "40 minutes".
    func counted(_ count: Int, _ word: String) -> String {
        "\(count) \(word)\(count == 1 ? "" : "s")"
    }

    /// `date` formatted with `pattern`, in `calendar`'s time zone. It's in
    /// English, like all the words here, whatever the Mac's language.
    /// Foundation caches the formatter for each pattern, so each is made once.
    private func format(_ date: Date, _ pattern: Date.FormatString) -> String {
        date.formatted(Date.VerbatimFormatStyle(format: pattern, locale: Words.english,
                                                timeZone: calendar.timeZone, calendar: calendar))
    }

    /// English, like all the words here, whatever the Mac's language.
    private static let english = Locale(identifier: "en_US_POSIX")
}
