//! How every account stands: its limits, where each is headed, how much
//! work each has left, and the limit that decides it. The app, the CLI and
//! the MCP server all show this one answer.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::path::Path;

use jiff::tz::TimeZone;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::forecast::{self, Window};
use crate::providers::Kind;
use crate::time::{self, DAY, HOUR, MINUTE};
use crate::{Result, agents};

mod text;
pub use text::{
    NONE_READ, agent_names, alternate_note, fit, fits, limit_text, line, list, most_room, none,
    stale, text, verdict, via,
};

/// How recently a response must have drawn on an account for it to be in use.
const IN_USE: i64 = 30 * MINUTE;

/// How recently, for it to be one the person uses.
const RECENT: i64 = 7 * DAY;

/// How long readings stand, as limits are read every 5 minutes in use and
/// every 20 not (`limits::Schedule`). After that no outlook is said, and the
/// account shows as of its last read.
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
    /// When `connection` is `Outdated`, the command path currently configured in the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured: Option<String>,
    /// Its responses drew on an account in the last half hour.
    pub in_use: bool,
}

/// How a limit or an account stands, which sets its color in the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Standing {
    Lasts,
    RunningOut,
    UsedUp,
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
    /// taken in the last half hour, from the one before it in its window
    /// (or in the day before, with no start), as use elsewhere makes it.
    pub in_use: bool,
    /// In use, or drawn on in the last 7 days: one the person uses.
    pub recent: bool,
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

    /// `in_use` in SQL, of an `account` table named `a`, as of `?1`.
    pub(crate) fn in_use_sql() -> String {
        format!(
            "(EXISTS (SELECT 1 FROM response WHERE account = a.id AND at >= ?1 - {IN_USE})
             OR EXISTS (SELECT 1 FROM reading r WHERE r.account = a.id AND r.at >= ?1 - {IN_USE}
                 AND {latest}
                 AND r.used > (SELECT used FROM reading p
                     WHERE p.account = r.account AND p.limit_key = r.limit_key AND p.at < r.at
                         AND p.at >= coalesce(r.starts, r.at - {DAY})
                         AND (p.resets IS r.resets OR abs(p.resets - r.resets) <= {drift})
                     ORDER BY p.at DESC LIMIT 1)))",
            drift = forecast::DRIFT,
            latest = crate::limits::LATEST_READING,
        )
    }

    /// `Claude Max (me@example.com)`.
    pub fn full_title(&self) -> String {
        match &self.label {
            Some(label) => format!("{} ({label})", self.title),
            None => self.title.clone(),
        }
    }

    /// Why none of its limits are known: none were read, and it isn't read
    /// now. One read with none, as a key whose provider Turnscope doesn't
    /// ask, may still draw on credit or a spend cap.
    pub fn unread(&self) -> Option<Stale> {
        self.limits.is_empty().then(|| self.state.why()).flatten()
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

    /// How its deciding limit stands.
    pub fn standing(&self) -> Standing {
        self.deciding_limit()
            .map_or(Standing::Lasts, Limit::standing)
    }

    /// Whether it stands by `limit`, for whether it can be used and how
    /// long: a limit on all its usage, or where it has none, one of the
    /// pool its deciding limit is in (`pool`).
    pub fn stands_by(&self, limit: &Limit) -> bool {
        if self.limits.iter().any(Limit::whole) {
            return limit.whole();
        }
        limit.scope.is_some()
            && !limit.is_key()
            && self
                .deciding_limit()
                .is_some_and(|deciding| deciding.scope == limit.scope)
    }

    /// Its login was refused, at its last read or every one.
    pub fn refused(&self) -> bool {
        self.state.why() == Some(Stale::SignIn)
    }
}

