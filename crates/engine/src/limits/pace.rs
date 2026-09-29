//! How fast a limit is rising, and when it runs out at that pace.
//!
//! A limit's pace is the slope of its readings over the recent stretch of
//! its current window, up to its latest reading, fitted by least squares,
//! in percentage points an hour: the last hour for a window of a day or
//! less, as five hours are, and the last day for a longer one, as a week
//! is. A week's pace is what a
//! day of use and rest takes of it; the last hour's, one busy hour, would
//! forecast it running out days early, and warn of it. Providers report
//! whole percents, so a pace needs at least fifteen minutes of readings; a
//! shorter stretch would turn one step of rounding into a pace.
//!
//! A limit only rises within its window, unless its provider gives use back,
//! as Anthropic did on 2026-09-29, taking a week from 84% to 5% used with
//! its reset unchanged. The readings before such a fall say nothing of the
//! pace after it, and fitted with those after it, they slope down, so the
//! limit was paced at nothing while it rose: only the readings since the
//! last fall count.

use crate::time::Instant;

/// The stretch of readings a pace is fitted over for a window of a day or
/// less: the last hour.
pub(super) const HOUR: i64 = 60 * 60 * 1000;

/// The stretch of readings a pace is fitted over for a longer window: the
/// last day, the most any pace is fitted over.
pub(super) const DAY: i64 = 24 * HOUR;

/// The stretch a pace is fitted over for a window `length` long, where it
/// is known: the last day for a window longer than a day, the last hour
/// for any other.
pub(super) fn span(length: Option<i64>) -> i64 {
    match length {
        Some(length) if length > DAY => DAY,
        _ => HOUR,
    }
}

/// The least stretch of readings a pace is fitted from: fifteen minutes.
const LEAST: i64 = 15 * 60 * 1000;

/// How far a limit falls from one reading to the next when its provider
/// has given use back: more than a point, so a step of rounding back is not
/// taken for one.
pub(super) const GIVEN_BACK: f64 = 1.0;

/// A limit's pace from `readings` of it in its window, oldest first, fitted
/// over the `over` milliseconds up to the latest of them, of those since it
/// last fell: `None` until they span fifteen minutes, and never below zero.
/// It is the pace as of the latest reading, as when it runs out is, so time
/// passing with no new reading changes neither.
pub(super) fn pace(readings: &[(Instant, f64)], over: i64) -> Option<f64> {
    let latest = readings.last()?.0;
    let since = readings
        .windows(2)
        .rposition(|pair| pair[1].1 < pair[0].1 - GIVEN_BACK)
        .map_or(0, |fell| fell + 1);
    let recent: Vec<(f64, f64)> = readings[since..]
        .iter()
        .filter(|(at, _)| latest.millis() - at.millis() <= over)
        // Hours since the epoch fit an f64 exactly to well below a second.
        .map(|(at, used)| (at.millis() as f64 / 3_600_000.0, *used))
        .collect();
    let (first, last) = (recent.first()?, recent.last()?);
    if (last.0 - first.0) * 3_600_000.0 < LEAST as f64 {
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
        .then(|| Instant::from_millis(at.millis() + (hours * 3_600_000.0).round() as i64))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::{DAY, HOUR, pace, reaches, span};
    use crate::time::Instant;

    fn at(minutes: i64) -> Instant {
        Instant::from_millis(1_789_000_000_000 + minutes * 60_000).unwrap()
    }

    #[test]
    fn a_steady_rise_is_its_pace() {
        // 2 points every 10 minutes is 12 points an hour.
        let readings = [
            (at(0), 40.0),
            (at(10), 42.0),
            (at(20), 44.0),
            (at(30), 46.0),
        ];
        let pace = pace(&readings, HOUR).unwrap();
        assert!((pace - 12.0).abs() < 1e-6);
        // 54 points left at 12 an hour is 4.5 hours after the last reading.
        assert_eq!(reaches(46.0, pace, at(30), 100.0), Some(at(30 + 270)));
    }

    #[test]
    fn a_pace_needs_fifteen_minutes_of_the_last_hour() {
        assert_eq!(pace(&[(at(0), 40.0), (at(10), 45.0)], HOUR), None);
        // Readings from more than an hour before the latest do not count.
        assert_eq!(
            pace(&[(at(0), 10.0), (at(80), 40.0), (at(90), 41.0)], HOUR),
            None
        );
        assert_eq!(
            pace(&[(at(0), 50.0), (at(20), 49.5)], HOUR),
            Some(0.0),
            "a limit a step of rounding lower is not rising"
        );
    }

    #[test]
    fn a_limit_given_back_is_paced_from_where_it_fell() {
        // A week at 84% until given back, then 5, 6 and 7 over the last
        // hour: a point each half hour, 2 an hour. Fitted with the 84 it
        // would slope down, and be paced at nothing.
        let readings = [
            (at(-400), 84.0),
            (at(-60), 5.0),
            (at(-30), 6.0),
            (at(0), 7.0),
        ];
        let pace = pace(&readings, DAY).unwrap();
        assert!((pace - 2.0).abs() < 1e-6, "{pace}");
        // Given back just now, nothing since spans fifteen minutes.
        assert_eq!(super::pace(&[(at(-20), 50.0), (at(0), 48.0)], HOUR), None);
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
        let pace = super::pace(&rounded, HOUR).unwrap();
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
        let hourly = pace(&readings, HOUR).unwrap();
        assert!((hourly - 6.0).abs() < 1e-6, "{hourly}");
        let daily = pace(&readings, DAY).unwrap();
        assert!((daily - 126.375 / 414.6875).abs() < 1e-6, "{daily}");
        // A week's window is paced over a day; five hours', or one of no
        // known length, over an hour.
        assert_eq!(span(Some(7 * DAY)), DAY);
        assert_eq!(span(Some(5 * HOUR)), HOUR);
        assert_eq!(span(None), HOUR);
    }

    #[test]
    fn a_full_or_idle_limit_does_not_run_out() {
        assert_eq!(reaches(100.0, 5.0, at(0), 100.0), None);
        assert_eq!(reaches(40.0, 0.0, at(0), 100.0), None);
    }
}
