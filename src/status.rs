//! How every account stands: its limits, where each is headed, how much
//! work each has left, and the limit that decides it. The app, the CLI and
//! the MCP server all show this one answer.
//!
//! Every decision is made here, as shown: percent left is rounded down so
//! it's never overstated; each time to come is rounded to the minute and
//! carries how soon it is (a [`Moment`]); a limit whose window reset since it
//! was read is full, as far as this Mac can tell. A status is worked out as
//! of the minute, so between readings it changes at most once a minute.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::forecast::{self, Window};
use crate::providers::Kind;
use crate::time::{self, DAY, HOUR, MINUTE};
use crate::{Result, agents};

mod text;
pub use text::{fit, limit_text, line, text, verdict};

/// How recently a response must have drawn on an account for it to be in use.
const IN_USE: i64 = 30 * MINUTE;

/// How recently, for it to be one the person uses.
const RECENT: i64 = 7 * DAY;

/// How long readings stand. After that an outlook is no longer said, and an
/// account with no newer read shows as of its last one, whatever stopped
/// the next: limits are read every 5 minutes while anything runs.
const STALE: i64 = 30 * MINUTE;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// The minute it's as of.
    pub at: i64,
    /// When the agents' history was last read.
    pub history_read_at: Option<i64>,
    /// In the order to list them: in use first, the most urgent first.
    pub accounts: Vec<Account>,
    pub agents: Vec<Agent>,
    pub settings: crate::alerts::Settings,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub installed: bool,
    pub connection: Connection,
    /// Its responses drew on an account in the last half hour.
    pub in_use: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Connection {
    /// It runs this Turnscope's MCP server.
    Connected,
    /// It runs another copy's, as one since moved.
    Outdated,
    Available,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub provider: String,
    pub kind: Kind,
    /// `Claude Max`, `Anthropic API key`.
    pub title: String,
    /// What tells it from another of its kind, such as an email address.
    pub label: Option<String>,
    /// The agents signed into it now.
    pub agents: Vec<String>,
    /// The agents whose responses drew on it in the last half hour, the
    /// latest first.
    pub used_by: Vec<String>,
    /// Used in the last half hour, or its limits rose at the last read, as
    /// use elsewhere makes them.
    pub in_use: bool,
    /// In use, or drawn on in the last 7 days: one the person uses.
    pub recent: bool,
    pub hidden: bool,
    pub state: State,
    pub limits: Vec<Limit>,
    /// The key of the limit that decides how it stands.
    pub deciding: Option<String>,
    /// For an API key, what it cost this month.
    pub spend: Option<Spend>,
}

impl Account {
    /// `Claude Max (me@example.com)`.
    pub fn full_title(&self) -> String {
        match &self.label {
            Some(label) => format!("{} ({label})", self.title),
            None => self.title.clone(),
        }
    }

    /// Its limit `name` names, or an error naming the limits it has.
    pub fn limit(&self, name: &str) -> crate::Result<&Limit> {
        self.limits
            .iter()
            .find(|limit| limit.answers_to(name))
            .ok_or_else(|| {
                let names: Vec<&str> = self
                    .limits
                    .iter()
                    .map(|limit| limit.name.as_str())
                    .collect();
                crate::Error::NotFound(match names.is_empty() {
                    true => format!("{} has no limits read", self.full_title()),
                    false => format!(
                        "{} has no limit named {name}; it has {}",
                        self.full_title(),
                        names.join(", ")
                    ),
                })
            })
    }

    pub fn deciding_limit(&self) -> Option<&Limit> {
        self.limits
            .iter()
            .find(|limit| Some(&limit.key) == self.deciding.as_ref())
    }

    /// Its login was refused, at its last read or every one.
    pub fn refused(&self) -> bool {
        self.state.why() == Some(Stale::SignIn)
    }
}

/// How an account's limits were last read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum State {
    #[serde(rename_all = "camelCase")]
    Live { read_at: i64 },
    /// As of an earlier read, and why not since.
    #[serde(rename_all = "camelCase")]
    AsOf { read_at: i64, why: Stale },
    /// Never read, and why: every account was recorded by a read that
    /// either worked or failed.
    Unread { why: Stale },
}