/// How an account's limits were last read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum State {
    /// Read at the last try.
    #[serde(rename_all = "camelCase")]
    Live { read_at: i64 },
    /// As of an earlier read, why not since, and the hours agents here
    /// worked on it since, to a tenth: what they used isn't in that read.
    #[serde(rename_all = "camelCase")]
    AsOf {
        read_at: i64,
        why: Stale,
        worked_hours: f64,
    },
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
    /// Its provider refused the login at two reads in a row.
    SignIn,
    /// Every login to it has expired; its agent renews one when next used.
    Expired,
    /// Its provider couldn't be reached, answered in a way not understood,
    /// or refused the login once, or nothing has read it for `STALE`. One
    /// refusal isn't `SignIn`: an agent replacing its token while it was
    /// sent is refused once, and read at the next try.
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
    /// The model it applies to, such as `Opus`, several joined with "and",
    /// such as `Claude and GPT`, or use no agent here makes, as `Other
    /// apps`. None for all the account's usage.
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
    /// The work rate, and the work left at it. None until an hour of work
    /// and 2 points are measured, and for a window that rolls.
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

    /// Whether it limits the whole account: it has no scope, and isn't one
    /// key's.
    pub fn whole(&self) -> bool {
        self.scope.is_none() && !self.is_key()
    }

    pub fn is_key(&self) -> bool {
        crate::providers::key_limit_fingerprint(&self.key).is_some()
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

    /// Its window's length, unless it rolls.
    pub(crate) fn length(&self) -> Option<i64> {
        let window = self.window?;
        Some(window.resets.at - window.starts_at?)
    }

    /// Whether its name is plural, for the words beside it to agree with:
    /// money's whose name ends in `s`, as Credits.
    pub fn plural(&self) -> bool {
        self.money.is_some() && self.name.ends_with('s')
    }

    /// Its name as a status line shows it: "5h", "weekly", "opus weekly".
    pub fn short_name(&self) -> String {
        self.lowercase("h")
    }

    /// Whether `name` names it: part of its name as shown, with its models
    /// after it as the command line shows them or before it as the app
    /// does, as the status line shows it, or part of its key. So `week`
    /// names Weekly and `seven_day`, `5h` names 5 hours, and `weekly
    /// (claude` and `gpt weekly` name Weekly (Claude and GPT). Of the limits
    /// a name fits, the first by key is taken: `gpt` alone is 5 hours
    /// (Claude and GPT).
    pub fn answers_to(&self, name: &str) -> bool {
        let name = name.to_lowercase();
        let holds = |text: &str| text.to_lowercase().contains(&name);
        holds(&self.named())
            || holds(&self.short_name())
            || holds(&self.key)
            || self
                .scope
                .as_ref()
                .is_some_and(|scope| holds(&format!("{scope} {}", self.name)))
    }

    pub fn standing(&self) -> Standing {
        match self.outlook {
            Outlook::UsedUp { .. } => Standing::UsedUp,
            Outlook::RunsOut { .. } => Standing::RunningOut,
            Outlook::Lasts { .. } | Outlook::Unknown { .. } => Standing::Lasts,
        }
    }

    /// When it's back, if it's used up. One with no reset, as credits, is
    /// back at no time that's known, so it counts as later than any: its
    /// account is never said to be back at a time it may still be used up.
    pub fn back(&self) -> Option<i64> {
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
        let at = time::round_minute(at);
        let horizon = if at - now < 3 * HOUR {
            Horizon::Soon
        } else {
            // By local calendar days, not 7 * 24 hours: right after a weekly
            // reset, the next reset is under seven days away on today's
            // weekday, so naming the weekday would read as today.
            match (time::local(at).date() - time::local(now).date()).get_days() {
                0 => Horizon::Today,
                1..7 => Horizon::ThisWeek,
                _ => Horizon::Later,
            }
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
    /// On one of the next six days, by local calendar days.
    ThisWeek,
    /// Seven or more local calendar days ahead.
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

/// The work rate on a limit, and the work left at it: by the hours agents
/// here worked on its account, not the clock.
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
    /// down; `None` with nothing used yet.
    #[serde(rename_all = "camelCase")]
    Lasts { left_at_reset: Option<Spread> },
    /// It runs out before its reset: likely then, and soonest at the fast
    /// end. Each is rounded to 10 minutes within a day and to the hour
    /// beyond, and is never in the past.
    #[serde(rename_all = "camelCase")]
    RunsOut { likely: Moment, soonest: Moment },
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
    /// Too little of the window has passed or been used to tell a pace, or
    /// its pace runs it out more than a quarter of the window away, too far
    /// to tell (`forecast::told`, docs/architecture.md#forecast).
    NotEnoughData,
    /// Read too long ago, or its window reset since.
    Stale,
    /// It never resets, as credits.
    NoReset,
    /// Its window rolls, with no start to pace from.
    Rolling,
}

/// Percent left at the reset, likely and at the fast end, each rounded down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spread {
    /// At the fast end.
    pub low: u8,
    pub likely: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spend {
    /// What it cost so far this calendar month.
    pub month_usd: f64,
    /// The whole month at its pace so far, to two figures, as $62 or $3.40;
    /// None in its first day.
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
    // Between readings it changes at most once a minute.
    let now = time::minute(now);
    let mut signed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut keys: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut rows = db.prepare(&format!(
        "SELECT s.agent, s.account, s.key FROM sign_in s
         WHERE {} AND s.account IS NOT NULL",
        crate::limits::SIGNED_IN_NOW
    ))?;
    for row in rows.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))? {
        let (agent, account, key): (String, String, Option<String>) = row?;
        let agents = signed.entry(account).or_default();
        if !agents.contains(&agent) {
            agents.push(agent.clone());
        }
        if let Some(key) = key {
            let agents = keys.entry(crate::providers::key_limit(&key)).or_default();
            if !agents.contains(&agent) {
                agents.push(agent);
            }
        }
    }
    let mut used = drawn(db, now)?;

    let mut accounts = Vec::new();
    let mut rows = db.prepare(&format!(
        "SELECT id, provider, kind, title, label, hidden, read_at, problem,
             EXISTS (SELECT 1 FROM response WHERE account = a.id AND at >= ?2),
             (SELECT reads FROM failing WHERE account = a.id),
             full_read_at, checked_at, {}
         FROM account a",
        Account::in_use_sql()
    ))?;
    let mut rows = rows.query([now, now - RECENT])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let read_at: Option<i64> = row.get(6)?;
        let problem: Option<String> = row.get(7)?;
        let failed: Option<i64> = row.get(9)?;
        let full_read_at: Option<i64> = row.get(10)?;
        let checked_at: Option<i64> = row.get(11)?;
        // Readings an agent wrote after the read that failed, as Codex does
        // as it works, are current (`limits::record_in_band`).
        let problem = problem.filter(|_| {
            checked_at.is_none_or(|checked| read_at.is_none_or(|read| read <= checked))
        });
        let agents = signed.remove(&id).unwrap_or_default();
        let why = match problem.as_deref() {
            _ if agents.is_empty() => Some(Stale::SignedOut),
            Some("signIn") if failed.unwrap_or(0) >= 2 => Some(Stale::SignIn),
            Some("expired") => Some(Stale::Expired),
            Some(_) => Some(Stale::ReadFailed),
            None => None,
        };
        let state = match (read_at, why) {
            (Some(read_at), None) if now - read_at <= STALE => State::Live { read_at },
            (Some(read_at), why) => State::AsOf {
                read_at,
                why: why.unwrap_or(Stale::ReadFailed),
                worked_hours: tenths(forecast::hours(
                    &worked(db, &id, &[], None, read_at)?,
                    read_at,
                    now,
                )),
            },
            (None, why) => State::Unread {
                why: why.unwrap_or(Stale::ReadFailed),
            },
        };
        let limits = match read_at {
            Some(read_at) => limits(db, &id, read_at, full_read_at, &keys, now)?,
            None => Vec::new(),
        };
        let kind: Kind = row.get(2)?;
        let in_use = row.get(12)?;
        accounts.push(Account {
            spend: (kind == Kind::ApiKey)
                .then(|| spend(db, &id, now))
                .transpose()?,
            deciding: deciding(&limits, pool(db, &id, &limits)?).map(|limit| limit.key.clone()),
            in_use,
            recent: in_use || row.get(8)?,
            provider: row.get(1)?,
            title: row.get(3)?,
            label: row.get(4)?,
            hidden: row.get(5)?,
            used_by: used.remove(&id).unwrap_or_default(),
            id,
            kind,
            agents,
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

/// Every agent, by `in_use`, whether an agent by id is in use. Kept out of
/// `status`, as it reads each agent's MCP settings: with a made-up 5.8 MB
/// `~/.claude.json`, `statusline` ran 183M instructions and peaked at 38 MB
/// with this in `status`, and 42M and 8 MB without.
pub fn agents(home: &Path, engine: &Path, in_use: impl Fn(&str) -> bool) -> Vec<Agent> {
    agents::ALL
        .iter()
        .map(|agent| {
            let info = agent.info();
            let (connection, configured) = crate::connect::connection(*agent, home, engine);
            Agent {
                id: info.id.to_owned(),
                name: info.name.to_owned(),
                installed: agent
                    .folders(home)
                    .first()
                    .is_some_and(|folder| agent.installed(folder)),
                connection,
                configured,
                in_use: in_use(info.id),
            }
        })
        .collect()
}

/// Whether the agent `id` drew on any of `accounts` in the last half hour.
pub fn in_use(accounts: &[Account], id: &str) -> bool {
    accounts
        .iter()
        .any(|account| account.used_by.iter().any(|by| by == id))
}

/// Another live account with room, to name where `current` runs out or is
/// used up, and its deciding limit: one `agent` is signed in to first.
pub fn alternate<'a>(
    accounts: &'a [Account],
    current: &Account,
    agent: Option<&str>,
) -> Option<(&'a Account, &'a Limit)> {
    roomy(accounts)
        .filter(|(account, _)| account.id != current.id)
        // Of the best, the first listed.
        .min_by_key(|(account, limit)| {
            Reverse((
                signed_in(account, agent),
                account.kind != Kind::ApiKey,
                limit.left_percent,
            ))
        })
}

/// The live account with the most left of its deciding limit, and that
/// limit: of those as roomy, one `agent` is signed in to first. Given
/// `hours` of work, one they don't fit is left out, and one they fit comes
/// before one that can't be told.
fn roomiest<'a>(
    accounts: &'a [Account],
    agent: Option<&str>,
    hours: Option<f64>,
    now: i64,
) -> Option<(&'a Account, &'a Limit)> {
    roomy(accounts)
        .map(|(account, limit)| {
            let fits = hours.map(|hours| text::judge(account, hours, now).0);
            (account, limit, fits)
        })
        .filter(|(_, _, fits)| *fits != Some(text::Fits::No))
        .min_by_key(|(account, limit, fits)| {
            Reverse((
                *fits,
                limit.left_percent,
                signed_in(account, agent),
                account.kind != Kind::ApiKey,
            ))
        })
        .map(|(account, limit, _)| (account, limit))
}

/// The accounts shown and read now whose deciding limit has room: a
/// quarter or more left, and neither running out nor used up.
fn roomy(accounts: &[Account]) -> impl Iterator<Item = (&Account, &Limit)> {
    accounts
        .iter()
        .filter(|account| !account.hidden && matches!(account.state, State::Live { .. }))
        .filter_map(|account| {
            let limit = account.deciding_limit()?;
            (limit.left_percent >= 25 && limit.standing() == Standing::Lasts)
                .then_some((account, limit))
        })
}

fn signed_in(account: &Account, agent: Option<&str>) -> bool {
    agent.is_some_and(|id| account.agents.iter().any(|by| by == id))
}

/// The agents that drew on any account in the last half hour, from the
/// database alone: for `turnscope connect`, which works out no status.
pub fn agents_in_use(db: &rusqlite::Connection, now: i64) -> Result<Vec<String>> {
    Ok(drawn(db, time::minute(now))?
        .into_values()
        .flatten()
        .collect())
}

/// The agents that drew on each account in the last half hour, by
/// account, the latest first.
fn drawn(db: &rusqlite::Connection, now: i64) -> Result<BTreeMap<String, Vec<String>>> {
    let mut used: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut rows = db.prepare(
        "SELECT account, agent FROM response WHERE at >= ?1 AND account IS NOT NULL
         GROUP BY account, agent ORDER BY max(at) DESC",
    )?;
    for row in rows.query_map([now - IN_USE], |row| Ok((row.get(0)?, row.get(1)?)))? {
        let (account, agent): (String, String) = row?;
        used.entry(account).or_default().push(agent);
    }
    Ok(used)
}

/// How far back a limit's readings, and its account's work, are looked at
/// for the work rate.
pub(crate) const WORKED: i64 = 14 * DAY;

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

/// A limit as last read, and its readings, oldest first, from at least its
/// window's start and two weeks back.
struct Latest {
    key: String,
    at: i64,
    name: String,
    scope: Option<String>,
    used: f64,
    size: Option<f64>,
    starts: Option<i64>,
    resets: Option<i64>,
    history: Vec<forecast::Reading>,
}

/// `account`'s limits as last read since `since`, by key.
fn latest(db: &rusqlite::Connection, account: &str, since: i64, now: i64) -> Result<Vec<Latest>> {
    // Each limit's newest row, kept by key. Chosen over `GROUP BY
    // limit_key`, which sorts every row: with 90 days of made-up readings
    // of 11 limits and no `full_read_at`, a status took 144 ms that way and
    // 57 ms this way.
    let mut latest: Vec<Latest> = Vec::new();
    let mut rows = db.prepare_cached(
        "SELECT limit_key, at, name, scope, used, size, starts, resets FROM reading
         WHERE account = ?1 AND at >= ?2 ORDER BY at DESC",
    )?;
    let mut rows = rows.query(params![account, since])?;
    while let Some(row) = rows.next()? {
        let key = row.get_ref(0)?.as_str()?;
        if let Err(place) = latest.binary_search_by(|limit| limit.key.as_str().cmp(key)) {
            let limit = Latest {
                key: key.to_owned(),
                at: row.get(1)?,
                name: row.get(2)?,
                scope: row.get(3)?,
                used: row.get(4)?,
                size: row.get(5)?,
                starts: row.get(6)?,
                resets: row.get(7)?,
                history: Vec::new(),
            };
            latest.insert(place, limit);
        }
    }
    // Every limit's readings at once. Chosen over a query per limit: the key
    // is last in `reading`'s primary key, so each such query reads every
    // limit's rows. With five limits read every 5 minutes for 30 days, made
    // up, that took 6.6 ms an account, and this 2.7 ms.
    let from = latest.iter().fold(now - WORKED, |from, limit| {
        from.min(limit.starts.unwrap_or(limit.at - DAY))
    });
    let mut rows = db.prepare_cached(
        "SELECT limit_key, at, used, starts, resets FROM reading
         WHERE account = ?1 AND at >= ?2 ORDER BY at",
    )?;
    let mut rows = rows.query(params![account, from])?;
    while let Some(row) = rows.next()? {
        let key = row.get_ref(0)?.as_str()?;
        if let Ok(place) = latest.binary_search_by(|limit| limit.key.as_str().cmp(key)) {
            latest[place].history.push(forecast::Reading {
                at: row.get(1)?,
                used: row.get(2)?,
                starts: row.get(3)?,
                resets: row.get(4)?,
            });
        }
    }
    Ok(latest)
}

/// An account's limits, each as last read since its last read in full,
/// `full_read_at`, or ever with none: an agent writes only the limits that
/// moved (`limits::record_in_band`).
fn limits(
    db: &rusqlite::Connection,
    account: &str,
    read_at: i64,
    full_read_at: Option<i64>,
    keys: &BTreeMap<String, Vec<String>>,
    now: i64,
) -> Result<Vec<Limit>> {
    let since = full_read_at.map_or(i64::MIN, |full| full.min(read_at));
    let latest = latest(db, account, since, now)?;
    if latest.is_empty() {
        return Ok(Vec::new());
    }
    let known: Option<i64> = db.query_row(
        "SELECT min(since) FROM sign_in WHERE account = ?1",
        [account],
        |row| row.get(0),
    )?;
    // The slots of work, by the models and key whose responses count.
    let mut slots: BTreeMap<(Vec<String>, Option<String>), Vec<i64>> = BTreeMap::new();
    let mut limits = Vec::new();
    for limit in latest {
        let reset_since = limit.resets.filter(|resets| *resets <= now);
        let left = if reset_since.is_some() {
            100.0
        } else {
            (100.0 - limit.used).clamp(0.0, 100.0)
        };
        let work = work(db, account, &limit, left, known, &mut slots, now)?;
        let Latest {
            key,
            at: limit_read_at,
            name,
            scope,
            used,
            size,
            starts,
            resets,
            history,
        } = limit;
        // Its current window's: those that reset with it.
        let readings: Vec<(i64, f64)> = history
            .iter()
            .filter(|reading| {
                reading.at >= starts.unwrap_or(limit_read_at - DAY)
                    && forecast::same_reset(resets, reading.resets)
            })
            .map(|reading| (reading.at, reading.used))
            .collect();
        let stale = reset_since.is_some() || now - limit_read_at > STALE;
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
        limits.push(Limit {
            held_by: keys.get(&key).cloned().unwrap_or_default(),
            key,
            name,
            scope,
            window: resets.map(|resets| LimitWindow {
                starts_at: starts,
                resets: Moment::of(resets, now),
            }),
            read_at: limit_read_at,
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
    Ok(limits)
}

/// The work rate on `limit`, and the work left at it with `left` percent
/// left. None for a window that rolls, which has no start to measure work
/// from.
fn work(
    db: &rusqlite::Connection,
    account: &str,
    limit: &Latest,
    left: f64,
    known: Option<i64>,
    slots: &mut BTreeMap<(Vec<String>, Option<String>), Vec<i64>>,
    now: i64,
) -> Result<Option<Work>> {
    let Latest {
        key,
        scope,
        starts,
        resets,
        history,
        ..
    } = limit;
    if starts.is_none() && resets.is_some() {
        return Ok(None);
    }
    // By the work on its own models or key: by all the account's, 1h of Opus
    // that rose Weekly (Opus) 8 points after 3h of Sonnet read as 2% an hour.
    let counted = (
        crate::usage::models(scope.as_deref()),
        crate::providers::key_limit_fingerprint(key).map(str::to_owned),
    );
    let slots = match slots.entry(counted) {
        Entry::Occupied(slots) => slots.into_mut(),
        Entry::Vacant(slot) => {
            let (models, key) = slot.key();
            let found = worked(db, account, models, key.as_deref(), now - WORKED)?;
            slot.insert(found)
        }
    };
    // Work is known only for the last two weeks: a rise before then would
    // count with no hours of work beside it.
    let walked = &history[history.partition_point(|reading| reading.at < now - WORKED)..];
    // Not `Limit::length`, whose reset is rounded to the minute.
    let length = starts.zip(*resets).map(|(starts, resets)| resets - starts);
    let Some(rate) = forecast::work_rate(walked, slots, known, forecast::enough_work(length))
    else {
        return Ok(None);
    };
    let until = resets.and_then(|resets| hours_to_reset(*starts, resets, resets <= now, now));
    let left_hours = forecast::work_left(100.0 - left, rate, forecast::work_band(length), until)
        .map(|(low, likely, high)| HoursLeft {
            low: tenths(low),
            likely: tenths(likely),
            high: tenths(high),
        });
    Ok(Some(Work {
        rate_per_hour: tenths(rate),
        left_hours,
    }))
}

fn tenths(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// The start of each slot of work on `account` from `since`, by the
/// responses that count toward a limit on `models` or `key`
/// (`usage::limited`).
///
/// One seek in `response_account` per slot, over grouping every response:
/// with a made-up 233,000 responses in two weeks, a status took 80 ms
/// grouping and 12 ms seeking; with 2,000 over 1,194 slots, 1.5 ms grouping
/// and 4.3 ms seeking.
pub(crate) fn worked(
    db: &rusqlite::Connection,
    account: &str,
    models: &[String],
    key: Option<&str>,
    since: i64,
) -> Result<Vec<i64>> {
    let (limited, values) = crate::usage::limited(models, key);
    let sql = format!(
        "WITH RECURSIVE slot (at) AS (
             SELECT min(r.at) FROM response r WHERE r.account = ? AND r.at >= ? AND {limited}
             UNION ALL
             SELECT (SELECT min(r.at) FROM response r
                     WHERE r.account = ? AND r.at >= (slot.at / ? + 1) * ? AND {limited})
             FROM slot WHERE slot.at IS NOT NULL
         )
         SELECT at FROM slot WHERE at IS NOT NULL"
    );
    let mut given: Vec<rusqlite::types::Value> = vec![account.to_owned().into(), since.into()];
    given.extend(values.iter().cloned());
    given.extend([
        account.to_owned().into(),
        forecast::SLOT.into(),
        forecast::SLOT.into(),
    ]);
    given.extend(values);
    Ok(db
        .prepare_cached(&sql)?
        .query_map(rusqlite::params_from_iter(given), |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn outlook(window: &Window, now: i64) -> Outlook {
    let Some(projection) = forecast::project(window, now) else {
        return Outlook::Unknown {
            reason: Unknown::NotEnoughData,
        };
    };
    let fast = forecast::fast(window);
    let margin = ((window.resets - window.starts) / 50).max(15 * MINUTE);
    let meaningful = |at: i64| at + margin < window.resets;
    if let Some(out) = projection.runs_out.filter(|at| meaningful(*at)) {
        if !forecast::told(window, now, out) {
            return Outlook::Unknown {
                reason: Unknown::NotEnoughData,
            };
        }
        // The fast end runs out first, as every fast end is over 1.
        let soonest = forecast::runs_out(&projection, fast);
        // A pace can't tell the minute a week runs out.
        let shown = |at: i64| {
            let step = if at - now <= DAY { 10 * MINUTE } else { HOUR };
            ((at + step / 2).div_euclid(step) * step).max(now)
        };
        let likely = shown(out);
        return Outlook::RunsOut {
            likely: Moment::of(likely, now),
            soonest: Moment::of(shown(soonest.unwrap_or(out)).min(likely), now),
        };
    }
    // Only one with nothing used says nothing of the reset: one whose use was
    // given back, with none since, lasts with what it has.
    if projection.used <= 0.0 {
        return Outlook::Lasts {
            left_at_reset: None,
        };
    }
    let least = forecast::at_reset(&projection, fast);
    let left = |used: f64| (100.0 - used).clamp(0.0, 100.0).floor() as u8;
    Outlook::Lasts {
        left_at_reset: Some(Spread {
            low: left(least),
            likely: left(projection.at_reset),
        }),
    }
}

/// The limit that decides how an account stands, of those in `pool` where
/// it draws on one. Of several used up, the last back decides: the account
/// can't be used until every one is back.
fn deciding<'a>(limits: &'a [Limit], pool: Option<&str>) -> Option<&'a Limit> {
    let standing: Vec<&Limit> = limits
        .iter()
        .filter(|limit| pool.is_none_or(|pool| limit.scope.as_deref() == Some(pool)))
        .collect();
    standing
        .iter()
        .copied()
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
                Reverse(limit.standing()),
                Reverse(limit.back()),
                limit.runs_out().unwrap_or(i64::MAX),
                least,
            )
        })
        .or_else(|| standing.into_iter().max_by_key(|limit| limit.reset_since))
}

/// The pool an account stands by when each of its limits is on some models
/// alone, as Google AI's Gemini and Claude and GPT pools: the one its latest
/// response drew on, or else the first, which its provider lists as the
/// plan's own. Chosen over the most urgent pool, which said an account
/// working on Gemini was used up when only Claude and GPT were.
fn pool<'a>(
    db: &rusqlite::Connection,
    account: &str,
    limits: &'a [Limit],
) -> Result<Option<&'a str>> {
    if limits.iter().any(Limit::whole) {
        return Ok(None);
    }
    let mut pools: Vec<&str> = Vec::new();
    for limit in limits.iter().filter(|limit| !limit.is_key()) {
        if let Some(scope) = limit.scope.as_deref()
            && !pools.contains(&scope)
        {
            pools.push(scope);
        }
    }
    for &pool in &pools {
        let (limited, mut values) = crate::usage::limited(&crate::usage::models(Some(pool)), None);
        values.push(account.to_owned().into());
        let drew: Option<bool> = db
            .prepare_cached(&format!(
                "SELECT {limited} FROM response r WHERE r.account = ? ORDER BY r.at DESC LIMIT 1"
            ))?
            .query_row(rusqlite::params_from_iter(values), |row| row.get(0))
            .optional()?;
        if drew == Some(true) {
            return Ok(Some(pool));
        }
    }
    Ok(pools.first().copied())
}

