//! A limit's past windows: how each window that reset ended, from the
//! readings the ledger keeps, as the weekly recap ([`super::recap`]) tells
//! them.
//!
//! A window is told apart from the next as a window's reckoning tells it
//! ([`super::share`]): its readings belong together while their resets
//! agree. Each past window is what its readings said of it: the most of it
//! used, and when a reading first found it used up. Readings are taken while
//! Turnscope runs, so a window whose last hours no reading saw says only as
//! much as was read; one read after it was used up says so however little
//! was read before.

use crate::error::Result;
use crate::ledger::{Ledger, Reading};
use crate::time::Instant;

use super::share::windows;

/// How a window of a limit that reset ended, as read.
#[derive(Clone, Debug, PartialEq)]
pub struct PastWindow {
    /// When it started, where known.
    pub starts: Option<Instant>,
    /// When it reset, where known.
    pub resets: Option<Instant>,
    /// The most of it any reading found used, in percent, which can be
    /// above 100.
    pub used: f64,
    /// When a reading first found it used up, if one did.
    pub reached: Option<Instant>,
}

/// The windows of `account`'s limit `key` that reset by `by`, oldest first,
/// as their readings from `since` on said each ended, the latest among them
/// when it has reset, though no reading since may follow yet.
/// A window that began before `since` is told only from what was read of it
/// after.
///
/// # Errors
///
/// Returns [`crate::Error::Ledger`] when the ledger cannot be read.
pub(crate) fn reset_by(
    ledger: &Ledger,
    account: &str,
    key: &str,
    since: Instant,
    by: Instant,
) -> Result<Vec<PastWindow>> {
    let readings: Vec<Reading> = ledger
        .readings(account, since)?
        .into_iter()
        .filter(|reading| reading.key == key)
        .collect();
    Ok(windows(readings)
        .iter()
        .filter_map(|window| summed(window))
        .filter(|window| window.resets.is_some_and(|resets| resets <= by))
        .collect())
}

/// How the window of `readings` ended; `None` for a window with none.
fn summed(readings: &[Reading]) -> Option<PastWindow> {
    let last = readings.last()?;
    let used = readings
        .iter()
        .map(|reading| reading.used)
        .filter(|used| used.is_finite())
        .fold(0., f64::max);
    Some(PastWindow {
        starts: last.starts,
        resets: last.resets,
        used,
        reached: readings
            .iter()
            .find(|reading| reading.used >= 100.)
            .map(|reading| reading.at),
    })
}

#[cfg(test)]
mod tests {
    use super::{PastWindow, summed};
    use crate::ledger::Reading;
    use crate::limits::share::windows;
    use crate::time::Instant;

    /// How each window of `readings` ended, oldest first.
    fn ended(readings: Vec<Reading>) -> Vec<PastWindow> {
        windows(readings)
            .iter()
            .filter_map(|window| summed(window))
            .collect()
    }

    const HOUR: i64 = 3_600_000;
    /// 2026-09-03T09:00:00Z, a Thursday.
    const WEEK_START: i64 = 1_788_426_000_000;

    fn at(hours: i64) -> Instant {
        Instant::from_millis(WEEK_START + hours * HOUR).unwrap()
    }

    /// A reading of a weekly limit `used` percent at `hours`, in the week
    /// starting `week` weeks on from the first.
    fn reading(week: i64, hours: i64, used: f64) -> Reading {
        let starts = week * 168;
        Reading {
            key: "seven_day".to_owned(),
            name: "Weekly".to_owned(),
            scope: None,
            at: at(starts + hours),
            used,
            starts: Some(at(starts)),
            resets: Some(at(starts + 168)),
        }
    }

    #[test]
    fn each_window_ends_at_the_most_read_of_it() {
        // Week 0 is read at 20%, 55% and 80%: it ended with 80 used, never
        // used up. Week 1 reads 40%, then 100% at hour 150, then 100% again:
        // used up at hour 168 + 150 = 318. Week 2 is read once, at 3%. A
        // reading lower than the one before, as rounding gives, doesn't
        // lower the most.
        let readings = vec![
            reading(0, 10, 20.),
            reading(0, 90, 55.),
            reading(0, 120, 54.),
            reading(0, 160, 80.),
            reading(1, 20, 40.),
            reading(1, 150, 100.),
            reading(1, 160, 100.),
            reading(2, 5, 3.),
        ];
        assert_eq!(
            ended(readings),
            vec![
                PastWindow {
                    starts: Some(at(0)),
                    resets: Some(at(168)),
                    used: 80.,
                    reached: None,
                },
                PastWindow {
                    starts: Some(at(168)),
                    resets: Some(at(336)),
                    used: 100.,
                    reached: Some(at(318)),
                },
                PastWindow {
                    starts: Some(at(336)),
                    resets: Some(at(504)),
                    used: 3.,
                    reached: None,
                },
            ]
        );
    }
}
