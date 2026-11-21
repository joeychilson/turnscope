//! Every word the app shows: the panel, the menu bar item, notifications and what VoiceOver
//! says. Times are in the Mac's own time zone, as of the minute the panel last ticked.

use crate::standing::*;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};
use turnscope::alerts::{Alert, Kind as AlertKind};
use turnscope::engine::UsedMost;

/// A notification's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    /// The account.
    pub title: String,
    /// What happened: "Weekly runs out Thursday 9 AM".
    pub subtitle: String,
    /// When it's back, or what to do. Empty when nothing more is known.
    pub body: String,
}

pub struct Words {
    /// The minute it is, in milliseconds.
    pub(crate) now: i64,
    tz: TimeZone,
}

impl Words {
    pub fn new(now_ms: i64) -> Words {
        Words {
            now: now_ms.div_euclid(60_000) * 60_000,
            tz: TimeZone::system(),
        }
    }

    // Figures

    fn dollars(&self, value: f64, down: bool) -> String {
        let value = value.max(0.0);
        if value == 0.0 {
            return "$0".into();
        }
        let hair = 1e-9;
        let cents = if down {
            (value * 100.0 + hair).floor()
        } else {
            (value * 100.0).round()
        };
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

    pub fn left(&self, limit: &Limit) -> String {
        match &limit.money {
            Some(money) => self.dollars(money.left_usd, true),
            None => format!("{}%", limit.left_percent),
        }
    }

    /// What's left, and of how much where that's money: "$10 of $20", "60%".
    pub fn left_of(&self, limit: &Limit) -> String {
        match self.size(limit) {
            Some(size) => format!("{} {size}", self.left(limit)),
            None => self.left(limit),
        }
    }

    fn unknown(&self, spend: &Spend) -> bool {
        spend.partial && spend.month_usd == 0.0
    }

    fn spent_figure(&self, spend: &Spend) -> String {
        if self.unknown(spend) {
            "—".into()
        } else {
            self.dollars(spend.month_usd, false)
        }
    }

    fn spent_line(&self, spend: &Spend) -> String {
        if self.unknown(spend) {
            return "No limit · cost unknown".into();
        }
        if spend.partial {
            "No limit · some unpriced".into()
        } else {
            "No limit".into()
        }
    }

    fn pace(&self, spend: &Spend) -> Option<String> {
        let forecast = spend.forecast_usd?;
        if self.unknown(spend)
            || self.dollars(forecast, false) == self.dollars(spend.month_usd, false)
        {
            return None;
        }
        Some(format!(
            "On pace for about {} this month.",
            self.dollars(forecast, false)
        ))
    }

    // Time

    fn zoned(&self, ms: i64) -> Zoned {
        Timestamp::from_millisecond(ms)
            .unwrap_or_default()
            .to_zoned(self.tz.clone())
    }

    fn date(&self, ms: i64) -> Date {
        self.zoned(ms).date()
    }

    fn same_day(&self, a: i64, b: i64) -> bool {
        self.date(a) == self.date(b)
    }

    /// How long until `at`, as the command line says it: "2h 10m". Past is "0m": a status can
    /// be minutes old, as only one that changed is told.
    fn until(&self, at: i64) -> String {
        turnscope::time::span((at - self.now).max(0))
    }

    /// When `moment` is, as the panel says it: "in 2h 10m", "3 PM", "Thu 9 AM", "Oct 30".
    fn at(&self, moment: &Moment) -> String {
        self.at_horizon(moment.at, moment.horizon, false)
    }

    /// When `at` is, as near as `horizon` says, with the day and month spelled out if `full`.
    fn at_horizon(&self, at: i64, horizon: Horizon, full: bool) -> String {
        match horizon {
            Horizon::Soon => format!("in {}", self.until(at)),
            Horizon::Today => self.time(at),
            Horizon::ThisWeek => {
                let day = if self.tomorrow(at) {
                    "tomorrow".into()
                } else {
                    self.weekday(at, full)
                };
                format!("{day} {}", self.time(at))
            }
            Horizon::Later => self.month_day(at, full),
        }
    }

    /// When `moment` is, for text that stays on screen, as a notification does: what's soon
    /// is given as the time of day it falls on. `full` spells out the day and month.
    fn clock(&self, moment: &Moment, full: bool) -> String {
        if moment.horizon != Horizon::Soon {
            return self.at_horizon(moment.at, moment.horizon, full);
        }
        let horizon = if self.same_day(moment.at, self.now) {
            Horizon::Today
        } else {
            Horizon::ThisWeek
        };
        self.at_horizon(moment.at, horizon, full)
    }

    fn ago(&self, ms: i64) -> String {
        let time = self.time(ms);
        if self.same_day(ms, self.now) {
            return time;
        }
        if self.date(self.now).yesterday().ok() == Some(self.date(ms)) {
            return format!("yesterday {time}");
        }
        if ms < self.now && self.now - ms < 6 * 86_400_000 {
            return format!("{} {time}", self.weekday(ms, false));
        }
        self.month_day(ms, false)
    }

    fn tomorrow(&self, ms: i64) -> bool {
        self.date(self.now).tomorrow().ok() == Some(self.date(ms))
    }

    fn day(&self, ms: i64) -> String {
        if self.same_day(ms, self.now) {
            return "today".into();
        }
        if self.tomorrow(ms) {
            return "tomorrow".into();
        }
        self.weekday(ms, true)
    }

    fn when(&self, ms: i64) -> String {
        let part = self.part_of_day(ms);
        let zoned = self.zoned(ms);
        // Before 5 AM is the night of the date before. By the date, not 24
        // hours back, which after a 23-hour day lands two dates back.
        let day = match zoned.yesterday() {
            Ok(before) if part == "night" && zoned.hour() < 5 && !self.same_day(ms, self.now) => {
                before.timestamp().as_millisecond()
            }
            _ => ms,
        };
        let today = self.same_day(day, self.now);
        if part == "noon" {
            return if today {
                "around noon".into()
            } else {
                format!("{} around noon", self.day(day))
            };
        }
        if !today {
            return format!("{} {part}", self.day(day));
        }
        if part == "night" {
            "tonight".into()
        } else {
            format!("this {part}")
        }
    }

    fn part_of_day(&self, ms: i64) -> &'static str {
        let z = self.zoned(ms);
        match z.hour() {
            12 if z.minute() < 30 => "noon",
            5..=11 => "morning",
            12..=16 => "afternoon",
            17..=21 => "evening",
            _ => "night",
        }
    }

    fn time(&self, ms: i64) -> String {
        let z = self.zoned(ms);
        let format = if z.minute() == 0 {
            "%-I %p"
        } else {
            "%-I:%M %p"
        };
        z.strftime(format).to_string()
    }

