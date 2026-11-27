//! Every word the app shows: the panel, the menu bar item, notifications and what VoiceOver
//! says. Times are in the Mac's own time zone, as of the minute the panel last ticked.

use jiff::ZonedDateTime;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::Serialize;
use turnscope::alerts::{Alert, Kind as AlertKind, replaces};
use turnscope::engine::{Stopped, UsedMost};
use turnscope::status::{
    Account, Agent, Connection, Horizon, Limit, Moment, Outlook, Spend, Stale, Standing, State,
    Unknown, agent_names, list,
};
use turnscope::usage::counted;

/// A notification, as the shell posts it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertNote {
    pub id: i64,
    pub account: String,
    pub replaces: String,
    pub sign_in: bool,
    pub title: String,
    pub subtitle: String,
    pub body: String,
}

pub struct Words {
    pub(crate) now: i64,
    tz: TimeZone,
    today: Date,
}

impl Words {
    pub fn new(now_ms: i64) -> Words {
        Self::in_zone(now_ms, TimeZone::system())
    }

    fn in_zone(now_ms: i64, tz: TimeZone) -> Words {
        let now = turnscope::time::minute(now_ms);
        let today = turnscope::time::zoned(now, &tz).date();
        Words { now, tz, today }
    }

    fn zoned(&self, ms: i64) -> ZonedDateTime {
        turnscope::time::zoned(ms, &self.tz)
    }

    /// Past is "0m": a status can be minutes old, as only one that changed is told.
    fn until(&self, at: i64) -> String {
        turnscope::time::span((at - self.now).max(0))
    }

    fn at(&self, moment: &Moment) -> String {
        self.at_horizon(moment.at, moment.horizon, false)
    }

    fn at_horizon(&self, at: i64, horizon: Horizon, full: bool) -> String {
        match horizon {
            Horizon::Soon => format!("in {}", self.until(at)),
            Horizon::Today => fmt_time(&self.zoned(at)),
            Horizon::ThisWeek => {
                let z = self.zoned(at);
                format!("{} {}", self.day(&z, full), fmt_time(&z))
            }
            // Said as nearer days are, with the date that tells it from this
            // week's: "Monday, January 25, 9 AM" beside "Monday 10 AM".
            Horizon::Later if full => {
                let z = self.zoned(at);
                format!(
                    "{}, {}, {}",
                    fmt_weekday(&z, true),
                    fmt_month_day(&z, true),
                    fmt_time(&z)
                )
            }
            Horizon::Later => fmt_month_day(&self.zoned(at), false),
        }
    }

    /// Clock time rather than countdown, for persistent notifications.
    fn clock(&self, moment: &Moment, full: bool) -> String {
        let horizon = match moment.horizon {
            Horizon::Soon if self.zoned(moment.at).date() == self.today => Horizon::Today,
            Horizon::Soon => Horizon::ThisWeek,
            other => other,
        };
        self.at_horizon(moment.at, horizon, full)
    }

    fn ago(&self, ms: i64) -> String {
        let z = self.zoned(ms);
        let date = z.date();
        let time = fmt_time(&z);
        if date == self.today {
            return time;
        }
        if self.today.yesterday().ok() == Some(date) {
            return format!("yesterday {time}");
        }
        if ms < self.now && self.now - ms < 6 * 86_400_000 {
            return format!("{} {time}", fmt_weekday(&z, false));
        }
        fmt_month_day(&z, false)
    }

    /// "tomorrow", or the weekday of a day later than that.
    fn day(&self, z: &ZonedDateTime, full: bool) -> String {
        if self.today.tomorrow().ok() == Some(z.date()) {
            "tomorrow".into()
        } else {
            fmt_weekday(z, full)
        }
    }

    fn when(&self, ms: i64) -> String {
        let zoned = self.zoned(ms);
        let part = match zoned.hour() {
            12 if zoned.minute() < 30 => "around noon",
            5..=11 => "morning",
            12..=16 => "afternoon",
            17..=21 => "evening",
            _ => "night",
        };
        // Before 5 AM is the night of the date before. By the date, not 24
        // hours back, which after a 23-hour day lands two dates back.
        let day = match zoned.yesterday() {
            Ok(before) if zoned.hour() < 5 && zoned.date() != self.today => before,
            _ => zoned,
        };
        match (day.date() == self.today, part) {
            (true, "night") => "tonight".into(),
            (true, "around noon") => part.into(),
            (true, _) => format!("this {part}"),
            (false, _) => format!("{} {part}", self.day(&day, true)),
        }
    }

    fn short(&self, limit: &Limit) -> String {
        let (named, plural) = short_name(limit);
        let s = if plural { "" } else { "s" };
        match &limit.outlook {
            Outlook::UsedUp { back: Some(back) } => format!("{named} back {}", self.at(back)),
            Outlook::UsedUp { back: None } => format!("{named} used up"),
            Outlook::RunsOut { likely, .. } => format!("{named} run{s} out {}", self.at(likely)),
            Outlook::Lasts { .. } | Outlook::Unknown { .. } => {
                if let Some(window) = &limit.window {
                    return format!("{named} reset{s} {}", self.at(&window.resets));
                }
                if limit.money.is_none() {
                    format!("{named} last{s}")
                } else {
                    named
                }
            }
        }
    }

    /// What a row says under an account's name while its limits aren't shown.
    pub fn status(&self, account: &Account) -> String {
        if account.refused() {
            return if account.limits.is_empty() {
                "Sign in again".into()
            } else {
                "Sign in again to update".into()
            };
        }
        if let Some(limit) = account.deciding_limit() {
            return match limit.reset_since {
                Some(reset) => format!("{} reset {}", short_name(limit).0, self.ago(reset)),
                None => self.short(limit),
            };
        }
        if let Some(spend) = no_limit(account) {
            return spent_line(spend);
        }
        match account.state.why() {
            Some(Stale::Expired) => "Login expired".into(),
            Some(Stale::ReadFailed) => "Couldn't be read yet".into(),
            Some(Stale::SignedOut) => "Signed out".into(),
            _ => "Not read yet".into(),
        }
    }

    pub fn window(&self, limit: &Limit) -> Option<String> {
        if let Some(reset) = limit.reset_since {
            return Some(format!("Reset {}", self.ago(reset)));
        }
        if let Outlook::UsedUp { back } = &limit.outlook {
            return Some(
                back.as_ref()
                    .map_or("Used up".into(), |b| format!("Back {}", self.at(b))),
            );
        }
        limit
            .window
            .as_ref()
            .map(|w| format!("Resets {}", self.at(&w.resets)))
    }

    pub fn ahead(&self, limit: &Limit) -> Option<String> {
        match &limit.outlook {
            Outlook::RunsOut { likely, .. } => Some(format!("Runs out {}", self.at(likely))),
            _ => None,
        }
    }

