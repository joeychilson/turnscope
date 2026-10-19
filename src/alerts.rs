//! Notifications: few, each worth acting on, never repeated.
//!
//! | Kind | Raised when |
//! |---|---|
//! | `runningOut` | a limit in use runs out before its reset at its pace, as the panel shows it |
//! | `usedUp` | a limit in use is used up |
//! | `reset` | a limit that was used up is back, and no other limit on the whole account still is |
//! | `signIn` | an account in use can't be read because its login was refused |
//!
//! Each is raised once per account, limit, kind and window, so neither a
//! restart nor a pace near the line repeats one. One of a hidden account, or
//! of a kind turned off, is recorded as already seen, so showing it or
//! turning it on doesn't bring back the past. An alert waits until a client
//! acknowledges it.

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
        let known = tx
            .query_row(
                "SELECT 1 FROM alert WHERE account = ?1 AND limit_key = ?2 AND kind = ?3 AND window BETWEEN ?4 - ?5 AND ?4 + ?5",
                params![account.id, key, kind_name(kind), window, forecast::DRIFT],
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
        // Back while another limit on the whole account is used up, the
        // account still can't be used: that limit's own return says so.
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
            let Some(window) = limit.window else { continue };
            match limit.outlook {
                Outlook::UsedUp { .. } if account.in_use => {
                    raise(account, Some(limit), Kind::UsedUp, window.resets.at, None)?
                }
                Outlook::RunsOut { likely, .. } if account.in_use => raise(
                    account,
                    Some(limit),
                    Kind::RunningOut,
                    window.resets.at,
                    Some(likely.at),
                )?,
                _ => {}
            }
            // Back after being used up: a window used up that ended in the
            // last day, or this one, when use was given back early.
            let used_up: Vec<i64> = tx
                .prepare_cached(
                    "SELECT window FROM alert WHERE account = ?1 AND limit_key = ?2
                     AND kind = 'usedUp' AND window >= ?3",
                )?
                .query_map(params![account.id, limit.key, now - DAY], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for ended in used_up {
                let this = forecast::same_reset(Some(ended), Some(window.resets.at));
                if (ended <= now && limit.left_percent > 0) || (this && limit.left_percent > 1) {
                    raise(account, Some(limit), Kind::Reset, ended, None)?;
                }
            }
        }
    }
    tx.commit()?;
    Ok(raised)
}

/// Whether a limit on the whole account other than `limit` is used up.
fn blocked(account: &Account, limit: &Limit) -> bool {
    account.limits.iter().any(|other| {
        other.key != limit.key && other.whole() && matches!(other.outlook, Outlook::UsedUp { .. })
    })
}

/// Every alert not yet acknowledged that still stands: a warning while its
/// window lasts, a reset or sign-in for a day.
pub fn pending(db: &Connection, now: i64) -> Result<Vec<Alert>> {
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
        let at = row.get(2)?;
        let stands = match kind {
            Kind::RunningOut | Kind::UsedUp => detail.resets.is_none_or(|resets| resets > now),
            Kind::Reset | Kind::SignIn => now - at <= DAY,
        };
        if stands {
            let (id, account, limit): (i64, String, String) =
                (row.get(0)?, row.get(3)?, row.get(4)?);
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
        assert_eq!(pending(&db, DAY).unwrap().len(), 1);
        assert_eq!(acknowledge(&db, &[raised[0].id]).unwrap(), 1);
        assert!(pending(&db, DAY).unwrap().is_empty());
        // Use given back within the window: it's back.
        let back = status(DAY + MINUTE, 60, LASTS);
        assert_eq!(kinds(&raise(&mut db, &back).unwrap()), [Kind::Reset]);
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
}
