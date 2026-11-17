//! How every account stands: its limits, where each is headed, how much
//! work each has left, and the limit that decides it. The app, the CLI and
//! the MCP server all show this one answer.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::path::Path;

use jiff::tz::TimeZone;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::forecast::{self, Window};
use crate::providers::Kind;
use crate::time::{self, DAY, HOUR, MINUTE};
use crate::{Result, agents};

mod text;
pub use text::{fit, limit_text, line, none, stale, text, verdict};

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
    /// Whether the first read of the agents' history finished.
    pub history_read: bool,
    /// Every account, hidden ones too, in the order to list them: in use
    /// first, the most urgent first (docs/architecture.md#status). Until
    /// `history_read`, each is listed as one in use, whatever its `in_use`
    /// says.
    pub accounts: Vec<Account>,
    pub settings: crate::alerts::Settings,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    /// `claude-code`.
    pub id: String,
    /// `Claude Code`.
    pub name: String,
    /// Its own folder exists. For Antigravity, one of its own two in
    /// `~/.gemini`, which the Gemini command line keeps too.
    pub installed: bool,
    /// Whether it runs Turnscope's MCP server.
    pub connection: Connection,
    /// Its responses drew on an account in the last half hour.
    pub in_use: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Connection {
    /// It runs this binary's MCP server in every one of its folders. A link
    /// to the binary counts.
    Connected,
    /// A folder of it runs another copy's, as one since moved.
    Outdated,
    /// Otherwise: a folder of it runs none.
    Available,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    /// `<provider>:<the provider's id for it>`: `anthropic:acct-1:org-1`.
    pub id: String,
    /// The provider's id: `anthropic`.
    pub provider: String,
    pub kind: Kind,
    /// `Claude Max`, `Anthropic API key`.
    pub title: String,
    /// What tells it from another of its kind, such as an email address.
    pub label: Option<String>,
    /// The agents signed into it now, by id.
    pub agents: Vec<String>,
    /// The agents that drew on it in the last half hour, the latest first.
    pub used_by: Vec<String>,
    /// Drawn on in the last half hour, or a limit rose at its last reading,
    /// taken in the last half hour, as use elsewhere makes it.
    pub in_use: bool,
    /// In use, or drawn on in the last 7 days: one the person uses.
    pub recent: bool,
    /// The person hid it.
    pub hidden: bool,
    pub state: State,
    /// Its limits as last read, by key. Empty if never read.
    pub limits: Vec<Limit>,
    /// The key of the limit that decides how it stands
    /// (docs/architecture.md#status). None with no limits.
    pub deciding: Option<String>,
    /// For an API key, what it cost this month. None for a subscription.
    pub spend: Option<Spend>,
}

impl Account {
    /// `full_title` in SQL, of an `account` table named `a`.
    pub const FULL_TITLE: &str = "a.title || coalesce(' (' || a.label || ')', '')";

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
                let names: Vec<String> = self.limits.iter().map(Limit::named).collect();
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
    /// Read at the last try.
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
            State::AsOf { why, .. } | State::Unread { why } => Some(*why),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stale {
    /// No agent here is signed in to it now.
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
    /// The provider's key: `five_hour`, `seven_day`. An API key's own limit
    /// is `key-<fingerprint>`.
    pub key: String,
    /// The provider's name: `5 hours`, `Weekly`, `Credits`.
    pub name: String,
    /// The model it applies to, such as `Opus`, or several joined with
    /// "and", such as `Claude and GPT`. None for every model.
    pub scope: Option<String>,
    /// None for a limit that never resets, such as credits.
    pub window: Option<LimitWindow>,
    pub read_at: i64,
    /// Percent left, 0 to 100, rounded down; 100 once its window reset
    /// since it was read.
    pub left_percent: u8,
    /// For a limit of money, its size and what's left.
    pub money: Option<Money>,
    /// When its window reset, if it has since it was read.
    pub reset_since: Option<i64>,
    /// For one API key's own limit, the agents holding the key. Otherwise
    /// empty.
    pub held_by: Vec<String>,
    pub outlook: Outlook,
    /// The pace of work, and the work left at it. None until an hour of
    /// work and 2 points are measured, and for a window that rolls.
    pub work: Option<Work>,
}

impl Limit {
    /// Its name, and the model it's on if it's on one: "Weekly (Opus)".
    pub fn named(&self) -> String {
        match &self.scope {
            Some(scope) => format!("{} ({scope})", self.name),
            None => self.name.clone(),
        }
    }

