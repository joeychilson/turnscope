//! Times as agents write and read them.
//!
//! Every time an answer gives is local, with its offset, so an agent can read
//! it as the person would and still compare it exactly.
//!
//! A `since` or `until` is written the way a person, or an agent echoing
//! one, says it: `now`; a stretch of the calendar, `today`, `yesterday`, a
//! weekday, `this week` or `last month`, a date or a month, which starts as
//! a `since` and ends as an `until`, so that `since` and `until` both
//! `last week` are last week alone; a span back from now, `30m`, `6h`, `7d`,
//! `2w`, `3mo`, `24 hours ago`, `last 7 days` or `past hour`; or a time,
//! local unless it carries an offset. Weeks start on Monday, as usage's
//! weeks do. `last week` is the calendar's week before this one, and `past
//! week` the seven days back from now, as people use them. Minutes, hours,
//! days and weeks back are exact lengths of time; months and years are the
//! calendar's, so `1mo` back from October 31 is September 30.

use jiff::civil::{Date, DateTime, Weekday};
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use turnscope_engine::Instant;

/// What a `since` or `until` can be, for tool descriptions and refusals.
pub(crate) const MOMENTS: &str = "now; today, yesterday, a weekday, this or last \
     week/month/year, a date (2026-09-18) or a month (2026-09), which since starts and until \
     ends; a span back such as 30m, 6h, 7d, 2w, 3mo or \"past 24 hours\"; or a time such as \
     2026-09-18T14:00, local unless it has an offset";

/// Which end of a span a moment is written for. A stretch of the calendar as
/// a `since` starts with it, and as an `until` ends with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum End {
    Since,
    Until,
}

/// The instant `text` names, at the `end` of a span, reading the calendar in
/// `zone` and spans back from `now`. `None` when it names none.
pub(crate) fn moment(text: &str, end: End, now: Timestamp, zone: &TimeZone) -> Option<Instant> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = text.to_lowercase();
    if text == "now" {
        return Some(Instant::from(now));
    }
    let today = now.to_zoned(zone.clone()).date();
    if let Some((first, after)) = stretch(&text, today) {
        let day = match end {
            End::Since => first,
            End::Until => after,
        };
        return Some(Instant::from(day.to_zoned(zone.clone()).ok()?.timestamp()));
    }
    if let Some(back) = span_back(&text) {
        let then = match back {
            Back::Exact(length) => now.checked_sub(length).ok()?,
            Back::Calendar(span) => now
                .to_zoned(zone.clone())
                .checked_sub(span)
                .ok()?
                .timestamp(),
        };
        return Some(Instant::from(then));
    }
    if let Ok(exact) = text.parse::<Timestamp>() {
        return Some(Instant::from(exact));
    }
    // A time without an offset is the person's own clock's.
    let local = text.parse::<DateTime>().ok()?;
    Some(Instant::from(
        local.to_zoned(zone.clone()).ok()?.timestamp(),
    ))
}

/// The stretch of the calendar `text` names, seen from `today`: its first
/// day, and the first day after it. `None` when it names none.
fn stretch(text: &str, today: Date) -> Option<(Date, Date)> {
    let day = |date: Date| Some((date, date.tomorrow().ok()?));
    let monday = today
        .checked_sub(jiff::Span::new().days(today.weekday().to_monday_zero_offset()))
        .ok()?;
    let week = |first: Date| Some((first, first.checked_add(jiff::Span::new().weeks(1)).ok()?));
    let month = |first: Date| Some((first, first.checked_add(jiff::Span::new().months(1)).ok()?));
    let year = |first: Date| Some((first, first.checked_add(jiff::Span::new().years(1)).ok()?));
    let before = |first: Date, length: jiff::Span| first.checked_sub(length).ok();
    match text {
        "today" => day(today),
        "yesterday" => day(today.yesterday().ok()?),
        "week" | "this week" => week(monday),
        "last week" | "previous week" => week(before(monday, jiff::Span::new().weeks(1))?),
        "month" | "this month" => month(today.first_of_month()),
        "last month" | "previous month" => {
            month(before(today.first_of_month(), jiff::Span::new().months(1))?)
        }
        "year" | "this year" => year(today.first_of_year()),
        "last year" | "previous year" => {
            year(before(today.first_of_year(), jiff::Span::new().years(1))?)
        }
        _ => {
            // The latest of that weekday: today's own for `monday` on a
            // Monday, the one before for `last monday`.
            let (strictly, name) = match text.strip_prefix("last ") {
                Some(name) => (true, name),
                None => (false, text),
            };
            if let Some(weekday) = weekday(name) {
                let back = (today.weekday().to_monday_zero_offset()
                    - weekday.to_monday_zero_offset())
                .rem_euclid(7);
                let back = if strictly && back == 0 { 7 } else { back };
                return day(today.checked_sub(jiff::Span::new().days(back)).ok()?);
            }
            if strictly {
                return None;
            }
            // Only a date or a month: jiff would read a whole time as its
            // date.
            match text.len() {
                10 => day(text.parse::<Date>().ok()?),
                7 => month(format!("{text}-01").parse::<Date>().ok()?),
                _ => None,
            }
        }
    }
}

