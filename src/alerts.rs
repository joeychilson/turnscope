//! Notifications: few, each worth acting on, never repeated.
//!
//! | Kind | Raised when |
//! |---|---|
//! | `runningOut` | a limit in use runs out before its reset at its pace, as the panel shows it |
//! | `usedUp` | a limit in use is used up: one with a window, or money that never resets, as credits |
//! | `reset` | a limit that was used up is back: its window ended in the last day, use was given back early, or money that never resets has some left |
//! | `signIn` | an account in use can't be read because its login was refused |
//!
//! Each is raised once per account, limit, kind and window, so neither a
//! restart nor a pace near the line repeats one. The window is its reset,
//! within five minutes; for money that never resets, its size ([`window`]);
//! for `signIn`, the last read that worked. One of a hidden account, or of
//! a kind turned off, is recorded as already seen, so showing it or turning
//! it on doesn't bring back the past. So is a `reset` while another limit
//! on the whole account, or on the same models, is still used up. An alert
//! waits until a client acknowledges it, and until then is sent again only
//! while it still stands ([`pending`]).

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::status::{Account, Limit, Moment, Outlook, State, Status};
use crate::time::DAY;
use crate::{Result, forecast};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    RunningOut,
    UsedUp,
    Reset,
    SignIn,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Alert {
    pub id: i64,
    pub kind: Kind,
    pub at: i64,
    pub account: String,
    pub account_title: String,
    pub limit: Option<String>,
    pub limit_name: Option<String>,
    /// The notification it takes the place of: `limit:<account>:<key>` or
    /// `account:<account>`, so a reset replaces the warning before it.
    pub replaces: String,
    pub resets: Option<Moment>,
    pub runs_out: Option<Moment>,
}

/// What an alert says, as it was raised.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Detail {
    account_title: String,
    limit_name: Option<String>,
    resets: Option<i64>,
    runs_out: Option<i64>,
}