    /// Its name in lowercase, with `hours` after a count of hours and its
    /// model before it: "weekly", "5h" or "5-hour", "Opus weekly".
    pub fn lowercase(&self, hours: &str) -> String {
        let name = self.name.to_lowercase();
        let name = match name.split_once(' ') {
            Some((count, "hours")) if count.parse::<u32>().is_ok() => format!("{count}{hours}"),
            _ => name,
        };
        match &self.scope {
            Some(scope) => format!("{scope} {name}"),
            None => name,
        }
    }

    /// Whether it limits the whole account: not one model's, nor one key's.
    pub fn whole(&self) -> bool {
        self.scope.is_none() && !self.key.starts_with("key-")
    }

    /// When its window resets, unless it already has since it was read.
    pub fn resets(&self) -> Option<i64> {
        self.window
            .filter(|_| self.reset_since.is_none())
            .map(|window| window.resets.at)
    }

    /// The hours until its window resets, as its work left counts them.
    pub fn hours_to_reset(&self, now: i64) -> Option<f64> {
        let window = self.window?;
        hours_to_reset(
            window.starts_at,
            window.resets.at,
            self.reset_since.is_some(),
            now,
        )
    }

    /// When its window started, unless it has reset since it was read: the
    /// start read is then the window before's.
    pub fn started(&self) -> Option<i64> {
        self.window
            .filter(|_| self.reset_since.is_none())?
            .starts_at
    }

    /// Whether `name` names it: part of its name as shown, with its models
    /// after it as the command line shows them or before it as the app
    /// does, or part of its key. So `week` names Weekly and `seven_day`, and
    /// `weekly (claude` and `gpt weekly` name Weekly (Claude and GPT). Of the
    /// limits a name fits, the first by key is taken: `gpt` alone is 5 hours
    /// (Claude and GPT).
    pub fn answers_to(&self, name: &str) -> bool {
        let name = name.to_lowercase();
        let holds = |text: &str| text.to_lowercase().contains(&name);
        let models_first = self
            .scope
            .as_ref()
            .is_some_and(|scope| holds(&format!("{scope} {}", self.name)));
        holds(&self.named()) || models_first || holds(&self.key)
    }

    fn urgency(&self) -> u8 {
        match self.outlook {
            Outlook::UsedUp { .. } => 2,
            Outlook::RunsOut { .. } => 1,
            _ => 0,
        }
    }