    /// The one thing to know about the first account in use, in large type, and the detail
    /// behind it.
    pub fn verdict(
        &self,
        first: Option<&Account>,
        hidden_in_use: bool,
        all: &[Account],
    ) -> (String, String) {
        let Some(account) = first else {
            let detail = if hidden_in_use {
                "Only accounts you hid are in use."
            } else {
                ""
            };
            return ("Nothing in use".into(), detail.into());
        };
        let Some(limit) = account.deciding_limit() else {
            if account.refused() {
                return (
                    format!("Sign in to {} again", account.title),
                    sign_in(account),
                );
            }
            if let Some(spend) = no_limit(account) {
                let detail =
                    pace(spend).unwrap_or_else(|| format!("Your {} has no limit.", account.title));
                return ("You can keep going".into(), detail);
            }
            let detail = self
                .trouble(account)
                .unwrap_or("They aren't read yet.".into());
            return (format!("{}'s limits are unknown", account.title), detail);
        };
        let (subject, plural) = match &limit.scope {
            Some(_) => short_name(limit),
            None => (account.title.clone(), false),
        };
        let with_alternate = |detail: String| match turnscope::status::alternate(all, account, None)
        {
            Some((other, other_limit)) => {
                let room = format!(
                    "{} has {} left.",
                    called(other, twinned(other, all)),
                    left(other_limit)
                );
                if detail.is_empty() {
                    room
                } else {
                    format!("{detail} {room}")
                }
            }
            None => detail,
        };
        match &limit.outlook {
            Outlook::UsedUp { back } => {
                let after = back
                    .as_ref()
                    .map_or("It isn't known when it's back.".into(), |b| {
                        format!("Back {}.", self.at(b))
                    });
                let is = if plural { "are" } else { "is" };
                (format!("{subject} {is} used up"), with_alternate(after))
            }
            Outlook::RunsOut { likely, .. } => {
                let when = match likely.horizon {
                    Horizon::Soon | Horizon::Later => self.at(likely),
                    Horizon::Today | Horizon::ThisWeek => self.when(likely.at),
                };
                let runs = if plural { "run" } else { "runs" };
                let headline = format!("{subject} {runs} out {}", unbroken(&when));
                let back = match &limit.window {
                    Some(w) => format!("Back {}.", self.at(&w.resets)),
                    None => unforecast(limit, account, Unknown::NoReset),
                };
                (headline, with_alternate(back))
            }
            Outlook::Unknown { reason } => (
                "You can keep going".into(),
                unforecast(limit, account, *reason),
            ),
            Outlook::Lasts { .. } => ("You can keep going".into(), self.lasting(limit, account)),
        }
    }

    fn lasting(&self, limit: &Limit, account: &Account) -> String {
        let others: Vec<&Limit> = account
            .limits
            .iter()
            .filter(|l| l.key != limit.key && l.reset_since.is_none())
            .collect();
        let current = matches!(account.state, State::Live { .. });
        let every = others
            .iter()
            .all(|l| matches!(l.outlook, Outlook::Lasts { .. }));
        let lasts = if every {
            format!("no {} limit runs out", account.title)
        } else if limit.held_by.is_empty() {
            format!(
                "{}'s {} lasts until it resets",
                account.title,
                in_sentence(limit)
            )
        } else {
            format!("{} lasts until it resets", in_sentence(limit))
        };
        let sentence = if current {
            format!("At this pace, {lasts}.")
        } else {
            format!("As last read, {lasts}.")
        };
        let used_up = others
            .iter()
            .filter(|l| l.standing() == Standing::UsedUp)
            .max_by_key(|l| l.back());
        let Some(other) =
            used_up.or_else(|| others.iter().find(|l| l.standing() != Standing::Lasts))
        else {
            return sentence;
        };
        match &other.outlook {
            Outlook::UsedUp { back } => format!(
                "{sentence} {} {} used up{}.",
                name(other),
                if plural(other) { "are" } else { "is" },
                back.as_ref()
                    .map_or(String::new(), |b| format!(", back {}", self.at(b)))
            ),
            Outlook::RunsOut { likely, .. } => format!(
                "{sentence} {} {} out {}.",
                name(other),
                if plural(other) { "run" } else { "runs" },
                self.at(likely)
            ),
            _ => sentence,
        }
    }

    /// What an opened account says under its limits that they can't say themselves.
    pub fn opened(&self, account: &Account, headlined: bool) -> Option<String> {
        if headlined && account.limits.is_empty() {
            return None;
        }
        if let Some(trouble) = self.trouble(account) {
            return Some(trouble);
        }
        if refilled(account) {
            return Some(RESET_FULL.into());
        }
        if !account.limits.is_empty() {
            return None;
        }
        account.spend.as_ref().and_then(pace)
    }

    fn trouble(&self, account: &Account) -> Option<String> {
        let with = first_agent(account);
        match &account.state {
            State::Unread { why } => Some(match why {
                Stale::SignIn => sign_in(account),
                Stale::Expired => format!("Its login expired; it's read when {with} runs."),
                Stale::ReadFailed => "It couldn't be read yet.".into(),
                Stale::SignedOut => "No agent here is signed in to it.".into(),
            }),
            State::AsOf { read_at, why, .. } => {
                if *why == Stale::SignedOut && refilled(account) {
                    return None;
                }
                let ago = self.ago(*read_at);
                Some(match why {
                    Stale::SignIn => format!("Last read {ago}. {}", sign_in(account)),
                    Stale::ReadFailed => format!("It couldn't be read since {ago}."),
                    Stale::Expired => {
                        format!(
                            "Last read {ago}. Its login expired; it's read again when {with} runs."
                        )
                    }
                    Stale::SignedOut => {
                        format!("Last read {ago}. No agent here is signed in to it.")
                    }
                })
            }
            State::Live { .. } => None,
        }
    }

    pub fn figure(&self, account: &Account) -> String {
        let Some(limit) = account.deciding_limit() else {
            if account.refused() {
                return "Sign in".into();
            }
            return account.spend.as_ref().map_or("—".into(), spent_figure);
        };
        match &limit.outlook {
            Outlook::UsedUp { back } => back.as_ref().map_or("Used up".into(), |b| {
                format!("Back {}", self.clock(b, false))
            }),
            Outlook::RunsOut { likely, .. } if likely.horizon == Horizon::Soon => {
                self.until(likely.at)
            }
            _ => left(limit),
        }
    }

    pub fn spoken_item(&self, accounts: &[&Account], all: &[Account], out_of_date: bool) -> String {
        let said = if accounts.is_empty() {
            "Turnscope".to_owned()
        } else {
            accounts
                .iter()
                .map(|a| format!("{}, {}", called(a, twinned(a, all)), self.spoken_figure(a)))
                .collect::<Vec<_>>()
                .join("; ")
        };
        if out_of_date {
            format!("{said}, not up to date")
        } else {
            said
        }
    }

    fn spoken_figure(&self, account: &Account) -> String {
        let Some(limit) = account.deciding_limit() else {
            if account.refused() {
                return "sign in again".into();
            }
            return account.spend.as_ref().map_or("not read yet".into(), spent);
        };
        match &limit.outlook {
            Outlook::UsedUp { back } => back.as_ref().map_or("used up".into(), |b| {
                format!("back {}", self.clock(b, true))
            }),
            Outlook::RunsOut { likely, .. } if likely.horizon == Horizon::Soon => {
                format!("runs out in {}", self.spoken(likely.at))
            }
            Outlook::RunsOut { .. } => format!("{} left, running out", left(limit)),
            Outlook::Lasts { .. } | Outlook::Unknown { .. } => format!("{} left", left(limit)),
        }
    }

    fn spoken(&self, at: i64) -> String {
        let minutes = (((at - self.now).max(0) + 30_000) / 60_000) as u64;
        let (hours, rest) = (minutes / 60, minutes % 60);
        if hours == 0 {
            return counted(rest, "minute");
        }
        if rest == 0 {
            counted(hours, "hour")
        } else {
            format!("{} {}", counted(hours, "hour"), counted(rest, "minute"))
        }
    }