/// Raise the alerts `status` makes due, and give back those to tell.
pub fn raise(db: &mut Connection, status: &Status) -> Result<Vec<Alert>> {
    let now = status.at;
    let notify = status.settings.notify;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut raised = Vec::new();
    let mut raise = |account: &Account,
                     limit: Option<&Limit>,
                     kind: Kind,
                     window: i64,
                     runs_out: Option<i64>|
     -> Result<()> {
        let key = limit.map_or("", |limit| limit.key.as_str());
        // A reset drifts from read to read. A size, below zero, doesn't.
        let drift = if window < 0 { 0 } else { forecast::DRIFT };
        let known = tx
            .query_row(
                "SELECT 1 FROM alert WHERE account = ?1 AND limit_key = ?2 AND kind = ?3 AND window BETWEEN ?4 - ?5 AND ?4 + ?5",
                params![account.id, key, kind_name(kind), window, drift],
                |_| Ok(()),
            )
            .optional()?;
        if known.is_some() {
            return Ok(());
        }
        let on = match kind {
            Kind::RunningOut => notify.running_out,
            Kind::UsedUp => notify.used_up,
            Kind::Reset => notify.reset,
            Kind::SignIn => notify.sign_in,
        };
        // Back while another limit on the whole account, or on the same
        // models, is used up, it still can't be used: that limit's own
        // return says so.
        let blocked = kind == Kind::Reset && limit.is_some_and(|limit| blocked(account, limit));
        let told = on && !account.hidden && !blocked;
        let detail = Detail {
            account_title: account.full_title(),
            limit_name: limit.map(|limit| limit.name.clone()),
            resets: limit
                .and_then(|limit| limit.window)
                .map(|window| window.resets.at),
            runs_out,
        };
        tx.execute(
            "INSERT INTO alert (account, limit_key, kind, window, at, acknowledged, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![account.id, key, kind_name(kind), window, now, !told, serde_json::to_string(&detail).unwrap_or_default()],
        )?;
        if told {
            let id = tx.last_insert_rowid();
            raised.push(alert(id, kind, now, &account.id, key, detail, now));
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
            match limit.outlook {
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
            // Back after being used up: a window used up that ended in the
            // last day, or this one, when use was given back early. `window`
            // is read as a time here, and a size isn't one: it's asked for
            // apart, below zero, where it has always ended and is never
            // this window. So money used up at any size is back once a cent
            // is left: by the cent, not the percent, as $5 more is under 1%
            // of $1,000 ever bought. Only the last time it was used up is
            // asked: of two with none back between, as a size that grew
            // while more was used than bought, one "back" tells both.
            let used_up: Option<i64> = tx
                .prepare_cached(
                    "SELECT window FROM alert WHERE account = ?1 AND limit_key = ?2
                     AND kind = 'usedUp' AND (window >= ?3 OR window < 0)
                     ORDER BY id DESC LIMIT 1",
                )?
                .query_row(params![account.id, limit.key, now - DAY], |row| row.get(0))
                .optional()?;
            if let Some(ended) = used_up {
                let left = match limit.money {
                    Some(money) => money.left_usd > 0.0,
                    None => limit.left_percent > 0,
                };
                let this = limit.window.is_some_and(|window| {
                    forecast::same_reset(Some(ended), Some(window.resets.at))
                });
                if (ended <= now && left) || (this && limit.left_percent > 1) {
                    raise(account, Some(limit), Kind::Reset, ended, None)?;
                }
            }
        }
    }
    tx.commit()?;
    Ok(raised)
}

/// What tells one time `limit` is used up from the next, as an alert's
/// `window`: its window's reset.
///
/// Money that never resets, as credits, has its size instead, in cents. Its
/// size is all that was ever bought (`providers::Reading::size`), so more
/// bought makes it larger: credits used up are told once, and again when
/// more were bought and used up too. A size it was used up at before isn't
/// told again. Chosen over when it was first read used up, which would take
/// its readings and is a time, as a reset is. A size is kept below zero,
/// where no time is, so neither is taken for the other, and is matched
/// exactly: within `forecast::DRIFT`, $10 more would be the same window.
/// One under half a cent counts as a cent, so it's below zero too.
///
/// `None` for a limit with neither, a window whose reset its provider
/// didn't say: nothing tells one time it's used up from the next, so it
/// raises nothing, over telling it once and never again.
fn window(limit: &Limit) -> Option<i64> {
    match (limit.window, limit.money) {
        (Some(window), _) => Some(window.resets.at),
        (None, Some(money)) => Some(-((money.size_usd * 100.0).round() as i64).max(1)),
        (None, None) => None,
    }
}

/// Whether a limit other than `limit` that stops what it covers is used up:
/// one on the whole account, or one on the same models, as a plan's 5-hour
/// and weekly limits on them are.
fn blocked(account: &Account, limit: &Limit) -> bool {
    account.limits.iter().any(|other| {
        other.key != limit.key
            && (other.whole() || (other.scope.is_some() && other.scope == limit.scope))
            && matches!(other.outlook, Outlook::UsedUp { .. })
    })
}

/// Every alert not yet acknowledged that still stands: a warning while its
/// window lasts, or with no window while its limit is still used up; a
/// reset for a day; and a sign-in for a day while its account still can't
/// be read. `status` says what still is. A sign-in for an account that can
/// be read again would be posted with its sound, then removed at once, and
/// credits bought since would be posted as used up.
pub fn pending(db: &Connection, status: &Status, now: i64) -> Result<Vec<Alert>> {
    let mut statement = db.prepare(
        "SELECT id, kind, at, account, limit_key, detail FROM alert WHERE acknowledged = 0 ORDER BY id",
    )?;
    let mut rows = statement.query([])?;
    let mut pending = Vec::new();
    while let Some(row) = rows.next()? {
        let kind = kind_of(&row.get::<_, String>(1)?);
        let detail = serde_json::from_str::<Detail>(&row.get::<_, String>(5)?);
        let (Some(kind), Ok(detail)) = (kind, detail) else {
            continue;
        };
        let (id, at, account, limit): (i64, i64, String, String) =
            (row.get(0)?, row.get(2)?, row.get(3)?, row.get(4)?);
        let stands = match kind {
            Kind::RunningOut | Kind::UsedUp => match detail.resets {
                Some(resets) => resets > now,
                None => status.accounts.iter().any(|known| {
                    known.id == account
                        && known.limits.iter().any(|known| {
                            known.key == limit && matches!(known.outlook, Outlook::UsedUp { .. })
                        })
                }),
            },
            Kind::Reset => now - at <= DAY,
            Kind::SignIn => {
                now - at <= DAY
                    && status
                        .accounts
                        .iter()
                        .any(|known| known.id == account && known.refused())
            }
        };
        if stands {
            pending.push(alert(id, kind, at, &account, &limit, detail, now));
        }
    }
    Ok(pending)
}

/// Mark alerts shown, so they aren't sent again.
pub fn acknowledge(db: &Connection, ids: &[i64]) -> Result<usize> {
    let mut statement =
        db.prepare("UPDATE alert SET acknowledged = 1 WHERE id = ?1 AND acknowledged = 0")?;
    let mut done = 0;
    for id in ids {
        done += statement.execute([id])?;
    }
    Ok(done)
}

fn alert(
    id: i64,
    kind: Kind,
    at: i64,
    account: &str,
    limit: &str,
    detail: Detail,
    now: i64,
) -> Alert {
    let limit = Some(limit.to_owned()).filter(|limit| !limit.is_empty());
    Alert {
        id,
        kind,
        at,
        replaces: match &limit {
            Some(limit) => format!("limit:{account}:{limit}"),
            None => format!("account:{account}"),
        },
        account: account.to_owned(),
        account_title: detail.account_title,
        limit,
        limit_name: detail.limit_name,
        resets: detail.resets.map(|at| Moment::of(at, now)),
        runs_out: detail.runs_out.map(|at| Moment::of(at, now)),
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::RunningOut => "runningOut",
        Kind::UsedUp => "usedUp",
        Kind::Reset => "reset",
        Kind::SignIn => "signIn",
    }
}

fn kind_of(name: &str) -> Option<Kind> {
    [Kind::RunningOut, Kind::UsedUp, Kind::Reset, Kind::SignIn]
        .into_iter()
        .find(|kind| kind_name(*kind) == name)
}

/// The person's settings: which notifications to send.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub notify: Notify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Notify {
    pub running_out: bool,
    pub used_up: bool,
    pub reset: bool,
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
    db.execute(
        "UPDATE state SET settings = ?1",
        [serde_json::to_string(settings).unwrap_or_default()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{Horizon, LimitWindow};
    use crate::time::MINUTE;

    fn status(at: i64, left: u8, outlook: Outlook) -> Status {
        let resets = Moment {
            at: 10 * DAY,
            horizon: Horizon::Later,
        };
        Status {
            at,
            history_read: false,
            accounts: vec![Account {
                id: "anthropic:a".to_owned(),
                provider: "anthropic".to_owned(),
                kind: crate::providers::Kind::Subscription,
                title: "Claude Max".to_owned(),
                label: None,
                agents: vec!["claude-code".to_owned()],
                used_by: vec!["claude-code".to_owned()],
                in_use: true,
                recent: true,
                hidden: false,
                state: State::Live { read_at: at },
                limits: vec![Limit {
                    key: "five_hour".to_owned(),
                    name: "5 hours".to_owned(),
                    scope: None,
                    window: Some(LimitWindow {
                        starts_at: Some(0),
                        resets,
                    }),
                    read_at: at,
                    left_percent: left,
                    money: None,
                    reset_since: None,
                    held_by: Vec::new(),
                    outlook,
                    work: None,
                }],
                deciding: Some("five_hour".to_owned()),
                spend: None,
            }],
            settings: Settings::default(),
        }
    }

    const LASTS: Outlook = Outlook::Lasts {
        left_at_reset: None,
    };

    fn kinds(raised: &[Alert]) -> Vec<Kind> {
        raised.iter().map(|alert| alert.kind).collect()
    }

    #[test]
    fn a_limit_used_up_is_told_once_and_its_return_after() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let used_up = status(DAY, 0, Outlook::UsedUp { back: None });
        let raised = raise(&mut db, &used_up).unwrap();
        assert_eq!(kinds(&raised), [Kind::UsedUp]);
        assert_eq!(raised[0].replaces, "limit:anthropic:a:five_hour");
        assert!(raise(&mut db, &used_up).unwrap().is_empty(), "told once");
        assert_eq!(pending(&db, &used_up, DAY).unwrap().len(), 1);
        assert_eq!(acknowledge(&db, &[raised[0].id]).unwrap(), 1);
        assert!(pending(&db, &used_up, DAY).unwrap().is_empty());
        // Use given back within the window: it's back.
        let back = status(DAY + MINUTE, 60, LASTS);
        assert_eq!(kinds(&raise(&mut db, &back).unwrap()), [Kind::Reset]);
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
    fn credits_used_up_are_told_once_and_again_when_more_are_used_up_too() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let used_up = credits(DAY, 10.0, 0.0);
        let raised = raise(&mut db, &used_up).unwrap();
        assert_eq!(kinds(&raised), [Kind::UsedUp]);
        assert_eq!(raised[0].replaces, "limit:openrouter:api:credits");
        assert_eq!(raised[0].resets, None);
        assert!(raise(&mut db, &used_up).unwrap().is_empty(), "told once");
        // $10 more bought: they're back.
        let bought = credits(DAY + MINUTE, 20.0, 10.0);
        assert_eq!(kinds(&raise(&mut db, &bought).unwrap()), [Kind::Reset]);
        assert!(raise(&mut db, &bought).unwrap().is_empty(), "told once");
        // Those used up too: a size $10 from the last is another, where a
        // reset five minutes from the last is the same.
        let again = credits(DAY + 2 * MINUTE, 20.0, 0.0);
        assert_eq!(kinds(&raise(&mut db, &again).unwrap()), [Kind::UsedUp]);
        assert!(raise(&mut db, &again).unwrap().is_empty(), "told once");
    }

    #[test]
    fn credits_used_up_are_sent_again_only_while_they_still_are() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
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
    fn credits_are_back_once_a_cent_is_left_and_are_told_back_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        // Used up at $1,000 bought, and still at $1,010, with more used than
        // bought: used up twice, with none back between.
        for (minute, size) in [(0, 1000.0), (1, 1010.0)] {
            let used_up = credits(DAY + minute * MINUTE, size, 0.0);
            assert_eq!(kinds(&raise(&mut db, &used_up).unwrap()), [Kind::UsedUp]);
        }
        // $5 more: under 1% of all ever bought, so 0% left, and back all
        // the same. Told once, for the two.
        let bought = credits(DAY + 2 * MINUTE, 1015.0, 5.0);
        assert_eq!(bought.accounts[0].limits[0].left_percent, 0);
        assert_eq!(kinds(&raise(&mut db, &bought).unwrap()), [Kind::Reset]);
        assert!(raise(&mut db, &bought).unwrap().is_empty(), "told once");
        // A size under half a cent is below zero too, as every size is.
        let tiny = credits(DAY, 0.001, 0.0);
        assert_eq!(window(&tiny.accounts[0].limits[0]), Some(-1));
    }

    #[test]
    fn a_limit_with_no_window_and_no_size_raises_nothing() {
        // A window whose reset its provider didn't say.
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let mut used_up = status(DAY, 0, Outlook::UsedUp { back: None });
        used_up.accounts[0].limits[0].window = None;
        assert!(raise(&mut db, &used_up).unwrap().is_empty());
    }

    #[test]
    fn a_sign_in_is_sent_again_only_while_its_account_cant_be_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let mut refused = status(DAY, 60, LASTS);
        refused.accounts[0].state = State::AsOf {
            read_at: DAY - MINUTE,
            why: crate::status::Stale::SignIn,
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
    }

    #[test]
    fn only_a_limit_used_up_is_back_and_only_once_the_account_can_be_used() {
        // Warned it would run out, it reset before it did: nothing is back.
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
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
                latest: Some(soon),
            },
        );
        assert_eq!(kinds(&raise(&mut db, &warned).unwrap()), [Kind::RunningOut]);
        let reset = status(10 * DAY + MINUTE, 100, LASTS);
        assert!(raise(&mut db, &reset).unwrap().is_empty());
        // Its 5 hours back while its week is still used up: not said.
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
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

    #[test]
    fn a_week_used_up_long_before_it_resets_says_its_back_after() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        raise(&mut db, &status(DAY, 0, Outlook::UsedUp { back: None })).unwrap();
        // Nine days on, an hour after the window reset.
        let back = status(10 * DAY + 60 * MINUTE, 100, LASTS);
        assert_eq!(kinds(&raise(&mut db, &back).unwrap()), [Kind::Reset]);
    }

    #[test]
    fn a_keys_limit_back_is_said_while_another_keys_is_used_up() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        // Two OpenRouter keys, each with a limit of its own: A's used up.
        let mut a_used_up = status(DAY, 0, Outlook::UsedUp { back: None });
        let account = &mut a_used_up.accounts[0];
        account.id = "openrouter:api".to_owned();
        account.limits[0].key = "key-a".to_owned();
        let mut b = account.limits[0].clone();
        b.key = "key-b".to_owned();
        b.window = Some(LimitWindow {
            starts_at: Some(0),
            resets: Moment {
                at: 20 * DAY,
                horizon: Horizon::Later,
            },
        });
        b.left_percent = 50;
        b.outlook = LASTS;
        account.limits.push(b);
        assert_eq!(kinds(&raise(&mut db, &a_used_up).unwrap()), [Kind::UsedUp]);
        // A's limit reset while B's is used up: A can be used again.
        let mut a_back = a_used_up.clone();
        a_back.at = 10 * DAY + MINUTE;
        let limits = &mut a_back.accounts[0].limits;
        limits[0].left_percent = 100;
        limits[0].outlook = LASTS;
        limits[1].left_percent = 0;
        limits[1].outlook = Outlook::UsedUp { back: None };
        assert_eq!(
            kinds(&raise(&mut db, &a_back).unwrap()),
            [Kind::Reset, Kind::UsedUp]
        );
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
}
