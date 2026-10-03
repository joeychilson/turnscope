//! How fast a limit is rising, and when it runs out at that pace.
//!
//! A limit's pace is the slope of its readings over the recent stretch of
//! its current window, up to its latest reading, fitted by least squares,
//! in percentage points an hour: the last hour for a window of a day or
//! less, as five hours are, and the last day for a longer one, as a week
//! is. A week's pace is what a day of use and rest takes of it; the last
//! hour's, one busy hour, would forecast it running out days early, and
//! warn of it. Providers report whole percents, so a pace needs at least
//! fifteen minutes of readings; a shorter stretch would turn one step of
//! rounding into a pace.
//!
//! A day's readings stand for a day only when they cover most of it,
//! eighteen hours. They don't when the window began within the day, when
//! use was given back within it, when the ledger is new, or when the Mac
//! was away for longer than a day, as a laptop closed over a weekend is:
//! fitted, the few busy minutes they hold would forecast a week running out
//! within a day, and spend the week's one warning on it. A longer window's
//! pace is then its average: its use since it began, or since it was given
//! back, over the time since, taken as a day at least. Twenty minutes into
//! a week, 2% used is 2 points a day, not 6 an hour; and a week that is
//! running out is paced from its first reading, as one 80% used in five
//! days is.
//!
//! A limit only rises within its window, unless its provider gives use back,
//! as Anthropic did on 2026-09-29, taking a week from 84% to 5% used with
//! its reset unchanged. The readings before such a fall say nothing of the
//! pace after it, and fitted with those after it, they slope down, so the
//! limit was paced at nothing while it rose: only the readings since the
//! last fall count. A fall before the readings a pace is worked out from,
//! the last day's, is not seen, so a longer window's average then counts
//! its use from its start, and is lower than the use since the fall.

use super::{DAY, HOUR};
use crate::time::Instant;

/// The stretch a pace is fitted over for a window `length` long, where it
/// is known: the last day for a window longer than a day, the last hour
/// for any other.
fn stretch(length: Option<i64>) -> i64 {
    match length {
        Some(length) if length > DAY => DAY,
        _ => HOUR,
    }
}

/// The least stretch of readings a pace is fitted from: fifteen minutes.
const LEAST: i64 = 15 * 60 * 1000;

/// How much of the last day a longer window's readings must cover to be
/// fitted: eighteen hours.
const COVERED: i64 = 18 * HOUR;

/// How far a limit falls from one reading to the next when its provider
/// has given use back: more than a point, so a step of rounding back is not
/// taken for one.
pub(super) const GIVEN_BACK: f64 = 1.0;

/// A limit's pace from `readings` of it in its window, oldest first, a
/// window `length` long that began at `starts`, where known.
///
/// Of the readings since it last fell, it is fitted over those of the last
/// hour, or the last day for a window longer than a day, up to the latest.
/// For a window longer than a day whose readings cover less than eighteen
/// hours of the last, it is instead the window's average since it began,
/// or since it fell, over a day at least. `None` until a fitted stretch
/// spans fifteen minutes, or for an average with no known start, and never
/// below zero. It is the pace as of the latest reading, as when it runs out
/// is, so time passing with no new reading changes neither.
pub(super) fn pace(
    readings: &[(Instant, f64)],
    starts: Option<Instant>,
    length: Option<i64>,
) -> Option<f64> {
    let &(latest, used) = readings.last()?;
    let since = readings
        .windows(2)
        .rposition(|pair| pair[1].1 < pair[0].1 - GIVEN_BACK)
        .map_or(0, |fell| fell + 1);
    let over = stretch(length);
    let recent: Vec<(Instant, f64)> = readings[since..]
        .iter()
        .filter(|(at, _)| latest.millis() - at.millis() <= over)
        .copied()
        .collect();
    let covers = recent
        .first()
        .map_or(0, |(first, _)| latest.millis() - first.millis());
    if over == DAY && covers < COVERED {
        // Use began at the fall, from where it fell to, or at the window's
        // start, from nothing.
        let (from, base) = if since > 0 {
            readings[since]
        } else {
            (starts?, 0.0)
        };
        let hours = (latest.millis() - from.millis()).max(DAY) as f64 / HOUR as f64;
        return Some(((used - base) / hours).max(0.0));
    }
    fitted(&recent)
}

