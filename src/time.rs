//! Time as people read and write it, in the local time zone (`TZ`). Inside
//! Turnscope a time is UTC milliseconds.

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan, Zoned};

use crate::{Error, Result};

pub const MINUTE: i64 = 60_000;
pub const HOUR: i64 = 60 * MINUTE;
pub const DAY: i64 = 24 * HOUR;

/// The time in an RFC 3339 string.
pub fn millis(text: &str) -> Option<i64> {
    text.parse::<Timestamp>().ok().map(|at| at.as_millisecond())
}

pub fn local(at: i64) -> Zoned {
    Timestamp::from_millisecond(at)
        .unwrap_or_default()
        .to_zoned(TimeZone::system())
}

#[derive(Clone, Copy)]
pub enum Period {
    Day,
    Week,
    Month,
}

/// The start of the local day, week (from Monday) or month `at` is in.
pub fn start_of(period: Period, at: i64) -> i64 {
    window(period, at, &TimeZone::system()).0
}

/// When the day, week (from Monday) or month `at` is in starts in `zone`,
/// and when the next one does.
pub fn window(period: Period, at: i64, zone: &TimeZone) -> (i64, i64) {
    let day = Timestamp::from_millisecond(at)
        .unwrap_or_default()
        .to_zoned(zone.clone())
        .date();
    let (start, span) = match period {
        Period::Day => (day, 1.day()),
        Period::Week => (
            day.checked_sub((day.weekday().to_monday_zero_offset() as i64).days())
                .unwrap_or(day),
            1.week(),
        ),
        Period::Month => (day.first_of_month(), 1.month()),
    };
    let end = start.checked_add(span).unwrap_or(start);
    (start_of_day(start, zone), start_of_day(end, zone))
}

fn start_of_day(day: Date, zone: &TimeZone) -> i64 {
    day.to_zoned(zone.clone())
        .map(|zoned| zoned.timestamp().as_millisecond())
        .unwrap_or_default()
}

/// The moment `text` names, as of `now`.
pub fn parse(text: &str, now: i64) -> Result<i64> {
    let text = text.trim();
    // A count too large to go back is no time at all, and a negative one,
    // which would be ahead, isn't one.
    let back = |unit: char, hours: i64| {
        let count: u32 = text.strip_suffix(unit)?.parse().ok()?;
        now.checked_sub(i64::from(count).checked_mul(hours * HOUR)?)
    };
    let found = match text {
        "now" => Some(now),
        "today" => Some(start_of(Period::Day, now)),
        "yesterday" => Some(start_of(Period::Day, start_of(Period::Day, now) - 1)),
        "week" => Some(start_of(Period::Week, now)),
        "month" => Some(start_of(Period::Month, now)),
        _ => back('h', 1)
            .or_else(|| back('d', 24))
            .or_else(|| back('w', 24 * 7))
            .or_else(|| {
                text.parse::<Date>()
                    .ok()
                    .map(|day| start_of_day(day, &TimeZone::system()))
            })
            .or_else(|| millis(text)),
    };
    found.ok_or_else(|| {
        Error::Usage(format!(
            "{text} isn't a time Turnscope reads: use today, yesterday, week, month, a count back such as 24h, 7d \
             or 4w, a date such as 2026-10-01, or an RFC 3339 time"
        ))
    })
}

/// Whether `at` falls on the same day as `now`, here.
pub fn today(at: i64, now: i64) -> bool {
    local(at).date() == local(now).date()
}

/// A time to come or gone, as a clock says it: `15:00` today, `Mon 09:30`
/// within a week, `Oct 12 09:30` beyond.
pub fn clock(at: i64, now: i64) -> String {
    let at = local(at);
    let format = match (at.date() - local(now).date()).get_days().abs() {
        0 => "%H:%M",
        1..7 => "%a %H:%M",
        _ => "%b %-d %H:%M",
    };
    at.strftime(format).to_string()
}

/// A time as people read it: `15:00 (in 3h)`, or gone by, `09:00 (3h ago)`.
pub fn when(at: i64, now: i64) -> String {
    let span = span(at - now);
    match at >= now {
        true => format!("{} (in {span})", clock(at, now)),
        false => format!("{} ({span} ago)", clock(at, now)),
    }
}

/// A length of time: `45m`, `3h 20m`, `2d 4h`.
pub fn span(ms: i64) -> String {
    let minutes = (ms.abs() + MINUTE / 2) / MINUTE;
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{minutes}m"),
        (0, _) if minutes > 0 => format!("{hours}h {minutes}m"),
        (0, _) => format!("{hours}h"),
        (_, 0) => format!("{days}d"),
        _ => format!("{days}d {hours}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moments_read_as_people_write_them() {
        let now = millis("2026-10-08T15:00:00Z").unwrap();
        let at = |text: &str| parse(text, now).unwrap();
        let day = |date: &str| start_of_day(date.parse().unwrap(), &TimeZone::system());
        assert_eq!(at("today"), day(&local(now).date().to_string()));
        assert_eq!(at("24h"), now - DAY);
        assert_eq!(at("2w"), now - 14 * DAY);
        assert_eq!(at("2026-10-01"), day("2026-10-01"));
        assert_eq!(
            at("month"),
            day(&local(now).date().first_of_month().to_string())
        );
        assert_eq!(
            local(at("week")).date().weekday(),
            jiff::civil::Weekday::Monday
        );
        assert_eq!(
            at("2026-10-01T09:00:00Z"),
            millis("2026-10-01T09:00:00Z").unwrap()
        );
        assert!(matches!(parse("last tuesday", now), Err(Error::Usage(_))));
        // A count back is never ahead.
        assert!(matches!(parse("-2d", now), Err(Error::Usage(_))));
        assert_eq!(span(110 * MINUTE), "1h 50m");
        assert_eq!(span(2 * DAY + 4 * HOUR + 10 * MINUTE), "2d 4h");
    }
}