    fn weekday(&self, ms: i64, full: bool) -> String {
        self.zoned(ms)
            .strftime(if full { "%A" } else { "%a" })
            .to_string()
    }

    /// "Oct 30", or "October 30" if `full`.
    fn month_day(&self, ms: i64, full: bool) -> String {
        self.zoned(ms)
            .strftime(if full { "%B %-d" } else { "%b %-d" })
            .to_string()
    }

    fn counted(&self, count: usize, word: &str) -> String {
        format!("{count} {word}{}", if count == 1 { "" } else { "s" })
    }

    // Limits and the panel

    pub fn name(&self, limit: &Limit) -> String {
        if !limit.held_by.is_empty() {
            let names: Vec<&str> = limit
                .held_by
                .iter()
                .map(|a| turnscope::agents::name(a))
                .collect();
            return format!("{}'s key", list(&names, "and"));
        }
        match &limit.scope {
            Some(scope) => format!("{scope} {}", limit.name.to_lowercase()),
            None => limit.name.clone(),
        }
    }

    fn in_sentence(&self, limit: &Limit) -> String {
        if limit.scope.is_none() && limit.held_by.is_empty() {
            self.name(limit).to_lowercase()
        } else {
            self.name(limit)
        }
    }

    fn short(&self, limit: &Limit) -> String {
        let name = self.name(limit);
        match &limit.outlook {
            Outlook::UsedUp { back } => format!(
                "{name} back {}",
                back.as_ref().map_or("later".into(), |b| self.at(b))
            ),
            Outlook::RunsOut { likely, .. } => {
                format!(
                    "{name} {} out {}",
                    if self.plural(limit) { "run" } else { "runs" },
                    self.at(likely)
                )
            }
            Outlook::Lasts { .. } | Outlook::Unknown { .. } => {
                if let Some(window) = &limit.window {
                    return format!("{name} resets {}", self.at(&window.resets));
                }
                if limit.money.is_none() {
                    format!("{name} lasts")
                } else {
                    name
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
        if account.state.why() == Some(Stale::Expired) {
            let agent = account
                .agents
                .first()
                .map_or("its agent", |a| turnscope::agents::name(a));
            return format!("Open {agent} to renew its login");
        }
        if let Some(limit) = account.deciding_limit() {
            return match limit.reset_since {
                Some(reset) => format!("{} reset {}", self.name(limit), self.ago(reset)),
                None => self.short(limit),
            };
        }
        if let Some(spend) = &account.spend {
            return self.spent_line(spend);
        }
        match account.state.why() {
            Some(Stale::ReadFailed) => "Couldn't be read yet".into(),
            Some(Stale::SignedOut) => "Signed out".into(),
            _ => "Not read yet".into(),
        }
    }

    /// The figure on an account's right, and the quieter text after it.
    pub fn row_figure(&self, account: &Account) -> (String, Option<String>) {
        if let Some(limit) = account.deciding_limit() {
            return (self.left(limit), self.size(limit));
        }
        if let Some(spend) = &account.spend {
            return (
                self.spent_figure(spend),
                if self.unknown(spend) {
                    None
                } else {
                    Some("this month".into())
                },
            );
        }
        ("—".into(), None)
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

    pub fn size(&self, limit: &Limit) -> Option<String> {
        limit
            .money
            .as_ref()
            .map(|m| format!("of {}", self.dollars(m.size_usd, false)))
    }

    pub fn of(&self, limit: &Limit) -> String {
        if limit.scope.is_some() || !limit.held_by.is_empty() {
            return format!("of {}", self.name(limit));
        }
        match limit.name.to_lowercase().as_str() {
            "weekly" => "this week".into(),
            "monthly" => "this month".into(),
            _ => format!("of {}", self.in_sentence(limit)),
        }
    }

    /// The one thing to know, in large type, and the detail behind it.
    pub fn verdict(&self, in_use: &[&Account], hidden_in_use: bool) -> (String, String, Standing) {
        let Some(account) = in_use.first() else {
            let detail = if hidden_in_use {
                "Only accounts you hid are in use."
            } else {
                ""
            };
            return ("Nothing in use".into(), detail.into(), Standing::Lasts);
        };
        let Some(limit) = account.deciding_limit() else {
            if account.refused() {
                let open = account
                    .agents
                    .first()
                    .map(|a| format!("Open {} to sign in.", turnscope::agents::name(a)));
                return (
                    format!("Sign in to {} again", account.title),
                    open.unwrap_or("Its limits can't be read until then.".into()),
                    Standing::Lasts,
                );
            }
            if let Some(spend) = &account.spend {
                let detail = self
                    .pace(spend)
                    .unwrap_or(format!("Your {} has no limit.", account.title));
                return ("You can keep going".into(), detail, Standing::Lasts);
            }
            let detail = self
                .trouble(account)
                .unwrap_or("They aren't read yet.".into());
            return (
                format!("{}'s limits are unknown", account.title),
                detail,
                Standing::Lasts,
            );
        };
        match &limit.outlook {
            Outlook::UsedUp { back } => {
                let after = back
                    .as_ref()
                    .map_or("It isn't known when it's back.".into(), |b| {
                        format!("Back {}.", self.at(b))
                    });
                (
                    format!("{} is used up", account.title),
                    after,
                    Standing::UsedUp,
                )
            }
            Outlook::RunsOut { likely, .. } => {
                let when = if likely.horizon == Horizon::Soon {
                    self.at(likely)
                } else {
                    self.when(likely.at)
                };
                let headline = format!("{} runs out {}", account.title, unbroken(&when));
                let back = limit
                    .window
                    .as_ref()
                    .map_or(String::new(), |w| format!("Back {}.", self.at(&w.resets)));
                (headline, back, Standing::RunningOut)
            }
            Outlook::Unknown { reason } => (
                "You can keep going".into(),
                self.unforecast(limit, account, *reason),
                Standing::Lasts,
            ),
            Outlook::Lasts { .. } => (
                "You can keep going".into(),
                self.lasting(limit, account),
                Standing::Lasts,
            ),
        }
    }

    fn unforecast(&self, limit: &Limit, account: &Account, reason: Unknown) -> String {
        let what = if limit.money.is_none() {
            format!("{} left {}", self.left(limit), self.of(limit))
        } else {
            format!("{} left", self.left_of(limit))
        };
        let standing = format!("{} has {what}", account.title);
        match reason {
            Unknown::NotEnoughData | Unknown::Rolling => format!("{standing}."),
            Unknown::Stale => format!("As last read, {standing}."),
            Unknown::NoReset => format!("{standing}; it doesn't reset."),
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
                self.in_sentence(limit)
            )
        } else {
            format!("{} lasts until it resets", self.in_sentence(limit))
        };
        let sentence = if current {
            format!("At this pace, {lasts}.")
        } else {
            format!("As last read, {lasts}.")
        };
        let used_up = others
            .iter()
            .filter(|l| l.standing() == Standing::UsedUp)
            .max_by_key(|l| back_at(l));
        let Some(other) =
            used_up.or_else(|| others.iter().find(|l| l.standing() != Standing::Lasts))
        else {
            return sentence;
        };
        match &other.outlook {
            Outlook::UsedUp { back } => format!(
                "{sentence} {} {} used up{}.",
                self.name(other),
                if self.plural(other) { "are" } else { "is" },
                back.as_ref()
                    .map_or(String::new(), |b| format!(", back {}", self.at(b)))
            ),
            Outlook::RunsOut { likely, .. } => format!(
                "{sentence} {} {} out {}.",
                self.name(other),
                if self.plural(other) { "run" } else { "runs" },
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
        if account.refilled_at().is_some() {
            return Some("Full since the reset, unless used on the web or another device.".into());
        }
        if !account.limits.is_empty() {
            return None;
        }
        account.spend.as_ref().and_then(|s| self.pace(s))
    }

    pub fn welcome(
        &self,
        agents: &[&Agent],
        reads: &[&str],
        reading: bool,
    ) -> (String, String, Standing) {
        if reading {
            return (
                "Getting your limits".into(),
                "They show up in a moment.".into(),
                Standing::Lasts,
            );
        }
        if agents.is_empty() {
            return (
                "No agents found".into(),
                format!(
                    "Turnscope reads {}. Sign in to one, and its limits show up.",
                    list(reads, "and")
                ),
                Standing::Lasts,
            );
        }
        let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        (
            "No accounts found yet".into(),
            format!(
                "Once {} is signed in, its limits show up.",
                list(&names, "or")
            ),
            Standing::Lasts,
        )
    }

    fn trouble(&self, account: &Account) -> Option<String> {
        let with = account
            .agents
            .first()
            .map_or("its agent", |a| turnscope::agents::name(a));
        Some(match &account.state {
            State::Unread { why: Stale::SignIn } => format!("Open {with} to sign in again."),
            State::Unread {
                why: Stale::Expired,
            } => format!("Its login expired. Open {with} to renew it."),
            State::Unread {
                why: Stale::ReadFailed,
            } => "It couldn't be read yet.".into(),
            State::Unread {
                why: Stale::SignedOut,
            } => "No agent here is signed in to it.".into(),
            State::AsOf {
                read_at,
                why: Stale::SignIn,
            } => {
                format!("Last read {}. Open {with} to sign in.", self.ago(*read_at))
            }
            State::AsOf {
                read_at,
                why: Stale::ReadFailed,
            } => {
                format!("It couldn't be read since {}.", self.ago(*read_at))
            }
            State::AsOf {
                read_at,
                why: Stale::Expired,
            } => {
                format!(
                    "Last read {}. Open {with} to renew its login.",
                    self.ago(*read_at)
                )
            }
            State::AsOf {
                read_at,
                why: Stale::SignedOut,
            } if account.refilled_at().is_none() => {
                format!(
                    "Last read {}. No agent here is signed in to it.",
                    self.ago(*read_at)
                )
            }
            _ => return None,
        })
    }

    pub fn unused(&self, count: usize, beside_others: bool) -> String {
        self.counted(
            count,
            if beside_others {
                "more account"
            } else {
                "account"
            },
        )
    }

    pub fn hidden(&self, count: usize) -> String {
        format!("{count} hidden")
    }

    /// What the menu bar shows for an account: what's left, how long it has once that's soon,
    /// or when it's back.
    pub fn figure(&self, account: &Account) -> String {
        let Some(limit) = account.deciding_limit() else {
            if account.refused() {
                return "Sign in".into();
            }
            return account
                .spend
                .as_ref()
                .map_or("—".into(), |s| self.spent_figure(s));
        };
        match &limit.outlook {
            Outlook::UsedUp { back } => back.as_ref().map_or("Used up".into(), |b| {
                format!("Back {}", self.clock(b, false))
            }),
            Outlook::RunsOut { likely, .. } if likely.horizon == Horizon::Soon => {
                self.until(likely.at)
            }
            _ => self.left(limit),
        }
    }

    /// What's wrong with how `agent` is connected, if anything, said under its name: "Uses
    /// another Turnscope".
    pub fn connection(&self, agent: &Agent) -> Option<String> {
        match agent.connection {
            Connection::Connected => None,
            Connection::Available => Some("Isn't connected".into()),
            Connection::Outdated => Some("Uses another Turnscope".into()),
        }
    }

    /// The agents that use `account`: "via Claude Code and Codex".
    pub fn via(&self, account: &Account) -> Option<String> {
        if account.agents.is_empty() {
            return None;
        }
        let names: Vec<&str> = account
            .agents
            .iter()
            .map(|a| turnscope::agents::name(a))
            .collect();
        Some(format!("via {}", list(&names, "and")))
    }

    /// The line under a session that used a limit most: its share, its project, and its
    /// agent if the sessions ran in more than one (`agents`). "34% · turnscope · Codex". The
    /// share is worded as the command line and MCP server word it, which round half to even:
    /// 12.5 is 12%, where the app's own wording had 13%.
    pub fn used(&self, used: &UsedMost, agents: bool) -> String {
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

    /// What Hide and Show call `account`: its title, and its label too where its row shows
    /// it (`labelled`), "Claude Max (me@example.com)", so two accounts with the same title are
    /// told apart as their rows are.
    pub fn called(&self, account: &Account, labelled: bool) -> String {
        if labelled {
            account.full_title()
        } else {
            account.title.clone()
        }
    }

    /// What a notification calls `account`, among `accounts`: its title, and its label too
    /// when another account in use has the same title. A notification stands alone, so unlike
    /// the headline it can't point to a row. Only an account in use counts: an idle account
    /// with the same title can't be taken for one in use.
    fn called_among(&self, account: &Account, accounts: &[Account]) -> String {
        self.called(
            account,
            account.twinned(accounts.iter().filter(|a| a.in_use)),
        )
    }

    /// What VoiceOver says for the menu bar item: each account it shows, with its figure.
    pub fn spoken_item(&self, accounts: &[&Account], out_of_date: bool) -> String {
        let said = if accounts.is_empty() {
            "Turnscope".to_owned()
        } else {
            accounts
                .iter()
                .map(|a| format!("{}, {}", a.title, self.spoken_figure(a)))
                .collect::<Vec<_>>()
                .join("; ")
        };
        if out_of_date {
            format!("{said}, not up to date")
        } else {
            said
        }
    }

    /// An account's figure in the menu bar, as a sentence VoiceOver can say.
    fn spoken_figure(&self, account: &Account) -> String {
        let Some(limit) = account.deciding_limit() else {
            if account.refused() {
                return "sign in again".into();
            }
            return account
                .spend
                .as_ref()
                .map_or("not read yet".into(), |s| self.spent(s));
        };
        match &limit.outlook {
            Outlook::UsedUp { back } => back.as_ref().map_or("used up".into(), |b| {
                format!("back {}", self.clock(b, true))
            }),
            Outlook::RunsOut { likely, .. } if likely.horizon == Horizon::Soon => {
                format!("runs out in {}", self.spoken(likely.at))
            }
            Outlook::RunsOut { .. } => format!("{} left, running out", self.left(limit)),
            Outlook::Lasts { .. } | Outlook::Unknown { .. } => format!("{} left", self.left(limit)),
        }
    }

    /// How long until `at`, as it's said: "1 hour 5 minutes". Rounded to the minute, as
    /// `until` is, so VoiceOver says what's shown.
    fn spoken(&self, at: i64) -> String {
        let minutes = (((at - self.now).max(0) + 30_000) / 60_000) as usize;
        let (hours, rest) = (minutes / 60, minutes % 60);
        if hours == 0 {
            return self.counted(rest, "minute");
        }
        if rest == 0 {
            self.counted(hours, "hour")
        } else {
            format!(
                "{} {}",
                self.counted(hours, "hour"),
                self.counted(rest, "minute")
            )
        }
    }

    /// What an account with no limit spent this month, as a phrase.
    fn spent(&self, spend: &Spend) -> String {
        if self.unknown(spend) {
            return "Cost unknown".into();
        }
        format!(
            "{}{} this month",
            if spend.partial { "At least " } else { "" },
            self.dollars(spend.month_usd, false)
        )
    }

    /// What an alert's notification says. The title is the account, named as the panel does.
    /// The subtitle is what happened, worded like the limit's line in the panel. The body is
    /// when it's back, or what to do. An alert's own title, as it was raised, is only for an
    /// account no longer in the status. Times are clock times, since a notification stays on
    /// screen.
    pub fn note(&self, alert: &Alert, accounts: &[Account]) -> Note {
        let account = accounts.iter().find(|a| a.id == alert.account);
        let title = account.map_or_else(
            || alert.account_title.clone(),
            |a| self.called_among(a, accounts),
        );
        let limit = account.and_then(|a| {
            a.limits
                .iter()
                .find(|l| Some(&l.key) == alert.limit.as_ref())
        });
        let name = limit
            .map(|l| self.name(l))
            .or_else(|| alert.limit_name.clone())
            .unwrap_or("A limit".into());
        let plural = limit.is_some_and(|l| self.plural(l));
        let (runs, be) = if plural {
            ("run", "are")
        } else {
            ("runs", "is")
        };
        let back = alert
            .resets
            .as_ref()
            .map_or(String::new(), |r| format!("Back {}.", self.clock(r, true)));
        match alert.kind {
            AlertKind::SignIn => {
                let agent = account
                    .and_then(|a| a.agents.first())
                    .map(|a| turnscope::agents::name(a));
                Note {
                    title,
                    subtitle: "Sign in again".into(),
                    body: agent.map_or(String::new(), |a| format!("Open {a} to sign in.")),
                }
            }
            AlertKind::RunningOut => {
                let out = alert
                    .runs_out
                    .as_ref()
                    .map_or("soon".into(), |r| self.clock(r, true));
                Note {
                    title,
                    subtitle: format!("{name} {runs} out {out}"),
                    body: back,
                }
            }
            AlertKind::UsedUp => Note {
                title,
                subtitle: format!("{name} {be} used up"),
                body: back,
            },
            AlertKind::Reset => {
                let room = limit.map_or(String::new(), |l| format!("{} left.", self.left_of(l)));
                Note {
                    title,
                    subtitle: format!("{name} {be} back"),
                    body: room,
                }
            }
        }
    }

    /// Whether `limit`'s name, as `name` gives it, is plural: as the command line's
    /// (`Limit::plural`), except that a limit named for the agents holding its key is one key.
    fn plural(&self, limit: &Limit) -> bool {
        limit.held_by.is_empty() && limit.plural()
    }
}

fn back_at(limit: &Limit) -> i64 {
    match &limit.outlook {
        Outlook::UsedUp { back: Some(b) } => b.at,
        _ => i64::MAX,
    }
}

fn list(names: &[&str], last: &str) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., final_] => format!("{} {last} {final_}", rest.join(", ")),
    }
}

fn unbroken(phrase: &str) -> String {
    phrase.replace(' ', "\u{00A0}")
}

// How what the engine decided is put into words, as of 12:00 UTC on Wednesday 30 September
// 2026, in UTC.
#[cfg(test)]
mod tests {
    use super::*;
    use Horizon::*;
    use turnscope::providers::Kind;
    use turnscope::status::{LimitWindow, Money};

    fn at(text: &str) -> i64 {
        text.parse::<Timestamp>().unwrap().as_millisecond()
    }

    fn noon() -> i64 {
        at("2026-09-30T12:00:00Z")
    }

    /// Words as of `now`, in `tz`.
    fn as_of_in(now: i64, tz: TimeZone) -> Words {
        Words {
            tz,
            ..Words::new(now)
        }
    }

    /// Words as of `now`, in UTC.
    fn as_of(now: i64) -> Words {
        as_of_in(now, TimeZone::UTC)
    }

    fn words() -> Words {
        as_of(noon())
    }

    /// The time `text` with `horizon`, as the engine gives a time to come.
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
            latest: None,
        }
    }

    /// Likely to run out at 14:00.
    fn runs_out() -> Outlook {
        runs_out_at(moment("2026-09-30T14:00:00Z", Soon))
    }

    fn used_up(back: Option<Moment>) -> Outlook {
        Outlook::UsedUp { back }
    }

    fn unknown(reason: Unknown) -> Outlook {
        Outlook::Unknown { reason }
    }

    /// A five-hour limit that resets at 15:00, with 20% left and the outlook `outlook`.
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

    /// A weekly limit that resets at midnight on Sunday.
    fn weekly(outlook: Outlook, left: u8) -> Limit {
        Limit {
            key: "seven_day".into(),
            name: "Weekly".into(),
            left_percent: left,
            window: window("2026-10-04T00:00:00Z", ThisWeek),
            ..limit(outlook)
        }
    }

    /// Credits that never reset.
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

    /// Claude Max, in use through Claude Code, read just now.
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

    fn agent(id: &str, name: &str, connection: Connection) -> Agent {
        Agent {
            id: id.into(),
            name: name.into(),
            installed: true,
            connection,
            in_use: false,
        }
    }

    fn some(text: &str) -> Option<String> {
        Some(text.into())
    }

    #[test]
    fn times_are_said_as_their_horizon_calls_for() {
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
        // Spelled out, for VoiceOver and a notification.
        assert_eq!(
            w.clock(&moment("2026-10-03T19:50:00Z", ThisWeek), true),
            "Saturday 7:50 PM"
        );
        assert_eq!(
            w.clock(&moment("2026-10-18T16:37:00Z", Later), true),
            "October 18"
        );
        // Roughly, as a headline words a forecast.
        assert_eq!(w.when(at("2026-10-03T19:51:00Z")), "Saturday evening");
        assert_eq!(w.when(at("2026-10-01T00:40:00Z")), "tonight");
        assert_eq!(w.ago(at("2026-09-30T00:00:00Z")), "12 AM");
        assert_eq!(w.ago(at("2026-09-29T23:59:00Z")), "yesterday 11:59 PM");
        assert_eq!(w.until(w.now + 52 * 3_600_000), "2d 4h");
        assert_eq!(w.until(w.now - 60_000), "0m");
    }

    #[test]
    fn times_are_in_the_macs_time_zone() {
        let w = as_of_in(noon(), TimeZone::get("America/Chicago").unwrap());
        assert_eq!(w.at(&moment("2026-10-18T16:37:00Z", Later)), "Oct 18");
        assert_eq!(w.ago(at("2026-09-22T12:00:00Z")), "Sep 22");
        // Five hours behind UTC.
        assert_eq!(
            w.at(&moment("2026-10-03T19:50:00Z", ThisWeek)),
            "Sat 2:50 PM"
        );
        // Past midnight is the night before, also after a 23-hour day: the
        // clocks spring forward on Sunday 14 March 2027.
        let w = as_of_in(
            at("2027-03-11T18:00:00Z"),
            TimeZone::get("America/Chicago").unwrap(),
        );
        assert_eq!(w.when(at("2027-03-15T05:30:00Z")), "Sunday night");
    }

    #[test]
    fn amounts_are_grouped_and_what_is_left_is_rounded_down() {
        let w = words();
        assert_eq!(w.dollars(0.0, false), "$0");
        assert_eq!(w.dollars(9.956, false), "$9.96");
        assert_eq!(w.dollars(9.956, true), "$9.95");
        assert_eq!(w.dollars(9.996, false), "$10");
        assert_eq!(w.dollars(9.996, true), "$9.99");
        // Rounding down allows a hair over, so 9.95, held as 9.9499…, doesn't lose a cent.
        assert_eq!(w.dollars(9.95, true), "$9.95");
        assert_eq!(w.dollars(84.9, true), "$84");
        assert_eq!(w.dollars(1250.4, false), "$1,250");
        assert_eq!(w.dollars(1_234_567.9, false), "$1,234,568");
    }

    #[test]
    fn a_limit_says_when_it_resets_and_when_it_runs_out() {
        let w = words();
        // When it resets and when it runs out are separate phrases; the row puts both beside
        // its name.
        assert_eq!(w.window(&limit(runs_out())), some("Resets in 3h"));
        assert_eq!(w.ahead(&limit(runs_out())), some("Runs out in 2h"));
        let week = weekly(lasts(), 60);
        assert_eq!(w.window(&week), some("Resets Sun 12 AM"));
        assert_eq!(w.ahead(&week), None);
        assert_eq!(w.short(&week), "Weekly resets Sun 12 AM");
        assert_eq!(w.of(&week), "this week");
        // Used up: when it's back replaces when it resets, and there's no "Runs out".
        let spent = Limit {
            left_percent: 0,
            ..limit(used_up(Some(moment("2026-09-30T15:00:00Z", Soon))))
        };
        assert_eq!(w.window(&spent), some("Back in 3h"));
        assert_eq!(w.ahead(&spent), None);
        assert_eq!(
            w.ahead(&Limit {
                left_percent: 99,
                ..limit(unknown(Unknown::NotEnoughData))
            }),
            None
        );
        // Reset since it was read, so it's full, as the engine says.
        let reset = Limit {
            left_percent: 100,
            reset_since: Some(at("2026-09-30T11:00:00Z")),
            ..limit(unknown(Unknown::Stale))
        };
        assert_eq!(w.window(&reset), some("Reset 11 AM"));
        assert_eq!(w.ahead(&reset), None);
        assert_eq!(w.left(&reset), "100%");
        let money = credits(29, 30.0, 8.705, unknown(Unknown::NoReset));
        assert_eq!(w.left(&money), "$8.70");
        assert_eq!(w.size(&money), some("of $30"));
        assert_eq!((w.window(&money), w.ahead(&money)), (None, None));
    }

    #[test]
    fn an_account_whose_every_limit_reset_speaks_for_the_last_to_reset() {
        let w = words();
        let five = Limit {
            left_percent: 100,
            reset_since: Some(at("2026-09-30T11:00:00Z")),
            ..limit(unknown(Unknown::Stale))
        };
        let week = Limit {
            reset_since: Some(at("2026-09-30T11:30:00Z")),
            ..weekly(unknown(Unknown::Stale), 100)
        };
        let full = Account {
            in_use: false,
            recent: false,
            deciding: some("seven_day"),
            ..account(vec![five, week])
        };
        assert_eq!(full.refilled_at(), Some(at("2026-09-30T11:30:00Z")));
        assert_eq!(w.status(&full), "Weekly reset 11:30 AM");
        assert_eq!(w.row_figure(&full), ("100%".into(), None));
        assert_eq!(
            w.opened(&full, false),
            some("Full since the reset, unless used on the web or another device.")
        );
    }

    #[test]
    fn the_verdict_says_the_most_urgent_and_when_its_back() {
        let w = words();
        let out = account(vec![limit(runs_out())]);
        assert_eq!(
            w.verdict(&[&out], false),
            (
                "Claude Max runs out in\u{a0}2h".into(),
                "Back in 3h.".into(),
                Standing::RunningOut
            )
        );
        let may_last = account(vec![Limit {
            window: window("2026-10-04T00:00:00Z", ThisWeek),
            ..limit(runs_out_at(moment("2026-10-03T06:00:00Z", ThisWeek)))
        }]);
        let (headline, detail, _) = w.verdict(&[&may_last], false);
        assert_eq!(headline, "Claude Max runs out Saturday\u{a0}morning");
        assert_eq!(detail, "Back Sun 12 AM.");
        let used = account(vec![Limit {
            left_percent: 0,
            ..limit(used_up(Some(moment("2026-09-30T15:00:00Z", Soon))))
        }]);
        assert_eq!(
            w.verdict(&[&used], false),
            (
                "Claude Max is used up".into(),
                "Back in 3h.".into(),
                Standing::UsedUp
            )
        );
        let fine = account(vec![Limit {
            left_percent: 80,
            ..limit(lasts())
        }]);
        assert_eq!(
            w.verdict(&[&fine], false).1,
            "At this pace, no Claude Max limit runs out."
        );
        // Nothing in use, so the accounts below say the rest.
        assert_eq!(
            w.verdict(&[], false),
            ("Nothing in use".into(), String::new(), Standing::Lasts)
        );
        assert_eq!(w.verdict(&[], true).1, "Only accounts you hid are in use.");
        // A subscription in use with no limit read: whether it has room isn't known, so it
        // says why.
        let new = Account {
            state: State::Unread {
                why: Stale::ReadFailed,
            },
            ..account(vec![])
        };
        assert_eq!(
            w.verdict(&[&new], false),
            (
                "Claude Max's limits are unknown".into(),
                "It couldn't be read yet.".into(),
                Standing::Lasts
            )
        );
        let gone = Account {
            state: State::Unread {
                why: Stale::SignedOut,
            },
            ..account(vec![])
        };
        assert_eq!(
            w.verdict(&[&gone], false).1,
            "No agent here is signed in to it."
        );
    }

    #[test]
    fn an_account_is_told_apart_only_where_it_has_to() {
        let w = words();
        let mine = Account {
            label: some("me@example.com"),
            ..account(vec![limit(runs_out())])
        };
        let mut twin = Account {
            id: "anthropic:b".into(),
            label: some("work@example.com"),
            ..account(vec![Limit {
                left_percent: 90,
                ..limit(lasts())
            }])
        };
        let other = Account {
            id: "openai:a".into(),
            title: "ChatGPT Pro".into(),
            label: some("me@example.com"),
            ..account(vec![limit(runs_out())])
        };
        // The headline names the account by its title alone, even beside another in use:
        // it's about the first row under it.
        assert_eq!(
            w.verdict(&[&mine, &twin], false).0,
            "Claude Max runs out in\u{a0}2h"
        );
        // A notification adds the label only beside another in use, and never beside a
        // hidden one.
        assert_eq!(
            w.called_among(&mine, &[mine.clone(), twin.clone()]),
            "Claude Max (me@example.com)"
        );
        twin.hidden = true;
        assert_eq!(
            w.called_among(&mine, &[mine.clone(), twin.clone()]),
            "Claude Max"
        );
        twin.hidden = false;
        twin.in_use = false;
        assert_eq!(
            w.called_among(&mine, &[mine.clone(), twin.clone()]),
            "Claude Max"
        );
        // A row, and its Hide and Show buttons, add the label beside any shown account of the
        // same title, in use or not.
        let all = [mine.clone(), twin.clone(), other.clone()];
        let row = |account: &Account| w.called(account, account.twinned(&all));
        assert_eq!(row(&mine), "Claude Max (me@example.com)");
        assert_eq!(row(&twin), "Claude Max (work@example.com)");
        assert_eq!(row(&other), "ChatGPT Pro");
        // Where every row shows its label, as in Settings.
        assert_eq!(w.called(&other, true), "ChatGPT Pro (me@example.com)");
    }

    #[test]
    fn the_menu_bar_says_what_is_left_or_how_long_or_when_it_is_back() {
        let w = words();
        assert_eq!(
            w.figure(&account(vec![Limit {
                left_percent: 67,
                ..limit(lasts())
            }])),
            "67%"
        );
        let used = account(vec![Limit {
            left_percent: 0,
            ..limit(used_up(Some(moment("2026-09-30T15:00:00Z", Soon))))
        }]);
        assert_eq!(w.figure(&used), "Back 3 PM");
        // Back soon, but after midnight, so the day is given, in the spoken figure too.
        let late = as_of(at("2026-09-30T22:30:00Z"));
        let overnight = account(vec![Limit {
            left_percent: 0,
            ..limit(used_up(Some(moment("2026-10-01T01:00:00Z", Soon))))
        }]);
        assert_eq!(late.figure(&overnight), "Back tomorrow 1 AM");
        assert_eq!(late.spoken_figure(&overnight), "back tomorrow 1 AM");
        // Back days later: the menu bar keeps the day short, and VoiceOver says it in full.
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
        assert_eq!(w.row_figure(&keys), ("$84".into(), some("this month")));
        assert_eq!(
            w.opened(&keys, false),
            some("On pace for about $210 this month.")
        );
        // Early in the month there's no forecast, so none is mentioned.
        assert_eq!(
            w.pace(&Spend {
                month_usd: 1.0,
                forecast_usd: None,
                partial: false
            }),
            None
        );
        assert_eq!(
            w.spent(&Spend {
                month_usd: 0.0,
                forecast_usd: Some(0.0),
                partial: true
            }),
            "Cost unknown"
        );
    }

    /// The minute's tick lands a little after the minute, and the panel opens at any second of
    /// it.
    #[test]
    fn a_countdown_counts_from_the_minute_it_is() {
        let claude = account(vec![limit(runs_out())]);
        for millis in [50, 40_000, 59_900] {
            let late = as_of(noon() + millis);
            assert_eq!(late.at(&moment("2026-09-30T14:00:00Z", Soon)), "in 2h");
            assert_eq!(late.figure(&claude), "2h");
            assert_eq!(late.spoken_figure(&claude), "runs out in 2 hours");
        }
        // Its last minute is a minute, not none.
        assert_eq!(as_of(at("2026-09-30T13:59:30Z")).figure(&claude), "1m");
        // Shown and said alike, to the nearest minute.
        let soon = account(vec![limit(runs_out_at(moment(
            "2026-09-30T12:49:40Z",
            Soon,
        )))]);
        let w = as_of(noon());
        assert_eq!(w.figure(&soon), "50m");
        assert_eq!(w.spoken_figure(&soon), "runs out in 50 minutes");
    }

    #[test]
    fn voice_over_reads_the_menu_bar_as_each_account_then_its_figure() {
        let w = words();
        let claude = account(vec![limit(runs_out())]);
        let keys = api_key(
            vec![],
            Some(Spend {
                month_usd: 84.2,
                forecast_usd: Some(210.0),
                partial: false,
            }),
        );
        assert_eq!(
            w.spoken_item(&[&claude, &keys], false),
            "Claude Max, runs out in 2 hours; OpenRouter API key, $84 this month"
        );
        assert_eq!(
            w.spoken_item(&[&claude], true),
            "Claude Max, runs out in 2 hours, not up to date"
        );
        assert_eq!(w.spoken_item(&[], false), "Turnscope");
    }

    #[test]
    fn a_session_that_used_a_limit_says_its_share_where_it_is_known() {
        let w = words();
        let mut used = UsedMost {
            session: "codex:s".into(),
            title: some("Fix it"),
            project: some("app"),
            agent: "codex".into(),
            share_percent: Some(12.6),
        };
        assert_eq!(w.used(&used, true), "13% · app · Codex");
        used.share_percent = Some(0.2);
        assert_eq!(w.used(&used, false), "<1% · app");
        // For a limit with no window start, the share can't be known.
        used.share_percent = None;
        assert_eq!(w.used(&used, false), "app");
    }

    #[test]
    fn an_opened_account_says_what_its_limits_cant() {
        let w = words();
        let as_of = |why: Stale, at: i64, limit: Limit| Account {
            state: State::AsOf { read_at: at, why },
            ..account(vec![limit])
        };
        let half = || Limit {
            left_percent: 50,
            ..limit(lasts())
        };
        assert_eq!(
            w.opened(&as_of(Stale::ReadFailed, noon(), half()), false),
            some("It couldn't be read since 12 PM.")
        );
        let gone = Account {
            in_use: false,
            recent: false,
            ..as_of(
                Stale::SignedOut,
                at("2026-09-30T09:00:00Z"),
                Limit {
                    left_percent: 50,
                    ..limit(unknown(Unknown::Stale))
                },
            )
        };
        assert_eq!(
            w.opened(&gone, false),
            some("Last read 9 AM. No agent here is signed in to it.")
        );
        assert_eq!(
            w.opened(&as_of(Stale::SignIn, noon(), half()), false),
            some("Last read 12 PM. Open Claude Code to sign in.")
        );
        // Every login expired. An agent renews it when next used, so no sign-in is needed.
        let expired = as_of(Stale::Expired, noon(), half());
        assert_eq!(
            w.opened(&expired, false),
            some("Last read 12 PM. Open Claude Code to renew its login.")
        );
        assert_eq!(w.status(&expired), "Open Claude Code to renew its login");
        assert!(!expired.refused());
        // Never read, each says why, and what to do where there's something.
        let lapsed = Account {
            state: State::Unread {
                why: Stale::Expired,
            },
            ..account(vec![])
        };
        assert_eq!(w.status(&lapsed), "Open Claude Code to renew its login");
        assert_eq!(
            w.opened(&lapsed, false),
            some("Its login expired. Open Claude Code to renew it.")
        );
        let unreachable = Account {
            state: State::Unread {
                why: Stale::ReadFailed,
            },
            ..account(vec![])
        };
        assert_eq!(w.status(&unreachable), "Couldn't be read yet");
        assert_eq!(
            w.opened(&unreachable, false),
            some("It couldn't be read yet.")
        );
        // When it runs out is on its limit lines, so it isn't repeated.
        assert_eq!(w.opened(&account(vec![limit(runs_out())]), false), None);
        // A key with no limit says so under its name, so it isn't repeated.
        assert_eq!(w.opened(&api_key(vec![], None), false), None);
        // Under the headline, an account with no limits leaves it to say.
        assert_eq!(w.opened(&unreachable, true), None);
    }

    fn alert(kind: AlertKind) -> Alert {
        Alert {
            id: 1,
            kind,
            account: "anthropic:a".into(),
            account_title: "Claude Max (me@example.com)".into(),
            limit: some("five_hour"),
            limit_name: some("5 hours"),
            replaces: "limit:anthropic:a:five_hour".into(),
            resets: Some(moment("2026-09-30T15:00:00Z", Soon)),
            runs_out: Some(moment("2026-09-30T14:00:00Z", Soon)),
        }
    }

    #[test]
    fn a_note_says_which_account_then_what_happened_then_what_it_means() {
        let w = words();
        let mut accounts = vec![
            Account {
                label: some("me@example.com"),
                ..account(vec![limit(runs_out())])
            },
            Account {
                id: "anthropic:b".into(),
                label: some("work@example.com"),
                in_use: false,
                recent: false,
                ..account(vec![])
            },
        ];
        // Named as the status has it now, not as the alert was raised.
        assert_eq!(
            w.note(&alert(AlertKind::RunningOut), &accounts),
            Note {
                title: "Claude Max".into(),
                subtitle: "5 hours runs out 2 PM".into(),
                body: "Back 3 PM.".into()
            }
        );
        let used = w.note(&alert(AlertKind::UsedUp), &accounts);
        assert_eq!(
            (used.subtitle.as_str(), used.body.as_str()),
            ("5 hours is used up", "Back 3 PM.")
        );
        let reset = w.note(&alert(AlertKind::Reset), &accounts);
        assert_eq!(
            (reset.subtitle.as_str(), reset.body.as_str()),
            ("5 hours is back", "20% left.")
        );
        let sign_in = w.note(&alert(AlertKind::SignIn), &accounts);
        assert_eq!(
            (sign_in.subtitle.as_str(), sign_in.body.as_str()),
            ("Sign in again", "Open Claude Code to sign in.")
        );
        // For an account not in the status, the title is the alert's name for it.
        assert_eq!(
            w.note(&alert(AlertKind::UsedUp), &[]).title,
            "Claude Max (me@example.com)"
        );
        // Another account in use with the same title: the label says which.
        accounts[1].in_use = true;
        assert_eq!(
            w.note(&alert(AlertKind::UsedUp), &accounts).title,
            "Claude Max (me@example.com)"
        );
        accounts[1].in_use = false;
        // Days later, as when a weekly limit runs out, given as a clock time with the day
        // spelled out.
        let days = Alert {
            resets: Some(moment("2026-10-03T00:00:00Z", ThisWeek)),
            runs_out: Some(moment("2026-10-02T09:00:00Z", ThisWeek)),
            ..alert(AlertKind::RunningOut)
        };
        let note = w.note(&days, &accounts);
        assert_eq!(
            (note.subtitle.as_str(), note.body.as_str()),
            ("5 hours runs out Friday 9 AM", "Back Saturday 12 AM.")
        );
    }

    #[test]
    fn a_note_for_credits_says_they_are_used_up_then_back_with_what_is_left() {
        let w = words();
        // Money that never resets: its alert has no reset, so no time it's back.
        let key = api_key(vec![credits(0, 10.0, 0.0, used_up(None))], None);
        let alert = |kind| Alert {
            account: "openrouter:api".into(),
            account_title: "OpenRouter API key".into(),
            limit: some("credits"),
            limit_name: some("Credits"),
            replaces: "limit:openrouter:api:credits".into(),
            resets: None,
            runs_out: None,
            ..alert(kind)
        };
        assert_eq!(
            w.note(&alert(AlertKind::UsedUp), std::slice::from_ref(&key)),
            Note {
                title: "OpenRouter API key".into(),
                subtitle: "Credits are used up".into(),
                body: String::new()
            }
        );
        // $10 more bought.
        let key = api_key(
            vec![credits(50, 20.0, 10.0, unknown(Unknown::NoReset))],
            None,
        );
        assert_eq!(
            w.note(&alert(AlertKind::Reset), &[key]),
            Note {
                title: "OpenRouter API key".into(),
                subtitle: "Credits are back".into(),
                body: "$10 of $20 left.".into()
            }
        );
    }

    #[test]
    fn before_any_account_the_welcome_says_why() {
        let w = words();
        let reads = ["Claude Code", "Codex"];
        assert_eq!(w.welcome(&[], &reads, true).0, "Getting your limits");
        assert_eq!(
            w.welcome(&[], &reads, false).1,
            "Turnscope reads Claude Code and Codex. Sign in to one, and its limits show up."
        );
        let codex = agent("codex", "Codex", Connection::Available);
        assert_eq!(w.welcome(&[&codex], &[], false).0, "No accounts found yet");
    }

    #[test]
    fn the_verdict_says_only_what_lasts_lasts() {
        let w = words();
        let week = weekly(lasts(), 60);
        // Its weekly limit lasts, but its Opus weekly limit, for one model, is used up.
        let opus = Limit {
            key: "seven_day_opus".into(),
            scope: some("Opus"),
            window: window("2026-10-03T09:00:00Z", ThisWeek),
            ..weekly(used_up(Some(moment("2026-10-03T09:00:00Z", ThisWeek))), 0)
        };
        let scoped = account(vec![week.clone(), opus]);
        assert_eq!(
            w.verdict(&[&scoped], false),
            (
                "You can keep going".into(),
                "At this pace, Claude Max's weekly lasts until it resets. Opus weekly is used up, back Sat 9 AM."
                    .into(),
                Standing::Lasts
            )
        );
        // Next to a limit that can't be forecast, it says only that the deciding one lasts.
        let money = credits(29, 30.0, 8.7, unknown(Unknown::NoReset));
        let mixed = account(vec![week.clone(), money.clone()]);
        assert_eq!(
            w.verdict(&[&mixed], false).1,
            "At this pace, Claude Max's weekly lasts until it resets."
        );
        // Credits used up are said as plural.
        let spent = credits(0, 30.0, 0.0, used_up(None));
        let spent = account(vec![week.clone(), spent]);
        assert_eq!(
            w.verdict(&[&spent], false).1,
            "At this pace, Claude Max's weekly lasts until it resets. Credits are used up."
        );
        // A key's own limit already says whose key it is.
        let held = Limit {
            key: "key-1".into(),
            name: "Daily".into(),
            left_percent: 60,
            held_by: vec!["pi".into()],
            ..limit(lasts())
        };
        let key = api_key(vec![held, money.clone()], None);
        assert_eq!(
            w.verdict(&[&key], false).1,
            "At this pace, Pi's key lasts until it resets."
        );
        // Money with no reset says so.
        let left = api_key(vec![money], None);
        assert_eq!(
            w.verdict(&[&left], false).1,
            "OpenRouter API key has $8.70 of $30 left; it doesn't reset."
        );
        // If it's as last read, it says so.
        let stale = Account {
            state: State::AsOf {
                read_at: noon(),
                why: Stale::ReadFailed,
            },
            ..account(vec![week])
        };
        assert_eq!(
            w.verdict(&[&stale], false).1,
            "As last read, no Claude Max limit runs out."
        );
        // A limit that can't be forecast is never said to last. What's left of it is given
        // instead.
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
                w.verdict(&[&unknown], false),
                ("You can keep going".into(), said.into(), Standing::Lasts)
            );
        }
        // When it's back is worded for its horizon, as in the row.
        let spent = account(vec![Limit {
            left_percent: 0,
            ..limit(used_up(Some(moment("2026-10-03T09:00:00Z", ThisWeek))))
        }]);
        assert_eq!(w.verdict(&[&spent], false).1, "Back Sat 9 AM.");
    }

    #[test]
    fn of_other_limits_used_up_the_verdict_names_the_last_back() {
        let w = words();
        // A plan whose own weekly limit lasts, with both its limits on Claude and GPT used up:
        // 5 hours back in an hour, weekly on Saturday. Those models can't be used until
        // Saturday.
        let week = Limit {
            key: "weekly".into(),
            ..weekly(lasts(), 60)
        };
        let five = |outlook| Limit {
            key: "5h:3p-5h".into(),
            left_percent: 0,
            scope: some("Claude and GPT"),
            window: window("2026-09-30T13:00:00Z", Soon),
            ..limit(outlook)
        };
        let used_weekly = Limit {
            key: "weekly:3p-weekly".into(),
            scope: some("Claude and GPT"),
            window: window("2026-10-03T09:00:00Z", ThisWeek),
            ..weekly(used_up(Some(moment("2026-10-03T09:00:00Z", ThisWeek))), 0)
        };
        let plan = |five: Limit| Account {
            title: "Google AI Pro".into(),
            ..account(vec![week.clone(), five, used_weekly.clone()])
        };
        let lasts = "At this pace, Google AI Pro's weekly lasts until it resets.";
        let back = plan(five(used_up(Some(moment("2026-09-30T13:00:00Z", Soon)))));
        assert_eq!(
            w.verdict(&[&back], false).1,
            format!("{lasts} Claude and GPT weekly is used up, back Sat 9 AM.")
        );
        // One with no reset is back at no known time, so it's the last.
        let never = plan(five(used_up(None)));
        assert_eq!(
            w.verdict(&[&never], false).1,
            format!("{lasts} Claude and GPT 5 hours is used up.")
        );
        // One running out, listed before the one used up: those models can't be used now, so
        // the one used up is named.
        let running = plan(five(runs_out()));
        assert_eq!(
            w.verdict(&[&running], false).1,
            format!("{lasts} Claude and GPT weekly is used up, back Sat 9 AM.")
        );
    }

    #[test]
    fn a_limit_is_called_what_its_provider_calls_it() {
        let w = words();
        let opus = Limit {
            key: "seven_day_opus".into(),
            scope: some("Opus"),
            ..weekly(lasts(), 50)
        };
        assert_eq!(w.name(&opus), "Opus weekly");
        assert_eq!(w.of(&opus), "of Opus weekly");
        assert_eq!(
            w.of(&Limit {
                name: "Monthly".into(),
                ..limit(lasts())
            }),
            "this month"
        );
        assert_eq!(w.of(&limit(lasts())), "of 5 hours");
        let held = Limit {
            key: "key-1".into(),
            name: "Credits".into(),
            held_by: vec!["codex".into(), "pi".into()],
            ..limit(lasts())
        };
        assert_eq!(w.name(&held), "Codex and Pi's key");
    }
}