/// The order every front end lists accounts in. Until the first read of
/// history ends, as on a first launch, none is known to be in use, so each
/// is ranked as one in use: the app shows them so, and ranked as not, the
/// one with the most left came first. `in_use` stays as measured, so no
/// alert is raised on a guess.
fn rank(accounts: &mut [Account], history_read: bool) {
    accounts.sort_by_cached_key(|account| {
        let in_use = account.in_use || !history_read;
        let deciding = account.deciding_limit();
        let left = deciding.map_or(i32::MAX, |limit| i32::from(limit.left_percent));
        let group = if in_use {
            0
        } else if account.refused() {
            2
        } else if deciding.is_some() {
            1
        } else {
            3
        };
        let urgent = deciding.filter(|_| in_use);
        (
            group,
            Reverse(urgent.map_or(Standing::Lasts, Limit::standing)),
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
    let forecast = (elapsed >= DAY).then(|| two_figures(cost * whole / elapsed as f64));
    Ok(Spend {
        month_usd: cost,
        forecast_usd: forecast,
        partial: unpriced > 0,
    })
}

/// To two figures, as a forecast is known: $62, $3.40, $0.85. Chosen over
/// cents, which change every minute and the status with them: with $60
/// spent on an API key and nothing new read, the engine told 60 statuses an
/// hour on the 2nd and 37 on the 15th in cents, and 1 and 0 in two figures.
fn two_figures(usd: f64) -> f64 {
    if usd <= 0.0 {
        return 0.0;
    }
    // Places kept after the point: 1 for $3.40, -1 for $2,500. Scaled by an
    // exact power of ten, so $3.40 is 3.4, not 3.4000000000000004.
    let kept = 1 - usd.log10().floor() as i32;
    if kept >= 0 {
        let scale = 10f64.powi(kept);
        (usd * scale).round() / scale
    } else {
        let unit = 10f64.powi(-kept);
        (usd / unit).round() * unit
    }
}

/// The one account `name` names, or why it names more than one.
pub fn one_account<'a>(status: &'a Status, name: &str) -> crate::Result<&'a Account> {
    match named(status, name)?.as_slice() {
        [one] => Ok(one),
        many => Err(crate::Error::Usage(format!(
            "{name} names {} accounts: {}. Name one by more of its title or label, or by its id.",
            many.len(),
            format_accounts(many.iter().copied())
        ))),
    }
}

fn format_accounts<'a>(accounts: impl IntoIterator<Item = &'a Account>) -> String {
    accounts
        .into_iter()
        .map(|account| format!("{} ({})", account.full_title(), account.id))
        .collect::<Vec<_>>()
        .join(", ")
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
        return Err(crate::Error::NotFound(match status.accounts.is_empty() {
            true => NONE_READ.to_owned(),
            false => format!(
                "no account is named {name}; there are: {}",
                format_accounts(&status.accounts)
            ),
        }));
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test::response;

    fn example() -> Status {
        serde_json::from_str(include_str!("../tests/fixtures/status.json")).unwrap()
    }

    #[test]
    fn a_limit_answers_to_its_name_as_shown_and_its_key() {
        let mut account = example().accounts[0].clone();
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
        let example = example();
        let now = example.at;
        let mut account = example.accounts[0].clone();
        let back = |hours: i64| Outlook::UsedUp {
            back: Some(Moment::of(now + hours * HOUR, now)),
        };
        let never = Outlook::UsedUp { back: None };
        let decides = |account: &Account| deciding(&account.limits, None).unwrap().key.clone();
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
            fit(&account, 1.0, now),
            format!(
                "1 more hour of work doesn't fit: Weekly is used up, back {}.",
                time::ahead(now + 72 * HOUR, now)
            )
        );
        // One with no reset is back at no known time, wherever it's listed.
        account.limits[1].outlook = never.clone();
        assert_eq!(decides(&account), "seven_day");
        (account.limits[0].outlook, account.limits[1].outlook) = (never, back(72));
        assert_eq!(decides(&account), "five_hour");
    }

    #[test]
    fn an_account_of_pools_stands_by_the_one_it_last_drew_on() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).unwrap();
        let now = 2 * HOUR;
        db.execute_batch(&format!(
            "INSERT INTO account (id, provider, kind, title, read_at, full_read_at)
                 VALUES ('google:g', 'google', 'subscription', 'Google AI Ultra', {now}, {now});
             INSERT INTO sign_in (agent, folder, provider, since, account)
                 VALUES ('antigravity', '/g', 'google', 0, 'google:g');
             INSERT INTO reading (account, limit_key, at, name, scope, used, starts, resets) VALUES
                 ('google:g', '5h', {now}, '5 hours', 'Gemini', 5.0, 0, {five}),
                 ('google:g', 'weekly:3p-weekly', {now}, 'Weekly', 'Claude and GPT', 100.0, 0, {week});",
            five = 5 * HOUR,
            week = 7 * DAY,
        ))
        .unwrap();
        let google = ("antigravity", "/g", "google", Some("google:g"));
        let drew = |id: &str, at: i64, model: &str| response(&db, google, id, at, model, None);
        let shown = |db: &rusqlite::Connection| status(db, now).unwrap().accounts.remove(0);
        // Drawn on by no response: by Gemini's, listed first, though Claude
        // and GPT are used up.
        let account = shown(&db);
        assert_eq!(account.deciding.as_deref(), Some("5h"));
        assert!(!verdict(&account, now).contains("used up"));
        assert!(
            line(&account, now).starts_with(
                "Google AI Ultra · Gemini 5h 95% · Claude and GPT weekly used up, back"
            )
        );
        // Working on Claude, its pool is used up: so is the account for it.
        drew("r1", now - MINUTE, "claude-opus-5-5");
        let account = shown(&db);
        assert_eq!(account.deciding.as_deref(), Some("weekly:3p-weekly"));
        assert!(
            verdict(&account, now)
                .starts_with("Google AI Ultra: Weekly (Claude and GPT) is used up, back"),
        );
        assert!(fit(&account, 1.0, now).starts_with(
            "1 more hour of work doesn't fit: Weekly (Claude and GPT) is used up, back"
        ));
        // Back on Gemini.
        drew("r2", now, "gemini-3.8-flash");
        assert_eq!(shown(&db).deciding.as_deref(), Some("5h"));
    }

    /// An account and its sign-in, read at `now`, with `readings` of its
    /// limit `five_hour`, a window from 0 to 5 hours: when, and percent used.
    fn read(now: i64, kind: &str, readings: &[(i64, f64)]) -> Status {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).unwrap();
        db.execute_batch(&format!(
            "INSERT INTO account (id, provider, kind, title, read_at)
                 VALUES ('anthropic:a', 'anthropic', '{kind}', 'Claude', {now});
             INSERT INTO sign_in (agent, folder, provider, since, account)
                 VALUES ('claude-code', '/a', 'anthropic', 0, 'anthropic:a');"
        ))
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

    /// Three hours of Sonnet, then one of Opus, over which Weekly (Opus)
    /// rose 8 points: 8% an hour of work, not 2%.
    #[test]
    fn a_limit_on_one_model_is_worked_by_that_models_responses() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).unwrap();
        let now = 1_792_065_600_000;
        let start = now - 4 * HOUR;
        db.execute_batch(&format!(
            "INSERT INTO account (id, provider, kind, title, read_at)
                 VALUES ('anthropic:a', 'anthropic', 'subscription', 'Claude Max', {now});
             INSERT INTO sign_in (agent, folder, provider, since, account)
                 VALUES ('claude-code', '/a', 'anthropic', 0, 'anthropic:a');
             INSERT INTO reading (account, limit_key, at, name, scope, used, starts, resets) VALUES
                 ('anthropic:a', 'seven_day_opus', {start}, 'Weekly', 'Opus', 10.0, {starts}, {resets}),
                 ('anthropic:a', 'seven_day_opus', {now}, 'Weekly', 'Opus', 18.0, {starts}, {resets});",
            starts = start - DAY,
            resets = start + 6 * DAY,
        ))
        .unwrap();
        let claude = ("claude-code", "/a", "anthropic", Some("anthropic:a"));
        for slot in 0..48 {
            let model = if slot < 36 {
                "claude-sonnet-5"
            } else {
                "claude-opus-5"
            };
            let at = start + slot * forecast::SLOT;
            response(&db, claude, &format!("r{slot}"), at, model, None);
        }
        let status = status(&db, now).unwrap();
        let work = status.accounts[0].limits[0].work.unwrap();
        assert_eq!(work.rate_per_hour, 8.0);
    }

    /// Its cents would change every minute, and the status with them.
    #[test]
    fn the_months_forecast_has_two_figures_and_holds_for_the_hour() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).unwrap();
        // $51.50 by 2026-10-15 12:00 UTC forecasts between $105 and $115
        // for the month, in every time zone from UTC-12 to UTC+14.
        let now = 1_792_065_600_000;
        let pi = ("pi", "f", "openrouter", Some("openrouter:api"));
        response(&db, pi, "r", now - HOUR, "m", Some(51.5));
        for minute in 0..60 {
            let spend = spend(&db, "openrouter:api", now + minute * MINUTE).unwrap();
            assert_eq!(spend.forecast_usd, Some(110.0), "minute {minute}");
        }
        for (usd, figures) in [(62.37, 62.0), (3.404, 3.4), (0.849, 0.85), (2469.0, 2500.0)] {
            assert_eq!(two_figures(usd), figures);
        }
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
        // From a thousand, dollars have a comma, as on the status line. A
        // forecast has two figures, so no cents from $10.
        let mut account = account.clone();
        account.spend = Some(Spend {
            month_usd: 1234.5,
            forecast_usd: Some(2500.0),
            partial: false,
        });
        let said = text(&status, &[&account], None);
        assert!(
            said.ends_with("This month, across its keys: $1,234.50, on pace for about $2,500."),
            "{said}"
        );
        // At the month's end, the forecast is what was spent.
        account.spend = Some(Spend {
            month_usd: 62.37,
            forecast_usd: Some(62.0),
            partial: false,
        });
        let said = text(&status, &[&account], None);
        assert!(
            said.ends_with("This month, across its keys: $62.37."),
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

    #[test]
    fn a_time_on_next_weeks_same_weekday_is_later() {
        // Noon on 2026-10-15 in the local time zone, clear of DST changes.
        let noon = time::start_of(time::Period::Day, 1_792_065_600_000) + 12 * HOUR;
        // Two hours after a weekly reset, the next reset is 6d 22h away: under
        // 7 * DAY in milliseconds, but 7 calendar days ahead on today's weekday.
        assert_eq!(
            Moment::of(noon + 7 * DAY - 2 * HOUR, noon).horizon,
            Horizon::Later
        );
        assert_eq!(
            Moment::of(noon + 6 * DAY + 6 * HOUR, noon).horizon,
            Horizon::ThisWeek
        );
        assert_eq!(Moment::of(noon + 4 * HOUR, noon).horizon, Horizon::Today);
        assert_eq!(Moment::of(noon + 2 * HOUR, noon).horizon, Horizon::Soon);
    }

    #[test]
    fn limits_an_agent_wrote_show_beside_those_it_did_not() {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).unwrap();
        let full_at = 2 * HOUR;
        let in_band_at = full_at + 10 * MINUTE;
        db.execute(
            "INSERT INTO account (id, provider, kind, title, read_at, full_read_at)
             VALUES ('anthropic:a', 'anthropic', 'subscription', 'Claude Max', ?1, ?2)",
            params![in_band_at, full_at],
        )
        .unwrap();
        db.execute(
            "INSERT INTO sign_in (agent, folder, provider, since, account)
             VALUES ('claude-code', '/a', 'anthropic', 0, 'anthropic:a')",
            [],
        )
        .unwrap();
        // A read of all of them at `full_at`; the agent wrote the plan's own
        // at `in_band_at`.
        db.execute(
            "INSERT INTO reading (account, limit_key, at, name, scope, used, starts, resets) VALUES
                 ('anthropic:a', 'five_hour', ?1, '5 hours', NULL, 20.0, 0, ?3),
                 ('anthropic:a', 'seven_day_opus', ?1, 'Weekly', 'Opus', 15.0, 0, ?4),
                 ('anthropic:a', 'five_hour', ?2, '5 hours', NULL, 30.0, 0, ?3)",
            params![full_at, in_band_at, 5 * HOUR, 7 * DAY],
        )
        .unwrap();
        let shown = status(&db, in_band_at).unwrap();
        let limits = &shown.accounts[0].limits;
        assert_eq!(limits.len(), 2);
        assert_eq!(limits[0].key, "five_hour");
        assert_eq!(limits[0].left_percent, 70);
        assert_eq!(limits[0].read_at, in_band_at);
        assert_eq!(limits[1].key, "seven_day_opus");
        assert_eq!(limits[1].left_percent, 85);
        assert_eq!(limits[1].read_at, full_at);
    }
}
