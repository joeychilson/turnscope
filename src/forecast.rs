//! Where a limit's window is headed, by the clock and by the work done:
//! what it will have used by its reset, when it runs out if it does, and how
//! many hours of work are left, each as an 80% range.
//!
//! Chosen by replaying this Mac's real limit history (the `backtest` test):
//! least squares, recent 1-, 3- and 24-hour rates, weighted rates, blends and
//! a duty cycle all did no better than the simplest on windows of a day or
//! less, and worse on longer ones, where they raised false alarms.
//!
//! - **Pace** is use so far over time so far in the window: since it began,
//!   or since use was last given back, as by an early reset, to the moment
//!   asked. Time with no new reading is time nothing more was used.
//! - **The range** scales the pace by how the pace that followed compared
//!   with it on ended windows, by window length and share of it left: the
//!   10th and 90th percentiles the backtest measured ([`BANDS`]); they held
//!   82% of the time.
//! - **Not enough data** in a window's first twentieth (15 minutes at
//!   least), or before 2 points are used: providers report whole percents,
//!   so one step of rounding would read as a pace, and a month's first hours
//!   say little of its weeks.
//! - **A run-out is told** only within a quarter of the window from now
//!   ([`told`]): 75 minutes of 5 hours, 42 hours of a week. Further out, the
//!   pace mostly carries a burst at the window's start forward, through the
//!   nights for a week. Four weeks of this Mac's work, replayed as 5-hour and
//!   weekly windows at 0.8 to 4 times its spend (October 2026): run-outs told
//!   as soon as the pace gave one came true 56–76% of the time, and those
//!   told within a quarter 75–100%, missing at most one more and still told
//!   about an hour, or a day and a half, ahead. Further out is not enough
//!   data.
//! - **Work** is what an hour of work uses: points risen over the hours an
//!   agent here worked on the account, the 5-minute slots holding a response
//!   on it, over its latest readings. The clock's pace counts nights and
//!   breaks, so for a week it understated what more hours of work use about
//!   2 times; this was unbiased, with an 80% range of what followed of 0.5 to
//!   1.6 times it for windows of a day or less, 0.4 to 2.2 for longer.

use crate::time::{DAY, HOUR, MINUTE};

/// The 80% range of the pace that followed over the pace projected, for
/// windows of a day or less, then longer, by share of the window left: the
/// last tenth, quarter, half, and more. Measured by the backtest, rounded
/// outward; a bucket measured on fewer than five windows takes the widest
/// of its length's.
const BANDS: [[(f64, f64); 4]; 2] = [
    [(0.05, 2.5), (0.05, 2.5), (0.25, 1.8), (0.35, 1.6)],
    [(0.0, 3.4), (0.0, 3.4), (0.0, 3.4), (0.7, 3.4)],
];

/// A limit's current window.
pub struct Window {
    pub starts: i64,
    pub resets: i64,
    /// When it was read, and percent used, oldest first.
    pub readings: Vec<(i64, f64)>,
}

/// Where a window is headed from `at` at a steady pace.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projection {
    pub at: i64,
    /// Percent used at `at`.
    pub used: f64,
    /// Points an hour.
    pub rate: f64,
    /// Percent used at the reset; over 100 when it runs out first.
    pub at_reset: f64,
    /// When it reaches 100%, if before the reset.
    pub runs_out: Option<i64>,
}

impl Projection {
    fn at_rate(used: f64, rate: f64, at: i64, resets: i64) -> Projection {
        let at_reset = used + rate * ((resets - at) as f64 / HOUR as f64).max(0.0);
        let runs_out = (rate > 0.0 && used < 100.0)
            .then(|| at + ((100.0 - used) / rate * HOUR as f64) as i64)
            .filter(|out| *out < resets);
        Projection {
            at,
            used,
            rate,
            at_reset,
            runs_out,
        }
    }
}

