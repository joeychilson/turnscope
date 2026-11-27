//! Notifications: few, each worth acting on, never repeated.
//!
//! The rules are in docs/architecture.md#alerts.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::status::{Account, Limit, Moment, Outlook, State, Status, Unknown};
use crate::time::{DAY, MINUTE};
use crate::{Result, forecast};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    RunningOut,
    UsedUp,
    Reset,
    SignIn,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::RunningOut => "runningOut",
            Kind::UsedUp => "usedUp",
            Kind::Reset => "reset",
            Kind::SignIn => "signIn",
        }
    }

    fn parse(text: &str) -> Option<Kind> {
        [Kind::RunningOut, Kind::UsedUp, Kind::Reset, Kind::SignIn]
            .into_iter()
            .find(|kind| kind.as_str() == text)
    }
}

/// An alert, as sent: when each kind is raised is in
/// docs/architecture.md#alerts. The horizons of `resets` and `runs_out` are
/// worked out when it's sent, not when it was raised.
#[derive(Clone, Debug, PartialEq)]
pub struct Alert {
    /// The id to acknowledge it by.
    pub id: i64,
    pub kind: Kind,
    /// The account's id.
    pub account: String,
    /// Its title and label when raised: `Claude Max (me@example.com)`.
    pub account_title: String,
    /// The limit's key. None for `signIn`.
    pub limit: Option<String>,
    /// The limit's name. None for `signIn`.
    pub limit_name: Option<String>,
    /// When the limit's window resets. None for `signIn`, and for a limit
    /// that never resets, such as credits.
    pub resets: Option<Moment>,
    /// For `runningOut`, when it likely runs out. Otherwise None.
    pub runs_out: Option<Moment>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Detail {
    account_title: String,
    limit_name: Option<String>,
    resets: Option<i64>,
    runs_out: Option<i64>,
}

pub fn raise(db: &mut Connection, status: &Status) -> Result<Vec<Alert>> {
    let now = status.at;
    let notify = status.settings.notify;
    let tx = crate::db::write(db)?;
    let mut raised = Vec::new();
    let mut raise = |account: &Account,
                     limit: Option<&Limit>,
                     kind: Kind,
                     window: i64,
                     runs_out: Option<i64>|
     -> Result<()> {
        let key = limit.map_or("", |limit| limit.key.as_str());
        // A reset drifts from read to read. A size, below zero, doesn't, and
        // within the drift $10 more would match; nor does a sign-in's last
        // read that worked, and one minutes after it is a sign-in since.
        let exact = window < 0 || kind == Kind::SignIn;
        let drift = if exact { 0 } else { forecast::DRIFT };
        let known = |kind: Kind| -> Result<bool> {
            Ok(tx
                .prepare_cached(
                    "SELECT 1 FROM alert WHERE account = ?1 AND limit_key = ?2 AND kind = ?3 AND window BETWEEN ?4 - ?5 AND ?4 + ?5",
                )?
                .query_row(params![account.id, key, kind.as_str(), window, drift], |_| {
                    Ok(())
                })
                .optional()?
                .is_some())
        };
        if known(kind)? {
            return Ok(());
        }
        // A reset ends the limit's warnings, and the limit used up again
        // ends its reset, so each is told again the next time.
        let ended: &[Kind] = match kind {
            Kind::Reset => &[Kind::RunningOut, Kind::UsedUp],
            Kind::UsedUp => &[Kind::Reset],
            Kind::RunningOut | Kind::SignIn => &[],
        };
        for ended in ended {
            tx.prepare_cached(
                "DELETE FROM alert WHERE account = ?1 AND limit_key = ?2 AND kind = ?3",
            )?
            .execute(params![account.id, key, ended.as_str()])?;
        }
        let on = match kind {
            Kind::RunningOut => notify.running_out,
            Kind::UsedUp => notify.used_up,
            Kind::Reset => notify.reset,
            Kind::SignIn => notify.sign_in,
        };
        let news = match kind {
            // Back while another limit that stops it is used up, it still
            // can't be used: that limit's own return says so.
            Kind::Reset => {
                recent(window, limit.and_then(Limit::length), now)
                    && !limit.is_some_and(|limit| blocked(account, limit))
            }
            // Warned that a window of a day or less runs out, the warning
            // said it, and when it's back. Replayed (November 2026), 5 hours
            // were used up 0.1–2.9 hours after their warning, weeks and months
            // 28 hours to 7 days, some sooner than said. With warnings off,
            // none was told.
            Kind::UsedUp => {
                limit
                    .and_then(Limit::length)
                    .is_none_or(|length| length > DAY)
                    || !notify.running_out
                    || !known(Kind::RunningOut)?
            }
            Kind::RunningOut | Kind::SignIn => true,
        };
        let told = on && !account.hidden && news;
        let detail = Detail {
            account_title: account.full_title(),
            limit_name: limit.map(|limit| limit.name.clone()),
            resets: limit
                .and_then(|limit| limit.window)
                .map(|window| window.resets.at),
            runs_out,
        };
        tx.prepare_cached(
            "INSERT INTO alert (account, limit_key, kind, window, at, acknowledged, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?
        .execute(params![
            account.id,
            key,
            kind.as_str(),
            window,
            now,
            !told,
            serde_json::to_string(&detail).unwrap_or_default()
        ])?;
        if told {
            let id = tx.last_insert_rowid();
            raised.push(alert(id, kind, &account.id, key, detail, now));
        }
        Ok(())
    };
    for account in &status.accounts {
        if account.refused() && account.in_use {
            let last_good = match account.state {
                State::AsOf { read_at, .. } => read_at,
                _ => 0,
            };
            raise(account, None, Kind::SignIn, last_good, None)?;
        }
        for limit in &account.limits {
            let Some(window) = window(limit) else {
                continue;
            };
            // Back after being used up: a window used up that ended in the
            // last day, or that the limit no longer reads, as when a
            // provider resets limits early, or this one when use was given
            // back. A size (below zero) is no time, so it has always ended:
            // money is back once a cent is left, as $5 is under 1% of $1,000
            // bought. Of two used up with none back between, one back tells
            // both.
            let left = match limit.money {
                Some(money) => money.left_usd > 0.0,
                None => limit.left_percent > 0,
            };
            if left
                && let Some(ended) = tx
                    .prepare_cached(
                        "SELECT window FROM alert WHERE account = ?1 AND limit_key = ?2
                         AND kind = 'usedUp' AND (window >= ?3 OR window < 0)
                         ORDER BY id DESC LIMIT 1",
                    )?
                    .query_row(params![account.id, limit.key, now - DAY], |row| {
                        row.get::<_, i64>(0)
                    })
                    .optional()?
            {
                let this = limit.window.is_some_and(|window| {
                    forecast::same_reset(Some(ended), Some(window.resets.at))
                });
                // Reset early, a read shows a new window with a new reset:
                // waiting for the old one told it days late.
                let moved = limit.window.is_some() && !this;
                if ended <= now || moved || (this && limit.left_percent > 1) {
                    raise(account, Some(limit), Kind::Reset, ended, None)?;
                }
            }
            // After its return, which ends the warnings told before it.
            match limit.outlook {
                // Back within 15 minutes, it's back about as soon as a read
                // sees it used up: neither is news.
                Outlook::UsedUp { back: Some(back) } if back.at - now <= 15 * MINUTE => {}
                Outlook::UsedUp { .. } if account.in_use => {
                    raise(account, Some(limit), Kind::UsedUp, window, None)?
                }
                Outlook::RunsOut { likely, .. } if account.in_use => raise(
                    account,
                    Some(limit),
                    Kind::RunningOut,
                    window,
                    Some(likely.at),
                )?,
                _ => {}
            }
        }
    }
    tx.commit()?;
    Ok(raised)
}

/// What tells one time `limit` is used up from the next, as an alert's
/// `window`: its window's reset. Money that never resets has its size in
/// cents instead, one at least, below zero so it's never taken for a time.
/// A size is all ever bought (`providers::Reading::size`), so credits are
/// told used up again only once more was bought and used up. Chosen over
/// when it was first read used up, which needs its readings. `None` with
/// neither: it raises nothing, over telling it once and never again.
fn window(limit: &Limit) -> Option<i64> {
    match (limit.window, limit.money) {
        (Some(window), _) => Some(window.resets.at),
        (None, Some(money)) => Some(-((money.size_usd * 100.0).round() as i64).max(1)),
        (None, None) => None,
    }
}

/// Whether a return at `back`, the reset of the window used up, is still
/// news: for its window's length, a day at most. Told when the Mac woke 13
/// hours after a 5-hour window came back, it's always back by then. Money
/// that never resets has no time it came back, and is news until told.
fn recent(back: i64, length: Option<i64>, now: i64) -> bool {
    back < 0 || now - back <= length.unwrap_or(DAY).min(DAY)
}

fn blocked(account: &Account, limit: &Limit) -> bool {
    account.limits.iter().any(|other| {
        other.key != limit.key
            && (other.whole() || (other.scope.is_some() && other.scope == limit.scope))
            && matches!(other.outlook, Outlook::UsedUp { .. })
    })
}

/// Unacknowledged alerts that still apply against `status`, skipping hidden
/// accounts and limits that have since recovered or moved to a new window.
pub fn pending(db: &Connection, status: &Status, now: i64) -> Result<Vec<Alert>> {
    let mut statement = db.prepare(
        "SELECT id, kind, at, account, limit_key, detail, window FROM alert WHERE acknowledged = 0 ORDER BY id",
    )?;
    let mut rows = statement.query([])?;
    let mut pending = Vec::new();
    while let Some(row) = rows.next()? {
        let (Some(kind), Ok(detail)) = (
            Kind::parse(row.get_ref(1)?.as_str()?),
            serde_json::from_str::<Detail>(row.get_ref(5)?.as_str()?),
        ) else {
            continue;
        };
        let (id, at): (i64, i64) = (row.get(0)?, row.get(2)?);
        let account = row.get_ref(3)?.as_str()?;
        let limit = row.get_ref(4)?.as_str()?;
        let known = status.accounts.iter().find(|known| known.id == account);
        if known.is_some_and(|account| account.hidden) {
            continue;
        }
        let known_limit = known.and_then(|known| known.limits.iter().find(|l| l.key == limit));
        let stands = match kind {
            Kind::RunningOut | Kind::UsedUp => {
                let applies = |limit: &Limit| match kind {
                    Kind::UsedUp => matches!(limit.outlook, Outlook::UsedUp { .. }),
                    Kind::RunningOut => matches!(
                        limit.outlook,
                        Outlook::RunsOut { .. }
                            | Outlook::Unknown {
                                reason: Unknown::Stale
                            }
                    ),
                    Kind::Reset | Kind::SignIn => false,
                };
                match detail.resets {
                    Some(resets) => {
                        let moved = known_limit.and_then(|l| l.window).is_some_and(|window| {
                            !forecast::same_reset(Some(resets), Some(window.resets.at))
                        });
                        resets > now && !moved && known_limit.is_none_or(applies)
                    }
                    None => known_limit.is_some_and(applies),
                }
            }
            Kind::Reset => {
                now - at <= DAY && recent(row.get(6)?, known_limit.and_then(Limit::length), now)
            }
            Kind::SignIn => now - at <= DAY && known.is_some_and(Account::refused),
        };
        if stands {
            pending.push(alert(id, kind, account, limit, detail, now));
        }
    }
    Ok(pending)
}

pub fn acknowledge(db: &mut Connection, id: i64) -> Result<()> {
    let tx = crate::db::write(db)?;
    tx.execute("UPDATE alert SET acknowledged = 1 WHERE id = ?1", [id])?;
    tx.commit()?;
    Ok(())
}

/// The notification an alert takes the place of: the last one about the same limit, or about
/// the account where it has no limit.
pub fn replaces(account: &str, limit: Option<&str>) -> String {
    match limit {
        Some(limit) => format!("limit:{account}:{limit}"),
        None => format!("account:{account}"),
    }
}

fn alert(id: i64, kind: Kind, account: &str, limit: &str, detail: Detail, now: i64) -> Alert {
    let limit = Some(limit.to_owned()).filter(|limit| !limit.is_empty());
    Alert {
        id,
        kind,
        account: account.to_owned(),
        account_title: detail.account_title,
        limit,
        limit_name: detail.limit_name,
        resets: detail.resets.map(|at| Moment::of(at, now)),
        runs_out: detail.runs_out.map(|at| Moment::of(at, now)),
    }
}

/// The person's settings: which notifications to send.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub notify: Notify,
}

/// Which alerts to send, each on by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Notify {
    /// When a limit in use will run out before it resets.
    pub running_out: bool,
    /// When a limit in use is used up.
    pub used_up: bool,
    /// When a limit that was used up is back.
    pub reset: bool,
    /// When an account in use needs signing in again.
    pub sign_in: bool,
}

