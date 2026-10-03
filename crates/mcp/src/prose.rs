//! How an answer's sentences say figures: times, lengths of time, token
//! counts, shares and money.
//!
//! An answer leads with sentences an agent can act on and repeat to the
//! person, so they say figures the way the person would: a time as a clock
//! reads it, near ones by their weekday, a length of time in hours and
//! minutes, tokens in thousands and millions. The exact figures, and times
//! with their offsets, are in the answer's data beside them.

use std::path::Path;

use jiff::Zoned;
use jiff::tz::TimeZone;
use turnscope_engine::Instant;

/// `at` as a clock reads it, as seen at `now` in `zone`: `4:10 PM` on the
/// same day, `Thu 9:00 AM` within six days either way, `Oct 3, 9:00 AM`
/// otherwise in the same year, and `Oct 3 2025, 9:00 AM` in another.
pub(crate) fn clock(at: Instant, now: Instant, zone: &TimeZone) -> String {
    let (at, now) = (zoned(at, zone), zoned(now, zone));
    let days = (at.date() - now.date()).get_days();
    let time = at.strftime("%-I:%M %p").to_string();
    if days == 0 {
        time
    } else if days.abs() <= 6 {
        format!("{} {time}", at.strftime("%a"))
    } else if at.year() == now.year() {
        format!("{}, {time}", at.strftime("%b %-d"))
    } else {
        format!("{}, {time}", at.strftime("%b %-d %Y"))
    }
}

/// The day of `at` as the person would say it, as seen at `now` in `zone`:
/// `today`, `yesterday`, a weekday within six days, or a date.
pub(crate) fn day(at: Instant, now: Instant, zone: &TimeZone) -> String {
    let (at, now) = (zoned(at, zone), zoned(now, zone));
    match (at.date() - now.date()).get_days() {
        0 => "today".to_owned(),
        -1 => "yesterday".to_owned(),
        -6..=-2 => at.strftime("%a").to_string(),
        _ if at.year() == now.year() => at.strftime("%b %-d").to_string(),
        _ => at.strftime("%b %-d %Y").to_string(),
    }
}

/// The time of day of `at` in `zone`, as `11:02 AM`.
pub(crate) fn hour(at: Instant, zone: &TimeZone) -> String {
    zoned(at, zone).strftime("%-I:%M %p").to_string()
}

fn zoned(at: Instant, zone: &TimeZone) -> Zoned {
    at.timestamp().to_zoned(zone.clone())
}

/// A length of time, `millis` long, as `45m`, `1h 30m`, `18h` or `2d 3h`;
/// under a minute as `under a minute`. A negative length reads as none.
pub(crate) fn span(millis: i64) -> String {
    let minutes = millis.max(0) / 60_000;
    let (days, hours, minutes) = (minutes / 1_440, minutes % 1_440 / 60, minutes % 60);
    match (days, hours, minutes) {
        (0, 0, 0) => "under a minute".to_owned(),
        (0, 0, minutes) => format!("{minutes}m"),
        (0, hours, 0) => format!("{hours}h"),
        (0, hours, minutes) => format!("{hours}h {minutes}m"),
        (days, 0, _) => format!("{days}d"),
        (days, hours, _) => format!("{days}d {hours}h"),
    }
}

/// A count of tokens as `950`, `182K`, `1.2M` or `3.4B`: to three
/// significant figures at most, rounded down from a thousand up, so a count
/// never reads as more than it is.
pub(crate) fn tokens(count: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1_000_000_000, "B"), (1_000_000, "M"), (1_000, "K")];
    for (size, unit) in UNITS {
        if count >= size {
            let whole = count / size;
            // Tenths only while they take the figure to three digits.
            return if whole < 100 {
                let tenths = count % size / (size / 10);
                if tenths == 0 {
                    format!("{whole}{unit}")
                } else {
                    format!("{whole}.{tenths}{unit}")
                }
            } else {
                format!("{whole}{unit}")
            };
        }
    }
    count.to_string()
}

/// A share of a limit, in points of its percent, as `9.1%`: to a tenth,
/// and below a tenth as `under 0.1%` rather than nothing.
pub(crate) fn share(points: f64) -> String {
    if points > 0.0 && points < 0.05 {
        return "under 0.1%".to_owned();
    }
    format!("{:.1}%", points.max(0.0))
}

/// A whole percent, as `18%`.
pub(crate) fn percent(value: f64) -> String {
    format!("{:.0}%", value)
}

/// What is left of a limit, in percent, as a whole percent rounded down, as
/// the app says it: 99.6% left is `99%`, never a full `100%`.
pub(crate) fn left(percent: f64) -> String {
    format!("{:.0}%", percent.clamp(0.0, 100.0).floor())
}

/// What is used of a limit, in percent, as a whole percent rounded up, so
/// that it and what is left, said by [`left`], come to 100%.
pub(crate) fn used(percent: f64) -> String {
    format!("{:.0}%", percent.clamp(0.0, 100.0).ceil())
}