/// `window` projected from `now` at its pace so far; `None` without enough
/// data.
pub fn project(window: &Window, now: i64) -> Option<Projection> {
    // Since use was last given back, as by an early reset.
    let from = window
        .readings
        .windows(2)
        .rposition(|pair| pair[1].1 < pair[0].1 - 1.0)
        .map_or(0, |fell| fell + 1);
    let since = &window.readings[from..];
    let &(_, used) = since.last()?;
    let (start, base) = if from == 0 {
        (window.starts, 0.0)
    } else {
        since[0]
    };
    let risen = (used - base).max(0.0);
    let soon = (15 * MINUTE).max((window.resets - window.starts) / 20);
    if now - start < soon || (risen > 0.0 && risen < 2.0) {
        return None;
    }
    let rate = risen / ((now - start) as f64 / HOUR as f64);
    Some(Projection::at_rate(used, rate, now, window.resets))
}

/// Whether a run-out at `out` is near enough to `at` to tell: within a
/// quarter of `window`'s length.
pub fn told(window: &Window, at: i64, out: i64) -> bool {
    out - at <= (window.resets - window.starts) / 4
}

/// The 80% range of pace, as multiples of the pace projected, for `window`
/// as of `at`.
pub fn band(window: &Window, at: i64) -> (f64, f64) {
    let length = (window.resets - window.starts).max(1);
    let left = ((window.resets - at) as f64 / length as f64).clamp(0.0, 1.0);
    let bucket = match left {
        left if left < 0.1 => 0,
        left if left < 0.25 => 1,
        left if left < 0.5 => 2,
        _ => 3,
    };
    BANDS[usize::from(length > DAY)][bucket]
}

/// Percent used at the reset at the slow and fast ends of `band`.
pub fn at_reset(projection: &Projection, (low, high): (f64, f64)) -> (f64, f64) {
    let added = projection.at_reset - projection.used;
    (
        projection.used + added * low,
        projection.used + added * high,
    )
}

/// When it runs out at the fast end of `band`, and at the slow end, each if
/// before `resets`.
pub fn runs_out(
    projection: &Projection,
    (low, high): (f64, f64),
    resets: i64,
) -> (Option<i64>, Option<i64>) {
    let reaches = |times: f64| {
        Projection::at_rate(
            projection.used,
            projection.rate * times,
            projection.at,
            resets,
        )
        .runs_out
    };
    (reaches(high), reaches(low))
}

/// How long a stretch of work is: a slot holding a response.
pub const SLOT: i64 = 5 * MINUTE;

/// A limit's reading, as the pace of work is measured from it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub at: i64,
    /// Percent used.
    pub used: f64,
    /// Its window's start and reset, where its provider says them.
    pub starts: Option<i64>,
    pub resets: Option<i64>,
}

/// Points of a limit an hour of work uses: what it rose by over the hours
/// worked between its latest `readings` (oldest first, across windows),
/// back to `enough` hours of work. `worked` holds the start of each slot of
/// work on its account, oldest first, and is whose work it is only from
/// `known`, when the sign-ins it rests on were first read. A window's first
/// reading counts from the window's start only where both reach back past
/// it: work from before would otherwise be counted as none, and work on
/// another account before Turnscope ran as this one's. `None` with less
/// than an hour of work or 2 points measured.
pub fn work_rate(
    readings: &[Reading],
    worked: &[i64],
    known: Option<i64>,
    enough: f64,
) -> Option<f64> {
    let hours = |from: i64, to: i64| {
        let slots =
            worked.partition_point(|at| *at <= to) - worked.partition_point(|at| *at <= from);
        (slots as i64 * SLOT) as f64 / HOUR as f64
    };
    let (mut rose, mut worked_hours) = (0.0, 0.0);
    for (index, reading) in readings.iter().enumerate().rev() {
        if worked_hours >= enough {
            break;
        }
        let before = index.checked_sub(1).map(|before| readings[before]);
        let same = before.is_some_and(|before| match (reading.resets, before.resets) {
            (Some(resets), Some(was)) => (resets - was).abs() <= 5 * MINUTE,
            (resets, was) => resets == was,
        });
        match (before, reading.starts) {
            // Use given back, as by an early reset, says nothing of work.
            (Some(before), _) if same && reading.used < before.used - 1.0 => {}
            (Some(before), _) if same => {
                rose += (reading.used - before.used).max(0.0);
                worked_hours += hours(before.at, reading.at);
            }
            (_, Some(starts))
                if known.is_some_and(|known| known <= starts)
                    && worked.first().is_some_and(|first| *first <= starts) =>
            {
                rose += reading.used;
                worked_hours += hours(starts, reading.at);
            }
            _ => {}
        }
    }
    (worked_hours >= 1.0 && rose >= 2.0).then(|| rose / worked_hours)
}

