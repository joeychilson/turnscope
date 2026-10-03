//! Alerts about limits, worked out on each new reading: a limit projected to
//! run out before it resets, one reached, and one back, which is news only
//! when the last alert about it said it was reached. Beside them go
//! milestones of a week or a month: each quarter of it used, and a
//! subscription's that resets within a day with more than half of it left.
//!
//! **Early resets.** A provider may reset a week or a month before it said
//! it would, giving everyone their limit back at once, as OpenAI has done
//! with ChatGPT's. That is news of its own, room no one counted on: the
//! reading before the latest was of a window longer than a day, with a
//! reset still ahead when the latest was taken, and the latest is of another
//! window, with at least [`GIVEN_BACK`] points less used ([`reset_early`]).
//! The window before is what says it was a week or a month, as the latest
//! may give none: Claude gives no reset for a week not yet begun. A limit
//! that had run out is said to be back instead, which says as much, and so
//! is one of which it was said that it would, where its new window gives a
//! reset. Readings are taken every five minutes while an account is
//! signed in, so an early reset is told within minutes of it.
//! An account's first reading, as every account's is when Turnscope first
//! runs, says only that a limit runs out or ran out: how much is left is
//! where it stands, not news, and its milestones are kept as told.
//!
//! Beside the limits, an account in use whose sign-in was refused, so its
//! limits can't be read, is said once, and again only once a read has
//! succeeded since.
//!
//! Each is sent once per window, however often the app restarts, as the
//! ledger records it as sent. Only the process that keeps the data
//! directory works them out, the app, which shows them: limits another
//! process reads, as an MCP server does while no app runs, are recorded
//! without them, so no alert is recorded as sent that no one saw.

use super::{
    AccountLimits, DAY, HOUR, LimitProblem, LimitState, SAME_WINDOW, Standing, Subscription,
};
use crate::error::Result;
use crate::ledger::Ledger;
use crate::time::Instant;

/// How soon after a window ends that a limit of an account signed in nowhere
/// is back is still news: an hour.
const BACK_WITHIN: i64 = HOUR;

/// How recent a reading must be to say how much of a limit is left as news:
/// half an hour. Readings are taken every five minutes while an account is
/// signed in.
const FRESH: i64 = 30 * 60 * 1000;

/// How many points of a week or a month an early reset gives back to be
/// news: a tenth of it. Less is room no one would miss.
const GIVEN_BACK: f64 = 10.0;

/// What an alert says of a limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertKind {
    /// At the pace it is rising, it runs out before its window resets.
    RunningOut,
    /// It is used up.
    Reached,
    /// Its window has reset since it was used up, or, for a week or a
    /// month, since it was running out.
    Available,
    /// A week or a month of a subscription resets within a day with more
    /// than half of it left.
    Unused,
    /// A week or a month is down to three quarters of it left.
    ThreeQuartersLeft,
    /// A week or a month is down to half of it left.
    HalfLeft,
    /// A week or a month is down to a quarter of it left.
    QuarterLeft,
    /// An account in use can't be read, its sign-in refused or expired
    /// until its agent renews it. Of the account, not of one limit.
    SignIn,
    /// A week or a month its provider reset well before it said it would,
    /// giving back at least a tenth of it.
    ResetEarly,
}

impl AlertKind {
    /// Every kind.
    const ALL: [AlertKind; 9] = [
        AlertKind::RunningOut,
        AlertKind::Reached,
        AlertKind::Available,
        AlertKind::Unused,
        AlertKind::ThreeQuartersLeft,
        AlertKind::HalfLeft,
        AlertKind::QuarterLeft,
        AlertKind::SignIn,
        AlertKind::ResetEarly,
    ];

    /// The kind as stored.
    pub(crate) fn key(self) -> &'static str {
        match self {
            AlertKind::RunningOut => "running-out",
            AlertKind::Reached => "reached",
            AlertKind::Available => "available",
            AlertKind::Unused => "unused",
            AlertKind::ThreeQuartersLeft => "left-75",
            AlertKind::HalfLeft => "left-50",
            AlertKind::QuarterLeft => "left-25",
            AlertKind::SignIn => "sign-in",
            AlertKind::ResetEarly => "reset-early",
        }
    }

    /// The kind with `key`.
    pub(crate) fn from_key(key: &str) -> Option<AlertKind> {
        AlertKind::ALL.into_iter().find(|kind| kind.key() == key)
    }

    /// Whether it says how much of a week or a month is left, a quarter
    /// at a time.
    fn quarter(self) -> bool {
        matches!(
            self,
            AlertKind::ThreeQuartersLeft | AlertKind::HalfLeft | AlertKind::QuarterLeft
        )
    }

    /// Whether it says a limit runs out, ran out or is back, as an early
    /// reset says it is, which decides whether a reset is news; the
    /// milestones said beside them don't.
    pub(crate) fn of_running_out(self) -> bool {
        matches!(
            self,
            AlertKind::RunningOut
                | AlertKind::Reached
                | AlertKind::Available
                | AlertKind::ResetEarly
        )
    }
}