/// Money, `dollars` of it, as `$412.50`, `$0.004` below a cent, and `under
/// $0.001` below that rather than nothing.
pub(crate) fn money(dollars: f64) -> String {
    if dollars > 0.0 && dollars < 0.001 {
        "under $0.001".to_owned()
    } else if dollars > 0.0 && dollars < 0.01 {
        format!("${dollars:.3}")
    } else {
        format!("${dollars:.2}")
    }
}

/// `count` of `noun`, as `1 response` or `3 responses`.
pub(crate) fn count(count: u64, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// `items` said as a list: `a`, `a and b`, `a, b and c`.
pub(crate) fn list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `text` on one line, runs of whitespace made single spaces, cut to `most`
/// characters with an ellipsis where it was cut.
pub(crate) fn line(text: &str, most: usize) -> String {
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match joined.char_indices().nth(most) {
        Some((cut, _)) => format!("{}\u{2026}", &joined[..cut]),
        None => joined,
    }
}

/// `path` as the person would write it, the home folder `home` as `~`.
pub(crate) fn folder(path: &str, home: &Path) -> String {
    let home = home.to_string_lossy();
    match path.strip_prefix(home.trim_end_matches('/')) {
        Some("") => "~".to_owned(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

/// `text` with its first letter capitalized, as a sentence starts.
pub(crate) fn capitalized(text: &str) -> String {
    let mut characters = text.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().chain(characters).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use jiff::tz::TimeZone;
    use turnscope_engine::Instant;

    use super::{clock, day, folder, left, list, money, share, span, tokens, used};

    fn at(text: &str) -> Instant {
        Instant::parse(text).unwrap()
    }

    #[test]
    fn times_read_as_a_clock_does_near_ones_by_weekday() {
        let chicago = TimeZone::get("America/Chicago").unwrap();
        // Tuesday 29 September 2026, 2:00 PM in Chicago, five hours behind.
        let now = at("2026-09-29T19:00:00Z");
        assert_eq!(clock(at("2026-09-29T21:10:00Z"), now, &chicago), "4:10 PM");
        assert_eq!(
            clock(at("2026-10-01T14:00:00Z"), now, &chicago),
            "Thu 9:00 AM"
        );
        assert_eq!(
            clock(at("2026-10-12T14:00:00Z"), now, &chicago),
            "Oct 12, 9:00 AM"
        );
        assert_eq!(
            clock(at("2025-10-12T14:00:00Z"), now, &chicago),
            "Oct 12 2025, 9:00 AM"
        );
        // 04:00 UTC is 11:00 PM the night before in Chicago.
        assert_eq!(day(at("2026-09-29T04:00:00Z"), now, &chicago), "yesterday");
        assert_eq!(day(at("2026-09-29T15:00:00Z"), now, &chicago), "today");
        assert_eq!(day(at("2026-09-27T15:00:00Z"), now, &chicago), "Sun");
    }

    #[test]
    fn lengths_of_time_read_in_days_hours_and_minutes() {
        let minutes = |minutes: i64| minutes * 60_000;
        assert_eq!(span(minutes(0)), "under a minute");
        assert_eq!(span(minutes(45)), "45m");
        assert_eq!(span(minutes(90)), "1h 30m");
        assert_eq!(span(minutes(18 * 60)), "18h");
        // 2 days, 3 hours and 20 minutes: the minutes are left out.
        assert_eq!(span(minutes(2 * 1_440 + 3 * 60 + 20)), "2d 3h");
        assert_eq!(span(-5), "under a minute");
    }

    #[test]
    fn counts_read_to_three_figures_never_more_than_they_are() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(182_499), "182K");
        // 1,299,999 is 1.2 million and a bit, never 1.3.
        assert_eq!(tokens(1_299_999), "1.2M");
        assert_eq!(tokens(3_000_000_000), "3B");
        assert_eq!(tokens(45_600), "45.6K");
        assert_eq!(share(9.14), "9.1%");
        assert_eq!(share(0.01), "under 0.1%");
        // 0.4% used is 99.6% left: 99% left and 1% used, never a full 100%.
        assert_eq!(left(99.6), "99%");
        assert_eq!(used(0.4), "1%");
        assert_eq!(left(100.0), "100%");
        assert_eq!(left(-2.0), "0%");
        assert_eq!(money(412.5), "$412.50");
        assert_eq!(money(0.004), "$0.004");
        assert_eq!(money(0.0004), "under $0.001");
        assert_eq!(money(0.0), "$0.00");
        let items = ["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(list(&items), "a, b and c");
        assert_eq!(list(&items[..1]), "a");
        let home = Path::new("/Users/joey");
        assert_eq!(folder("/Users/joey/work/atlas", home), "~/work/atlas");
        assert_eq!(folder("/Users/joeyb/x", home), "/Users/joeyb/x");
    }
}