/// The least-squares slope of `readings`, in points an hour: `None` unless
/// they span fifteen minutes, and never below zero.
fn fitted(readings: &[(Instant, f64)]) -> Option<f64> {
    let recent: Vec<(f64, f64)> = readings
        .iter()
        // Hours since the epoch fit an f64 exactly to well below a second.
        .map(|(at, used)| (at.millis() as f64 / HOUR as f64, *used))
        .collect();
    let (first, last) = (recent.first()?, recent.last()?);
    if (last.0 - first.0) * (HOUR as f64) < LEAST as f64 {
        return None;
    }
    let count = recent.len() as f64;
    let mean_hour = recent.iter().map(|(hour, _)| hour).sum::<f64>() / count;
    let mean_used = recent.iter().map(|(_, used)| used).sum::<f64>() / count;
    let spread: f64 = recent
        .iter()
        .map(|(hour, _)| (hour - mean_hour).powi(2))
        .sum();
    let together: f64 = recent
        .iter()
        .map(|(hour, used)| (hour - mean_hour) * (used - mean_used))
        .sum();
    (spread > 0.0).then(|| (together / spread).max(0.0))
}

/// When a limit `used` percent full at `at` reaches `share` percent at `pace`
/// points an hour, or `None` when it is not rising or is there already.
pub(super) fn reaches(used: f64, pace: f64, at: Instant, share: f64) -> Option<Instant> {
    if pace <= 0.0 || used >= share {
        return None;
    }
    let hours = (share - used) / pace;
    // Beyond a thousand years is never, as far as a window is concerned.
    // To the nearest millisecond, so a fitted pace a hair from exact does not
    // move the moment by one.
    (hours < 8_760_000.0)
        .then(|| Instant::from_millis(at.millis() + (hours * HOUR as f64).round() as i64))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::{DAY, HOUR, pace, reaches, stretch};
    use crate::time::Instant;

    fn at(minutes: i64) -> Instant {
        Instant::from_millis(1_789_000_000_000 + minutes * 60_000).unwrap()
    }

    /// Five hours, in milliseconds, as a window's length.
    const FIVE_HOURS: Option<i64> = Some(5 * HOUR);

    /// A week, in milliseconds, as a window's length.
    const WEEK: Option<i64> = Some(7 * DAY);

    #[test]
    fn a_steady_rise_is_its_pace() {
        // 2 points every 10 minutes is 12 points an hour.
        let readings = [
            (at(0), 40.0),
            (at(10), 42.0),
            (at(20), 44.0),
            (at(30), 46.0),
        ];
        let pace = pace(&readings, None, FIVE_HOURS).unwrap();
        assert!((pace - 12.0).abs() < 1e-6);
        // 54 points left at 12 an hour is 4.5 hours after the last reading.
        assert_eq!(reaches(46.0, pace, at(30), 100.0), Some(at(30 + 270)));
    }

    #[test]
    fn a_pace_needs_fifteen_minutes_of_the_last_hour() {
        assert_eq!(
            pace(&[(at(0), 40.0), (at(10), 45.0)], None, FIVE_HOURS),
            None
        );
        // Readings from more than an hour before the latest do not count.
        assert_eq!(
            pace(
                &[(at(0), 10.0), (at(80), 40.0), (at(90), 41.0)],
                None,
                FIVE_HOURS
            ),
            None
        );
        assert_eq!(
            pace(&[(at(0), 50.0), (at(20), 49.5)], None, FIVE_HOURS),
            Some(0.0),
            "a limit a step of rounding lower is not rising"
        );
    }

    #[test]
    fn a_limit_given_back_is_paced_from_where_it_fell() {
        // A week at 84% until given back, then 5, 6 and 7 over the last
        // twenty hours, covering enough of the day to be fitted: a point
        // each ten hours, 0.1 an hour. Fitted with the 84 it would slope
        // down, and be paced at nothing.
        let readings = [
            (at(-2_000), 84.0),
            (at(-1_200), 5.0),
            (at(-600), 6.0),
            (at(0), 7.0),
        ];
        let pace = pace(&readings, Some(at(-5_000)), WEEK).unwrap();
        assert!((pace - 0.1).abs() < 1e-6, "{pace}");
        // Given back an hour ago, the hour since covers too little of the
        // day to fit: the 2 points risen since the fall, over a day, are
        // 1/12 of a point an hour, where the hour alone would be 2.
        let lately = [
            (at(-400), 84.0),
            (at(-60), 5.0),
            (at(-30), 6.0),
            (at(0), 7.0),
        ];
        let pace = super::pace(&lately, Some(at(-5_000)), WEEK).unwrap();
        assert!((pace - 2.0 / 24.0).abs() < 1e-9, "{pace}");
        // Five hours given back just now: nothing since spans fifteen
        // minutes.
        assert_eq!(
            super::pace(&[(at(-20), 50.0), (at(0), 48.0)], None, FIVE_HOURS),
            None
        );
        // A point back is a step of rounding, not use given back: in hours
        // 0, 1/6, 1/3 and 1/2, 40, 42, 41 and 44 have mean hour 0.25 and
        // mean use 41.75; the products of their deviations sum to 0.91667
        // and the hours' squared deviations to 0.13889, 6.6 an hour.
        let rounded = [
            (at(0), 40.0),
            (at(10), 42.0),
            (at(20), 41.0),
            (at(30), 44.0),
        ];
        let pace = super::pace(&rounded, None, FIVE_HOURS).unwrap();
        assert!((pace - 6.6).abs() < 1e-6, "{pace}");
    }

    #[test]
    fn a_week_is_paced_by_its_last_day_and_five_hours_by_their_last_hour() {
        // A day ago 40, then 44, 47 and 50 over the last hour. The last
        // hour alone is 3 points each half hour: 6 an hour. The day, in
        // hours from now (-24, 40), (-1, 44), (-0.5, 47), (0, 50): mean hour
        // -6.375, mean use 45.25; the sum of the products of their
        // deviations 126.375 over the sum of the hours' squared deviations
        // 414.6875 is 0.3047 an hour, a day of work and rest.
        let readings = [
            (at(-1_440), 40.0),
            (at(-60), 44.0),
            (at(-30), 47.0),
            (at(0), 50.0),
        ];
        let hourly = pace(&readings, None, FIVE_HOURS).unwrap();
        assert!((hourly - 6.0).abs() < 1e-6, "{hourly}");
        let daily = pace(&readings, Some(at(-3 * 1_440)), WEEK).unwrap();
        assert!((daily - 126.375 / 414.6875).abs() < 1e-6, "{daily}");
        // A week's window is paced over a day; five hours', or one of no
        // known length, over an hour.
        assert_eq!(stretch(WEEK), DAY);
        assert_eq!(stretch(FIVE_HOURS), HOUR);
        assert_eq!(stretch(None), HOUR);
    }

    #[test]
    fn a_week_is_fitted_only_over_most_of_a_day() {
        // Three days into a week, from 20% to 38% over the last 18 hours:
        // covering enough of the day, fitted, 18 points in 18 hours, 1 an
        // hour.
        let starts = Some(at(-3 * 1_440));
        let covered = [(at(-1_080), 20.0), (at(0), 38.0)];
        let pace = pace(&covered, starts, WEEK).unwrap();
        assert!((pace - 1.0).abs() < 1e-9, "{pace}");
        // Over 17, too little: the week's average, 38 points in its 72
        // hours, 0.5278 an hour.
        let short = [(at(-1_020), 20.0), (at(0), 38.0)];
        let pace = super::pace(&short, starts, WEEK).unwrap();
        assert!((pace - 38.0 / 72.0).abs() < 1e-9, "{pace}");
    }

    #[test]
    fn a_week_just_begun_or_come_back_to_is_not_paced_by_its_first_minutes() {
        // Twenty minutes into a week, 0, 0, 1, 1 and 2%: fitted, 6 points
        // an hour, running out in 16 hours. Its 2 points, over a day at
        // least, are 1/12 of a point an hour.
        let begun = [
            (at(10), 0.0),
            (at(15), 0.0),
            (at(20), 1.0),
            (at(25), 1.0),
            (at(30), 2.0),
        ];
        let pace = pace(&begun, Some(at(0)), WEEK).unwrap();
        assert!((pace - 2.0 / 24.0).abs() < 1e-9, "{pace}");
        // Friday at 30%, a laptop closed over the weekend, and Monday 30,
        // 30, 31, 31 and 32% over twenty minutes, four days into the week:
        // 32 points in its 96 hours, 1/3 of a point an hour.
        let monday = [
            (at(-4_320), 30.0),
            (at(-20), 30.0),
            (at(-15), 30.0),
            (at(-10), 31.0),
            (at(-5), 31.0),
            (at(0), 32.0),
        ];
        let pace = super::pace(&monday, Some(at(-4 * 1_440)), WEEK).unwrap();
        assert!((pace - 1.0 / 3.0).abs() < 1e-9, "{pace}");
        // A week 80% used five days in is paced from its first reading: 80
        // points in 120 hours, 2/3 of a point an hour.
        let late = [(at(0), 80.0)];
        let pace = super::pace(&late, Some(at(-5 * 1_440)), WEEK).unwrap();
        assert!((pace - 2.0 / 3.0).abs() < 1e-9, "{pace}");
        // With no start known and nothing fitted, no pace.
        assert_eq!(super::pace(&late, None, WEEK), None);
    }

    #[test]
    fn a_full_or_idle_limit_does_not_run_out() {
        assert_eq!(reaches(100.0, 5.0, at(0), 100.0), None);
        assert_eq!(reaches(40.0, 0.0, at(0), 100.0), None);
    }
}