/// An alert about a limit, sent once per window.
#[derive(Clone, Debug, PartialEq)]
pub struct Alert {
    /// The account.
    pub account: String,
    /// What the account is called ([`AccountLimits::title`]).
    pub title: String,
    /// What tells the account apart from others of the subscription.
    pub label: Option<String>,
    /// The limit's name; empty for [`AlertKind::SignIn`], which is of the
    /// account.
    pub limit: String,
    /// The one model the limit applies to, when not all.
    pub scope: Option<String>,
    /// What the alert says.
    pub kind: AlertKind,
    /// When it runs out, for [`AlertKind::RunningOut`]; when it resets, for
    /// [`AlertKind::Reached`], [`AlertKind::Unused`] and the quarters left,
    /// and for [`AlertKind::ResetEarly`], when the window it was reset to
    /// resets in turn.
    pub when: Option<Instant>,
    /// How much of it was used, in percent, by the reading the alert was
    /// worked out from; `None` where that is unknown, as for a window that
    /// has reset since.
    pub used: Option<f64>,
}

/// The alerts `accounts` give rise to that have not been sent for their
/// windows, recorded as sent. An account the person hid gives rise to none.
///
/// # Errors
///
/// Returns an error when the ledger cannot be written.
pub(crate) fn alerts(
    ledger: &mut Ledger,
    accounts: &[AccountLimits],
    now: Instant,
) -> Result<Vec<Alert>> {
    let mut alerts = Vec::new();
    // An account the person hid is not to be heard of.
    for account in accounts.iter().filter(|account| !account.hidden) {
        // Read for the first time, as every account is when Turnscope first
        // runs, how much of a week is left is where it stands, not news: its
        // milestones are kept as told, and only that it runs out, ran out or
        // is back is said.
        let first = ledger.read_once(&account.id)?;
        // A refused sign-in is said once, the alert named by the last read
        // that succeeded, which renewing the sign-in moves on.
        if account.in_use && account.problem == Some(LimitProblem::SignIn) {
            let window = account.read_at.map_or(0, Instant::millis);
            if ledger.send_alert(&account.id, "", AlertKind::SignIn, window, now)? {
                alerts.push(Alert {
                    account: account.id.clone(),
                    title: account.title(),
                    label: account.label.clone(),
                    limit: String::new(),
                    scope: None,
                    kind: AlertKind::SignIn,
                    when: None,
                    used: None,
                });
            }
        }
        for limit in &account.limits {
            // A week or a month someone was warned of is worth saying is
            // full again; five hours, which reset within the day, aren't.
            let long = limit
                .starts
                .zip(limit.resets)
                .is_some_and(|(starts, resets)| resets.millis() - starts.millis() > DAY);
            let due = if limit.refilled {
                // An account signed in nowhere isn't read again, so that a
                // limit it used up is back is known only from its window
                // ending: said once, soon after, and not days late.
                if account.signed_in {
                    None
                } else {
                    warned_before(ledger, &account.id, &limit.key, now.millis(), false)?
                        .filter(|window| now.millis() - window <= BACK_WITHIN)
                        .map(|window| (AlertKind::Available, None, window))
                }
            } else if let Some(resets) = limit.resets {
                let window = resets.millis();
                // Windows before this one, told apart from it by more than a
                // countdown's drift.
                let earlier = window - SAME_WINDOW;
                if limit.used.is_some_and(|used| used >= 100.0) {
                    Some((AlertKind::Reached, Some(resets), window))
                } else if let (Standing::RunningOut, Some(runs_out)) =
                    (limit.standing(), limit.runs_out_at())
                {
                    Some((AlertKind::RunningOut, Some(runs_out), window))
                } else if warned_before(ledger, &account.id, &limit.key, earlier, long)?.is_some() {
                    Some((AlertKind::Available, None, window))
                } else {
                    None
                }
            } else {
                // A limit its provider gives no reset for, as Claude gives
                // none for five hours not yet begun, has no window to name
                // an alert by. That it ran out is said once, named by when,
                // until the last thing said of it is that it is back, which
                // is said once, named as the alert it answers.
                let reached = warned_before(ledger, &account.id, &limit.key, i64::MAX, false)?;
                match (limit.used.is_some_and(|used| used >= 100.0), reached) {
                    (true, None) => Some((AlertKind::Reached, None, now.millis())),
                    (false, Some(window)) => Some((AlertKind::Available, None, window)),
                    _ => None,
                }
            };
            // A week or a month reset early is said when nothing above was,
            // which, of one that ran out or would have, says it is back.
            let due = match due {
                None if !limit.refilled => reset_early(ledger, &account.id, limit)?,
                due => due,
            };
            // A warning that it runs out, or ran out, says all a quarter
            // crossed in the same read would: the quarter is kept as told,
            // and not said.
            let mut warned = false;
            for (kind, when, window) in due.into_iter().chain(milestones(account, limit, long, now))
            {
                if ledger.send_alert(&account.id, &limit.key, kind, window, now)? {
                    if (warned || first) && kind.quarter() || first && kind == AlertKind::Unused {
                        continue;
                    }
                    warned |= matches!(kind, AlertKind::RunningOut | AlertKind::Reached);
                    alerts.push(Alert {
                        account: account.id.clone(),
                        title: account.title(),
                        label: account.label.clone(),
                        limit: limit.name.clone(),
                        scope: limit.scope.clone(),
                        kind,
                        when,
                        used: limit.used,
                    });
                }
            }
        }
    }
    Ok(alerts)
}

