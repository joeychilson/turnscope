//! The weekly recap: from 9 AM each Monday, where the clock is, how each
//! account's week that ended went.
//!
//! An account's week is its limit on all of its use, not one model's, whose
//! windows last a week, give or take a day. The recap tells of the window of
//! it that reset in the seven days before Monday 9 AM, as its readings said
//! it ended: the most of it used, and when a reading first found it used
//! up, if one did. Its readings are told apart from the next window's as a
//! window's reckoning tells them ([`super::share`]): they belong together
//! while their resets agree. Beside that goes the project that took most of
//! what this Mac's agents spent of the account then at list prices, which is
//! what a window's reckoning shares its rises by.
//!
//! Readings are taken only while Turnscope runs, so a week none were taken
//! in has nothing to tell, and one whose last hours no reading saw says only
//! as much as was read; one read after it was used up says so however
//! little was read before.
//!
//! It is due from Monday 9 AM until the next, and sent once in that time,
//! the first time an account has a week to tell. The weeks it told of are
//! recorded in the ledger beside the alerts, so it is not sent again however
//! often the app restarts, and no week is told of twice, as it would be
//! once the clock moved to a zone whose Monday morning comes later. It
//! tells of the weeks that reset after the Monday 9 AM before and by this
//! one, each a local week, which a change of the clock makes an hour short
//! or long. An account the person hid is not told of.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};

use rusqlite::Connection;

use super::share::{counts_in, spending_by, windows_of};
use super::{AccountLimits, DAY, WEEK};
use crate::error::Result;
use crate::ledger::{Ledger, Reading};
use crate::time::{Instant, Zone, monday_morning};

/// An account's week that ended, as the weekly recap tells it.
#[derive(Clone, Debug, PartialEq)]
pub struct WeekEnded {
    /// The account.
    pub account: String,
    /// What the account is called ([`AccountLimits::title`]).
    pub title: String,
    /// What tells the account apart from others of the subscription.
    pub label: Option<String>,
    /// The key of its weekly limit.
    pub key: String,
    /// The weekly limit's name.
    pub limit: String,
    /// How the week ended, as its readings said.
    pub window: PastWindow,
    /// The project that took most of what this Mac's agents spent of the
    /// account that week at list prices, as its folder; `None` when nothing
    /// priced was spent here, or the most went to no project known.
    pub project: Option<String>,
}

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