/// The weekday `name` names, in full or by its first three letters.
fn weekday(name: &str) -> Option<Weekday> {
    const DAYS: [(&str, Weekday); 7] = [
        ("monday", Weekday::Monday),
        ("tuesday", Weekday::Tuesday),
        ("wednesday", Weekday::Wednesday),
        ("thursday", Weekday::Thursday),
        ("friday", Weekday::Friday),
        ("saturday", Weekday::Saturday),
        ("sunday", Weekday::Sunday),
    ];
    DAYS.into_iter()
        .find(|(full, _)| name == *full || (name.len() == 3 && full.starts_with(name)))
        .map(|(_, weekday)| weekday)
}

/// How far back a span reaches.
enum Back {
    /// An exact length of time: minutes, hours, days or weeks.
    Exact(jiff::SignedDuration),
    /// Months or years, as the calendar counts them.
    Calendar(jiff::Span),
}

/// The span back from now `text` names: a whole number and a unit, as `7d`,
/// `7 days`, `7 days ago`, `last 7 days` or `past 7 days`, the number left
/// out after `last` or `past`, as `past hour`, or written `a` or `an`, as
/// `an hour ago`. `None` for none, or for one longer than a duration holds.
fn span_back(text: &str) -> Option<Back> {
    let bare = text.strip_prefix("the ").unwrap_or(text);
    let (text, counted) = match bare
        .strip_prefix("last ")
        .or_else(|| bare.strip_prefix("past "))
    {
        Some(rest) => (rest, false),
        None => (text, true),
    };
    let text = text.strip_suffix(" ago").unwrap_or(text);
    // The number, and the unit after it, with or without a space.
    let digits = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (count, unit) = match (&text[..digits], text[digits..].trim_start()) {
        ("", unit) => match unit.split_once(' ') {
            Some(("a" | "an" | "one", unit)) => (1, unit),
            // A unit alone is one of it only where `last` or `past` said so.
            _ if !counted => (1, unit),
            _ => return None,
        },
        (number, unit) => (number.parse::<i64>().ok()?, unit),
    };
    let minutes = match unit {
        "m" | "min" | "mins" | "minute" | "minutes" => 1,
        "h" | "hr" | "hrs" | "hour" | "hours" => 60,
        "d" | "day" | "days" => 24 * 60,
        "w" | "wk" | "wks" | "week" | "weeks" => 7 * 24 * 60,
        "mo" | "month" | "months" => {
            return Some(Back::Calendar(jiff::Span::new().try_months(count).ok()?));
        }
        "y" | "yr" | "yrs" | "year" | "years" => {
            return Some(Back::Calendar(jiff::Span::new().try_years(count).ok()?));
        }
        _ => return None,
    };
    jiff::SignedDuration::try_from_mins(count.checked_mul(minutes)?).map(Back::Exact)
}

/// `at` in `zone`, as `2026-09-23T14:03:05-05:00`.
pub(crate) fn local(at: Instant, zone: &TimeZone) -> String {
    written(at, zone, "%Y-%m-%dT%H:%M:%S%:z")
}

/// `at` in `zone` for a line of a transcript, as `2026-09-23 14:03`.
pub(crate) fn short(at: Instant, zone: &TimeZone) -> String {
    written(at, zone, "%Y-%m-%d %H:%M")
}