/// That `account`'s `limit` was reset early, as its kind, when its new
/// window resets, and that window: when the reading before its latest was
/// of a window longer than a day, giving a reset more than a countdown's
/// drift after the latest was taken, and the latest is of another window,
/// or of one not yet begun, with at least [`GIVEN_BACK`] points less used.
/// `None` otherwise, as of a window that reset when it said it would, or of
/// five hours.
fn reset_early(
    ledger: &Ledger,
    account: &str,
    limit: &LimitState,
) -> Result<Option<(AlertKind, Option<Instant>, i64)>> {
    let Some(used) = limit.used else {
        return Ok(None);
    };
    let Some(before) = ledger.reading_before(account, &limit.key, limit.read_at)? else {
        return Ok(None);
    };
    // The window reset is the one before, which says how long it is: the
    // latest gives none for a window not yet begun.
    let (Some(began), Some(was_due)) = (before.starts, before.resets) else {
        return Ok(None);
    };
    if was_due.millis() - began.millis() <= DAY {
        return Ok(None);
    }
    let ahead = was_due.millis() - limit.read_at.millis() > SAME_WINDOW;
    let another = limit
        .resets
        .is_none_or(|resets| (resets.millis() - was_due.millis()).abs() > SAME_WINDOW);
    let given_back = before.used - used >= GIVEN_BACK;
    Ok((ahead && another && given_back).then(|| {
        let window = limit.resets.unwrap_or(limit.read_at).millis();
        (AlertKind::ResetEarly, limit.resets, window)
    }))
}

/// The milestones of `account`'s `limit`, a week or a month if `long`, due
/// at `now`, beside anything said of its running out, each as its kind,
/// when the limit resets, and the window, named by that reset:
///
/// - the quarter of it last used, as [`AlertKind::ThreeQuartersLeft`], then
///   [`AlertKind::HalfLeft`] and [`AlertKind::QuarterLeft`]; crossing
///   several between two readings says only the last, and a limit used up
///   says none, as that it ran out says it;
/// - and, of a subscription's, not an API key's, whose limit left isn't
///   lost, that it resets within a day with more than half of it left and
///   won't run out first, [`AlertKind::Unused`].
///
/// Only a reading of the last half hour says either, as an older one's
/// account may have been used since, or signed out and not read again.
fn milestones(
    account: &AccountLimits,
    limit: &LimitState,
    long: bool,
    now: Instant,
) -> Vec<(AlertKind, Option<Instant>, i64)> {
    let (Some(used), Some(resets)) = (limit.used, limit.resets) else {
        return Vec::new();
    };
    let fresh = now.millis() - limit.read_at.millis() <= FRESH;
    if !long || limit.refilled || !fresh || resets <= now {
        return Vec::new();
    }
    let mut due = Vec::new();
    let quarter = if used >= 100.0 {
        None
    } else if used >= 75.0 {
        Some(AlertKind::QuarterLeft)
    } else if used >= 50.0 {
        Some(AlertKind::HalfLeft)
    } else if used >= 25.0 {
        Some(AlertKind::ThreeQuartersLeft)
    } else {
        None
    };
    due.extend(quarter.map(|kind| (kind, Some(resets), resets.millis())));
    let unused = account.subscription != Subscription::ApiKey
        && used < 50.0
        && resets.millis() - now.millis() <= DAY
        && limit.standing() != Standing::RunningOut;
    if unused {
        due.push((AlertKind::Unused, Some(resets), resets.millis()));
    }
    due
}