/// The weeks the weekly recap due at `now` in `zone` tells of, from
/// `ledger`'s readings of `accounts` and the `cache`'s usage, recorded in
/// `ledger` as told: none when it was sent since Monday 9 AM already, or no
/// account has a week to tell.
///
/// # Errors
///
/// Returns an error when the ledger or the cache cannot be read, or the
/// ledger cannot be written.
pub(crate) fn recap(
    ledger: &mut Ledger,
    cache: &Connection,
    accounts: &[AccountLimits],
    now: Instant,
    zone: &Zone,
) -> Result<Vec<WeekEnded>> {
    let Some(morning) = monday_morning(now, zone) else {
        return Ok(Vec::new());
    };
    if ledger.recapped_since(morning)? {
        return Ok(Vec::new());
    }
    let Some(before) =
        Instant::from_millis(morning.millis() - 1).and_then(|at| monday_morning(at, zone))
    else {
        return Ok(Vec::new());
    };
    let from = before.millis();
    // A week told of resets after `from` and lasts at most eight days, so
    // it began after this.
    let Some(since) = Instant::from_millis(from - WEEK - DAY) else {
        return Ok(Vec::new());
    };
    let mut weeks = Vec::new();
    for account in accounts.iter().filter(|account| !account.hidden) {
        for limit in account.limits.iter().filter(|limit| limit.scope.is_none()) {
            let ended = reset_by(ledger, &account.id, &limit.key, since, morning)?;
            let Some(window) = ended.into_iter().rev().find(|window| {
                a_week(window) && window.resets.is_some_and(|at| at.millis() > from)
            }) else {
                continue;
            };
            if let Some(resets) = window.resets
                && ledger.recapped(&account.id, &limit.key, resets.millis())?
            {
                continue;
            }
            let project = match window.starts.zip(window.resets) {
                Some((starts, resets)) => most(cache, &account.id, starts, resets)?,
                None => None,
            };
            weeks.push(WeekEnded {
                account: account.id.clone(),
                title: account.title(),
                label: account.label.clone(),
                key: limit.key.clone(),
                limit: limit.name.clone(),
                window,
                project,
            });
            break;
        }
    }
    let told: Vec<(&str, &str, i64)> = weeks
        .iter()
        .filter_map(|week| {
            let resets = week.window.resets?;
            Some((week.account.as_str(), week.key.as_str(), resets.millis()))
        })
        .collect();
    ledger.send_recap(&told, now)?;
    Ok(weeks)
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
fn reset_by(
    ledger: &Ledger,
    account: &str,
    key: &str,
    since: Instant,
    by: Instant,
) -> Result<Vec<PastWindow>> {
    Ok(windows_of(ledger, account, key, since)?
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

/// Whether `window` lasted a week, give or take a day.
fn a_week(window: &PastWindow) -> bool {
    window
        .starts
        .zip(window.resets)
        .is_some_and(|(starts, resets)| {
            (WEEK - DAY..=WEEK + DAY).contains(&(resets.millis() - starts.millis()))
        })
}

/// The project, as its folder, that took most of what `account`'s usage
/// after `starts` and through `resets` cost at list prices, from the
/// `cache`; `None` when none of it was priced, or the most went to no
/// project known. Of two that took as much, the first by folder.
fn most(
    cache: &Connection,
    account: &str,
    starts: Instant,
    resets: Instant,
) -> Result<Option<String>> {
    let mut by_session: HashMap<i64, i64> = HashMap::new();
    for row in spending_by(cache, account, None, starts.millis(), resets.millis())?.rows {
        if let Some(cost) = row.cost {
            let sum = by_session.entry(row.session).or_default();
            *sum = sum.saturating_add(cost);
        }
    }
    let mut spent: BTreeMap<Option<String>, i64> = BTreeMap::new();
    for (session, cost) in by_session {
        let sum = spent.entry(counts_in(cache, session)?.1).or_default();
        *sum = sum.saturating_add(cost);
    }
    Ok(spent
        .into_iter()
        .min_by_key(|(project, spent)| (Reverse(*spent), project.is_none(), project.clone()))
        .and_then(|(project, _)| project))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use rusqlite::{Connection, params};

    use super::{PastWindow, WeekEnded, recap, summed};
    use crate::agent::Agent;
    use crate::ledger::tests::scratch;
    use crate::ledger::{Ledger, Reading};
    use crate::limits::share::windows;
    use crate::limits::{AccountRead, HOUR, Reported, Subscription, state};
    use crate::time::{Instant, Zone};

    fn at(text: &str) -> Instant {
        Instant::parse(text).unwrap()
    }

    /// A read of `id`'s weekly limit, `used` percent, in the week from
    /// `starts` to `resets`.
    fn week(id: &str, used: f64, starts: &str, resets: &str) -> AccountRead {
        AccountRead {
            id: id.to_owned(),
            label: None,
            plan: None,
            agents: vec![Agent::ClaudeCode],
            limits: Ok(vec![Reported {
                key: "seven_day".into(),
                name: "Weekly".into(),
                scope: None,
                used,
                starts: Some(at(starts)),
                resets: Some(at(resets)),
            }]),
        }
    }

    /// A cache holding `sessions`, each its id, its key, the key of the
    /// session it counts in and its project, and `usage`, each of a session,
    /// the account it drew on, when, and what it cost.
    fn cache(
        sessions: &[(i64, &str, &str, Option<&str>)],
        usage: &[(i64, &str, &str, i64)],
    ) -> Connection {
        let cache = Connection::open_in_memory().unwrap();
        cache
            .execute_batch(
                "CREATE TABLE usage (session INTEGER, account TEXT, at INTEGER, cost INTEGER,
                                     agent TEXT NOT NULL DEFAULT 'claude-code',
                                     model_key TEXT NOT NULL DEFAULT 'claude-opus-5');
                 CREATE INDEX usage_account ON usage (account, at);
                 CREATE TABLE session (id INTEGER PRIMARY KEY, key TEXT, root TEXT, project TEXT,
                                       project_name TEXT);",
            )
            .unwrap();
        for (id, key, root, project) in sessions {
            cache
                .execute(
                    "INSERT INTO session (id, key, root, project) VALUES (?1, ?2, ?3, ?4)",
                    params![id, key, root, project],
                )
                .unwrap();
        }
        for (session, account, when, cost) in usage {
            cache
                .execute(
                    "INSERT INTO usage (session, account, at, cost) VALUES (?1, ?2, ?3, ?4)",
                    params![session, account, at(when).millis(), cost],
                )
                .unwrap();
        }
        cache
    }

    #[test]
    fn from_monday_morning_each_accounts_week_that_ended_is_told_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite");
        let mut ledger = Ledger::open(&path).unwrap();
        // Work's week ran from Thu 2026-09-17 to Thu 09-24 at 16:00 UTC, read
        // at 20%, 71% and 64%: it ended at 71%, the most read. Personal's ran
        // from Sat 09-19 to Sat 09-26 at 12:00 and was used up by 20:00 on
        // the 25th. Hidden's ended on the 25th too, but the person hid it.
        // Each was read once in its next week.
        let reads = [
            (
                "2026-09-18T12:00:00Z",
                week(
                    "claude:work",
                    20.0,
                    "2026-09-17T16:00:00Z",
                    "2026-09-24T16:00:00Z",
                ),
            ),
            (
                "2026-09-22T12:00:00Z",
                week(
                    "claude:work",
                    71.0,
                    "2026-09-17T16:00:00Z",
                    "2026-09-24T16:00:00Z",
                ),
            ),
            (
                "2026-09-24T15:00:00Z",
                week(
                    "claude:work",
                    64.0,
                    "2026-09-17T16:00:00Z",
                    "2026-09-24T16:00:00Z",
                ),
            ),
            (
                "2026-09-25T12:00:00Z",
                week(
                    "claude:work",
                    5.0,
                    "2026-09-24T16:00:00Z",
                    "2026-10-01T16:00:00Z",
                ),
            ),
            (
                "2026-09-20T12:00:00Z",
                week(
                    "claude:personal",
                    40.0,
                    "2026-09-19T12:00:00Z",
                    "2026-09-26T12:00:00Z",
                ),
            ),
            (
                "2026-09-25T20:00:00Z",
                week(
                    "claude:personal",
                    100.0,
                    "2026-09-19T12:00:00Z",
                    "2026-09-26T12:00:00Z",
                ),
            ),
            (
                "2026-09-26T13:00:00Z",
                week(
                    "claude:personal",
                    1.0,
                    "2026-09-26T12:00:00Z",
                    "2026-10-03T12:00:00Z",
                ),
            ),
            (
                "2026-09-20T12:00:00Z",
                week(
                    "claude:hidden",
                    30.0,
                    "2026-09-18T12:00:00Z",
                    "2026-09-25T12:00:00Z",
                ),
            ),
            (
                "2026-09-26T12:00:00Z",
                week(
                    "claude:hidden",
                    2.0,
                    "2026-09-25T12:00:00Z",
                    "2026-10-02T12:00:00Z",
                ),
            ),
        ];
        for (when, read) in reads {
            ledger
                .record_limits(Subscription::Claude, &[read], at(when))
                .unwrap();
        }
        // Work's week: 2 in turnscope's session, and 2 in a subagent of it
        // with no project of its own, counted in turnscope's, 4 in all, over
        // ledger's 3. The 10 spent in the next week is the next week's.
        let cache = cache(
            &[
                (1, "claude-code:a", "claude-code:a", Some("/work/turnscope")),
                (2, "claude-code:b", "claude-code:b", Some("/work/ledger")),
                (3, "claude-code:a/agent-1", "claude-code:a", None),
            ],
            &[
                (1, "claude:work", "2026-09-18T13:00:00Z", 2),
                (2, "claude:work", "2026-09-20T13:00:00Z", 3),
                (3, "claude:work", "2026-09-22T13:00:00Z", 2),
                (2, "claude:work", "2026-09-25T13:00:00Z", 10),
            ],
        );
        let told = |ledger: &mut Ledger, now: &str, zone: &Zone| -> Vec<WeekEnded> {
            let mut accounts = state(ledger, &HashSet::new(), at(now)).unwrap();
            for account in &mut accounts {
                account.hidden = account.id == "claude:hidden";
            }
            recap(ledger, &cache, &accounts, at(now), zone).unwrap()
        };
        // Monday 2026-09-28, in Los Angeles, seven hours behind UTC: 9 AM is
        // 16:00 UTC. A minute before, the recap due is the week before's,
        // and no week read here reset between 09-14 and 09-21 at 16:00.
        let los_angeles = Zone::named("America/Los_Angeles").unwrap();
        assert_eq!(told(&mut ledger, "2026-09-28T15:59:00Z", &los_angeles), []);
        let sent = told(&mut ledger, "2026-09-28T16:30:00Z", &los_angeles);
        assert_eq!(
            sent.iter()
                .map(|week| (
                    week.account.as_str(),
                    week.title.as_str(),
                    week.window.clone(),
                    week.project.as_deref()
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "claude:personal",
                    "Claude",
                    PastWindow {
                        starts: Some(at("2026-09-19T12:00:00Z")),
                        resets: Some(at("2026-09-26T12:00:00Z")),
                        used: 100.0,
                        reached: Some(at("2026-09-25T20:00:00Z")),
                    },
                    None,
                ),
                (
                    "claude:work",
                    "Claude",
                    PastWindow {
                        starts: Some(at("2026-09-17T16:00:00Z")),
                        resets: Some(at("2026-09-24T16:00:00Z")),
                        used: 71.0,
                        reached: None,
                    },
                    Some("/work/turnscope"),
                ),
            ]
        );
        // Once a week, however often it is asked, and after a restart.
        assert_eq!(told(&mut ledger, "2026-09-28T17:00:00Z", &los_angeles), []);
        drop(ledger);
        let mut ledger = Ledger::open(&path).unwrap();
        assert_eq!(told(&mut ledger, "2026-10-01T10:00:00Z", &los_angeles), []);

        // In Tokyo, nine hours ahead, 9 AM that Monday is midnight UTC: at
        // 03:00 UTC, noon there, it is due, where in Los Angeles, 8 PM on
        // Sunday, it isn't yet.
        let (_dir, mut elsewhere) = scratch();
        for (when, read) in reads_again() {
            elsewhere
                .record_limits(Subscription::Claude, &[read], at(when))
                .unwrap();
        }
        let tokyo = Zone::named("Asia/Tokyo").unwrap();
        assert_eq!(
            told(&mut elsewhere, "2026-09-28T03:00:00Z", &los_angeles),
            []
        );
        assert_eq!(
            told(&mut elsewhere, "2026-09-28T03:00:00Z", &tokyo)
                .iter()
                .map(|week| week.account.as_str())
                .collect::<Vec<_>>(),
            ["claude:work"]
        );
        // Taken to Los Angeles, where Monday 9 AM comes 16 hours later, the
        // week told of already is not told again.
        assert_eq!(
            told(&mut elsewhere, "2026-09-28T16:30:00Z", &los_angeles),
            []
        );
    }

    #[test]
    fn a_recap_tells_of_the_local_week_when_the_clock_changes() {
        // Los Angeles's clocks go forward on 14 March 2027: Monday the 8th
        // at 9 AM is 17:00 UTC, and the 15th at 9 AM is 16:00, 167 hours
        // later. A week that reset on the 8th at 16:30 UTC, 8:30 AM there,
        // was the 8th's to tell, 167.5 hours before the 15th's morning; the
        // next resets on the 15th at 16:30, 9:30 AM there, the 22nd's.
        let reads = [
            (
                "2027-03-05T12:00:00Z",
                week(
                    "claude:work",
                    50.0,
                    "2027-03-01T16:30:00Z",
                    "2027-03-08T16:30:00Z",
                ),
            ),
            (
                "2027-03-10T12:00:00Z",
                week(
                    "claude:work",
                    10.0,
                    "2027-03-08T16:30:00Z",
                    "2027-03-15T16:30:00Z",
                ),
            ),
        ];
        let (_dir, mut ledger) = scratch();
        for (when, read) in reads {
            ledger
                .record_limits(Subscription::Claude, &[read], at(when))
                .unwrap();
        }
        let los_angeles = Zone::named("America/Los_Angeles").unwrap();
        let cache = cache(&[], &[]);
        let mut told = |now: &str| -> Vec<(String, Option<Instant>)> {
            let accounts = state(&ledger, &HashSet::new(), at(now)).unwrap();
            recap(&mut ledger, &cache, &accounts, at(now), &los_angeles)
                .unwrap()
                .into_iter()
                .map(|week| (week.account, week.window.resets))
                .collect()
        };
        assert_eq!(told("2027-03-15T16:30:00Z"), []);
        assert_eq!(
            told("2027-03-22T16:30:00Z"),
            [("claude:work".to_owned(), Some(at("2027-03-15T16:30:00Z")))]
        );
    }

    /// Work's week of the test above, alone.
    fn reads_again() -> Vec<(&'static str, AccountRead)> {
        vec![
            (
                "2026-09-22T12:00:00Z",
                week(
                    "claude:work",
                    71.0,
                    "2026-09-17T16:00:00Z",
                    "2026-09-24T16:00:00Z",
                ),
            ),
            (
                "2026-09-25T12:00:00Z",
                week(
                    "claude:work",
                    5.0,
                    "2026-09-24T16:00:00Z",
                    "2026-10-01T16:00:00Z",
                ),
            ),
        ]
    }

    #[test]
    fn a_limit_of_five_hours_or_a_month_is_no_week() {
        let (_dir, mut ledger) = scratch();
        let limit = |key: &str, starts: &str, resets: &str| Reported {
            key: key.into(),
            name: key.into(),
            scope: None,
            used: 50.0,
            starts: Some(at(starts)),
            resets: Some(at(resets)),
        };
        // Five hours that reset on Saturday, and a month on the 27th, each
        // read again in the window after, all before Monday 9 AM.
        let reads = [
            (
                "2026-09-26T10:00:00Z",
                [
                    limit("five_hour", "2026-09-26T08:00:00Z", "2026-09-26T13:00:00Z"),
                    limit("monthly", "2026-08-27T00:00:00Z", "2026-09-27T00:00:00Z"),
                ],
            ),
            (
                "2026-09-27T10:00:00Z",
                [
                    limit("five_hour", "2026-09-27T08:00:00Z", "2026-09-27T13:00:00Z"),
                    limit("monthly", "2026-09-27T00:00:00Z", "2026-10-27T00:00:00Z"),
                ],
            ),
        ];
        for (when, limits) in reads {
            let read = AccountRead {
                limits: Ok(limits.to_vec()),
                ..week("claude:work", 0.0, when, when)
            };
            ledger
                .record_limits(Subscription::Claude, &[read], at(when))
                .unwrap();
        }
        let now = at("2026-09-28T17:00:00Z");
        let accounts = state(&ledger, &HashSet::new(), now).unwrap();
        assert_eq!(accounts[0].limits.len(), 2);
        let utc = Zone::named("UTC").unwrap();
        assert_eq!(
            recap(&mut ledger, &cache(&[], &[]), &accounts, now, &utc).unwrap(),
            []
        );
    }

    /// How each window of `readings` ended, oldest first.
    fn ended(readings: Vec<Reading>) -> Vec<PastWindow> {
        windows(readings)
            .iter()
            .filter_map(|window| summed(window))
            .collect()
    }

    /// 2026-09-03T09:00:00Z, a Thursday.
    const WEEK_START: i64 = 1_788_426_000_000;

    /// `hours` on from [`WEEK_START`].
    fn hour(hours: i64) -> Instant {
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
            at: hour(starts + hours),
            used,
            starts: Some(hour(starts)),
            resets: Some(hour(starts + 168)),
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
                    starts: Some(hour(0)),
                    resets: Some(hour(168)),
                    used: 80.,
                    reached: None,
                },
                PastWindow {
                    starts: Some(hour(168)),
                    resets: Some(hour(336)),
                    used: 100.,
                    reached: Some(hour(318)),
                },
                PastWindow {
                    starts: Some(hour(336)),
                    resets: Some(hour(504)),
                    used: 3.,
                    reached: None,
                },
            ]
        );
    }
}