/// The 80% range of what an hour of work used next over the rate measured,
/// for a window of `length`: a day or less, or longer.
pub fn work_band(length: Option<i64>) -> (f64, f64) {
    match length {
        Some(length) if length <= DAY => (0.5, 1.6),
        _ => (0.4, 2.2),
    }
}

/// Hours of work left at `rate` with `used` percent used, as the slow end,
/// likely, and fast end of `band`: `None` when even the fastest lasts
/// `until` its reset, as many hours of work as there are hours to it.
pub fn work_left(
    used: f64,
    rate: f64,
    (low, high): (f64, f64),
    until: Option<f64>,
) -> Option<(f64, f64, f64)> {
    let left = (100.0 - used).max(0.0);
    let likely = left / rate;
    let until = until.unwrap_or(f64::INFINITY);
    (likely < until).then(|| {
        (
            left / (rate * high),
            likely,
            (left / (rate * low)).min(until),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(readings: &[(f64, f64)]) -> Window {
        Window {
            starts: 0,
            resets: 5 * HOUR,
            readings: readings
                .iter()
                .map(|&(hours, used)| ((hours * HOUR as f64) as i64, used))
                .collect(),
        }
    }

    #[test]
    fn the_pace_is_use_so_far_over_time_so_far() {
        // 60% in 2 hours is 30 points an hour: out at 3h20m, before 5h.
        let readings = [(1.0, 30.0), (2.0, 60.0)];
        let projection = project(&window(&readings), 2 * HOUR).unwrap();
        assert_eq!(projection.rate, 30.0);
        assert_eq!(projection.runs_out, Some(3 * HOUR + 20 * MINUTE));
        // An hour on with nothing more read, the pace has slowed to 20,
        // which just lasts.
        let later = project(&window(&readings), 3 * HOUR).unwrap();
        assert_eq!(
            (later.rate, later.runs_out, later.at_reset),
            (20.0, None, 100.0)
        );
    }

    #[test]
    fn use_given_back_restarts_the_pace() {
        // Down from 40 to 5 at 2h: 10 more points over the hour since.
        let projection =
            project(&window(&[(1.0, 40.0), (2.0, 5.0), (3.0, 15.0)]), 3 * HOUR).unwrap();
        assert_eq!(projection.rate, 10.0);
    }

    #[test]
    fn too_little_is_not_enough_data() {
        // The first quarter hour, and one point of rounding.
        assert!(project(&window(&[(0.1, 5.0)]), HOUR / 10).is_none());
        assert!(project(&window(&[(1.0, 1.0)]), HOUR).is_none());
        // Nothing used is a pace of none, which lasts.
        assert_eq!(
            project(&window(&[(1.0, 0.0)]), HOUR).unwrap().runs_out,
            None
        );
    }

    #[test]
    fn a_run_out_is_told_within_a_quarter_of_the_window() {
        // A burst at the start of a 5-hour window: 25% in the first hour
        // runs out at 4h, too far to tell; by 2h45m, 75 minutes off, it is.
        let window = window(&[(1.0, 25.0)]);
        let out = project(&window, HOUR).unwrap().runs_out.unwrap();
        assert_eq!(out, 4 * HOUR);
        assert!(!told(&window, HOUR, out));
        assert!(told(&window, 2 * HOUR + 45 * MINUTE, out));
    }

    #[test]
    fn the_range_scales_the_pace() {
        let window = window(&[(2.0, 30.0)]);
        let projection = project(&window, 2 * HOUR).unwrap();
        // 60% of the window left: 0.35 to 1.6 times the pace.
        assert_eq!(band(&window, 2 * HOUR), (0.35, 1.6));
        let (soonest, latest) = runs_out(&projection, band(&window, 2 * HOUR), window.resets);
        // At 24 points an hour, 70 more take 2h55m; at 5.25 it lasts.
        assert_eq!(soonest, Some(2 * HOUR + 2 * HOUR + 55 * MINUTE));
        assert_eq!(latest, None);
    }

    #[test]
    fn work_is_what_rose_over_the_hours_worked() {
        let at = |hours: f64| (hours * HOUR as f64) as i64;
        // A week from hour 0, read each hour: 10 points by hour 2, with work
        // throughout; none in hour 3; 4 more in hour 4, with work throughout.
        let week = Some(at(168.0));
        let readings: Vec<Reading> = [(1.0, 5.0), (2.0, 10.0), (3.0, 10.0), (4.0, 14.0)]
            .iter()
            .map(|&(hours, used)| Reading {
                at: at(hours),
                used,
                starts: Some(0),
                resets: week,
            })
            .collect();
        let mut worked: Vec<i64> = (0..=24).map(|slot| slot * SLOT).collect();
        worked.extend((37..=48).map(|slot| slot * SLOT));
        // 14 points over 3 hours of work, walking back to the window's start.
        assert_eq!(
            work_rate(&readings, &worked, Some(0), 5.0),
            Some(14.0 / 3.0)
        );
        // Enough once the latest stretches hold an hour of work: 4 points.
        assert_eq!(work_rate(&readings, &worked, Some(0), 1.0), Some(4.0));
        // Work seen only after the window began can't say what was used
        // before its first reading: 9 points over 2 hours.
        assert_eq!(work_rate(&readings, &worked[1..], Some(0), 5.0), Some(4.5));
        // Nor can work from before whose account was known.
        assert_eq!(work_rate(&readings, &worked, Some(at(0.5)), 5.0), Some(4.5));
        assert_eq!(work_rate(&readings, &worked, None, 5.0), Some(4.5));
        let (low, likely, high) =
            work_left(14.0, 4.0, work_band(Some(at(168.0))), Some(164.0)).unwrap();
        assert_eq!(likely, 21.5);
        assert!((low - 21.5 / 2.2).abs() < 1e-9 && (high - 21.5 / 0.4).abs() < 1e-9);
        // More work left than hours to the reset lasts until it, even
        // working all the time.
        assert_eq!(
            work_left(14.0, 4.0, work_band(Some(at(5.0))), Some(3.0)),
            None
        );
        // Less runs out first, its slow end no later than the reset.
        assert_eq!(
            work_left(90.0, 4.0, work_band(Some(at(5.0))), Some(4.0)),
            Some((10.0 / 6.4, 2.5, 4.0))
        );
    }

    /// Replay the limit history in a copy of a data directory and score the
    /// forecast on every window whose outcome is known: one that ran out, or
    /// was read in its last sixth. At every tenth of a window (a minute to an
    /// hour), it's forecast from the readings before, and scored on its point
    /// error, how often its 80% range held, and how often it declined.
    #[test]
    #[ignore = "replays a copy of real history: TURNSCOPE_BACKTEST=<data dir>"]
    fn backtest() {
        let dir = std::env::var("TURNSCOPE_BACKTEST")
            .expect("TURNSCOPE_BACKTEST names a copy of a data directory");
        let db = rusqlite::Connection::open(std::path::Path::new(&dir).join("turnscope.sqlite"))
            .unwrap();
        let mut rows = db
            .prepare("SELECT account, limit_key, at, used, starts, resets FROM reading WHERE starts IS NOT NULL AND resets IS NOT NULL ORDER BY account, limit_key, at")
            .unwrap();
        let rows: Vec<(String, String, i64, f64, i64, i64)> = rows
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        // Windows: a limit's readings with one reset, within the seconds it drifts.
        let mut windows: Vec<(String, Window)> = Vec::new();
        for (account, key, at, used, starts, resets) in rows {
            let limit = format!("{account} {key}");
            match windows.last_mut() {
                Some((last, window))
                    if *last == limit && (window.resets - resets).abs() <= 5 * MINUTE =>
                {
                    window.readings.push((at, used))
                }
                _ => windows.push((
                    limit,
                    Window {
                        starts,
                        resets,
                        readings: vec![(at, used)],
                    },
                )),
            }
        }
        // By length: a day or less, then longer. Forecasts, declined, point
        // errors, ranges that held, run-outs, run-out ranges that held.
        let mut scores: [(usize, usize, Vec<f64>, usize, usize, usize); 2] = Default::default();
        for (_, window) in &windows {
            let ran_out = window
                .readings
                .iter()
                .find(|(_, used)| *used >= 100.0)
                .map(|(at, _)| *at);
            let &(last, _) = window.readings.last().unwrap();
            let length = window.resets - window.starts;
            if ran_out.is_none() && window.resets - last > length / 6 {
                continue;
            }
            let came_to = window
                .readings
                .iter()
                .map(|(_, used)| *used)
                .fold(0.0, f64::max)
                .min(100.0);
            let score = &mut scores[usize::from(length > DAY)];
            let step = (length / 10).clamp(MINUTE, HOUR);
            let end = ran_out.unwrap_or(window.resets).min(window.resets);
            let mut origin = window.readings[0].0 + step / 2;
            while origin + step / 2 < end {
                let seen = Window {
                    starts: window.starts,
                    resets: window.resets,
                    readings: window
                        .readings
                        .iter()
                        .copied()
                        .filter(|(at, _)| *at <= origin)
                        .collect(),
                };
                origin += step;
                let Some(projection) = project(&seen, origin - step) else {
                    score.1 += 1;
                    continue;
                };
                score.0 += 1;
                score
                    .2
                    .push((came_to - projection.at_reset.min(100.0)).abs());
                let band = band(&seen, projection.at);
                let (low, high) = at_reset(&projection, band);
                score.3 +=
                    usize::from((low.min(100.0) - 0.5..=high.min(100.0) + 0.5).contains(&came_to));
                if let Some(out) = ran_out {
                    score.4 += 1;
                    let (soonest, latest) = runs_out(&projection, band, window.resets);
                    let held = soonest.is_some_and(|soonest| {
                        soonest - out <= 15 * MINUTE
                            && latest.is_none_or(|latest| out - latest <= 15 * MINUTE)
                    });
                    score.5 += usize::from(held);
                }
            }
        }
        for (name, (forecasts, declined, mut errors, held, outs, outs_held)) in
            ["a day or less", "longer"].into_iter().zip(scores)
        {
            errors.sort_by(f64::total_cmp);
            let median = errors.get(errors.len() / 2).copied().unwrap_or(f64::NAN);
            let share = |part: usize, whole: usize| 100.0 * part as f64 / whole.max(1) as f64;
            println!(
                "{name}: {forecasts} forecasts, {:.0}% declined, median error {median:.1} points, 80% range held {:.0}%, run-out range held {:.0}% of {outs}",
                share(declined, forecasts + declined),
                share(held, forecasts),
                share(outs_held, outs),
            );
        }
    }
}