    pub fn note(&self, alert: Alert, accounts: &[Account]) -> AlertNote {
        let account = accounts.iter().find(|a| a.id == alert.account);
        let title = account.map_or_else(
            || alert.account_title.clone(),
            |a| called_among(a, accounts),
        );
        let limit = account.and_then(|a| {
            a.limits
                .iter()
                .find(|l| Some(&l.key) == alert.limit.as_ref())
        });
        let limit_name = limit
            .map(name)
            .or_else(|| alert.limit_name.clone())
            .unwrap_or("A limit".into());
        let (runs, be, it) = if limit.is_some_and(plural) {
            ("run", "are", "They don't")
        } else {
            ("runs", "is", "It doesn't")
        };
        let back = || match &alert.resets {
            Some(resets) => format!("Back {}.", self.clock(resets, true)),
            None => format!("{it} reset."),
        };
        let (subtitle, body) = match alert.kind {
            AlertKind::SignIn => (
                "Sign in again".into(),
                account.map_or(String::new(), sign_in),
            ),
            AlertKind::RunningOut => {
                let out = alert
                    .runs_out
                    .as_ref()
                    .map_or("soon".into(), |r| self.clock(r, true));
                (format!("{limit_name} {runs} out {out}"), back())
            }
            AlertKind::UsedUp => (format!("{limit_name} {be} used up"), back()),
            AlertKind::Reset => (
                format!("{limit_name} {be} back"),
                limit.map_or(String::new(), |l| format!("{} left.", left_of(l))),
            ),
        };
        AlertNote {
            id: alert.id,
            replaces: replaces(&alert.account, alert.limit.as_deref()),
            account: alert.account,
            sign_in: alert.kind == AlertKind::SignIn,
            title,
            subtitle,
            body,
        }
    }
}

fn fmt_time(z: &ZonedDateTime) -> String {
    let format = if z.minute() == 0 {
        "%-I %p"
    } else {
        "%-I:%M %p"
    };
    z.strftime(format).to_string()
}

fn fmt_weekday(z: &ZonedDateTime, full: bool) -> String {
    z.strftime(if full { "%A" } else { "%a" }).to_string()
}

/// "Oct 30", or "October 30" if `full`.
fn fmt_month_day(z: &ZonedDateTime, full: bool) -> String {
    z.strftime(if full { "%B %-d" } else { "%b %-d" })
        .to_string()
}

fn dollars(value: f64, down: bool) -> String {
    let value = value.max(0.0);
    let hair = 1e-9;
    let cents = if down {
        (value * 100.0 + hair).floor()
    } else {
        (value * 100.0).round()
    };
    if cents == 0.0 {
        return "$0".into();
    }
    if cents < 1000.0 {
        return format!("${:.2}", cents / 100.0);
    }
    let whole = if down {
        (value + hair).floor()
    } else {
        value.round()
    } as u64;
    format!("${}", turnscope::usage::count(whole))
}

pub fn left(limit: &Limit) -> String {
    match &limit.money {
        Some(money) => dollars(money.left_usd, true),
        None => format!("{}%", limit.left_percent),
    }
}

/// What's left, and of how much where that's money: "$10 of $20", "60%".
fn left_of(limit: &Limit) -> String {
    match size(limit) {
        Some(size) => format!("{} {size}", left(limit)),
        None => left(limit),
    }
}

pub fn size(limit: &Limit) -> Option<String> {
    limit
        .money
        .as_ref()
        .map(|m| format!("of {}", dollars(m.size_usd, false)))
}

/// What an API key with no deciding limit spent, once it's been read: one
/// never read has no limits known.
fn no_limit(account: &Account) -> Option<&Spend> {
    account
        .spend
        .as_ref()
        .filter(|_| !matches!(account.state, State::Unread { .. }))
}

fn unknown(spend: &Spend) -> bool {
    spend.partial && spend.month_usd == 0.0
}

fn spent_figure(spend: &Spend) -> String {
    if unknown(spend) {
        "—".into()
    } else {
        dollars(spend.month_usd, false)
    }
}

fn spent_line(spend: &Spend) -> String {
    if unknown(spend) {
        return "No limit · cost unknown".into();
    }
    if spend.partial {
        "No limit · some unpriced".into()
    } else {
        "No limit".into()
    }
}

fn pace(spend: &Spend) -> Option<String> {
    spend
        .pace()
        .map(|pace| format!("On pace for about {} this month.", dollars(pace, false)))
}

/// What an account with no limit spent this month, as a phrase.
fn spent(spend: &Spend) -> String {
    if unknown(spend) {
        return "Cost unknown".into();
    }
    format!(
        "{}{} this month",
        if spend.partial { "At least " } else { "" },
        dollars(spend.month_usd, false)
    )
}

/// What a line too short for [`name`] calls a limit, and whether that's
/// plural. One on some models, as a Google AI plan's pool, goes by its
/// models: with its own name too, a row's "Claude and GPT weekly resets Sat
/// 12 PM" was cut short, and the headline ran to two lines.
fn short_name(limit: &Limit) -> (String, bool) {
    match &limit.scope {
        Some(models) => (models.clone(), models.contains(" and ")),
        None => (name(limit), plural(limit)),
    }
}

pub fn name(limit: &Limit) -> String {
    if !limit.held_by.is_empty() {
        return format!("{}'s key", agent_names(&limit.held_by, "and"));
    }
    match &limit.scope {
        Some(scope) => format!("{scope} {}", limit.name.to_lowercase()),
        None => limit.name.clone(),
    }
}

fn in_sentence(limit: &Limit) -> String {
    if limit.scope.is_none() && limit.held_by.is_empty() {
        name(limit).to_lowercase()
    } else {
        name(limit)
    }
}

pub fn of(limit: &Limit) -> String {
    match in_sentence(limit).as_str() {
        "weekly" => "this week".into(),
        "monthly" => "this month".into(),
        named => format!("of {named}"),
    }
}

/// The figure on an account's right, and the quieter text after it.
pub fn row_figure(account: &Account) -> (String, Option<String>) {
    if let Some(limit) = account.deciding_limit() {
        return (left(limit), size(limit));
    }
    if let Some(spend) = &account.spend {
        return (
            spent_figure(spend),
            (!unknown(spend)).then(|| "this month".into()),
        );
    }
    ("—".into(), None)
}

/// What a limit's figure says to VoiceOver.
pub fn left_spoken(limit: &Limit) -> String {
    format!("{} left", left_of(limit))
}

/// What a row's figure says to VoiceOver: nothing for a "—", whose line says why.
pub fn row_figure_spoken(account: &Account) -> String {
    if let Some(limit) = account.deciding_limit() {
        return left_spoken(limit);
    }
    account
        .spend
        .as_ref()
        .filter(|spend| !unknown(spend))
        .map_or(String::new(), spent)
}

fn unforecast(limit: &Limit, account: &Account, reason: Unknown) -> String {
    if reason == Unknown::Stale && limit.reset_since.is_some() {
        return RESET_FULL.into();
    }
    let what = if limit.money.is_none() {
        format!("{} left {}", left(limit), of(limit))
    } else {
        format!("{} left", left_of(limit))
    };
    let standing = format!("{} has {what}", account.title);
    match reason {
        Unknown::NotEnoughData | Unknown::Rolling => format!("{standing}."),
        Unknown::Stale => format!("As last read, {standing}."),
        Unknown::NoReset => format!("{standing}; it doesn't reset."),
    }
}