    /// When it's back, if it's used up. One with no reset, as credits, is
    /// back at no time that's known, so it counts as later than any: its
    /// account is never said to be back at a time it may still be used up.
    fn back(&self) -> Option<i64> {
        match self.outlook {
            Outlook::UsedUp { back } => Some(back.map_or(i64::MAX, |back| back.at)),
            _ => None,
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
    /// None for a window that rolls.
    pub starts_at: Option<i64>,
    pub resets: Moment,
}

/// A time to come, to the minute, and how soon it is, which decides how
/// it's said: as a countdown, a time of day, a weekday or a date.
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
        } else if time::today(at, now) {
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
    /// Later today, in local time.
    Today,
    /// Within a week.
    ThisWeek,
    /// Further off.
    Later,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Money {
    /// The limit's size.
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
    /// None when it lasts until its reset even with work all the while.
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
    /// from soonest to latest (`None` where the slow end lasts). Each is
    /// rounded to 10 minutes within a day and to the hour beyond, and is
    /// never in the past.
    #[serde(rename_all = "camelCase")]
    RunsOut {
        likely: Moment,
        soonest: Moment,
        latest: Option<Moment>,
    },
    /// It's used up, and back when it resets: `None` for a limit that never
    /// resets.
    #[serde(rename_all = "camelCase")]
    UsedUp { back: Option<Moment> },
    /// No forecast, and why.
    #[serde(rename_all = "camelCase")]
    Unknown { reason: Unknown },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Unknown {
    /// Too little of the window has passed or been used to tell a pace
    /// (docs/architecture.md#forecast).
    NotEnoughData,
    /// Read too long ago, or its window reset since.
    Stale,
    /// It never resets, as credits.
    NoReset,
    /// Its window rolls, with no start to pace from.
    Rolling,
}

/// Percent left at the reset, as an 80% range, each end rounded down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spread {
    /// At the fast end of the range.
    pub low: u8,
    pub likely: u8,
    /// At the slow end.
    pub high: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spend {
    /// What it cost so far this calendar month.
    pub month_usd: f64,
    /// The whole month at its pace so far; None in its first day.
    pub forecast_usd: Option<f64>,
    /// Some of this month's usage has no known price, so it cost more than
    /// `month_usd`.
    pub partial: bool,
}

/// Leave an account out of lists and alerts, or show it again.
pub fn hide(db: &rusqlite::Connection, account: &str, hidden: bool) -> Result<()> {
    crate::db::writing(db)?;
    db.execute(
        "UPDATE account SET hidden = ?2 WHERE id = ?1",
        params![account, hidden],
    )?;
    Ok(())
}

/// How every account stands at `now`.
pub fn status(db: &rusqlite::Connection, now: i64) -> Result<Status> {
    // As of the minute: between readings it changes at most once a minute.
    let now = now.div_euclid(MINUTE) * MINUTE;
    // Which agents each account's logins are in now, and each key's holders.
    let mut signed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut keys: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut rows = db.prepare(
        "SELECT s.agent, s.account, s.key FROM sign_in s
         WHERE s.since = (SELECT max(since) FROM sign_in WHERE folder = s.folder AND provider = s.provider)
           AND s.account IS NOT NULL",
    )?;
    for row in rows.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))? {
        let (agent, account, key): (String, String, Option<String>) = row?;
        let agents = signed.entry(account).or_default();
        if !agents.contains(&agent) {
            agents.push(agent.clone());
        }
        if let Some(key) = key {
            let agents = keys.entry(format!("key-{key}")).or_default();
            if !agents.contains(&agent) {
                agents.push(agent);
            }
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

    let mut accounts = Vec::new();
    // Whether each was drawn on in the last 7 days: one seek in
    // `response_account` per account.
    let mut rows = db.prepare(
        "SELECT id, provider, kind, title, label, hidden, read_at, problem,
             EXISTS (SELECT 1 FROM response WHERE account = a.id AND at >= ?1)
         FROM account a",
    )?;
    let mut rows = rows.query([now - RECENT])?;
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
            recent: !used_by.is_empty() || rose || row.get(8)?,
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
    let history_read = db.query_row("SELECT history_at IS NOT NULL FROM state", [], |row| {
        row.get(0)
    })?;
    rank(&mut accounts, history_read);
    Ok(Status {
        at: now,
        history_read,
        accounts,
        settings: crate::alerts::settings(db)?,
    })
}

/// Every agent, apart from `status`: only the app and `turnscope connect`
/// show it, and it reads each agent's MCP settings, where Claude Code's
/// `~/.claude.json` can be megabytes. With a made-up one of 5.8 MB,
/// `statusline` ran 183M instructions and peaked at 38 MB when `status`
/// worked this out, and 42M and 8 MB once it didn't.
pub fn agents(home: &Path, accounts: &[Account]) -> Vec<Agent> {
    agents::ALL
        .iter()
        .map(|agent| {
            let info = agent.info();
            Agent {
                id: info.id.to_owned(),
                name: info.name.to_owned(),
                installed: agent
                    .folders(home)
                    .first()
                    .is_some_and(|folder| agent.installed(folder)),
                connection: crate::connect::connection(*agent, home),
                in_use: accounts
                    .iter()
                    .any(|account| account.used_by.iter().any(|by| by == info.id)),
            }
        })
        .collect()
}

/// How far back a limit's readings, and its account's work, are looked at
/// for the pace of work.
const WORKED: i64 = 14 * DAY;

/// The hours from `now` until a window resets. One that reset since it was
/// read is a new one, as long: its start isn't known, and is taken as now.
fn hours_to_reset(starts: Option<i64>, resets: i64, reset: bool, now: i64) -> Option<f64> {
    let until = if reset {
        resets - starts?
    } else {
        resets - now
    };
    Some(until.max(0) as f64 / HOUR as f64)
}

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
    // The start of each slot of work on the account: one seek in
    // `response_account` per slot, over grouping every response. With a
    // made-up 233,000 responses in two weeks, each push took 80 ms of CPU
    // grouping and 12 ms seeking. The cost: 2,000 responses over 1,194 slots
    // took 4.3 ms, not 1.5 ms.
    let worked: Vec<i64> = db
        .prepare_cached(
            "WITH RECURSIVE slot (at) AS (
                 SELECT min(at) FROM response WHERE account = ?1 AND at >= ?2
                 UNION ALL
                 SELECT (SELECT min(at) FROM response WHERE account = ?1 AND at >= (slot.at / ?3 + 1) * ?3)
                 FROM slot WHERE slot.at IS NOT NULL
             )
             SELECT at FROM slot WHERE at IS NOT NULL",
        )?
        .query_map(params![account, now - WORKED, forecast::SLOT], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let known: Option<i64> = db.query_row(
        "SELECT min(since) FROM sign_in WHERE account = ?1",
        [account],
        |row| row.get(0),
    )?;
    // Every limit's readings at once, from its earliest window's start or
    // two weeks back, whichever is earlier: each limit needs its window's
    // and its last two weeks'. Chosen over a query per limit: the key is
    // last in `reading`'s primary key, so each such query reads every
    // limit's rows. With five limits read every 5 minutes for 30 days, made
    // up, that took 6.6 ms an account, and this 2.7 ms.
    let from: Option<i64> = db.query_row(
        "SELECT min(coalesce(starts, ?2 - ?3)) FROM reading WHERE account = ?1 AND at = ?2",
        params![account, read_at, DAY],
        |row| row.get(0),
    )?;
    let mut histories: BTreeMap<String, Vec<forecast::Reading>> = BTreeMap::new();
    let mut rows = db.prepare_cached(
        "SELECT limit_key, at, used, starts, resets FROM reading
         WHERE account = ?1 AND at >= ?2 ORDER BY at",
    )?;
    let mut rows = rows.query(params![account, from.unwrap_or(now).min(now - WORKED)])?;
    while let Some(row) = rows.next()? {
        histories
            .entry(row.get(0)?)
            .or_default()
            .push(forecast::Reading {
                at: row.get(1)?,
                used: row.get(2)?,
                starts: row.get(3)?,
                resets: row.get(4)?,
            });
    }
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
        let history = histories.get(&key).map_or(&[][..], Vec::as_slice);
        // Its current window's: those that reset with it.
        let readings: Vec<(i64, f64)> = history
            .iter()
            .filter(|reading| {
                reading.at >= starts.unwrap_or(read_at - DAY)
                    && forecast::same_reset(resets, reading.resets)
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
        // Work is known only for the last two weeks: a rise before then
        // would count with no hours of work beside it.
        let walked = &history[history.partition_point(|reading| reading.at < now - WORKED)..];
        let work = forecast::work_rate(walked, &worked, known, forecast::enough_work(length))
            .filter(|_| !rolling)
            .map(|rate| {
                let tenths = |value: f64| (value * 10.0).round() / 10.0;
                let until = resets
                    .and_then(|resets| hours_to_reset(starts, resets, reset_since.is_some(), now));
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
    // Nothing is said of the reset only for a limit with nothing used. One
    // whose use was given back, with none since, has a pace of none and
    // lasts with what it has now, over not enough data: no use for a
    // twentieth of the window is a pace, as before any use. Taken for one
    // with nothing used, a limit 40% used said "No use shows in its reading
    // yet" beside 60% left.
    if projection.used <= 0.0 {
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

/// The limit that decides how an account stands. Of several used up, the
/// last back decides, over the first listed: the account can't be used
/// until every one is back. With 5 hours back in an hour and Weekly in
/// three days, the first listed said the account was back in an hour.
fn deciding(limits: &[Limit]) -> Option<&Limit> {
    limits
        .iter()
        .filter(|limit| limit.reset_since.is_none())
        .min_by_key(|limit| {
            let least = match &limit.outlook {
                Outlook::Lasts {
                    left_at_reset: Some(spread),
                } => spread.likely,
                _ => limit.left_percent,
            };
            (
                !limit.whole(),
                Reverse(limit.urgency()),
                Reverse(limit.back()),
                limit.runs_out().unwrap_or(i64::MAX),
                least,
            )
        })
        .or_else(|| limits.iter().max_by_key(|limit| limit.reset_since))
}

/// The order every front end lists accounts in.
///
/// Until the first read of history ends, every account is ranked as one in
/// use: on a first launch the limits are pushed before any history
/// (`serve::keep`), so none is known to be. Ranked as not in use, the most
/// left came first, and the app, which shows every account as in use until
/// then, would have headlined the account least used. `in_use` itself stays
/// as measured, so no alert is raised on a guess.
fn rank(accounts: &mut [Account], history_read: bool) {
    accounts.sort_by_cached_key(|account| {
        let in_use = account.in_use || !history_read;
        let deciding = account.deciding_limit();
        let left = deciding.map_or(i32::MAX, |limit| i32::from(limit.left_percent));
        let group = match () {
            () if in_use => 0,
            () if account.refused() => 2,
            () if deciding.is_some() => 1,
            () => 3,
        };
        let urgent = deciding.filter(|_| in_use);
        (
            group,
            Reverse(urgent.map_or(0, Limit::urgency)),
            urgent.and_then(Limit::runs_out).unwrap_or(i64::MAX),
            if in_use { left } else { -left },
            account.id.clone(),
        )
    });
}

/// What an API-key account cost this month, and the month at that pace.
fn spend(db: &rusqlite::Connection, account: &str, now: i64) -> Result<Spend> {
    let (month, next) = time::window(time::Period::Month, now, &TimeZone::system());
    let (cost, unpriced): (f64, i64) = db.query_row(
        "SELECT total(cost), count(*) - count(cost) FROM response WHERE account = ?1 AND at >= ?2",
        params![account, month],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let whole = (next - month) as f64;
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
    if let Some(exact) = status.accounts.iter().find(|account| account.id == name) {
        return Ok(vec![exact]);
    }
    let wanted = name.to_lowercase();
    let found: Vec<&Account> = status
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
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_limit_answers_to_its_name_as_shown_and_its_key() {
        let example: serde_json::Value =
            serde_json::from_str(include_str!("../contract/status.notification.json")).unwrap();
        let mut account: Account =
            serde_json::from_value(example["params"]["accounts"][0].clone()).unwrap();
        // As Google's limit on Claude and GPT is: its key doesn't name them.
        account.limits[2].key = "weekly:3p-weekly".to_owned();
        account.limits[2].scope = Some("Claude and GPT".to_owned());
        assert_eq!(account.limit("week").unwrap().key, "seven_day");
        // With its models after its name, as the command line shows it, or
        // before, as the app does.
        for name in [
            "gpt",
            "Weekly (Claude",
            "3p",
            "Claude and GPT weekly",
            "gpt week",
        ] {
            assert_eq!(account.limit(name).unwrap().key, "weekly:3p-weekly");
        }
        let none = account.limit("month").unwrap_err().to_string();
        let names = "has no limit named month; it has 5 hours, Weekly, Weekly (Claude and GPT)";
        assert!(none.ends_with(names), "{none}");
    }

    #[test]
    fn of_the_limits_used_up_the_last_back_decides() {
        let example: serde_json::Value =
            serde_json::from_str(include_str!("../contract/status.notification.json")).unwrap();
        let now = example["params"]["at"].as_i64().unwrap();
        let mut account: Account =
            serde_json::from_value(example["params"]["accounts"][0].clone()).unwrap();
        let back = |hours: i64| Outlook::UsedUp {
            back: Some(Moment::of(now + hours * HOUR, now)),
        };
        let never = Outlook::UsedUp { back: None };
        let decides = |account: &Account| deciding(&account.limits).unwrap().key.clone();
        // 5 hours is back in an hour and Weekly in three days: the account
        // can't be used until Weekly is.
        (account.limits[0].outlook, account.limits[1].outlook) = (back(1), back(72));
        assert_eq!(decides(&account), "seven_day");
        account.deciding = Some(decides(&account));
        assert_eq!(
            verdict(&account, now),
            format!(
                "Claude Max (me@example.com): Weekly is used up, back {}.",
                time::ahead(now + 72 * HOUR, now)
            )
        );
        assert_eq!(
            fit(&account, 1.0, now).unwrap(),
            "1 more hour of work doesn't fit: Weekly is used up."
        );
        // One with no reset is back at no known time, wherever it's listed.
        account.limits[1].outlook = never.clone();
        assert_eq!(decides(&account), "seven_day");
        (account.limits[0].outlook, account.limits[1].outlook) = (never, back(72));
        assert_eq!(decides(&account), "five_hour");
    }

    /// An account and its sign-in, read at `now`, with `readings` of its
    /// limit `five_hour`, a window from 0 to 5 hours: when, and percent used.
    fn read(now: i64, kind: &str, readings: &[(i64, f64)]) -> Status {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).unwrap();
        db.execute(
            "INSERT INTO account (id, provider, kind, title, read_at)
             VALUES ('anthropic:a', 'anthropic', ?1, 'Claude', ?2)",
            params![kind, now],
        )
        .unwrap();
        db.execute(
            "INSERT INTO sign_in (agent, folder, provider, since, account)
             VALUES ('claude-code', '/a', 'anthropic', 0, 'anthropic:a')",
            [],
        )
        .unwrap();
        for (at, used) in readings {
            db.execute(
                "INSERT INTO reading (account, limit_key, at, name, used, starts, resets)
                 VALUES ('anthropic:a', 'five_hour', ?1, '5 hours', ?2, 0, ?3)",
                params![at, used, 5 * HOUR],
            )
            .unwrap();
        }
        status(&db, now).unwrap()
    }

    #[test]
    fn a_limit_with_use_given_back_and_none_since_lasts_with_what_it_has() {
        // Half used after an hour, 40% at two hours, as an early reset gives
        // back, and still 40% half an hour on.
        let now = 2 * HOUR + 30 * MINUTE;
        let given_back = [(HOUR, 50.0), (2 * HOUR, 40.0), (now, 40.0)];
        let status = read(now, "subscription", &given_back);
        let limit = &status.accounts[0].limits[0];
        let left = Some(Spread {
            low: 60,
            likely: 60,
            high: 60,
        });
        assert_eq!(limit.left_percent, 60);
        assert_eq!(
            limit.outlook,
            Outlook::Lasts {
                left_at_reset: left
            }
        );
        let said = limit_text(limit, status.at);
        assert!(
            said.ends_with("Lasts, with about 60% left at the reset."),
            "{said}"
        );
        // Only one with nothing used says no use shows.
        let status = read(now, "subscription", &[(HOUR, 0.0), (now, 0.0)]);
        let limit = &status.accounts[0].limits[0];
        assert_eq!(
            limit.outlook,
            Outlook::Lasts {
                left_at_reset: None
            }
        );
        let said = limit_text(limit, status.at);
        assert!(said.ends_with("No use shows in its reading yet."), "{said}");
    }

    /// As on a first launch, between the limits read and the history read:
    /// two accounts read two hours into their 5 hours, with no response yet.
    #[test]
    fn until_history_is_read_accounts_are_ranked_as_if_in_use() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).unwrap();
        let now = 2 * HOUR;
        for (id, used) in [("anthropic:a", 90.0), ("openai:b", 10.0)] {
            let provider = id.split(':').next().unwrap();
            db.execute(
                "INSERT INTO account (id, provider, kind, title, read_at)
                 VALUES (?1, ?2, 'subscription', ?1, ?3)",
                params![id, provider, now],
            )
            .unwrap();
            db.execute(
                "INSERT INTO reading (account, limit_key, at, name, used, starts, resets)
                 VALUES (?1, 'five_hour', ?2, '5 hours', ?3, 0, ?4)",
                params![id, now, used, 5 * HOUR],
            )
            .unwrap();
        }
        let ranked = |db: &rusqlite::Connection| -> Vec<String> {
            let status = status(db, now).unwrap();
            // Neither is said to be in use, which alerts go by.
            assert!(status.accounts.iter().all(|account| !account.in_use));
            status
                .accounts
                .iter()
                .map(|account| account.id.clone())
                .collect()
        };
        // Neither is known to be in use yet: the one running out comes first.
        assert_eq!(ranked(&db), ["anthropic:a", "openai:b"]);
        // Once history is read, neither is in use: the most left first.
        db.execute("UPDATE state SET history_at = ?1", [now])
            .unwrap();
        assert_eq!(ranked(&db), ["openai:b", "anthropic:a"]);
    }

    #[test]
    fn a_key_that_spent_nothing_says_no_pace() {
        // 2026-10-15 12:00 UTC: mid-month in every time zone, so the month
        // has a pace.
        let now = 1_792_065_600_000;
        let status = read(now, "apiKey", &[]);
        let account = &status.accounts[0];
        let spent = Spend {
            month_usd: 0.0,
            forecast_usd: Some(0.0),
            partial: false,
        };
        assert_eq!(account.spend, Some(spent));
        let said = text(&status, &[account], None);
        assert!(
            said.ends_with("This month, across its keys: $0.00."),
            "{said}"
        );
        // From a thousand, dollars have a comma, as on the status line.
        let mut account = account.clone();
        account.spend = Some(Spend {
            month_usd: 1234.5,
            forecast_usd: Some(2469.0),
            partial: false,
        });
        let said = text(&status, &[&account], None);
        assert!(
            said.ends_with("This month, across its keys: $1,234.50, on pace for about $2,469.00."),
            "{said}"
        );
    }

    #[test]
    fn an_account_no_agent_is_signed_in_to_is_signed_out_whatever_its_read_said() {
        // Its login refused, then signed out of: the agents to sign in with
        // are named only for a login refused or expired, so one is always
        // there to name.
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).unwrap();
        db.execute_batch(
            "INSERT INTO account (id, provider, kind, title, read_at, problem) VALUES
                 ('xai:a', 'xai', 'subscription', 'SuperGrok', NULL, 'signIn'),
                 ('xai:b', 'xai', 'subscription', 'SuperGrok', 0, 'expired');",
        )
        .unwrap();
        let status = status(&db, HOUR).unwrap();
        for account in &status.accounts {
            assert!(account.agents.is_empty());
            assert_eq!(account.state.why(), Some(Stale::SignedOut));
        }
    }

    #[test]
    fn a_key_held_by_one_agent_in_two_folders_names_it_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).unwrap();
        db.execute_batch(
            "INSERT INTO account (id, provider, kind, title, read_at)
                 VALUES ('openrouter:api', 'openrouter', 'apiKey', 'OpenRouter API key', 0);
             INSERT INTO reading (account, limit_key, at, name, used, size, starts, resets)
                 VALUES ('openrouter:api', 'key-abc', 0, 'Weekly key limit', 10, 20, 0, 604800000);
             INSERT INTO sign_in (agent, folder, provider, since, account, key)
                 VALUES ('pi', '/a', 'openrouter', 0, 'openrouter:api', 'abc'),
                        ('pi', '/b', 'openrouter', 0, 'openrouter:api', 'abc');",
        )
        .unwrap();
        let status = status(&db, HOUR).unwrap();
        let limit = &status.accounts[0].limits[0];
        assert_eq!(limit.held_by, ["pi"]);
    }
}
