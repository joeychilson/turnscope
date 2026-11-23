//! Time as people read and write it, in the local time zone (`TZ`). Inside
//! Turnscope a time is UTC milliseconds.

use std::cmp::Ordering;

use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan, ZonedDateTime};

use crate::{Error, Result};

pub const MINUTE: i64 = 60_000;
pub const HOUR: i64 = 60 * MINUTE;
pub const DAY: i64 = 24 * HOUR;

pub fn minute(at: i64) -> i64 {
    at.div_euclid(MINUTE) * MINUTE
}

pub fn round_minute(at: i64) -> i64 {
    (at + MINUTE / 2).div_euclid(MINUTE) * MINUTE
}

pub fn millis(text: &str) -> Option<i64> {
    text.parse::<Timestamp>().ok().map(|at| at.as_millisecond())
}

pub fn local(at: i64) -> ZonedDateTime {
    Timestamp::from_millisecond(at)
        .unwrap_or_default()
        .to_zoned_datetime(TimeZone::system())
}

#[derive(Clone, Copy)]
pub enum Period {
    Day,
    Week,
    Month,
}

pub fn start_of(period: Period, at: i64) -> i64 {
    let zone = TimeZone::system();
    start_of_day(period_date(period, at, &zone), &zone)
}

pub fn period_date(period: Period, at: i64, zone: &TimeZone) -> Date {
    let day = Timestamp::from_millisecond(at)
        .unwrap_or_default()
        .to_zoned_datetime(zone.clone())
        .date();
    start_date(period, day).0
}

fn start_date(period: Period, day: Date) -> (Date, jiff::Span) {
    match period {
        Period::Day => (day, 1.day()),
        Period::Week => (
            day.checked_sub((day.weekday().to_monday_zero_offset() as i64).days())
                .unwrap_or(day),
            1.week(),
        ),
        Period::Month => (day.first_of_month(), 1.month()),
    }
}

pub fn window(period: Period, at: i64, zone: &TimeZone) -> (i64, i64) {
    let day = Timestamp::from_millisecond(at)
        .unwrap_or_default()
        .to_zoned_datetime(zone.clone())
        .date();
    let (start, span) = start_date(period, day);
    let end = start.checked_add(span).unwrap_or(start);
    (start_of_day(start, zone), start_of_day(end, zone))
}

fn start_of_day(day: Date, zone: &TimeZone) -> i64 {
    day.to_zoned_datetime(zone.clone())
        .map(|zoned| zoned.timestamp().as_millisecond())
        .unwrap_or_default()
}

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
            // A date alone. Parsed as a `Date`, an RFC 3339 time with an
            // offset such as `+02:00` would be its date, at midnight.
            .or_else(|| {
                Date::strptime("%Y-%m-%d", text)
                    .ok()
                    .map(|day| start_of_day(day, &TimeZone::system()))
            })
            .or_else(|| millis(text)),
    };
    found.ok_or_else(|| {
        Error::Usage(format!(
            "{text} isn't a time Turnscope reads: use today, yesterday, week, month, a count back such as 24h, 7d \
             or 4w, a date such as 2026-10-01, an RFC 3339 time such as 2026-10-01T09:00:00Z, or now"
        ))
    })
}

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

/// A time to come, as people read it: `15:00 (in 3h)`.
pub fn ahead(at: i64, now: i64) -> String {
    told(at, now, Ordering::Greater)
}

/// A time gone by, as people read it: `09:00 (3h ago)`.
pub fn ago(at: i64, now: i64) -> String {
    told(at, now, Ordering::Less)
}

/// `at` as far off as the clocks say, minute to minute: at 12:00:40,
/// 11:00:40 is 11:00, 1h ago. Counted from the second, an answer said 59m
/// beside since 11:00 (1h ago). In now's own minute, `in_minute` says which
/// way it reads: a read then has gone by, a reset or run-out is to come.
fn told(at: i64, now: i64, in_minute: Ordering) -> String {
    let span = span(minute(at) - minute(now));
    match minute(at).cmp(&minute(now)).then(in_minute) {
        Ordering::Greater => format!("{} (in {span})", clock(at, now)),
        _ => format!("{} ({span} ago)", clock(at, now)),
    }
}

/// A length of time: `45m`, `3h 20m`, `2d 4h`.
pub fn span(ms: i64) -> String {
    let minutes = ms.unsigned_abs().saturating_add((MINUTE / 2) as u64) / MINUTE as u64;
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
        let nine = millis("2026-10-01T09:00:00Z").unwrap();
        assert_eq!(at("2026-10-01T09:00:00Z"), nine);
        assert_eq!(at("2026-10-01T09:00:00+00:00"), nine);
        assert_eq!(at("2026-10-01T04:00:00-05:00"), nine);
        // With no offset, it could be any time zone's.
        assert!(matches!(
            parse("2026-10-01T09:00", now),
            Err(Error::Usage(_))
        ));
        assert!(matches!(parse("last tuesday", now), Err(Error::Usage(_))));
        // A count back is never ahead.
        assert!(matches!(parse("-2d", now), Err(Error::Usage(_))));
        assert_eq!(span(110 * MINUTE), "1h 50m");
        // By the minutes the clocks show. At 15:00:40, 14:00:40 is an hour
        // back, and 16:00:05 an hour ahead.
        let later = now + 40_000;
        let (back, coming) = (later - HOUR, later + HOUR - 35_000);
        assert_eq!(ago(back, later), format!("{} (1h ago)", clock(back, later)));
        assert_eq!(
            ahead(coming, later),
            format!("{} (in 1h)", clock(coming, later))
        );
        // In another minute, a time says which way it is itself.
        assert_eq!(ahead(back, later), ago(back, later));
        assert_eq!(ago(coming, later), ahead(coming, later));
        // In now's own minute, the caller says. As of 15:00, a read at
        // 15:00:10 has gone by, and a reset rounded to 15:00 is to come.
        for at in [now + 10_000, now] {
            assert_eq!(ago(at, now), format!("{} (0m ago)", clock(at, now)));
            assert_eq!(ahead(at, now), format!("{} (in 0m)", clock(at, now)));
        }
        assert_eq!(span(2 * DAY + 4 * HOUR + 10 * MINUTE), "2d 4h");
    }

    /// Each case above works out its expected time in the machine's zone,
    /// as `parse` does, so in UTC none crosses a change of the clocks.
    #[test]
    fn a_day_lasts_as_long_as_the_clocks_say() {
        let zone = TimeZone::get("America/New_York").unwrap();
        // The clocks went forward an hour on 2026-03-08, and back on
        // 2026-11-01.
        for (noon, hours) in [
            ("2026-03-08T12:00:00-04:00", 23),
            ("2026-11-01T12:00:00-05:00", 25),
            ("2026-11-02T12:00:00-05:00", 24),
        ] {
            let (start, end) = window(Period::Day, millis(noon).unwrap(), &zone);
            assert_eq!(end - start, hours * HOUR, "{noon}");
        }
    }
}