pub fn welcome(agents: &[&Agent], reads: &[&str], reading: bool) -> (String, String) {
    if reading {
        return (
            "Getting your limits".into(),
            "They show up in a moment.".into(),
        );
    }
    if agents.is_empty() {
        return (
            "No agents found".into(),
            format!(
                "Turnscope reads {}. Sign in to one, and its limits show up.",
                list(reads, "and")
            ),
        );
    }
    let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
    (
        "No accounts found yet".into(),
        format!(
            "Once {} is signed in, its limits show up.",
            list(&names, "or")
        ),
    )
}

pub fn unused(count: usize, beside_others: bool) -> String {
    counted(
        count as u64,
        if beside_others {
            "more account"
        } else {
            "account"
        },
    )
}

pub fn hidden(count: usize) -> String {
    format!("{count} hidden")
}

const RESET_FULL: &str = "Full since the reset, unless used on the web or another device.";
pub const UNTITLED_SESSION: &str = "Untitled session";
pub const READING_TEXT: &str = "Reading your agents' history…";
pub const READING_HELP: &str = "Turnscope reads your agents' history once, when it first runs. Limits show up as soon as they are read.";
pub const READING_SPOKEN: &str = "Reading your agents' history";
pub const REFRESHING_TEXT: &str = "Refreshing limits…";
pub const REFRESHING_SPOKEN: &str = "Refreshing limits";

pub fn connection(agent: &Agent) -> Option<String> {
    match agent.connection {
        Connection::Connected => None,
        Connection::Available => Some("Isn't connected".into()),
        Connection::Outdated => Some("Uses another Turnscope".into()),
    }
}

pub fn connect_action(agent: &Agent) -> (&'static str, String) {
    if agent.connection == Connection::Outdated {
        (
            "Reconnect",
            format!("Points {} at this copy of Turnscope", agent.name),
        )
    } else {
        (
            "Connect",
            format!(
                "Adds Turnscope to {}'s MCP servers, so it can pace itself",
                agent.name
            ),
        )
    }
}

pub fn agents_title(from_disk_image: bool) -> &'static str {
    if from_disk_image {
        "Move Turnscope to Applications to connect agents"
    } else {
        "Let these agents check their limits"
    }
}

pub fn agent_changing(to: Connection) -> &'static str {
    if to == Connection::Connected {
        "Connecting…"
    } else {
        "Disconnecting…"
    }
}

pub fn stale_footer(problem: &Stopped) -> (String, String, String) {
    let (said, then) = match problem {
        Stopped::Failing(_) => ("Not up to date · trying again", " Trying again."),
        Stopped::Newer { replaced: true } => ("Not up to date · start Turnscope again", ""),
        Stopped::Newer { replaced: false } => ("Not up to date · update Turnscope", ""),
    };
    (
        said.to_owned(),
        problem.to_string(),
        format!("Not up to date: {problem}.{then}"),
    )
}

pub fn used_most_heading(limit: &Limit) -> String {
    format!("Used most {}", of(limit))
}