impl Default for Notify {
    fn default() -> Notify {
        Notify {
            running_out: true,
            used_up: true,
            reset: true,
            sign_in: true,
        }
    }
}

pub fn settings(db: &rusqlite::Connection) -> Result<Settings> {
    let saved: Option<String> = db.query_row("SELECT settings FROM state", [], |row| row.get(0))?;
    Ok(saved
        .and_then(|saved| serde_json::from_str(&saved).ok())
        .unwrap_or_default())
}

pub fn save_settings(db: &rusqlite::Connection, settings: &Settings) -> Result<()> {
    crate::db::writing(db)?;
    db.execute(
        "UPDATE state SET settings = ?1",
        [serde_json::to_string(settings).unwrap_or_default()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test::rows;
    use crate::status::{Horizon, LimitWindow};
    use crate::time::HOUR;

    fn status(at: i64, left: u8, outlook: Outlook) -> Status {
        let mut status: Status =
            serde_json::from_str(include_str!("../tests/fixtures/status.json")).unwrap();
        status.at = at;
        status.accounts.truncate(1);
        let account = &mut status.accounts[0];
        account.state = State::Live { read_at: at };
        account.limits.truncate(1);
        let limit = &mut account.limits[0];
        limit.window = Some(LimitWindow {
            starts_at: Some(0),
            resets: Moment {
                at: 10 * DAY,
                horizon: Horizon::Later,
            },
        });
        (limit.left_percent, limit.outlook) = (left, outlook);
        status
    }

    const LASTS: Outlook = Outlook::Lasts {
        left_at_reset: None,
    };

    /// Its window 5 hours long, resetting on day 10.
    fn five_hours(at: i64, left: u8, outlook: Outlook) -> Status {
        let mut status = status(at, left, outlook);
        let window = status.accounts[0].limits[0].window.as_mut().unwrap();
        window.starts_at = Some(10 * DAY - 5 * HOUR);
        status
    }

    fn soon(at: i64) -> Moment {
        Moment {
            at,
            horizon: Horizon::Soon,
        }
    }

    fn open() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).unwrap();
        (dir, db)
    }

    fn kinds(raised: &[Alert]) -> Vec<Kind> {
        raised.iter().map(|alert| alert.kind).collect()
    }

    #[test]
    fn a_limit_used_up_is_told_once_and_its_return_after_each_time() {
        let (_dir, mut db) = open();
        let used_up = status(DAY, 0, Outlook::UsedUp { back: None });
        let raised = raise(&mut db, &used_up).unwrap();
        assert_eq!(kinds(&raised), [Kind::UsedUp]);
        assert_eq!(raised[0].limit.as_deref(), Some("five_hour"));
        assert!(raise(&mut db, &used_up).unwrap().is_empty(), "told once");
        assert_eq!(pending(&db, &used_up, DAY).unwrap().len(), 1);
        acknowledge(&mut db, raised[0].id).unwrap();
        assert!(pending(&db, &used_up, DAY).unwrap().is_empty());
        // Use given back within the window: it's back.
        let back = status(DAY + MINUTE, 60, LASTS);
        assert_eq!(kinds(&raise(&mut db, &back).unwrap()), [Kind::Reset]);
        // Used up again in the same window, and back once it ends.
        let again = status(2 * DAY, 0, Outlook::UsedUp { back: None });
        assert_eq!(kinds(&raise(&mut db, &again).unwrap()), [Kind::UsedUp]);
        assert!(raise(&mut db, &again).unwrap().is_empty(), "told once");
        let ended = status(10 * DAY + MINUTE, 100, LASTS);
        assert_eq!(kinds(&raise(&mut db, &ended).unwrap()), [Kind::Reset]);
        assert!(raise(&mut db, &ended).unwrap().is_empty(), "told once");
    }

    /// 5 hours used up on day 10, back at its reset: told within 5 hours,
    /// with the first status too while the app hasn't shown it, and not
    /// when the Mac wakes the next morning.
    #[test]
    fn a_return_is_told_only_while_its_window_could_not_have_reset_anyway() {
        let used_up = five_hours(10 * DAY - HOUR, 0, Outlook::UsedUp { back: None });
        let back = |hours| five_hours(10 * DAY + hours * HOUR, 100, LASTS);
        let (_dir, mut db) = open();
        raise(&mut db, &used_up).unwrap();
        assert_eq!(kinds(&raise(&mut db, &back(4)).unwrap()), [Kind::Reset]);
        let first = |hours| kinds(&pending(&db, &back(hours), 10 * DAY + hours * HOUR).unwrap());
        assert_eq!(first(5), [Kind::Reset]);
        assert!(first(6).is_empty());

        let (_dir, mut db) = open();
        raise(&mut db, &used_up).unwrap();
        assert!(raise(&mut db, &back(13)).unwrap().is_empty());
    }

    #[test]
    fn a_limit_back_within_15_minutes_is_told_neither_used_up_nor_back() {
        for (minutes, told) in [(15, vec![]), (20, vec![Kind::UsedUp, Kind::Reset])] {
            let (_dir, mut db) = open();
            let back = Some(soon(10 * DAY));
            let used_up = status(10 * DAY - minutes * MINUTE, 0, Outlook::UsedUp { back });
            let mut said = raise(&mut db, &used_up).unwrap();
            said.extend(raise(&mut db, &status(10 * DAY + MINUTE, 100, LASTS)).unwrap());
            assert_eq!(kinds(&said), told);
        }
    }

    /// Warned that 5 hours runs out, it's told back but not used up: the
    /// warning said that. Told used up when not warned, when warnings are
    /// off, and of a longer window, whose used up can come days after its
    /// warning.
    #[test]
    fn a_5_hour_window_warned_of_is_told_back_but_not_used_up() {
        let out = Outlook::RunsOut {
            likely: soon(10 * DAY - 30 * MINUTE),
            soonest: soon(10 * DAY - 30 * MINUTE),
        };
        let used_up = Outlook::UsedUp { back: None };
        let (_dir, mut db) = open();
        raise(&mut db, &five_hours(10 * DAY - HOUR, 20, out.clone())).unwrap();
        let five_used_up = five_hours(10 * DAY - 30 * MINUTE, 0, used_up.clone());
        assert!(raise(&mut db, &five_used_up).unwrap().is_empty());
        let back = five_hours(10 * DAY + MINUTE, 100, LASTS);
        assert_eq!(kinds(&raise(&mut db, &back).unwrap()), [Kind::Reset]);

        let (_dir, mut db) = open();
        assert_eq!(
            kinds(&raise(&mut db, &five_used_up).unwrap()),
            [Kind::UsedUp]
        );

        let (_dir, mut db) = open();
        raise(&mut db, &runs_out(DAY)).unwrap();
        let week_used_up = status(2 * DAY, 0, used_up);
        assert_eq!(
            kinds(&raise(&mut db, &week_used_up).unwrap()),
            [Kind::UsedUp]
        );

        let (_dir, mut db) = open();
        let mut warned = five_hours(10 * DAY - HOUR, 20, out);
        warned.settings.notify.running_out = false;
        raise(&mut db, &warned).unwrap();
        let mut used_up = five_used_up;
        used_up.settings.notify.running_out = false;
        assert_eq!(kinds(&raise(&mut db, &used_up).unwrap()), [Kind::UsedUp]);
    }

    /// An OpenRouter account whose credits, `size` dollars of them, have
    /// `left` dollars left, with the percent the status works out for it.
    fn credits(at: i64, size: f64, left: f64) -> Status {
        let outlook = match left > 0.0 {
            false => Outlook::UsedUp { back: None },
            true => Outlook::Unknown {
                reason: crate::status::Unknown::NoReset,
            },
        };
        let mut status = status(at, (left / size * 100.0) as u8, outlook);
        let account = &mut status.accounts[0];
        account.id = "openrouter:api".to_owned();
        let limit = &mut account.limits[0];
        limit.key = "credits".to_owned();
        limit.name = "Credits".to_owned();
        limit.window = None;
        limit.money = Some(crate::status::Money {
            size_usd: size,
            left_usd: left,
        });
        status
    }

    #[test]
    fn credits_are_told_used_up_once_a_size_and_back_once_a_cent_is_left() {
        let (_dir, mut db) = open();
        let used_up = credits(DAY, 1000.0, 0.0);
        let raised = raise(&mut db, &used_up).unwrap();
        assert_eq!(kinds(&raised), [Kind::UsedUp]);
        assert_eq!(raised[0].limit.as_deref(), Some("credits"));
        assert_eq!(raised[0].resets, None);
        assert!(raise(&mut db, &used_up).unwrap().is_empty(), "told once");
        // Still used up at $1,010 bought, with more used than bought: a size
        // $10 from the last is another, where a reset five minutes off is the
        // same.
        let used_up = credits(DAY + MINUTE, 1010.0, 0.0);
        assert_eq!(kinds(&raise(&mut db, &used_up).unwrap()), [Kind::UsedUp]);
        // $5 more: under 1% of all ever bought, so 0% left, and back all the
        // same. Told once, for the two.
        let bought = credits(DAY + 2 * MINUTE, 1015.0, 5.0);
        assert_eq!(bought.accounts[0].limits[0].left_percent, 0);
        assert_eq!(kinds(&raise(&mut db, &bought).unwrap()), [Kind::Reset]);
        assert!(raise(&mut db, &bought).unwrap().is_empty(), "told once");
        // Those used up too.
        let again = credits(DAY + 3 * MINUTE, 1015.0, 0.0);
        assert_eq!(kinds(&raise(&mut db, &again).unwrap()), [Kind::UsedUp]);
        assert!(raise(&mut db, &again).unwrap().is_empty(), "told once");
        // A size under half a cent is below zero too, as every size is. With
        // no size and no window, as one whose reset its provider didn't say,
        // nothing tells one time it's used up from the next.
        let mut limit = credits(DAY, 0.001, 0.0).accounts[0].limits[0].clone();
        assert_eq!(window(&limit), Some(-1));
        limit.money = None;
        assert_eq!(window(&limit), None);
    }

    #[test]
    fn credits_used_up_are_sent_again_only_while_they_still_are() {
        let (_dir, mut db) = open();
        let used_up = credits(DAY, 10.0, 0.0);
        assert_eq!(kinds(&raise(&mut db, &used_up).unwrap()), [Kind::UsedUp]);
        // Not acknowledged, as when notifications were off: a week on, it
        // still stands.
        assert_eq!(
            kinds(&pending(&db, &used_up, 8 * DAY).unwrap()),
            [Kind::UsedUp]
        );
        // More bought, with the kind that says so turned off.
        let mut bought = credits(8 * DAY, 20.0, 10.0);
        bought.settings.notify.reset = false;
        assert!(raise(&mut db, &bought).unwrap().is_empty());
        assert!(pending(&db, &bought, 8 * DAY).unwrap().is_empty());
    }

    #[test]
    fn a_sign_in_is_sent_again_only_while_its_account_cant_be_read() {
        let (_dir, mut db) = open();
        let mut refused = status(DAY, 60, LASTS);
        refused.accounts[0].state = State::AsOf {
            read_at: DAY - MINUTE,
            why: crate::status::Stale::SignIn,
            worked_minutes: 0,
        };
        assert_eq!(kinds(&raise(&mut db, &refused).unwrap()), [Kind::SignIn]);
        // Not acknowledged, as when notifications were off.
        assert_eq!(kinds(&pending(&db, &refused, DAY).unwrap()), [Kind::SignIn]);
        let read = status(DAY + MINUTE, 60, LASTS);
        assert!(raise(&mut db, &read).unwrap().is_empty());
        assert!(pending(&db, &read, DAY + MINUTE).unwrap().is_empty());
        // Refused again before the next read worked: it's the same alert.
        assert_eq!(
            kinds(&pending(&db, &refused, DAY + 2 * MINUTE).unwrap()),
            [Kind::SignIn]
        );
        assert!(
            pending(&db, &refused, 2 * DAY + MINUTE).unwrap().is_empty(),
            "for a day"
        );
        // Refused again after the read that worked, however soon: told again.
        let mut again = status(DAY + 3 * MINUTE, 60, LASTS);
        again.accounts[0].state = State::AsOf {
            read_at: DAY + MINUTE,
            why: crate::status::Stale::SignIn,
            worked_minutes: 0,
        };
        assert_eq!(kinds(&raise(&mut db, &again).unwrap()), [Kind::SignIn]);
    }

    /// Due to run out at day 5, as likely the soonest.
    fn runs_out(at: i64) -> Status {
        let likely = Moment {
            at: 5 * DAY,
            horizon: Horizon::Later,
        };
        let outlook = Outlook::RunsOut {
            likely,
            soonest: likely,
        };
        status(at, 40, outlook)
    }

    /// Recorded as shown, so showing the account or turning the kind on
    /// later doesn't bring back the past.
    #[test]
    fn an_alert_for_a_hidden_account_or_a_kind_turned_off_is_never_told() {
        let mut hidden = status(DAY, 0, Outlook::UsedUp { back: None });
        hidden.accounts[0].hidden = true;
        let mut used_up = status(DAY, 0, Outlook::UsedUp { back: None });
        used_up.settings.notify.used_up = false;
        let mut running_out = runs_out(DAY);
        running_out.settings.notify.running_out = false;
        for (mut status, kind) in [
            (hidden, "usedUp"),
            (used_up, "usedUp"),
            (running_out, "runningOut"),
        ] {
            let (_dir, mut db) = open();
            assert!(raise(&mut db, &status).unwrap().is_empty());
            let recorded = rows(&db, "SELECT kind, acknowledged FROM alert");
            assert_eq!(recorded, [format!("{kind}|1")]);
            assert!(pending(&db, &status, DAY).unwrap().is_empty());
            status.accounts[0].hidden = false;
            status.settings.notify = Notify::default();
            assert!(raise(&mut db, &status).unwrap().is_empty());
        }
    }

    /// Not raised, so not recorded either: told once the account is in use.
    #[test]
    fn an_account_not_in_use_is_told_nothing_until_it_is() {
        let mut refused = status(DAY, 60, LASTS);
        refused.accounts[0].state = State::AsOf {
            read_at: DAY - MINUTE,
            why: crate::status::Stale::SignIn,
            worked_minutes: 0,
        };
        for (mut status, kind) in [
            (status(DAY, 0, Outlook::UsedUp { back: None }), Kind::UsedUp),
            (runs_out(DAY), Kind::RunningOut),
            (refused, Kind::SignIn),
        ] {
            let (_dir, mut db) = open();
            status.accounts[0].in_use = false;
            assert!(raise(&mut db, &status).unwrap().is_empty());
            status.accounts[0].in_use = true;
            assert_eq!(kinds(&raise(&mut db, &status).unwrap()), [kind]);
        }
    }

    #[test]
    fn running_out_is_told_once_while_its_reset_drifts() {
        let (_dir, mut db) = open();
        let mut status = runs_out(DAY);
        assert_eq!(kinds(&raise(&mut db, &status).unwrap()), [Kind::RunningOut]);
        let window = status.accounts[0].limits[0].window.as_mut().unwrap();
        window.resets.at += forecast::DRIFT;
        assert!(
            raise(&mut db, &status).unwrap().is_empty(),
            "the same window"
        );
        let window = status.accounts[0].limits[0].window.as_mut().unwrap();
        window.resets.at += forecast::DRIFT + MINUTE;
        assert_eq!(
            kinds(&raise(&mut db, &status).unwrap()),
            [Kind::RunningOut],
            "another"
        );
    }

    #[test]
    fn only_a_limit_used_up_is_back_and_only_once_the_account_can_be_used() {
        // Warned it would run out, it reset before it did: nothing is back.
        let (_dir, mut db) = open();
        let soon = Moment {
            at: 9 * DAY,
            horizon: Horizon::Later,
        };
        let warned = status(
            DAY,
            20,
            Outlook::RunsOut {
                likely: soon,
                soonest: soon,
            },
        );
        assert_eq!(kinds(&raise(&mut db, &warned).unwrap()), [Kind::RunningOut]);
        let reset = status(10 * DAY + MINUTE, 100, LASTS);
        assert!(raise(&mut db, &reset).unwrap().is_empty());
        // Its 5 hours back while its week is still used up: not said.
        let (_dir, mut db) = open();
        let mut used_up = status(DAY, 0, Outlook::UsedUp { back: None });
        let mut week = used_up.accounts[0].limits[0].clone();
        week.key = "seven_day".to_owned();
        week.window = Some(LimitWindow {
            starts_at: Some(0),
            resets: Moment {
                at: 20 * DAY,
                horizon: Horizon::Later,
            },
        });
        used_up.accounts[0].limits.push(week);
        assert_eq!(
            kinds(&raise(&mut db, &used_up).unwrap()),
            [Kind::UsedUp, Kind::UsedUp]
        );
        let mut five_back = used_up.clone();
        five_back.at = 10 * DAY + MINUTE;
        five_back.accounts[0].limits[0].left_percent = 100;
        five_back.accounts[0].limits[0].outlook = LASTS;
        assert!(raise(&mut db, &five_back).unwrap().is_empty());
    }

    /// Used up on day 1, its window resetting on day 10, and reset early on
    /// day 3 to a new window: it's back then, and used up no more.
    #[test]
    fn a_limit_reset_early_is_back_at_once() {
        let (_dir, mut db) = open();
        raise(&mut db, &status(DAY, 0, Outlook::UsedUp { back: None })).unwrap();
        let mut early = status(3 * DAY, 100, LASTS);
        early.accounts[0].limits[0].window = Some(LimitWindow {
            starts_at: Some(3 * DAY),
            resets: Moment {
                at: 13 * DAY,
                horizon: Horizon::Later,
            },
        });
        assert_eq!(kinds(&raise(&mut db, &early).unwrap()), [Kind::Reset]);
        let pending_at = |now| kinds(&pending(&db, &early, now).unwrap());
        assert_eq!(pending_at(3 * DAY), [Kind::Reset]);
        // Not sent days late, as at a launch after the Mac was off.
        assert!(pending_at(5 * DAY).is_empty());
    }

    /// Reset early on day 3, and already running out in its new window: told
    /// back and running out, once each.
    #[test]
    fn a_limit_back_and_running_out_on_one_read_is_told_each_once() {
        let (_dir, mut db) = open();
        raise(&mut db, &status(DAY, 0, Outlook::UsedUp { back: None })).unwrap();
        let mut early = runs_out(3 * DAY);
        let window = early.accounts[0].limits[0].window.as_mut().unwrap();
        window.resets.at = 13 * DAY;
        let raised = raise(&mut db, &early).unwrap();
        assert_eq!(kinds(&raised), [Kind::Reset, Kind::RunningOut]);
        assert!(raise(&mut db, &early).unwrap().is_empty(), "told once");
    }

    #[test]
    fn a_limit_on_some_models_is_blocked_by_another_on_the_same() {
        let account = &status(DAY, 100, LASTS).accounts[0];
        let limit = |key: &str, scope: Option<&str>, used_up: bool| {
            let mut limit = account.limits[0].clone();
            limit.key = key.to_owned();
            limit.scope = scope.map(str::to_owned);
            if used_up {
                limit.outlook = Outlook::UsedUp { back: None };
            }
            limit
        };
        let with = |others: Vec<Limit>| {
            let mut account = account.clone();
            account.limits = vec![limit("5h:3p-5h", Some("Claude and GPT"), false)];
            account.limits.extend(others);
            blocked(&account, &account.limits[0])
        };
        // Its own week used up stops it; a week on other models doesn't.
        assert!(with(vec![limit(
            "weekly:3p-weekly",
            Some("Claude and GPT"),
            true
        )]));
        assert!(!with(vec![limit("weekly:opus", Some("Opus"), true)]));
        assert!(!with(vec![limit(
            "weekly:3p-weekly",
            Some("Claude and GPT"),
            false
        )]));
        assert!(with(vec![limit("weekly", None, true)]));
        // One key's limit doesn't stop another's.
        assert!(!with(vec![limit("key-b", None, true)]));
    }

    #[test]
    fn pending_skips_hidden_accounts_and_resolved_limits() {
        let (_dir, mut db) = open();
        let used_up = status(DAY, 0, Outlook::UsedUp { back: None });
        assert_eq!(kinds(&raise(&mut db, &used_up).unwrap()), [Kind::UsedUp]);
        // Hidden since the alert was raised: never delivered on relaunch.
        let mut hidden = used_up.clone();
        hidden.accounts[0].hidden = true;
        assert!(pending(&db, &hidden, DAY).unwrap().is_empty());
        // Use given back early in the same window: relaunch tells only Reset,
        // not the old UsedUp alongside it.
        let back = status(DAY + MINUTE, 60, LASTS);
        assert!(pending(&db, &back, DAY + MINUTE).unwrap().is_empty());
        assert_eq!(kinds(&raise(&mut db, &back).unwrap()), [Kind::Reset]);

        // RunningOut is dropped once the limit recovers or is used up, but kept
        // when relaunching with a stale reading.
        let (_dir, mut db) = open();
        let warned = runs_out(DAY);
        assert_eq!(kinds(&raise(&mut db, &warned).unwrap()), [Kind::RunningOut]);
        let mut hidden_warned = warned.clone();
        hidden_warned.accounts[0].hidden = true;
        assert!(pending(&db, &hidden_warned, DAY).unwrap().is_empty());
        let recovered = status(DAY + MINUTE, 50, LASTS);
        assert!(pending(&db, &recovered, DAY + MINUTE).unwrap().is_empty());
        let now_used_up = status(DAY + MINUTE, 0, Outlook::UsedUp { back: None });
        assert!(pending(&db, &now_used_up, DAY + MINUTE).unwrap().is_empty());
        let stale = status(
            DAY + MINUTE,
            40,
            Outlook::Unknown {
                reason: crate::status::Unknown::Stale,
            },
        );
        assert_eq!(
            kinds(&pending(&db, &stale, DAY + MINUTE).unwrap()),
            [Kind::RunningOut]
        );
    }
}