fn written(at: Instant, zone: &TimeZone, format: &str) -> String {
    Zoned::new(at.timestamp(), zone.clone())
        .strftime(format)
        .to_string()
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use jiff::tz::TimeZone;

    use super::{End, local, moment};

    fn chicago() -> TimeZone {
        TimeZone::get("America/Chicago").unwrap()
    }

    /// Wednesday 2026-09-23 15:30 in Chicago, five hours behind UTC in
    /// September.
    fn now() -> Timestamp {
        "2026-09-23T20:30:00Z".parse().unwrap()
    }

    fn at(text: &str, end: End) -> String {
        local(moment(text, end, now(), &chicago()).unwrap(), &chicago())
    }

    #[test]
    fn days_start_and_end_at_local_midnight() {
        assert_eq!(at("today", End::Since), "2026-09-23T00:00:00-05:00");
        assert_eq!(at("Today", End::Until), "2026-09-24T00:00:00-05:00");
        assert_eq!(at("yesterday", End::Since), "2026-09-22T00:00:00-05:00");
        assert_eq!(at("2026-09-18", End::Until), "2026-09-19T00:00:00-05:00");
        // Clocks go back on 2026-11-01, so its midnight has a different offset.
        assert_eq!(at("2026-11-01", End::Until), "2026-11-02T00:00:00-06:00");
        // The 23rd is a Wednesday: Monday is the 21st, and `wed` today.
        assert_eq!(at("monday", End::Since), "2026-09-21T00:00:00-05:00");
        assert_eq!(at("wed", End::Since), "2026-09-23T00:00:00-05:00");
        assert_eq!(
            at("last wednesday", End::Since),
            "2026-09-16T00:00:00-05:00"
        );
        assert_eq!(at("Thursday", End::Since), "2026-09-17T00:00:00-05:00");
    }

    #[test]
    fn weeks_months_and_years_start_and_end_with_the_calendar() {
        // This week runs from Monday the 21st to Monday the 28th; the one
        // before from the 14th.
        assert_eq!(at("this week", End::Since), "2026-09-21T00:00:00-05:00");
        assert_eq!(at("this week", End::Until), "2026-09-28T00:00:00-05:00");
        assert_eq!(at("last week", End::Since), "2026-09-14T00:00:00-05:00");
        assert_eq!(at("last  week", End::Until), "2026-09-21T00:00:00-05:00");
        assert_eq!(at("this month", End::Since), "2026-09-01T00:00:00-05:00");
        // August 1 in Chicago is on daylight time too.
        assert_eq!(at("last month", End::Since), "2026-08-01T00:00:00-05:00");
        assert_eq!(at("last month", End::Until), "2026-09-01T00:00:00-05:00");
        assert_eq!(at("2026-02", End::Until), "2026-03-01T00:00:00-06:00");
        // January 1 is on standard time, six hours behind.
        assert_eq!(at("this year", End::Since), "2026-01-01T00:00:00-06:00");
        assert_eq!(at("last year", End::Since), "2025-01-01T00:00:00-06:00");
    }

    #[test]
    fn spans_count_back_from_now() {
        assert_eq!(at("now", End::Until), "2026-09-23T15:30:00-05:00");
        for (written, back) in [
            ("30m", "2026-09-23T15:00:00-05:00"),
            ("6h", "2026-09-23T09:30:00-05:00"),
            ("24 hours ago", "2026-09-22T15:30:00-05:00"),
            ("past 24 hours", "2026-09-22T15:30:00-05:00"),
            ("last 90 minutes", "2026-09-23T14:00:00-05:00"),
            ("an hour ago", "2026-09-23T14:30:00-05:00"),
            ("past hour", "2026-09-23T14:30:00-05:00"),
            ("7d", "2026-09-16T15:30:00-05:00"),
            ("7 days", "2026-09-16T15:30:00-05:00"),
            ("last 7 days", "2026-09-16T15:30:00-05:00"),
            ("past week", "2026-09-16T15:30:00-05:00"),
            ("4w", "2026-08-26T15:30:00-05:00"),
            ("2 weeks ago", "2026-09-09T15:30:00-05:00"),
            // A month back is the calendar's: August 23, same time of day.
            ("1mo", "2026-08-23T15:30:00-05:00"),
            ("the past 3 months", "2026-06-23T15:30:00-05:00"),
            ("1y", "2025-09-23T15:30:00-05:00"),
        ] {
            assert_eq!(at(written, End::Since), back, "{written}");
        }
    }

    #[test]
    fn times_are_local_unless_they_carry_an_offset() {
        assert_eq!(
            at("2026-09-18T14:00:00+02:00", End::Since),
            "2026-09-18T07:00:00-05:00"
        );
        assert_eq!(
            at("2026-09-18T14:00:00Z", End::Since),
            "2026-09-18T09:00:00-05:00"
        );
        assert_eq!(
            at("2026-09-18T14:00", End::Since),
            "2026-09-18T14:00:00-05:00"
        );
        assert_eq!(
            at("2026-09-18 14:00:30", End::Since),
            "2026-09-18T14:00:30-05:00"
        );
    }

    #[test]
    fn what_names_no_moment_is_refused() {
        // The last is more minutes than a duration holds as seconds.
        for refused in [
            "soon",
            "-3d",
            "3x",
            "",
            "d",
            "hour",
            "last",
            "last soon",
            "next week",
            "2026-13-01",
            "200000000000000000m",
        ] {
            assert_eq!(
                moment(refused, super::End::Since, now(), &chicago()),
                None,
                "{refused}"
            );
        }
    }
}