pub fn used(used: &UsedMost, agents: bool) -> String {
    [
        used.share_percent.map(turnscope::usage::share),
        used.project.clone(),
        agents.then(|| turnscope::agents::name(&used.agent).to_owned()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

pub fn called(account: &Account, labelled: bool) -> String {
    if labelled {
        account.full_title()
    } else {
        account.title.clone()
    }
}

/// Whether another shown account of `among` has `account`'s title, so its label tells them apart.
pub fn twinned<'a>(account: &Account, among: impl IntoIterator<Item = &'a Account>) -> bool {
    among
        .into_iter()
        .any(|a| a.id != account.id && !a.hidden && a.title == account.title)
}

/// Notifications stand alone, so include the label only when another in-use account shares the title.
fn called_among(account: &Account, accounts: &[Account]) -> String {
    called(
        account,
        twinned(account, accounts.iter().filter(|a| a.in_use)),
    )
}

fn plural(limit: &Limit) -> bool {
    limit.held_by.is_empty() && limit.plural()
}

fn first_agent(account: &Account) -> &str {
    account
        .agents
        .first()
        .map_or("its agent", |a| turnscope::agents::name(a))
}

/// "Open Claude Code to sign in.": the agent whose login was refused renews it.
fn sign_in(account: &Account) -> String {
    format!("Open {} to sign in.", first_agent(account))
}

fn refilled(account: &Account) -> bool {
    account
        .deciding_limit()
        .and_then(|l| l.reset_since)
        .is_some()
}

fn unbroken(phrase: &str) -> String {
    phrase.replace(' ', "\u{00A0}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use Horizon::*;
    use turnscope::providers::Kind;
    use turnscope::status::{LimitWindow, Money};

    fn at(text: &str) -> i64 {
        text.parse::<jiff::Timestamp>().unwrap().as_millisecond()
    }

    fn noon() -> i64 {
        at("2026-09-30T12:00:00Z")
    }

    fn as_of(now: i64) -> Words {
        Words::in_zone(now, TimeZone::UTC)
    }

    fn words() -> Words {
        as_of(noon())
    }

    fn moment(text: &str, horizon: Horizon) -> Moment {
        Moment {
            at: at(text),
            horizon,
        }
    }

    fn window(text: &str, horizon: Horizon) -> Option<LimitWindow> {
        Some(LimitWindow {
            starts_at: None,
            resets: moment(text, horizon),
        })
    }

    fn lasts() -> Outlook {
        Outlook::Lasts {
            left_at_reset: None,
        }
    }

    fn runs_out_at(likely: Moment) -> Outlook {
        Outlook::RunsOut {
            likely,
            soonest: likely,
        }
    }

    fn runs_out() -> Outlook {
        runs_out_at(moment("2026-09-30T14:00:00Z", Soon))
    }

    fn used_up(back: Option<Moment>) -> Outlook {
        Outlook::UsedUp { back }
    }

    fn unknown(reason: Unknown) -> Outlook {
        Outlook::Unknown { reason }
    }

    fn limit(outlook: Outlook) -> Limit {
        Limit {
            key: "five_hour".into(),
            name: "5 hours".into(),
            scope: None,
            window: window("2026-09-30T15:00:00Z", Soon),
            read_at: noon(),
            left_percent: 20,
            money: None,
            reset_since: None,
            held_by: vec![],
            outlook,
            work: None,
        }
    }

    fn weekly(outlook: Outlook, left: u8) -> Limit {
        Limit {
            key: "seven_day".into(),
            name: "Weekly".into(),
            left_percent: left,
            window: window("2026-10-04T00:00:00Z", ThisWeek),
            ..limit(outlook)
        }
    }

    fn credits(left: u8, size: f64, left_usd: f64, outlook: Outlook) -> Limit {
        Limit {
            key: "credits".into(),
            name: "Credits".into(),
            window: None,
            left_percent: left,
            money: Some(Money {
                size_usd: size,
                left_usd,
            }),
            ..limit(outlook)
        }
    }

    fn account(limits: Vec<Limit>) -> Account {
        Account {
            id: "anthropic:a".into(),
            provider: "anthropic".into(),
            kind: Kind::Subscription,
            title: "Claude Max".into(),
            label: None,
            agents: vec!["claude-code".into()],
            used_by: vec![],
            in_use: true,
            recent: true,
            hidden: false,
            state: State::Live { read_at: noon() },
            deciding: limits.first().map(|l| l.key.clone()),
            limits,
            spend: None,
        }
    }

    fn api_key(limits: Vec<Limit>, spend: Option<Spend>) -> Account {
        Account {
            id: "openrouter:api".into(),
            provider: "openrouter".into(),
            kind: Kind::ApiKey,
            title: "OpenRouter API key".into(),
            spend,
            ..account(limits)
        }
    }

    #[test]
    fn a_time_is_said_as_its_horizon_calls_for_in_the_macs_zone() {
        let w = words();
        assert_eq!(w.at(&moment("2026-09-30T14:10:00Z", Soon)), "in 2h 10m");
        assert_eq!(w.at(&moment("2026-09-30T18:40:00Z", Today)), "6:40 PM");
        assert_eq!(
            w.at(&moment("2026-10-01T09:00:00Z", ThisWeek)),
            "tomorrow 9 AM"
        );
        assert_eq!(
            w.at(&moment("2026-10-03T19:50:00Z", ThisWeek)),
            "Sat 7:50 PM"
        );
        assert_eq!(w.at(&moment("2026-10-18T16:37:00Z", Later)), "Oct 18");
        assert_eq!(
            w.clock(&moment("2026-10-03T19:50:00Z", ThisWeek), true),
            "Saturday 7:50 PM"
        );
        assert_eq!(
            w.clock(&moment("2026-10-18T16:37:00Z", Later), true),
            "Sunday, October 18, 4:37 PM"
        );
        assert_eq!(w.when(at("2026-10-03T19:51:00Z")), "Saturday evening");
        assert_eq!(w.when(at("2026-10-01T00:40:00Z")), "tonight");
        assert_eq!(w.when(at("2026-09-30T12:10:00Z")), "around noon");
        assert_eq!(w.when(at("2026-09-30T18:00:00Z")), "this evening");
        assert_eq!(w.ago(at("2026-09-30T00:00:00Z")), "12 AM");
        assert_eq!(w.ago(at("2026-09-29T23:59:00Z")), "yesterday 11:59 PM");
        assert_eq!(w.until(w.now + 52 * 3_600_000), "2d 4h");
        assert_eq!(w.until(w.now - 60_000), "0m");

        let chicago = TimeZone::get("America/Chicago").unwrap();
        let local = Words::in_zone(noon(), chicago.clone());
        assert_eq!(
            local.at(&moment("2026-10-03T19:50:00Z", ThisWeek)),
            "Sat 2:50 PM"
        );
        assert_eq!(local.ago(at("2026-09-22T12:00:00Z")), "Sep 22");
        // Past midnight after a 23-hour DST spring-forward day (2027-03-14).
        let dst = Words::in_zone(at("2027-03-11T18:00:00Z"), chicago);
        assert_eq!(dst.when(at("2027-03-15T05:30:00Z")), "Sunday night");
    }

    /// The minute's tick lands a little after the minute, and the panel opens at any second.
    #[test]
    fn a_countdown_counts_from_the_minute_it_is() {
        let w = words();
        let claude = account(vec![limit(runs_out())]);
        for millis in [50, 40_000, 59_900] {
            let late = as_of(noon() + millis);
            assert_eq!(late.at(&moment("2026-09-30T14:00:00Z", Soon)), "in 2h");
            assert_eq!(late.figure(&claude), "2h");
            assert_eq!(late.spoken_figure(&claude), "runs out in 2 hours");
        }
        assert_eq!(as_of(at("2026-09-30T13:59:30Z")).figure(&claude), "1m");
        let soon = account(vec![limit(runs_out_at(moment(
            "2026-09-30T12:49:40Z",
            Soon,
        )))]);
        assert_eq!(w.figure(&soon), "50m");
        assert_eq!(w.spoken_figure(&soon), "runs out in 50 minutes");
    }

    #[test]
    fn amounts_are_grouped_and_what_is_left_is_rounded_down() {
        assert_eq!(dollars(0.004, false), "$0");
        assert_eq!(dollars(0.004, true), "$0");
        assert_eq!(dollars(9.956, false), "$9.96");
        assert_eq!(dollars(9.956, true), "$9.95");
        assert_eq!(dollars(9.996, false), "$10");
        assert_eq!(dollars(9.996, true), "$9.99");
        assert_eq!(dollars(9.95, true), "$9.95");
        assert_eq!(dollars(84.9, true), "$84");
        assert_eq!(dollars(1250.4, false), "$1,250");
        assert_eq!(dollars(1_234_567.9, false), "$1,234,568");
    }

    #[test]
    fn a_limit_says_its_name_when_it_resets_and_when_it_runs_out() {
        let w = words();
        assert_eq!(
            w.window(&limit(runs_out())).as_deref(),
            Some("Resets in 3h")
        );
        assert_eq!(
            w.ahead(&limit(runs_out())).as_deref(),
            Some("Runs out in 2h")
        );

        let week = weekly(lasts(), 60);
        assert_eq!(w.window(&week).as_deref(), Some("Resets Sun 12 AM"));
        assert_eq!(w.ahead(&week), None);
        assert_eq!(w.short(&week), "Weekly resets Sun 12 AM");
        assert_eq!(of(&week), "this week");
        assert_eq!(of(&limit(lasts())), "of 5 hours");
        assert_eq!(
            of(&Limit {
                name: "Monthly".into(),
                ..limit(lasts())
            }),
            "this month"
        );

        let opus = Limit {
            key: "seven_day_opus".into(),
            scope: Some("Opus".into()),
            ..weekly(lasts(), 50)
        };
        assert_eq!(name(&opus), "Opus weekly");
        assert_eq!(of(&opus), "of Opus weekly");
        assert_eq!(w.short(&opus), "Opus resets Sun 12 AM");
        let pool = Limit {
            scope: Some("Claude and GPT".into()),
            ..weekly(lasts(), 40)
        };
        assert_eq!(w.short(&pool), "Claude and GPT reset Sun 12 AM");

        let held = Limit {
            key: "key-1".into(),
            name: "Credits".into(),
            held_by: vec!["codex".into(), "pi".into()],
            ..limit(lasts())
        };
        assert_eq!(name(&held), "Codex and Pi's key");

        let spent = Limit {
            left_percent: 0,
            ..limit(used_up(Some(moment("2026-09-30T15:00:00Z", Soon))))
        };
        assert_eq!(w.window(&spent).as_deref(), Some("Back in 3h"));
        assert_eq!(w.ahead(&spent), None);

        let money = credits(29, 30.0, 8.705, unknown(Unknown::NoReset));
        assert_eq!(left(&money), "$8.70");
        assert_eq!(size(&money).as_deref(), Some("of $30"));
        assert_eq!((w.window(&money), w.ahead(&money)), (None, None));
        assert_eq!(
            w.short(&credits(0, 30.0, 0.0, used_up(None))),
            "Credits used up"
        );
    }

    #[test]
    fn a_limit_reset_since_it_was_read_is_full_and_doesnt_run_out() {
        let w = words();
        let five_reset = Limit {
            left_percent: 100,
            reset_since: Some(at("2026-09-30T11:00:00Z")),
            ..limit(unknown(Unknown::Stale))
        };
        assert_eq!(w.window(&five_reset).as_deref(), Some("Reset 11 AM"));
        assert_eq!(
            (left(&five_reset).as_str(), w.ahead(&five_reset)),
            ("100%", None)
        );
        let week_reset = Limit {
            reset_since: Some(at("2026-09-30T11:30:00Z")),
            ..weekly(unknown(Unknown::Stale), 100)
        };
        let full = Account {
            in_use: false,
            recent: false,
            deciding: Some("seven_day".into()),
            ..account(vec![five_reset, week_reset])
        };
        assert_eq!(w.status(&full), "Weekly reset 11:30 AM");
        assert_eq!(row_figure(&full), ("100%".into(), None));
        assert_eq!(
            w.opened(&full, false).as_deref(),
            Some("Full since the reset, unless used on the web or another device.")
        );
    }

    #[test]
    fn the_verdict_says_a_reset_limit_is_full_since_the_reset() {
        let w = words();
        let reset = Limit {
            left_percent: 100,
            reset_since: Some(at("2026-09-30T11:00:00Z")),
            ..limit(unknown(Unknown::Stale))
        };
        let full = Account {
            state: State::AsOf {
                read_at: at("2026-09-30T10:00:00Z"),
                why: Stale::Expired,
                worked_minutes: 0,
            },
            ..account(vec![reset])
        };
        assert_eq!(
            w.verdict(Some(&full), false, &[]),
            (
                "You can keep going".into(),
                "Full since the reset, unless used on the web or another device.".into()
            )
        );
    }

    #[test]
    fn the_verdict_says_the_most_urgent_and_when_its_back() {
        let w = words();
        let out = account(vec![limit(runs_out())]);
        assert_eq!(
            w.verdict(Some(&out), false, &[]),
            (
                "Claude Max runs out in\u{a0}2h".into(),
                "Back in 3h.".into()
            )
        );
        let this_week = account(vec![Limit {
            window: window("2026-10-04T00:00:00Z", ThisWeek),
            ..limit(runs_out_at(moment("2026-10-03T06:00:00Z", ThisWeek)))
        }]);
        assert_eq!(
            w.verdict(Some(&this_week), false, &[]),
            (
                "Claude Max runs out Saturday\u{a0}morning".into(),
                "Back Sun 12 AM.".into()
            )
        );
        let monthly = Account {
            title: "OpenCode Go".into(),
            ..account(vec![Limit {
                key: "monthly".into(),
                name: "Monthly".into(),
                window: window("2026-10-30T12:00:00Z", Later),
                ..limit(runs_out_at(moment("2026-10-08T06:00:00Z", Later)))
            }])
        };
        assert_eq!(
            w.verdict(Some(&monthly), false, &[]),
            (
                "OpenCode Go runs out Oct\u{a0}8".into(),
                "Back Oct 30.".into()
            )
        );
        let credits_out = runs_out_at(moment("2026-10-02T16:00:00Z", ThisWeek));
        let key = api_key(vec![credits(10, 60.0, 6.0, credits_out)], None);
        assert_eq!(
            w.verdict(Some(&key), false, &[]),
            (
                "OpenRouter API key runs out Friday\u{a0}afternoon".into(),
                "OpenRouter API key has $6.00 of $60 left; it doesn't reset.".into()
            )
        );

        let used = |back| {
            account(vec![Limit {
                left_percent: 0,
                ..limit(used_up(Some(back)))
            }])
        };
        assert_eq!(
            w.verdict(
                Some(&used(moment("2026-10-03T09:00:00Z", ThisWeek))),
                false,
                &[]
            ),
            ("Claude Max is used up".into(), "Back Sat 9 AM.".into())
        );
        assert_eq!(
            w.verdict(
                Some(&used(moment("2026-09-30T15:00:00Z", Soon))),
                false,
                &[]
            )
            .1,
            "Back in 3h."
        );

        assert_eq!(
            w.verdict(None, false, &[]),
            ("Nothing in use".into(), String::new())
        );
        assert_eq!(
            w.verdict(None, true, &[]).1,
            "Only accounts you hid are in use."
        );

        let unread = Account {
            state: State::Unread {
                why: Stale::ReadFailed,
            },
            ..account(vec![])
        };
        assert_eq!(
            w.verdict(Some(&unread), false, &[]),
            (
                "Claude Max's limits are unknown".into(),
                "It couldn't be read yet.".into()
            )
        );
        let signed_out = Account {
            state: State::Unread {
                why: Stale::SignedOut,
            },
            ..account(vec![])
        };
        assert_eq!(
            w.verdict(Some(&signed_out), false, &[]).1,
            "No agent here is signed in to it."
        );

        // When running out or used up, name a live alternate with headroom.
        let backup = Account {
            id: "openai:b".into(),
            provider: "openai".into(),
            title: "ChatGPT Pro".into(),
            agents: vec!["codex".into()],
            ..account(vec![Limit {
                left_percent: 97,
                ..weekly(lasts(), 97)
            }])
        };
        assert_eq!(
            w.verdict(Some(&out), false, &[out.clone(), backup.clone()])
                .1,
            "Back in 3h. ChatGPT Pro has 97% left."
        );
        // Signed-out backup is not suggested.
        let signed_out_backup = Account {
            state: State::AsOf {
                read_at: noon(),
                why: Stale::SignedOut,
                worked_minutes: 0,
            },
            agents: vec![],
            ..backup
        };
        assert_eq!(
            w.verdict(Some(&out), false, &[out.clone(), signed_out_backup])
                .1,
            "Back in 3h."
        );
    }

    #[test]
    fn the_verdict_says_only_what_lasts_lasts() {
        let w = words();
        let week = weekly(lasts(), 60);
        assert_eq!(
            w.verdict(Some(&account(vec![week.clone()])), false, &[]).1,
            "At this pace, no Claude Max limit runs out."
        );
        let opus = Limit {
            key: "seven_day_opus".into(),
            scope: Some("Opus".into()),
            window: window("2026-10-03T09:00:00Z", ThisWeek),
            ..weekly(used_up(Some(moment("2026-10-03T09:00:00Z", ThisWeek))), 0)
        };
        assert_eq!(
            w.verdict(Some(&account(vec![week.clone(), opus])), false, &[])
                .1,
            "At this pace, Claude Max's weekly lasts until it resets. Opus weekly is used up, back Sat 9 AM."
        );
        let money = credits(29, 30.0, 8.7, unknown(Unknown::NoReset));
        assert_eq!(
            w.verdict(
                Some(&account(vec![week.clone(), money.clone()])),
                false,
                &[]
            )
            .1,
            "At this pace, Claude Max's weekly lasts until it resets."
        );
        let spent_credits = credits(0, 30.0, 0.0, used_up(None));
        assert_eq!(
            w.verdict(
                Some(&account(vec![week.clone(), spent_credits])),
                false,
                &[]
            )
            .1,
            "At this pace, Claude Max's weekly lasts until it resets. Credits are used up."
        );
        let held = Limit {
            key: "key-1".into(),
            name: "Daily".into(),
            left_percent: 60,
            held_by: vec!["pi".into()],
            ..limit(lasts())
        };
        assert_eq!(
            w.verdict(Some(&api_key(vec![held, money.clone()], None)), false, &[])
                .1,
            "At this pace, Pi's key lasts until it resets."
        );
        assert_eq!(
            w.verdict(Some(&api_key(vec![money], None)), false, &[]).1,
            "OpenRouter API key has $8.70 of $30 left; it doesn't reset."
        );
        let stale = Account {
            state: State::AsOf {
                read_at: noon(),
                why: Stale::ReadFailed,
                worked_minutes: 0,
            },
            ..account(vec![week.clone()])
        };
        assert_eq!(
            w.verdict(Some(&stale), false, &[]).1,
            "As last read, no Claude Max limit runs out."
        );
        for (reason, said) in [
            (
                Unknown::NotEnoughData,
                "Claude Max has 50% left of 5 hours.",
            ),
            (
                Unknown::Stale,
                "As last read, Claude Max has 50% left of 5 hours.",
            ),
            (Unknown::Rolling, "Claude Max has 50% left of 5 hours."),
            (
                Unknown::NoReset,
                "Claude Max has 50% left of 5 hours; it doesn't reset.",
            ),
        ] {
            let unknown = account(vec![Limit {
                left_percent: 50,
                ..limit(unknown(reason))
            }]);
            assert_eq!(
                w.verdict(Some(&unknown), false, &[]),
                ("You can keep going".into(), said.into())
            );
        }

        // Multiple other limits used up: names the last back, prioritizing used-up over running-out.
        let five = |outlook| Limit {
            key: "5h:3p-5h".into(),
            left_percent: 0,
            scope: Some("Claude and GPT".into()),
            window: window("2026-09-30T13:00:00Z", Soon),
            ..limit(outlook)
        };
        let used_weekly = Limit {
            key: "weekly:3p-weekly".into(),
            scope: Some("Claude and GPT".into()),
            window: window("2026-10-03T09:00:00Z", ThisWeek),
            ..weekly(used_up(Some(moment("2026-10-03T09:00:00Z", ThisWeek))), 0)
        };
        let plan = |five: Limit| Account {
            title: "Google AI Pro".into(),
            ..account(vec![
                Limit {
                    key: "weekly".into(),
                    scope: Some("Gemini".into()),
                    ..week.clone()
                },
                five,
                used_weekly.clone(),
            ])
        };
        let base = "At this pace, Google AI Pro's Gemini weekly lasts until it resets.";
        let back = plan(five(used_up(Some(moment("2026-09-30T13:00:00Z", Soon)))));
        assert_eq!(
            w.verdict(Some(&back), false, &[]).1,
            format!("{base} Claude and GPT weekly is used up, back Sat 9 AM.")
        );
        assert_eq!(
            w.verdict(Some(&plan(five(used_up(None)))), false, &[]).1,
            format!("{base} Claude and GPT 5 hours is used up.")
        );
        assert_eq!(
            w.verdict(Some(&plan(five(runs_out()))), false, &[]).1,
            format!("{base} Claude and GPT weekly is used up, back Sat 9 AM.")
        );

        // Drawn on for Claude, its pool decides: that pool is used up, not the plan.
        let claude = Account {
            deciding: Some("weekly:3p-weekly".into()),
            ..back
        };
        assert_eq!(
            w.verdict(Some(&claude), false, &[]),
            ("Claude and GPT are used up".into(), "Back Sat 9 AM.".into())
        );
        assert_eq!(w.status(&claude), "Claude and GPT back Sat 9 AM");
    }

    #[test]
    fn an_account_is_told_apart_only_where_it_has_to_be() {
        let w = words();
        let mine = Account {
            label: Some("me@example.com".into()),
            ..account(vec![limit(runs_out())])
        };
        let mut twin = Account {
            id: "anthropic:b".into(),
            label: Some("work@example.com".into()),
            ..account(vec![Limit {
                left_percent: 90,
                ..limit(lasts())
            }])
        };
        let other = Account {
            id: "openai:a".into(),
            title: "ChatGPT Pro".into(),
            label: Some("me@example.com".into()),
            ..account(vec![limit(runs_out())])
        };
        assert_eq!(
            w.verdict(Some(&mine), false, &[mine.clone(), twin.clone()])
                .0,
            "Claude Max runs out in\u{a0}2h"
        );
        assert_eq!(
            called_among(&mine, &[mine.clone(), twin.clone()]),
            "Claude Max (me@example.com)"
        );
        twin.hidden = true;
        assert_eq!(
            called_among(&mine, &[mine.clone(), twin.clone()]),
            "Claude Max"
        );
        twin.hidden = false;
        twin.in_use = false;
        assert_eq!(
            called_among(&mine, &[mine.clone(), twin.clone()]),
            "Claude Max"
        );

        let all = [mine.clone(), twin.clone(), other.clone()];
        let row = |a: &Account| called(a, twinned(a, &all));
        assert_eq!(
            w.spoken_item(&[&mine, &twin], &all, false),
            "Claude Max (me@example.com), runs out in 2 hours; Claude Max (work@example.com), 90% left"
        );
        assert_eq!(row(&other), "ChatGPT Pro");
        assert_eq!(called(&other, true), "ChatGPT Pro (me@example.com)");
    }

    #[test]
    fn the_menu_bar_says_what_is_left_how_long_or_when_its_back() {
        let w = words();
        let ok = account(vec![Limit {
            left_percent: 67,
            ..limit(lasts())
        }]);
        assert_eq!(w.figure(&ok), "67%");

        let used = account(vec![Limit {
            left_percent: 0,
            ..limit(used_up(Some(moment("2026-09-30T15:00:00Z", Soon))))
        }]);
        assert_eq!(w.figure(&used), "Back 3 PM");

        // Back soon, but past midnight.
        let late = as_of(at("2026-09-30T22:30:00Z"));
        let overnight = account(vec![Limit {
            left_percent: 0,
            ..limit(used_up(Some(moment("2026-10-01T01:00:00Z", Soon))))
        }]);
        assert_eq!(late.figure(&overnight), "Back tomorrow 1 AM");
        assert_eq!(late.spoken_figure(&overnight), "back tomorrow 1 AM");

        let days = account(vec![Limit {
            left_percent: 0,
            ..limit(used_up(Some(moment("2026-10-03T09:00:00Z", ThisWeek))))
        }]);
        assert_eq!(w.figure(&days), "Back Sat 9 AM");
        assert_eq!(w.spoken_figure(&days), "back Saturday 9 AM");

        let refused = Account {
            state: State::Unread { why: Stale::SignIn },
            ..account(vec![])
        };
        assert_eq!(w.figure(&refused), "Sign in");
        assert_eq!(w.status(&refused), "Sign in again");

        let keys = api_key(
            vec![],
            Some(Spend {
                month_usd: 84.2,
                forecast_usd: Some(210.0),
                partial: false,
            }),
        );
        assert_eq!(w.figure(&keys), "$84");
        assert_eq!(row_figure(&keys), ("$84".into(), Some("this month".into())));
        assert_eq!(
            w.opened(&keys, false).as_deref(),
            Some("On pace for about $210 this month.")
        );
        // Late in a month, its forecast to two figures falls below what's spent.
        let late = Spend {
            month_usd: 154.6,
            forecast_usd: Some(150.0),
            partial: false,
        };
        assert_eq!(pace(&late), None);
        let cent = Spend {
            month_usd: 0.011,
            forecast_usd: Some(0.014),
            partial: false,
        };
        assert_eq!(pace(&cent), None);
        // One never read has no limits known.
        let unread = Account {
            state: State::Unread {
                why: Stale::ReadFailed,
            },
            ..keys.clone()
        };
        assert_eq!(w.status(&unread), "Couldn't be read yet");
        assert_eq!(
            w.verdict(Some(&unread), false, &[]),
            (
                "OpenRouter API key's limits are unknown".into(),
                "It couldn't be read yet.".into()
            )
        );
        assert_eq!(
            spent(&Spend {
                month_usd: 0.0,
                forecast_usd: Some(0.0),
                partial: true
            }),
            "Cost unknown"
        );

        let claude = account(vec![limit(runs_out())]);
        assert_eq!(
            w.spoken_item(&[&claude, &keys], &[], false),
            "Claude Max, runs out in 2 hours; OpenRouter API key, $84 this month"
        );
        assert_eq!(
            w.spoken_item(&[&claude], &[], true),
            "Claude Max, runs out in 2 hours, not up to date"
        );
        assert_eq!(w.spoken_item(&[], &[], false), "Turnscope");
    }

    #[test]
    fn an_opened_account_says_why_its_limits_are_old_or_unread() {
        let w = words();
        let as_of_state = |why: Stale, at: i64, limit: Limit| Account {
            state: State::AsOf {
                read_at: at,
                why,
                worked_minutes: 0,
            },
            ..account(vec![limit])
        };
        let half = || Limit {
            left_percent: 50,
            ..limit(lasts())
        };
        assert_eq!(
            w.opened(&as_of_state(Stale::ReadFailed, noon(), half()), false)
                .as_deref(),
            Some("It couldn't be read since 12 PM.")
        );
        let gone = Account {
            in_use: false,
            recent: false,
            ..as_of_state(
                Stale::SignedOut,
                at("2026-09-30T09:00:00Z"),
                Limit {
                    left_percent: 50,
                    ..limit(unknown(Unknown::Stale))
                },
            )
        };
        assert_eq!(
            w.opened(&gone, false).as_deref(),
            Some("Last read 9 AM. No agent here is signed in to it.")
        );
        assert_eq!(
            w.opened(&as_of_state(Stale::SignIn, noon(), half()), false)
                .as_deref(),
            Some("Last read 12 PM. Open Claude Code to sign in.")
        );

        let expired = as_of_state(Stale::Expired, noon(), half());
        assert_eq!(
            w.opened(&expired, false).as_deref(),
            Some("Last read 12 PM. Its login expired; it's read again when Claude Code runs.")
        );
        assert_eq!(w.status(&expired), "5 hours resets in 3h");
        assert!(!expired.refused());

        let lapsed = Account {
            state: State::Unread {
                why: Stale::Expired,
            },
            ..account(vec![])
        };
        assert_eq!(w.status(&lapsed), "Login expired");
        assert_eq!(
            w.opened(&lapsed, false).as_deref(),
            Some("Its login expired; it's read when Claude Code runs.")
        );

        let unreachable = Account {
            state: State::Unread {
                why: Stale::ReadFailed,
            },
            ..account(vec![])
        };
        assert_eq!(w.status(&unreachable), "Couldn't be read yet");
        assert_eq!(
            w.opened(&unreachable, false).as_deref(),
            Some("It couldn't be read yet.")
        );
        assert_eq!(w.opened(&unreachable, true), None);
        let refused = Account {
            state: State::Unread { why: Stale::SignIn },
            ..account(vec![])
        };
        assert_eq!(
            w.opened(&refused, false).as_deref(),
            Some("Open Claude Code to sign in.")
        );
        assert_eq!(w.opened(&account(vec![limit(runs_out())]), false), None);
        assert_eq!(w.opened(&api_key(vec![], None), false), None);
    }

    #[test]
    fn a_note_says_which_account_then_what_happened_then_what_it_means() {
        let w = words();
        let make_alert = |kind| Alert {
            id: 1,
            kind,
            account: "anthropic:a".into(),
            account_title: "Claude Max (me@example.com)".into(),
            limit: Some("five_hour".into()),
            limit_name: Some("5 hours".into()),
            resets: Some(moment("2026-09-30T15:00:00Z", Soon)),
            runs_out: Some(moment("2026-09-30T14:00:00Z", Soon)),
        };
        let mut accounts = vec![
            Account {
                label: Some("me@example.com".into()),
                ..account(vec![limit(runs_out())])
            },
            Account {
                id: "anthropic:b".into(),
                label: Some("work@example.com".into()),
                in_use: false,
                recent: false,
                ..account(vec![])
            },
        ];
        let said = |note: AlertNote| (note.title, note.subtitle, note.body);
        assert_eq!(
            w.note(make_alert(AlertKind::RunningOut), &accounts),
            AlertNote {
                id: 1,
                account: "anthropic:a".into(),
                replaces: "limit:anthropic:a:five_hour".into(),
                sign_in: false,
                title: "Claude Max".into(),
                subtitle: "5 hours runs out 2 PM".into(),
                body: "Back 3 PM.".into()
            }
        );
        let used_note = w.note(make_alert(AlertKind::UsedUp), &accounts);
        assert_eq!(
            (used_note.subtitle.as_str(), used_note.body.as_str()),
            ("5 hours is used up", "Back 3 PM.")
        );
        let reset = w.note(make_alert(AlertKind::Reset), &accounts);
        assert_eq!(
            (reset.subtitle.as_str(), reset.body.as_str()),
            ("5 hours is back", "20% left.")
        );
        let sign_in = w.note(make_alert(AlertKind::SignIn), &accounts);
        assert_eq!(
            (
                sign_in.subtitle.as_str(),
                sign_in.body.as_str(),
                sign_in.sign_in
            ),
            ("Sign in again", "Open Claude Code to sign in.", true)
        );
        assert_eq!(
            w.note(make_alert(AlertKind::UsedUp), &[]).title,
            "Claude Max (me@example.com)"
        );
        accounts[1].in_use = true;
        assert_eq!(
            w.note(make_alert(AlertKind::UsedUp), &accounts).title,
            "Claude Max (me@example.com)"
        );
        accounts[1].in_use = false;
        let days = Alert {
            resets: Some(moment("2026-10-03T00:00:00Z", ThisWeek)),
            runs_out: Some(moment("2026-10-02T09:00:00Z", ThisWeek)),
            ..make_alert(AlertKind::RunningOut)
        };
        let note = w.note(days, &accounts);
        assert_eq!(
            (note.subtitle.as_str(), note.body.as_str()),
            ("5 hours runs out Friday 9 AM", "Back Saturday 12 AM.")
        );

        // Credits notifications (plural, no reset time).
        let key_empty = api_key(vec![credits(0, 10.0, 0.0, used_up(None))], None);
        let credit_alert = |kind| Alert {
            account: "openrouter:api".into(),
            account_title: "OpenRouter API key".into(),
            limit: Some("credits".into()),
            limit_name: Some("Credits".into()),
            resets: None,
            runs_out: None,
            ..make_alert(kind)
        };
        assert_eq!(
            said(w.note(
                credit_alert(AlertKind::UsedUp),
                std::slice::from_ref(&key_empty)
            )),
            (
                "OpenRouter API key".into(),
                "Credits are used up".into(),
                "They don't reset.".into()
            )
        );
        let key_refilled = api_key(
            vec![credits(50, 20.0, 10.0, unknown(Unknown::NoReset))],
            None,
        );
        assert_eq!(
            said(w.note(credit_alert(AlertKind::Reset), &[key_refilled])),
            (
                "OpenRouter API key".into(),
                "Credits are back".into(),
                "$10 of $20 left.".into()
            )
        );
    }

    #[test]
    fn a_session_that_used_a_limit_says_its_share_project_and_agent() {
        let mut used_most = UsedMost {
            session: "codex:s".into(),
            title: Some("Fix it".into()),
            project: Some("app".into()),
            project_path: None,
            agent: "codex".into(),
            share_percent: Some(12.6),
        };
        assert_eq!(used(&used_most, true), "13% · app · Codex");
        used_most.share_percent = Some(0.2);
        assert_eq!(used(&used_most, false), "<1% · app");
        used_most.share_percent = None;
        assert_eq!(used(&used_most, false), "app");
    }
}