impl State {
    /// Why it isn't read now, if it isn't.
    pub fn why(&self) -> Option<Stale> {
        match self {
            State::Live { .. } => None,
            State::AsOf { why, .. } => Some(*why),
            State::Unread { why } => Some(*why),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stale {
    /// No agent here is signed into it now.
    SignedOut,
    /// Its provider refused the login.
    SignIn,
    /// Every login to it has expired; its agent renews one when next used.
    Expired,
    /// Its provider couldn't be reached or answered in a way not understood,
    /// or nothing has read it for `STALE`.
    ReadFailed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Limit {
    pub key: String,
    /// `5 hours`, `Weekly`.
    pub name: String,
    /// The one model it applies to, when not all.
    pub scope: Option<String>,
    pub window: Option<LimitWindow>,
    pub read_at: i64,
    /// Rounded down; 100 once its window reset since it was read.
    pub left_percent: u8,
    pub money: Option<Money>,
    /// When its window reset, if it has since it was read.
    pub reset_since: Option<i64>,
    /// For one API key's own limit, the agents holding the key.
    pub held_by: Vec<String>,
    pub outlook: Outlook,
    /// How much work is left at the pace of work; none until an hour of it
    /// has been measured, or for a window that rolls.
    pub work: Option<Work>,
}

impl Limit {
    /// Whether it limits the whole account: not one model's, nor one key's.
    pub fn whole(&self) -> bool {
        self.scope.is_none() && self.held_by.is_empty() && !self.key.starts_with("key-")
    }

    /// Whether `name` names it: part of its name or key, as `week` names
    /// Weekly and `seven_day`.
    pub fn answers_to(&self, name: &str) -> bool {
        let name = name.to_lowercase();
        self.name.to_lowercase().contains(&name) || self.key.to_lowercase().contains(&name)
    }

    fn urgency(&self) -> u8 {
        match self.outlook {
            Outlook::UsedUp { .. } => 2,
            Outlook::RunsOut { .. } => 1,
            _ => 0,
        }
    }

    fn runs_out(&self) -> Option<i64> {
        match self.outlook {
            Outlook::RunsOut { likely, .. } => Some(likely.at),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitWindow {
    pub starts_at: Option<i64>,
    pub resets: Moment,
}

/// A time to come, to the minute, and how soon it is, which decides how
/// it's said.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Moment {
    pub at: i64,
    pub horizon: Horizon,
}

impl Moment {
    pub fn of(at: i64, now: i64) -> Moment {
        let at = (at + MINUTE / 2).div_euclid(MINUTE) * MINUTE;
        let horizon = if at - now < 3 * HOUR {
            Horizon::Soon
        } else if time::local(at).date() == time::local(now).date() {
            Horizon::Today
        } else if at - now < 7 * DAY {
            Horizon::ThisWeek
        } else {
            Horizon::Later
        };
        Moment { at, horizon }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Horizon {
    /// Within three hours.
    Soon,
    Today,
    /// Within a week.
    ThisWeek,
    Later,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Money {
    pub size_usd: f64,
    /// Rounded down to the cent.
    pub left_usd: f64,
}

/// The pace of work on a limit, and how much is left at it: by the hours
/// agents here worked on its account, not the clock.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Work {
    /// Points an hour of work uses, to a tenth.
    pub rate_per_hour: f64,
    /// Hours of work left at that pace, as an 80% range; none when it lasts
    /// until its reset even with work all the while.
    pub left_hours: Option<HoursLeft>,
}

/// Hours of work left, to a tenth: the fewest and the most of the 80% range,
/// and the likely.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HoursLeft {
    pub low: f64,
    pub likely: f64,
    pub high: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Outlook {
    /// It lasts until its reset, with about this percent left then, rounded
    /// down, as an 80% range; `None` with nothing used yet.
    #[serde(rename_all = "camelCase")]
    Lasts { left_at_reset: Option<Spread> },
    /// It runs out before its reset: likely then, and within the 80% range
    /// from soonest to latest (`None` where the slow end lasts).
    #[serde(rename_all = "camelCase")]
    RunsOut {
        likely: Moment,
        soonest: Moment,
        latest: Option<Moment>,
    },
    #[serde(rename_all = "camelCase")]
    UsedUp { back: Option<Moment> },
    #[serde(rename_all = "camelCase")]
    Unknown { reason: Unknown },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Unknown {
    NotEnoughData,
    /// Read too long ago, or its window reset since.
    Stale,
    /// It never resets, as credits.
    NoReset,
    /// Its window rolls, with no start to pace from.
    Rolling,
}

/// Percents as an 80% range: its low and high ends, and the likely.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spread {
    pub low: u8,
    pub likely: u8,
    pub high: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spend {
    pub month_usd: f64,
    /// The month at its pace so far; none in its first day.
    pub forecast_usd: Option<f64>,
    /// Some usage has no known price.
    pub partial: bool,
}

/// Leave an account out of lists and alerts, or show it again.
pub fn hide(db: &rusqlite::Connection, account: &str, hidden: bool) -> Result<()> {
    db.execute(
        "UPDATE account SET hidden = ?2 WHERE id = ?1",
        params![account, hidden],
    )?;
    Ok(())
}

/// How every account stands at `now`.
pub fn status(db: &rusqlite::Connection, home: &Path, now: i64) -> Result<Status> {
    let now = now.div_euclid(MINUTE) * MINUTE;
    // Which agents each account's logins are in now, and each key's holders.
    let mut signed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut keys: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut rows = db.prepare(
        "SELECT s.agent, s.account, s.key FROM sign_in s
         WHERE s.since = (SELECT max(since) FROM sign_in WHERE folder = s.folder AND provider = s.provider)
           AND s.account IS NOT NULL",
    )?;
    for row in rows.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
    })? {
        let (agent, account, key): (String, String, Option<String>) = row?;
        let agents = signed.entry(account).or_default();
        if !agents.contains(&agent) {
            agents.push(agent.clone());
        }
        if let Some(key) = key {
            keys.entry(format!("key-{key}")).or_default().push(agent);
        }
    }
    let mut used: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut rows = db.prepare(
        "SELECT account, agent FROM response WHERE at >= ?1 AND account IS NOT NULL
         GROUP BY account, agent ORDER BY max(at) DESC",
    )?;
    for row in rows.query_map([now - IN_USE], |row| Ok((row.get(0)?, row.get(1)?)))? {
        let (account, agent): (String, String) = row?;
        used.entry(account).or_default().push(agent);
    }
    let recent: std::collections::BTreeSet<String> = db
        .prepare("SELECT DISTINCT account FROM response WHERE at >= ?1 AND account IS NOT NULL")?
        .query_map([now - RECENT], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;

    let mut accounts = Vec::new();
    let mut rows = db.prepare(
        "SELECT id, provider, kind, title, label, hidden, read_at, problem FROM account",
    )?;
    let mut rows = rows.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let read_at: Option<i64> = row.get(6)?;
        let problem: Option<String> = row.get(7)?;
        let agents = signed.remove(&id).unwrap_or_default();
        let why = match problem.as_deref() {
            _ if agents.is_empty() => Some(Stale::SignedOut),
            Some("signIn") => Some(Stale::SignIn),
            Some("expired") => Some(Stale::Expired),
            Some(_) => Some(Stale::ReadFailed),
            None => None,
        };
        let state = match (read_at, why) {
            (Some(read_at), None) if now - read_at <= STALE => State::Live { read_at },
            (Some(read_at), why) => State::AsOf {
                read_at,
                why: why.unwrap_or(Stale::ReadFailed),
            },
            (None, why) => State::Unread {
                why: why.unwrap_or(Stale::ReadFailed),
            },
        };
        let (limits, rose) = match read_at {
            Some(read_at) => limits(db, &id, read_at, &keys, now)?,
            None => (Vec::new(), false),
        };
        let used_by = used.remove(&id).unwrap_or_default();
        let kind = if row.get::<_, String>(2)? == "apiKey" {
            Kind::ApiKey
        } else {
            Kind::Subscription
        };
        accounts.push(Account {
            spend: (kind == Kind::ApiKey)
                .then(|| spend(db, &id, now))
                .transpose()?,
            deciding: deciding(&limits).map(|limit| limit.key.clone()),
            in_use: !used_by.is_empty() || rose,
            recent: !used_by.is_empty() || rose || recent.contains(&id),
            provider: row.get(1)?,
            title: row.get(3)?,
            label: row.get(4)?,
            hidden: row.get(5)?,
            id,
            kind,
            agents,
            used_by,
            state,
            limits,
        });
    }
    rank(&mut accounts);

    // Compared as files, so a link to this binary, as one on the PATH, is it.
    let agents = agents::ALL
        .iter()
        .map(|agent| {
            let info = agent.info();
            Agent {
                id: info.id.to_owned(),
                name: info.name.to_owned(),
                installed: agent
                    .folders(home)
                    .first()
                    .is_some_and(|folder| folder.exists()),
                connection: crate::connect::connection(*agent, home),
                in_use: accounts
                    .iter()
                    .any(|account| account.used_by.iter().any(|by| by == info.id)),
            }
        })
        .collect();
    Ok(Status {
        at: now,
        history_read_at: db.query_row("SELECT history_at FROM state", [], |row| row.get(0))?,
        accounts,
        agents,
        settings: crate::alerts::settings(db)?,
    })
}

/// How far back a limit's readings, and its account's work, are looked at
/// for the pace of work.
const WORKED: i64 = 14 * DAY;

/// An account's limits as last read, at `read_at`, and whether any rose
/// between its last two reads within the half hour, as use elsewhere makes
/// them.
fn limits(
    db: &rusqlite::Connection,
    account: &str,
    read_at: i64,
    keys: &BTreeMap<String, Vec<String>>,
    now: i64,
) -> Result<(Vec<Limit>, bool)> {
    // The start of each slot of work on the account.
    let worked: Vec<i64> = db
        .prepare_cached(
            "SELECT min(at) FROM response WHERE account = ?1 AND at >= ?2 GROUP BY at / ?3 ORDER BY 1",
        )?
        .query_map(params![account, now - WORKED, forecast::SLOT], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    // From when whose work it is was known: the account's first sign-in read.
    let known: Option<i64> = db.query_row(
        "SELECT min(since) FROM sign_in WHERE account = ?1",
        [account],
        |row| row.get(0),
    )?;
    let mut latest = db.prepare(
        "SELECT limit_key, name, scope, used, size, starts, resets FROM reading
         WHERE account = ?1 AND at = ?2 ORDER BY limit_key",
    )?;
    let mut latest = latest.query(params![account, read_at])?;
    let mut rose = false;
    let mut limits = Vec::new();
    while let Some(row) = latest.next()? {
        let (key, name, scope): (String, String, Option<String>) =
            (row.get(0)?, row.get(1)?, row.get(2)?);
        let (used, size): (f64, Option<f64>) = (row.get(3)?, row.get(4)?);
        let (starts, resets): (Option<i64>, Option<i64>) = (row.get(5)?, row.get(6)?);
        // Its readings, oldest first: back to its window's start, and to
        // two weeks of work.
        let since = starts.unwrap_or(read_at - DAY).min(now - WORKED);
        let history: Vec<forecast::Reading> = db
            .prepare_cached(
                "SELECT at, used, starts, resets FROM reading
                 WHERE account = ?1 AND at >= ?2 AND limit_key = ?3 ORDER BY at",
            )?
            .query_map(params![account, since, key], |row| {
                Ok(forecast::Reading {
                    at: row.get(0)?,
                    used: row.get(1)?,
                    starts: row.get(2)?,
                    resets: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        // Its current window's: those that reset with it, within the drift
        // of a few seconds providers have.
        let readings: Vec<(i64, f64)> = history
            .iter()
            .filter(|reading| {
                reading.at >= starts.unwrap_or(read_at - DAY)
                    && match (resets, reading.resets) {
                        (Some(resets), Some(read)) => (resets - read).abs() <= 5 * MINUTE,
                        (resets, read) => resets == read,
                    }
            })
            .map(|reading| (reading.at, reading.used))
            .collect();
        if let [.., before, last] = readings.as_slice() {
            rose |= last.1 > before.1 && now - last.0 <= IN_USE;
        }
        let reset_since = resets.filter(|resets| *resets <= now);
        let left = if reset_since.is_some() {
            100.0
        } else {
            (100.0 - used).clamp(0.0, 100.0)
        };
        let stale = reset_since.is_some() || now - read_at > STALE;
        let outlook = match (starts, resets) {
            _ if reset_since.is_none() && used >= 100.0 => Outlook::UsedUp {
                back: resets.map(|resets| Moment::of(resets, now)),
            },
            _ if stale => Outlook::Unknown {
                reason: Unknown::Stale,
            },
            (Some(starts), Some(resets)) => outlook(
                &Window {
                    starts,
                    resets,
                    readings,
                },
                now,
            ),
            (None, Some(_)) => Outlook::Unknown {
                reason: Unknown::Rolling,
            },
            (_, None) => Outlook::Unknown {
                reason: Unknown::NoReset,
            },
        };
        let length = starts.zip(resets).map(|(starts, resets)| resets - starts);
        let rolling = starts.is_none() && resets.is_some();
        let enough = if length.is_some_and(|length| length <= DAY) {
            3.0
        } else {
            5.0
        };
        // Work is known only for the last two weeks: a rise before then
        // would count with no hours of work beside it.
        let walked = &history[history.partition_point(|reading| reading.at < now - WORKED)..];
        let work = forecast::work_rate(walked, &worked, known, enough)
            .filter(|_| !rolling)
            .map(|rate| {
                let tenths = |value: f64| (value * 10.0).round() / 10.0;
                // A window that reset since is a new one, as long.
                let until = match reset_since {
                    Some(_) => length,
                    None => resets.map(|resets| resets - now),
                };
                let until = until.map(|until| until.max(0) as f64 / HOUR as f64);
                let left_hours =
                    forecast::work_left(100.0 - left, rate, forecast::work_band(length), until)
                        .map(|(low, likely, high)| HoursLeft {
                            low: tenths(low),
                            likely: tenths(likely),
                            high: tenths(high),
                        });
                Work {
                    rate_per_hour: tenths(rate),
                    left_hours,
                }
            });
        limits.push(Limit {
            held_by: keys.get(&key).cloned().unwrap_or_default(),
            key,
            name,
            scope,
            window: resets.map(|resets| LimitWindow {
                starts_at: starts,
                resets: Moment::of(resets, now),
            }),
            read_at,
            left_percent: left.floor() as u8,
            // What's left of money, from what was used rather than the
            // percent shown; a millionth of a cent over, as 66.6% of $50 is
            // 3329.99… cents in floating point.
            money: size.map(|size| Money {
                size_usd: size,
                left_usd: ((size * left + 1e-6).floor() / 100.0),
            }),
            reset_since,
            outlook,
            work,
        });
    }
    Ok((limits, rose))
}

/// A window's outlook as of `now`.
fn outlook(window: &Window, now: i64) -> Outlook {
    let Some(projection) = forecast::project(window, now) else {
        return Outlook::Unknown {
            reason: Unknown::NotEnoughData,
        };
    };
    let band = forecast::band(window, now);
    // It runs out only a meaningful while before its reset: a fiftieth of
    // the window, a quarter hour at least.
    let margin = ((window.resets - window.starts) / 50).max(15 * MINUTE);
    let meaningful = |at: i64| at + margin < window.resets;
    if let Some(out) = projection.runs_out.filter(|at| meaningful(*at)) {
        if !forecast::told(window, now, out) {
            return Outlook::Unknown {
                reason: Unknown::NotEnoughData,
            };
        }
        // The fast end runs out first, as every band's fast end is over 1.
        let (soonest, latest) = forecast::runs_out(&projection, band, window.resets);
        // To ten minutes within a day, the hour beyond: a pace can't say the
        // minute a week runs out; and never before now.
        let shown = |at: i64| {
            let step = if at - now <= DAY { 10 * MINUTE } else { HOUR };
            ((at + step / 2).div_euclid(step) * step).max(now)
        };
        let likely = shown(out);
        return Outlook::RunsOut {
            likely: Moment::of(likely, now),
            soonest: Moment::of(shown(soonest.unwrap_or(out)).min(likely), now),
            latest: latest
                .filter(|at| meaningful(*at))
                .map(|at| Moment::of(shown(at).max(likely), now)),
        };
    }
    if projection.rate <= 0.0 {
        return Outlook::Lasts {
            left_at_reset: None,
        };
    }
    // Used at the slow and fast ends, as left: the fast end leaves least.
    let (slow, fast) = forecast::at_reset(&projection, band);
    let left = |used: f64| (100.0 - used).clamp(0.0, 100.0).floor() as u8;
    Outlook::Lasts {
        left_at_reset: Some(Spread {
            low: left(fast),
            likely: left(projection.at_reset),
            high: left(slow),
        }),
    }
}

/// The limit that decides how an account stands: of those not reset since
/// read, on all its usage where any is, the most urgent, then the soonest
/// out, then the least left. When every one reset since, the last to.
fn deciding(limits: &[Limit]) -> Option<&Limit> {
    let known: Vec<&Limit> = limits
        .iter()
        .filter(|limit| limit.reset_since.is_none())
        .collect();
    if known.is_empty() {
        return limits.iter().max_by_key(|limit| limit.reset_since);
    }
    let whole: Vec<&Limit> = known
        .iter()
        .copied()
        .filter(|limit| limit.whole())
        .collect();
    let candidates = if whole.is_empty() { known } else { whole };
    candidates.into_iter().min_by_key(|limit| {
        let least = match &limit.outlook {
            Outlook::Lasts {
                left_at_reset: Some(spread),
            } => spread.likely,
            _ => limit.left_percent,
        };
        (
            Reverse(limit.urgency()),
            limit.runs_out().unwrap_or(i64::MAX),
            least,
        )
    })
}

/// The order every front-end lists accounts in: those in use, the most
/// urgent and soonest out first, then the least left; then the rest, the
/// most left first; then those whose login was refused; then those with
/// nothing to show.
fn rank(accounts: &mut [Account]) {
    accounts.sort_by_cached_key(|account| {
        let deciding = account.deciding_limit();
        let left = deciding.map_or(i32::MAX, |limit| i32::from(limit.left_percent));
        let group = match () {
            () if account.in_use => 0,
            () if account.refused() => 2,
            () if deciding.is_some() => 1,
            () => 3,
        };
        let urgent = deciding.filter(|_| account.in_use);
        (
            group,
            Reverse(urgent.map_or(0, Limit::urgency)),
            urgent.and_then(Limit::runs_out).unwrap_or(i64::MAX),
            if account.in_use { left } else { -left },
            account.id.clone(),
        )
    });
}

/// What an API-key account cost this month, and the month at that pace.
fn spend(db: &rusqlite::Connection, account: &str, now: i64) -> Result<Spend> {
    let month = time::start_of(time::Period::Month, now);
    let (cost, unpriced): (f64, i64) = db.query_row(
        "SELECT total(cost), count(*) - count(cost) FROM response WHERE account = ?1 AND at >= ?2",
        params![account, month],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let whole = (time::next(time::Period::Month, month) - month) as f64;
    // Too soon to say in the first day: a dollar in the first minutes would
    // forecast thousands.
    let elapsed = now - month;
    let forecast =
        (elapsed >= DAY).then(|| (cost * whole / elapsed as f64 * 100.0).round() / 100.0);
    Ok(Spend {
        month_usd: cost,
        forecast_usd: forecast,
        partial: unpriced > 0,
    })
}

/// The accounts `name` names: its id, or part of its title, label or id.
pub fn named<'a>(status: &'a Status, name: &str) -> crate::Result<Vec<&'a Account>> {
    let wanted = name.to_lowercase();
    let exact: Vec<&Account> = status
        .accounts
        .iter()
        .filter(|account| account.id == name)
        .collect();
    let found: Vec<&Account> = if exact.is_empty() {
        status
            .accounts
            .iter()
            .filter(|account| {
                [
                    Some(&account.title),
                    account.label.as_ref(),
                    Some(&account.id),
                ]
                .into_iter()
                .flatten()
                .any(|text| text.to_lowercase().contains(&wanted))
            })
            .collect()
    } else {
        exact
    };
    if found.is_empty() {
        let known: Vec<String> = status
            .accounts
            .iter()
            .map(|account| format!("{} ({})", account.full_title(), account.id))
            .collect();
        return Err(crate::Error::NotFound(format!(
            "no account is named {name}; there are: {}",
            known.join(", ")
        )));
    }
    Ok(found)
}