/// The window of the alert last sent about `account`'s limit `key` for a
/// window resetting before `before`, when that alert said the limit was
/// reached, or, `long` as a week or a month is, that it was running out: a
/// limit is back only when the last thing said of it was that it ran out,
/// or of a long one, that it would.
fn warned_before(
    ledger: &Ledger,
    account: &str,
    key: &str,
    before: i64,
    long: bool,
) -> Result<Option<i64>> {
    Ok(ledger
        .last_alert(account, key, before)?
        .filter(|(_, kind)| *kind == AlertKind::Reached || (long && *kind == AlertKind::RunningOut))
        .map(|(window, _)| window))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{AlertKind, alerts, milestones};
    use crate::agent::Agent;
    use crate::ledger::tests::scratch;
    use crate::limits::tests::{account, minute, read};
    use crate::limits::{
        AccountLimits, AccountRead, LimitProblem, LimitState, Reported, Subscription, state,
    };
    use crate::time::Instant;

    #[test]
    fn a_refused_sign_in_is_said_once_until_a_read_succeeds_again() {
        let (_dir, mut ledger) = scratch();
        // In use, its last read at minute 0 and every read since refused.
        let refused = |read_at: i64| AccountLimits {
            problem: Some(LimitProblem::SignIn),
            read_at: Some(minute(read_at)),
            ..account(Vec::new())
        };
        let kinds = |ledger: &mut crate::ledger::Ledger, accounts: &[AccountLimits], at| {
            alerts(ledger, accounts, minute(at))
                .unwrap()
                .into_iter()
                .map(|alert| alert.kind)
                .collect::<Vec<_>>()
        };
        assert_eq!(kinds(&mut ledger, &[refused(0)], 10), [AlertKind::SignIn]);
        assert_eq!(kinds(&mut ledger, &[refused(0)], 15), []);
        // Renewed and read at minute 60, then refused again: said again.
        assert_eq!(kinds(&mut ledger, &[refused(60)], 70), [AlertKind::SignIn]);
        // Read at minute 100, refused at 101, read again at 102 and refused
        // at 103: each refusal is said, though the reads are closer than a
        // reset's drift.
        let quick = |read_at: i64| AccountLimits {
            id: "d".into(),
            ..refused(read_at)
        };
        assert_eq!(kinds(&mut ledger, &[quick(100)], 101), [AlertKind::SignIn]);
        assert_eq!(kinds(&mut ledger, &[quick(102)], 103), [AlertKind::SignIn]);
        assert_eq!(kinds(&mut ledger, &[quick(102)], 104), []);
        // Not in use, or hidden, it isn't said.
        let idle = AccountLimits {
            id: "b".into(),
            in_use: false,
            ..refused(0)
        };
        let hidden = AccountLimits {
            id: "c".into(),
            hidden: true,
            ..refused(0)
        };
        assert_eq!(kinds(&mut ledger, &[idle, hidden], 80), []);
    }

    #[test]
    fn a_rising_limit_warns_once_before_it_runs_out_and_once_when_it_does() {
        let (_dir, mut ledger) = scratch();
        let quiet = HashSet::new();
        // 60% to 90% over half an hour: 60 points an hour, so the rest runs out
        // ten minutes after the last reading, long before the reset at 300.
        for (at, used) in [(0, 60.0), (10, 70.0), (20, 80.0), (30, 90.0)] {
            ledger
                .record_limits(Subscription::Claude, &[read(used, 300)], minute(at))
                .unwrap();
        }
        let now = minute(30);
        let accounts = state(&ledger, &quiet, now).unwrap();
        let limit = &accounts[0].limits[0];
        assert!((limit.pace.unwrap() - 60.0).abs() < 1e-6);
        assert_eq!(limit.reaches(100.0), Some(minute(40)));
        assert!(accounts[0].in_use, "a limit that rose is in use");

        let sent = alerts(&mut ledger, &accounts, now).unwrap();
        assert_eq!(
            sent.iter()
                .map(|alert| (alert.kind, alert.title.as_str()))
                .collect::<Vec<_>>(),
            [(AlertKind::RunningOut, "Claude Max")]
        );
        assert!(
            alerts(&mut ledger, &accounts, now).unwrap().is_empty(),
            "once per window"
        );

        ledger
            .record_limits(Subscription::Claude, &[read(100.0, 300)], minute(40))
            .unwrap();
        let full = state(&ledger, &quiet, minute(40)).unwrap();
        let sent = alerts(&mut ledger, &full, minute(40)).unwrap();
        assert_eq!(
            sent.iter().map(|alert| alert.kind).collect::<Vec<_>>(),
            [AlertKind::Reached]
        );

        // After the reset, how much of the new window is used is unknown until
        // it is read, not nothing; then it is available again, once.
        let after = state(&ledger, &quiet, minute(301)).unwrap();
        assert!(after[0].limits[0].refilled);
        assert_eq!(after[0].limits[0].used, None);
        ledger
            .record_limits(Subscription::Claude, &[read(2.0, 600)], minute(305))
            .unwrap();
        let fresh = state(&ledger, &quiet, minute(305)).unwrap();
        let sent = alerts(&mut ledger, &fresh, minute(305)).unwrap();
        assert_eq!(
            sent.iter().map(|alert| alert.kind).collect::<Vec<_>>(),
            [AlertKind::Available]
        );
    }

    #[test]
    fn a_week_warned_of_is_full_again_when_it_resets_and_five_hours_are_not() {
        // What is said of running out; the milestones beside it are
        // another test's.
        let said = |reads| -> Vec<(i64, AlertKind)> {
            alerted(reads)
                .into_iter()
                .filter(|(_, kind)| kind.of_running_out())
                .collect()
        };
        // 60% of the week as it begins: over a day at least, 2.5 points an
        // hour, so the 40 left run out 16 hours on, days before its reset,
        // said once. The next week, read with 1% used, is full again, once.
        assert_eq!(
            said(vec![
                (0, weekly(60.0, 0)),
                (30, weekly(70.0, 0)),
                (WEEK + 5, weekly(1.0, WEEK)),
                (WEEK + 5, weekly(1.0, WEEK)),
            ]),
            [(0, AlertKind::RunningOut), (WEEK + 5, AlertKind::Available)]
        );
        // Five hours warned of, at 60 points an hour, but never used up say
        // nothing when they reset.
        assert_eq!(
            said(vec![
                (0, read(60.0, 300)),
                (10, read(70.0, 300)),
                (20, read(80.0, 300)),
                (305, read(2.0, 600)),
            ]),
            [(20, AlertKind::RunningOut)]
        );
    }

    #[test]
    fn a_limit_used_up_on_an_account_signed_out_is_back_when_its_window_ends() {
        let (_dir, mut ledger) = scratch();
        let quiet = HashSet::new();
        // Used up in the window that resets at minute 300, then signed out
        // of, as when Claude Code is switched to another account.
        ledger
            .record_limits(Subscription::Claude, &[read(100.0, 300)], minute(0))
            .unwrap();
        let full = state(&ledger, &quiet, minute(0)).unwrap();
        let used_up = alerts(&mut ledger, &full, minute(0)).unwrap();
        assert_eq!(
            used_up.iter().map(|alert| alert.kind).collect::<Vec<_>>(),
            [AlertKind::Reached]
        );
        ledger
            .record_limits(Subscription::Claude, &[], minute(5))
            .unwrap();
        let before = state(&ledger, &quiet, minute(200)).unwrap();
        assert!(
            alerts(&mut ledger, &before, minute(200))
                .unwrap()
                .is_empty()
        );
        // Its window ends: it is back, once.
        let after = state(&ledger, &quiet, minute(305)).unwrap();
        assert_eq!(
            alerts(&mut ledger, &after, minute(305))
                .unwrap()
                .iter()
                .map(|alert| alert.kind)
                .collect::<Vec<_>>(),
            [AlertKind::Available]
        );
        assert!(alerts(&mut ledger, &after, minute(310)).unwrap().is_empty());
        // Signed into again in the next window: that it is back was said.
        ledger
            .record_limits(Subscription::Claude, &[read(5.0, 600)], minute(320))
            .unwrap();
        let again = state(&ledger, &quiet, minute(320)).unwrap();
        assert!(alerts(&mut ledger, &again, minute(320)).unwrap().is_empty());
    }

    #[test]
    fn a_limit_back_long_ago_is_no_news() {
        let (_dir, mut ledger) = scratch();
        let quiet = HashSet::new();
        ledger
            .record_limits(Subscription::Claude, &[read(100.0, 300)], minute(0))
            .unwrap();
        let full = state(&ledger, &quiet, minute(0)).unwrap();
        alerts(&mut ledger, &full, minute(0)).unwrap();
        ledger
            .record_limits(Subscription::Claude, &[], minute(5))
            .unwrap();
        // The app was closed when the window ended, and opened a day later.
        let later = state(&ledger, &quiet, minute(300 + 24 * 60)).unwrap();
        assert!(
            alerts(&mut ledger, &later, minute(300 + 24 * 60))
                .unwrap()
                .is_empty()
        );
    }

    /// `read`, its reset moved by `seconds`, as a countdown moves it from
    /// read to read.
    fn drifting(mut read: AccountRead, seconds: i64) -> AccountRead {
        for limit in read.limits.iter_mut().flatten() {
            limit.resets = limit
                .resets
                .and_then(|resets| Instant::from_millis(resets.millis() + seconds * 1_000));
        }
        read
    }

    /// The alerts `reads` give rise to, each recorded at its minute: the
    /// minute and the kind of each.
    fn alerted(reads: Vec<(i64, AccountRead)>) -> Vec<(i64, AlertKind)> {
        let (_dir, mut ledger) = scratch();
        let mut sent = Vec::new();
        for (at, read) in reads {
            ledger
                .record_limits(Subscription::Claude, &[read], minute(at))
                .unwrap();
            let accounts = state(&ledger, &HashSet::new(), minute(at)).unwrap();
            let alerts = alerts(&mut ledger, &accounts, minute(at)).unwrap();
            sent.extend(alerts.into_iter().map(|alert| (at, alert.kind)));
        }
        sent
    }

    #[test]
    fn a_limit_used_up_is_said_once_however_its_reset_drifts() {
        // ChatGPT gives some resets only as a countdown, so each read of one
        // window finds its reset a second or so from the last.
        let sent = alerted(vec![
            (0, drifting(read(100.0, 300), 0)),
            (5, drifting(read(100.0, 300), 1)),
            (10, drifting(read(100.0, 300), 2)),
            // Just under full in the same window is not back.
            (15, drifting(read(99.0, 300), 3)),
            // The next window is.
            (305, read(2.0, 600)),
        ]);
        assert_eq!(sent, [(0, AlertKind::Reached), (305, AlertKind::Available)]);
    }

    #[test]
    fn a_limit_is_back_when_the_last_alert_sent_said_it_ran_out() {
        // A drifting reset records one window's alerts against resets
        // seconds apart, here the warning's later than the one it ran out.
        let sent = alerted(vec![
            (0, drifting(read(60.0, 300), 10)),
            (10, drifting(read(70.0, 300), 9)),
            // 60 points an hour: it runs out at minute 40.
            (20, drifting(read(80.0, 300), 8)),
            (30, drifting(read(100.0, 300), 7)),
            (305, read(2.0, 600)),
        ]);
        assert_eq!(
            sent,
            [
                (20, AlertKind::RunningOut),
                (30, AlertKind::Reached),
                (305, AlertKind::Available)
            ]
        );
    }

    /// A read of the account of [`read`] whose five-hour limit is `used`
    /// percent full, in a window whose reset its provider doesn't give.
    fn unset(used: f64) -> AccountRead {
        let mut read = read(used, 300);
        for limit in read.limits.iter_mut().flatten() {
            (limit.starts, limit.resets) = (None, None);
        }
        read
    }

    #[test]
    fn a_limit_without_a_reset_runs_out_and_is_back_once_each_time() {
        let sent = alerted(vec![
            (0, unset(100.0)),
            (5, unset(100.0)),
            (10, unset(40.0)),
            (15, unset(40.0)),
            // Used up again, and back again.
            (20, unset(100.0)),
            (25, unset(100.0)),
            (30, unset(10.0)),
        ]);
        assert_eq!(
            sent,
            [
                (0, AlertKind::Reached),
                (10, AlertKind::Available),
                (20, AlertKind::Reached),
                (30, AlertKind::Available)
            ]
        );
    }

    #[test]
    fn a_limit_used_up_is_back_when_its_next_window_gives_no_reset_yet() {
        // Used up in the window resetting at minute 300; after it, the five
        // hours haven't begun, so Claude gives no reset.
        let sent = alerted(vec![(0, read(100.0, 300)), (305, unset(0.0))]);
        assert_eq!(sent, [(0, AlertKind::Reached), (305, AlertKind::Available)]);
    }

    /// A day in minutes.
    const DAY: i64 = 24 * 60;
    /// A week in minutes.
    const WEEK: i64 = 7 * DAY;

    /// A read of the account of [`read`] whose weekly limit is `used`
    /// percent full, in the window from minute `starts`, a week long.
    fn weekly(used: f64, starts: i64) -> AccountRead {
        AccountRead {
            limits: Ok(vec![Reported {
                key: "seven_day".into(),
                name: "Weekly".into(),
                scope: None,
                used,
                starts: Some(minute(starts)),
                resets: Some(minute(starts + WEEK)),
            }]),
            ..read(0.0, 300)
        }
    }

    #[test]
    fn a_week_reset_early_is_said_once_and_only_when_it_gives_back_room() {
        // What is said of running out or being back; the milestones beside
        // it are another test's.
        let said = |reads| -> Vec<(i64, AlertKind)> {
            alerted(reads)
                .into_iter()
                .filter(|(_, kind)| kind.of_running_out())
                .collect()
        };
        let four_days = 4 * DAY;
        // 38% used four days into the week, 0.4 points an hour, lasting the
        // 62 left past its reset three days on. Five minutes later the
        // provider gives a new week, begun then, with nothing used: 38
        // points back, three days early, said once however often it is read.
        assert_eq!(
            said(vec![
                (four_days, weekly(38.0, 0)),
                (four_days + 5, weekly(0.0, four_days + 5)),
                (four_days + 10, weekly(0.0, four_days + 5)),
            ]),
            [(four_days + 5, AlertKind::ResetEarly)]
        );
        // Claude gives no reset for a week not yet begun, so the new week
        // says nothing of how long it is: the one before says it is a week.
        // Said once, and not again as the new week begins.
        let unbegun = |used: f64| {
            let mut read = weekly(used, 0);
            for limit in read.limits.iter_mut().flatten() {
                (limit.starts, limit.resets) = (None, None);
            }
            read
        };
        assert_eq!(
            said(vec![
                (four_days, weekly(38.0, 0)),
                (four_days + 5, unbegun(0.0)),
                (four_days + 10, unbegun(0.0)),
                (four_days + 60, weekly(2.0, four_days + 55)),
            ]),
            [(four_days + 5, AlertKind::ResetEarly)]
        );
        // A week that resets when it said it would is no news, and neither is
        // one reset early with 8 points used, under a tenth of it.
        assert_eq!(
            said(vec![
                (four_days, weekly(38.0, 0)),
                (WEEK + 5, weekly(1.0, WEEK)),
            ]),
            []
        );
        assert_eq!(
            said(vec![(four_days, weekly(38.0, 0)), (WEEK + 5, unbegun(0.0))]),
            []
        );
        assert_eq!(
            said(vec![
                (four_days, weekly(8.0, 0)),
                (four_days + 5, weekly(0.0, four_days + 5)),
            ]),
            []
        );
        // A week used up and reset early is back, which says it.
        assert_eq!(
            said(vec![
                (four_days, weekly(100.0, 0)),
                (four_days + 5, weekly(0.0, four_days + 5)),
            ]),
            [
                (four_days, AlertKind::Reached),
                (four_days + 5, AlertKind::Available)
            ]
        );
        // Five hours reset early are no news: they reset within the day,
        // whether the new five hours have begun or not.
        assert_eq!(
            said(vec![(100, read(40.0, 300)), (105, read(0.0, 405))]),
            []
        );
        assert_eq!(said(vec![(100, read(40.0, 300)), (105, unset(0.0))]), []);
    }

    #[test]
    fn a_week_says_each_quarter_used_once_and_only_the_last_crossed() {
        // From 10% a day in to 55% four days in: past a quarter used and
        // half, said as half left. 55 points in 96 hours, 0.573 an hour,
        // last the 45 left 78.5 hours, past the reset 72 hours on. An hour
        // on, 62%: 62 points in 97 hours, 0.639 an hour, run the 38 left
        // out in 59.5 hours, before it, which is said on its own. A day on,
        // 80%, a quarter left, is said though a warning came before. Used
        // up, it is said as reached, not as a quarter left. The next week,
        // 30% three days in, 0.417 an hour, lasts: its first quarter is
        // said again.
        let milestones: Vec<(i64, AlertKind)> = alerted(vec![
            (DAY, weekly(10.0, 0)),
            (4 * DAY, weekly(55.0, 0)),
            (4 * DAY + 60, weekly(62.0, 0)),
            (5 * DAY, weekly(80.0, 0)),
            (6 * DAY, weekly(100.0, 0)),
            (WEEK + 3 * DAY, weekly(30.0, WEEK)),
        ])
        .into_iter()
        .filter(|(_, kind)| !kind.of_running_out())
        .collect();
        assert_eq!(
            milestones,
            [
                (4 * DAY, AlertKind::HalfLeft),
                (5 * DAY, AlertKind::QuarterLeft),
                (WEEK + 3 * DAY, AlertKind::ThreeQuartersLeft)
            ]
        );
    }

    #[test]
    fn an_account_read_for_the_first_time_says_only_that_it_runs_out() {
        // First read four days in at 30%, 0.3125 points an hour, lasting: a
        // quarter used is where it stands, not news. An hour on, 51%, 0.526
        // an hour, the 49 left lasting 93 hours, past the reset 71 hours
        // off: half left is news.
        assert_eq!(
            alerted(vec![
                (4 * DAY, weekly(30.0, 0)),
                (4 * DAY + 60, weekly(51.0, 0)),
            ]),
            [(4 * DAY + 60, AlertKind::HalfLeft)]
        );
        // First read six days in at 90%, 0.625 an hour: the 10 left run out
        // in 16 hours, before the reset a day off, which is said.
        assert_eq!(
            alerted(vec![(6 * DAY, weekly(90.0, 0))]),
            [(6 * DAY, AlertKind::RunningOut)]
        );
    }

    #[test]
    fn a_quarter_crossed_as_a_week_starts_running_out_is_not_said_beside_it() {
        // 10% a day in, then 30% an hour later: 20 points an hour, so the
        // 70 left last three and a half hours, far before the week resets
        // six days on. That it runs out is said; the quarter used, crossed
        // in the same read, is not, then or later, as it was kept as told.
        let sent = alerted(vec![
            (DAY, weekly(10.0, 0)),
            (DAY + 60, weekly(30.0, 0)),
            (DAY + 70, weekly(33.0, 0)),
        ]);
        assert_eq!(sent, [(DAY + 60, AlertKind::RunningOut)]);
    }

    #[test]
    fn a_week_mostly_left_a_day_before_it_resets_is_said_once() {
        let sent = alerted(vec![
            // Read before, so what follows is news.
            (DAY, weekly(10.0, 0)),
            // 36 hours before the reset, a day and a half: not yet.
            (WEEK - 36 * 60, weekly(30.0, 0)),
            // 18 hours before it, with 69% left.
            (WEEK - 18 * 60, weekly(31.0, 0)),
            (WEEK - 6 * 60, weekly(32.0, 0)),
        ]);
        assert_eq!(
            sent,
            [
                (WEEK - 36 * 60, AlertKind::ThreeQuartersLeft),
                (WEEK - 18 * 60, AlertKind::Unused)
            ]
        );
    }

    #[test]
    fn only_a_fresh_reading_of_a_subscriptions_week_says_it_is_mostly_left() {
        let now = minute(WEEK - 18 * 60);
        let limit = LimitState {
            key: "seven_day".into(),
            name: "Weekly".into(),
            scope: None,
            used: Some(30.0),
            starts: Some(minute(0)),
            resets: Some(minute(WEEK)),
            read_at: now,
            pace: None,
            refilled: false,
        };
        let account = AccountLimits {
            id: "claude:".into(),
            agents: vec![Agent::ClaudeCode],
            read_at: Some(now),
            checked_at: Some(now),
            in_use: false,
            ..account(Vec::new())
        };
        let kinds = |account: &AccountLimits, limit: &LimitState, long: bool| -> Vec<AlertKind> {
            milestones(account, limit, long, now)
                .into_iter()
                .map(|(kind, _, _)| kind)
                .collect()
        };
        assert_eq!(
            kinds(&account, &limit, true),
            [AlertKind::ThreeQuartersLeft, AlertKind::Unused]
        );
        // Read 31 minutes ago, it may have been used since.
        let stale = LimitState {
            read_at: minute(WEEK - 18 * 60 - 31),
            ..limit.clone()
        };
        assert_eq!(kinds(&account, &stale, true), []);
        // Five hours are no week.
        assert_eq!(kinds(&account, &limit, false), []);
        // An API key's limit left isn't lost at its window's end.
        let key = AccountLimits {
            subscription: Subscription::ApiKey,
            ..account.clone()
        };
        assert_eq!(kinds(&key, &limit, true), [AlertKind::ThreeQuartersLeft]);
        // Rising 5 points an hour from 30%, it runs out in 14 hours, before
        // the reset 18 hours off.
        let rising = LimitState {
            pace: Some(5.0),
            ..limit.clone()
        };
        assert_eq!(
            kinds(&account, &rising, true),
            [AlertKind::ThreeQuartersLeft]
        );
    }

    #[test]
    fn a_milestone_said_after_a_warning_does_not_keep_a_week_from_being_back() {
        let sent = alerted(vec![
            // Read before, so what follows is news.
            (12 * 60, weekly(5.0, 0)),
            // 26% two days in, 13 points a day: the 74 left last 137 hours,
            // past the reset five days on.
            (2 * DAY, weekly(26.0, 0)),
            // A day on, 45%: fitted over the day, 19 points in 24 hours, the
            // 55 left last 69 hours, days before the reset.
            (3 * DAY, weekly(45.0, 0)),
            (3 * DAY + 240, weekly(50.0, 0)),
            (WEEK + 10, weekly(1.0, WEEK)),
        ]);
        assert_eq!(
            sent,
            [
                (2 * DAY, AlertKind::ThreeQuartersLeft),
                (3 * DAY, AlertKind::RunningOut),
                (3 * DAY + 240, AlertKind::HalfLeft),
                (WEEK + 10, AlertKind::Available)
            ]
        );
    }
}
