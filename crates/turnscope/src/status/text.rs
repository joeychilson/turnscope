//! The status in words, as the command line and the MCP server say it:
//! each account and limit as lines, whether more hours of work fit, and the
//! one sentence that leads an answer. The app says it in its own words.

use super::{
    Account, Horizon, Limit, MinutesLeft, Moment, Outlook, Spend, Spread, Stale, Standing, State,
    Status, Unknown, Work, minutes,
};
use crate::agents;
use crate::time::{self, MINUTE};

const UNDER: &str = "under 10m";

/// Minutes of work as `minutes` rounds them: `1h 50m`, `26h`.
fn worked(minutes: i64) -> String {
    match minutes {
        ..10 => UNDER.to_owned(),
        10..600 => time::span(minutes * MINUTE),
        _ => format!("{}h", minutes / 60),
    }
}

/// The likely minutes of work left, as a sentence says them: `about 1h 50m`,
/// or `under 10m` alone, which is already no exact amount.
fn about(minutes: i64) -> String {
    match worked(minutes) {
        under if under == UNDER => under,
        amount => format!("about {amount}"),
    }
}

fn capital(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or(String::new(), |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// `one` or `many`, to agree with `limit`'s name (`Limit::plural`).
fn agree<'a>(limit: &Limit, one: &'a str, many: &'a str) -> &'a str {
    match limit.plural() {
        true => many,
        false => one,
    }
}

/// Why there's no account to show or name, before any is known.
pub const NONE_READ: &str =
    "No account's limits have been read yet: no agent here is signed in to one Turnscope reads.";

/// What's said where no account is shown: none is known yet, or every one
/// is hidden, and then `show`, how to see one.
pub fn none(status: &Status, show: &str) -> String {
    match status.accounts.is_empty() {
        true => NONE_READ.to_owned(),
        false => format!("Every account is hidden: {show}."),
    }
}

/// Why `account`'s limits aren't read now, and what to do about it: `its
/// login was refused; open Codex to sign in again`. Read before, it says since
/// when, or only `since` when the time of that read is `dated` beside it.
pub fn stale(account: &Account, dated: bool, now: i64) -> Option<String> {
    let (why, read_at) = match account.state {
        State::Live { .. } => return None,
        State::AsOf { read_at, why, .. } => (why, Some(read_at)),
        State::Unread { why } => (why, None),
    };
    // The agents to sign in with, `Codex or Pi`: one is always signed in to
    // an account not `SignedOut`.
    let agents = agent_names(&account.agents, "or");
    Some(match why {
        Stale::SignedOut => unread(why).to_owned(),
        Stale::SignIn => format!("{}; open {agents} to sign in again", unread(why)),
        Stale::Expired => format!(
            "{}; it's read {}when {agents} runs",
            unread(why),
            if read_at.is_some() { "again " } else { "" }
        ),
        Stale::ReadFailed => format!(
            "it couldn't be read {}; it's tried again every {} to {} minutes",
            match read_at {
                Some(_) if dated => "since".to_owned(),
                Some(read_at) => format!("since {}", time::clock(read_at, now)),
                None => "yet".to_owned(),
            },
            crate::limits::DUE / MINUTE,
            crate::limits::BACKED_OFF / MINUTE
        ),
    })
}

/// Why an account isn't read now, without what to do about it.
fn unread(why: Stale) -> &'static str {
    match why {
        Stale::SignedOut => "no agent here is signed in to it now",
        Stale::SignIn => "its login was refused",
        Stale::Expired => "its login expired",
        Stale::ReadFailed => "it couldn't be read",
    }
}

/// The status of `accounts`, `yours` marked so.
pub fn text(status: &Status, accounts: &[&Account], yours: Option<&str>) -> String {
    let now = status.at;
    let mut out = String::new();
    for account in accounts {
        out += &account.full_title();
        // Which agents draw on it, to choose where to work.
        if let Some(via) = via(&account.agents) {
            out += &format!(" · {via}");
        }
        for (on, mark) in [
            (Some(account.id.as_str()) == yours, "yours"),
            (account.in_use, "in use"),
            (account.hidden, "hidden"),
        ] {
            if on {
                out += &format!(" · {mark}");
            }
        }
        out += "\n";
        if let Some(stale) = stale(account, true, now) {
            out += &match account.state {
                State::AsOf { read_at, .. } => {
                    format!("  As of {}: {stale}.\n", time::ago(read_at, now))
                }
                _ => format!("  {}.\n", capital(&stale)),
            };
        }
        for limit in &account.limits {
            out += &format!("  {}\n", limit_text(limit, now));
        }
        if let Some(spend) = &account.spend {
            let partial = partial(spend);
            let spent = crate::usage::dollars(spend.month_usd);
            let pace = spend.pace().map_or(String::new(), |pace| {
                format!(", on pace for about {}", forecast_dollars(pace))
            });
            out += &format!("  This month, across its keys: {spent}{pace}{partial}.\n");
        }
        out += "\n";
    }
    out.trim_end().to_owned()
}

fn partial(spend: &Spend) -> &'static str {
    match spend.partial {
        true => ", and some usage has no known price",
        false => "",
    }
}

/// A forecast's two figures: whole dollars from $10, as $62 or $2,500, and
/// cents below, as $3.40.
fn forecast_dollars(usd: f64) -> String {
    if usd < 10.0 {
        crate::usage::dollars(usd)
    } else {
        format!("${}", crate::usage::count(usd.round() as u64))
    }
}

/// What's left of `limit`: `25%`, or of money, `$11.00 of $20.00`.
fn left(limit: &Limit) -> String {
    match limit.money {
        Some(money) => format!(
            "{} of {}",
            crate::usage::dollars(money.left_usd),
            crate::usage::dollars(money.size_usd)
        ),
        None => format!("{}%", limit.left_percent),
    }
}

/// One limit as a line: what's left, the reset, the outlook, and the work
/// left at the work rate.
pub fn limit_text(limit: &Limit, now: i64) -> String {
    let mut out = format!("{}: {} left", limit.named(), left(limit));
    match (&limit.window, limit.reset_since) {
        (Some(_), Some(reset)) => out += &format!(", reset {}", time::ago(reset, now)),
        (Some(window), None) if !matches!(limit.outlook, Outlook::UsedUp { .. }) => {
            out += &format!(", resets {}", time::ahead(window.resets.at, now))
        }
        _ => {}
    }
    out += ". ";
    out += &match &limit.outlook {
        Outlook::Lasts { left_at_reset } => match left_at_reset {
            Some(Spread { likely: 0, .. }) => {
                "At this pace it runs out about when it resets.".to_owned()
            }
            // A least of 0% was half the forecasts of weeks and months, and
            // 21% of those ran out, against 16% of the rest: it told nothing.
            Some(Spread { low, likely }) if low == likely || *low == 0 => {
                format!("Lasts, with about {likely}% left at the reset.")
            }
            Some(Spread { low, likely }) => {
                format!("Lasts, with about {likely}% left at the reset (as little as {low}%).")
            }
            None => "No use shows in its reading yet.".to_owned(),
        },
        Outlook::RunsOut { likely, soonest } if soonest.at == likely.at => {
            format!("At this pace it runs out {}.", time::ahead(likely.at, now))
        }
        Outlook::RunsOut { likely, soonest } => {
            // Today's day said where the likely time says one, as "13:30"
            // beside "Sat 18:00" would read as Saturday's.
            let near = |at: i64| match time::today(at, now) && !time::today(likely.at, now) {
                true => format!("today {}", time::clock(at, now)),
                false => time::clock(at, now),
            };
            format!(
                "At this pace it runs out {}, as soon as {}.",
                time::ahead(likely.at, now),
                near(soonest.at)
            )
        }
        Outlook::UsedUp { back: Some(back) } => {
            format!("Used up; back {}.", time::ahead(back.at, now))
        }
        Outlook::UsedUp { back: None } => "Used up.".to_owned(),
        Outlook::Unknown { reason } => match reason {
            Unknown::NotEnoughData => "No forecast yet.".to_owned(),
            Unknown::Stale if limit.reset_since.is_some() => {
                "Full since the reset, unless used on the web or another device.".to_owned()
            }
            Unknown::Stale => format!("Not read since {}.", time::clock(limit.read_at, now)),
            Unknown::NoReset => "Doesn't reset.".to_owned(),
        },
    };
    if let Some(work) = &limit.work
        && !matches!(limit.outlook, Outlook::UsedUp { .. })
    {
        let rate = work.rate_per_hour;
        out += &match work.left_minutes {
            Some(left) => format!(
                " {} of work left{}, at {rate}% an hour of work.",
                capital(&about(left.likely)),
                fewest(&left)
            ),
            None => {
                format!(" More work left than time until the reset, at {rate}% an hour of work.")
            }
        };
    }
    out
}

/// The fewest hours of work left, at the fast end of the work rate, after
/// the likely hours: ` (as little as 1h 10m)`, or nothing for "under 10m",
/// which is no amount.
fn fewest(left: &MinutesLeft) -> String {
    match worked(left.low) {
        low if low == UNDER => String::new(),
        low => format!(" (as little as {low})"),
    }
}

fn used_up_text(limit: &Limit, back: Option<Moment>, now: i64) -> String {
    let back = back.map_or(String::new(), |back| {
        format!(", back {}", time::ahead(back.at, now))
    });
    format!(
        "{} {} used up{back}",
        limit.named(),
        agree(limit, "is", "are")
    )
}

/// `account`'s limit it stands by that's used up, and when it's back: "5
/// hours is used up, back 14:00 (in 2h)". It's the limit that decides it,
/// so of several, the last back.
fn used_up(account: &Account, now: i64) -> Option<String> {
    let limit = account
        .deciding_limit()
        .filter(|limit| account.stands_by(limit))?;
    match limit.outlook {
        Outlook::UsedUp { back } => Some(used_up_text(limit, back, now)),
        _ => None,
    }
}

/// `account`'s limit it stands by with the fewest likely hours of work
/// left, and those hours. One reset since it was read counts, with a new
/// window's work left: left out, an account whose 5 hours had just reset
/// was said to have room to keep going, even working nonstop.
fn first_out(account: &Account) -> Option<(&Limit, MinutesLeft)> {
    account
        .limits
        .iter()
        .filter(|limit| account.stands_by(limit))
        .filter_map(|limit| Some((limit, limit.work?.left_minutes?)))
        .min_by_key(|(_, left)| left.likely)
}

/// The likely minutes of work left before `limit` runs out, endless where
/// that isn't measured.
fn likely_left(limit: &Limit) -> i64 {
    limit
        .work
        .and_then(|work| work.left_minutes)
        .map_or(i64::MAX, |left| left.likely)
}

/// Whether `hours` more of work fit each of `accounts`, after its title: a
/// line each. Of one, its line is its verdict too, so it says when that
/// rests on an earlier read.
pub fn fits(accounts: &[&Account], hours: f64, now: i64) -> String {
    let line = |account: &Account| {
        let (_, fit, dated) = judge(account, hours, now);
        (format!("{}: {fit}", account.full_title()), dated)
    };
    if let [account] = accounts {
        return match line(account) {
            (line, true) => line,
            (line, false) => line + &as_of(account, now),
        };
    }
    let lines: Vec<String> = accounts
        .iter()
        .map(|account| format!("- {}", line(account).0))
        .collect();
    lines.join("\n")
}

/// Whether `hours` more of work fit the limits `account` stands by, at the
/// work rate so far. One with no work rate says it can't be told: said to
/// fit with a note after, a 5 hours at 4% left read as room for hours of
/// work.
pub fn fit(account: &Account, hours: f64, now: i64) -> String {
    capital(&judge(account, hours, now).1)
}

/// Whether hours of work fit an account, as `fit` says it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Fits {
    No,
    Untold,
    Yes,
}

/// `fit` as it follows a title, whether that says they fit, and whether it
/// says why the account isn't read now.
pub(super) fn judge(account: &Account, hours: f64, now: i64) -> (Fits, String, bool) {
    let (more, fit, dont_fit) = match hours == 1.0 {
        true => ("1 more hour".to_owned(), "fits", "doesn't fit"),
        false => (format!("{hours} more hours"), "fit", "don't fit"),
    };
    let untold = |why: &str| format!("whether {more} of work {fit} can't be told: {why}.");
    if let Some(why) = account.unread() {
        return (Fits::Untold, untold(unread(why)), true);
    }
    if let Some(said) = used_up(account, now) {
        return (
            Fits::No,
            format!("{more} of work {dont_fit}: {said}."),
            false,
        );
    }
    // What agents here used since a read is no part of it, so what's left
    // now isn't known. Said of the limit it stands by of those the read is
    // still of: the deciding one with no work rate, as `standing` says it,
    // or else the one with the fewest hours of work left. With every one
    // reset since, what the new windows hold is as unknown.
    if let Some((read_at, worked_minutes)) = account.worked_since_read()
        && account.limits.iter().any(|limit| account.stands_by(limit))
    {
        let still = |limit: &&Limit| account.stands_by(limit) && limit.reset_since.is_none();
        let limit = account
            .deciding_limit()
            .filter(|limit| still(limit) && limit.work.is_none())
            .or_else(|| {
                account
                    .limits
                    .iter()
                    .filter(still)
                    .min_by_key(|limit| likely_left(limit))
            });
        let then = match limit {
            Some(limit) => format!(
                "{} had {} left at {}",
                limit.named(),
                left(limit),
                time::clock(read_at, now)
            ),
            None => format!("its limits were last read at {}", time::clock(read_at, now)),
        };
        let why = format!(
            "{then}, and your agents have worked {} since",
            about(worked_minutes)
        );
        return (Fits::Untold, untold(&why), true);
    }
    let mut out: Option<(&Limit, i64, Option<MinutesLeft>)> = None;
    let mut after = Vec::new();
    let mut untold_why = None;
    for limit in account
        .limits
        .iter()
        .filter(|limit| account.stands_by(limit))
    {
        let Some(work) = &limit.work else {
            // Where the clock's pace takes it, to weigh until hours of work
            // are measured.
            let why = format!(
                "{}, but too little work on {} has been measured",
                paced(limit, now),
                agree(limit, "it", "them")
            );
            untold_why = untold_why.or(Some(why));
            continue;
        };
        match ends(limit, work, hours, now) {
            End::Out(at, within) if out.is_none_or(|(_, first, _)| at < first) => {
                out = Some((limit, at, within))
            }
            End::Out(..) => {}
            End::Leaves(left) => after.push(match left <= 0.5 {
                true => format!("under 1% of {}", limit.named()),
                false => format!("about {left:.0}% of {}", limit.named()),
            }),
        }
    }
    if let Some((limit, at, within)) = out {
        let run = agree(limit, "runs", "run");
        let within = within.map_or(String::new(), |left| fewest(&left) + &resets(limit, now));
        return (
            Fits::No,
            format!(
                "{more} of work {dont_fit}: {} {run} out after {} of work{within}.",
                limit.named(),
                about(at)
            ),
            false,
        );
    }
    let after = after.join(", ");
    let (fits, said) = match (untold_why, after.is_empty()) {
        (Some(why), true) => (Fits::Untold, untold(&why)),
        (Some(why), false) => (
            Fits::Untold,
            format!("{} At your work rate they'd leave {after}.", untold(&why)),
        ),
        (None, true) => (Fits::Untold, untold("no limit on all its usage was read")),
        (None, false) => (
            Fits::Yes,
            format!("{more} of work {fit} at your work rate, leaving {after}."),
        ),
    };
    (fits, said, false)
}

/// Where `hours` more of work leave a limit.
enum End {
    /// Out after these minutes of work, and the minutes it has left where
    /// that's before its reset.
    Out(i64, Option<MinutesLeft>),
    /// With this percent left.
    Leaves(f64),
}

/// Where `hours` more of work at `work`'s rate leave `limit`, working all
/// the while. Past its reset, in the windows after, each a new one as long:
/// counted only to the reset, 6 hours at 25% an hour on a 5 hours resetting
/// in 30 minutes left 16%, though the window after runs out 4 hours in.
fn ends(limit: &Limit, work: &Work, hours: f64, now: i64) -> End {
    let rate = work.rate_per_hour;
    // Out before its reset, as forecast with the spread of work's pace.
    if let Some(left) = work.left_minutes
        && (left.likely as f64) < hours * 60.0
    {
        return End::Out(left.likely, Some(left));
    }
    let left = f64::from(limit.left_percent);
    let Some(until) = limit.hours_to_reset(now).filter(|until| *until < hours) else {
        return End::Leaves((left - rate * hours).max(0.0));
    };
    let length = limit
        .length()
        .map(|length| length as f64 / time::HOUR as f64);
    let past = hours - until;
    // The work in the first window after, and in the last.
    let (first, last) = match length.filter(|length| *length > 0.0) {
        Some(length) => (
            past.min(length),
            past - ((past / length).ceil() - 1.0).max(0.0) * length,
        ),
        None => (past, past),
    };
    match first * rate >= 100.0 {
        true => End::Out(minutes(until + 100.0 / rate), None),
        false => End::Leaves((100.0 - rate * last).max(0.0)),
    }
}

/// The one thing to know of `account`: whether there's room to keep going,
/// and for how much work, by the limit it stands by that runs out first.
pub fn verdict(account: &Account, now: i64) -> String {
    let title = account.full_title();
    match (&account.state, stale(account, true, now)) {
        (State::Unread { .. }, Some(stale)) => format!("{title}: {stale}."),
        _ => standing(account, &title, now) + &as_of(account, now),
    }
}

/// That what's said of `account` rests on an earlier read, after it, as an
/// agent acts on an answer's first line: ` That's as of 11:25: it couldn't
/// be read since; …`. Nothing for one read now.
fn as_of(account: &Account, now: i64) -> String {
    match (&account.state, stale(account, true, now)) {
        (State::AsOf { read_at, .. }, Some(stale)) => {
            format!(" That's as of {}: {stale}.", time::clock(*read_at, now))
        }
        _ => String::new(),
    }
}

/// `; it resets 14:00 (in 2h)`, unless `limit` reset since it was read or
/// never does.
fn resets(limit: &Limit, now: i64) -> String {
    limit.resets().map_or(String::new(), |resets| {
        format!("; it resets {}", time::ahead(resets, now))
    })
}

/// How `account`, read, stands: used up until the limit that decides it is
/// back, or else by the limit it stands by closest to running out, by the
/// work left or else the clock's pace. The deciding limit with no work rate
/// is said by the clock's pace, over another's work left: a 5 hours at 4%
/// left was passed over for "about 12h of work left before Weekly runs
/// out".
fn standing(account: &Account, title: &str, now: i64) -> String {
    if let Some(said) = used_up(account, now) {
        return format!("{title}: {said}.");
    }
    let unmeasured = account
        .deciding_limit()
        .is_some_and(|limit| account.stands_by(limit) && limit.work.is_none());
    if !unmeasured && let Some((limit, left)) = first_out(account) {
        return format!(
            "{title}: {} of work left{} before {} {} out{}.",
            about(left.likely),
            fewest(&left),
            limit.named(),
            agree(limit, "runs", "run"),
            resets(limit, now)
        );
    }
    let stood_by: Vec<&Limit> = account
        .limits
        .iter()
        .filter(|limit| account.stands_by(limit))
        .collect();
    if !stood_by.is_empty() && stood_by.iter().all(|limit| limit.work.is_some()) {
        return format!(
            "{title}: room to keep going; no limit runs out before it resets, even working nonstop."
        );
    }
    // Until an hour of work is measured, the clock's pace says it.
    let Some(limit) = account.deciding_limit() else {
        return match &account.spend {
            Some(spend) => format!(
                "{title}: no limit of it was read; {} this month at list prices{}.",
                crate::usage::dollars(spend.month_usd),
                partial(spend)
            ),
            None => format!("{title}: no limit of it was read."),
        };
    };
    format!("{title}: {}.", paced(limit, now))
}

/// What's left of `limit`, and where the clock's pace takes it by its
/// reset: `Weekly has 80% left and lasts at this pace until it resets Tue
/// 00:00 (in 4d 12h)`.
fn paced(limit: &Limit, now: i64) -> String {
    if let Outlook::UsedUp { back } = limit.outlook {
        return used_up_text(limit, back, now);
    }
    let resets = limit.resets().map(|resets| time::ahead(resets, now));
    let ahead = match (&limit.outlook, resets) {
        (Outlook::RunsOut { likely, .. }, Some(resets)) => format!(
            " and {} out {} at this pace, before it resets {resets}",
            agree(limit, "runs", "run"),
            time::ahead(likely.at, now)
        ),
        (Outlook::Lasts { .. }, Some(resets)) => format!(
            " and {} at this pace until it resets {resets}",
            agree(limit, "lasts", "last")
        ),
        (_, Some(resets)) => format!("; it resets {resets}"),
        (_, None) => String::new(),
    };
    let has = agree(limit, "has", "have");
    format!("{} {has} {} left{ahead}", limit.named(), left(limit))
}

/// `account` as one line, for an agent's status line: its title, the limits
/// it stands by, and any other running out or used up, as `Claude Max · 5h
/// 86% · weekly 25%, out Sat 18:00`. Once one it stands by is used up, only
/// that one.
pub fn line(account: &Account, now: i64) -> String {
    if account.refused() {
        return format!("{} · sign in again", account.title);
    }
    let mut parts = vec![account.title.clone()];
    let blocking: Vec<&Limit> = account
        .limits
        .iter()
        .filter(|limit| account.stands_by(limit) && limit.standing() == Standing::UsedUp)
        .collect();
    let shown: Vec<&Limit> = if blocking.is_empty() {
        account
            .limits
            .iter()
            .filter(|limit| account.stands_by(limit) || limit.standing() != Standing::Lasts)
            .collect()
    } else {
        blocking
    };
    parts.extend(shown.iter().map(|limit| short(limit, now)));
    match (&account.state, &account.spend) {
        (State::Unread { why }, None) => parts.push(
            match why {
                Stale::Expired => "login expired",
                Stale::SignedOut => "signed out",
                Stale::SignIn | Stale::ReadFailed => "not read yet",
            }
            .to_owned(),
        ),
        (State::AsOf { read_at, .. }, _) => {
            parts.push(format!("as of {}", time::clock(*read_at, now)))
        }
        _ => {}
    }
    if let Some(spend) = account.spend.filter(|_| account.limits.is_empty()) {
        parts.push(format!(
            "{} this month",
            crate::usage::dollars(spend.month_usd)
        ));
    }
    parts.join(" · ")
}

/// `via Codex` or `via Claude Code and OpenCode`.
pub fn via(ids: &[String]) -> Option<String> {
    (!ids.is_empty()).then(|| format!("via {}", agent_names(ids, "and")))
}

/// The names of the agents `ids`, in a sentence with `last` before the
/// last: `Codex or Pi`.
pub fn agent_names(ids: &[String], last: &str) -> String {
    let names: Vec<&str> = ids.iter().map(|id| agents::name(id)).collect();
    list(&names, last)
}

/// `names` in a sentence: `a`, `a or b`, `a, b or c`, with `last`, `and`
/// or `or`, before the last.
pub fn list(names: &[&str], last: &str) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., final_] => format!("{} {last} {final_}", rest.join(", ")),
    }
}

/// Another account with room, where `current` runs out or is used up: with
/// the agents that use it when `agent` doesn't.
pub fn alternate_note(status: &Status, current: &Account, agent: Option<&str>) -> Option<String> {
    current
        .deciding_limit()
        .filter(|limit| limit.standing() != Standing::Lasts)?;
    let (other, limit) = super::alternate(&status.accounts, current, agent)?;
    Some(format!(
        "Another account has room: {} has {} left ({}{}).",
        other.full_title(),
        left(limit),
        limit.named(),
        elsewhere(other, agent)
    ))
}

/// The account with the most room, to choose where to work, given `hours`
/// of it: `yours` marked so.
pub fn most_room(
    status: &Status,
    hours: Option<f64>,
    yours: Option<&str>,
    agent: Option<&str>,
) -> Option<String> {
    let (account, limit) = super::roomiest(&status.accounts, agent, hours, status.at)?;
    let whose = match Some(account.id.as_str()) == yours {
        true => ", yours".to_owned(),
        false => elsewhere(account, agent),
    };
    Some(format!(
        "Most room: {}{whose}: {}.",
        account.full_title(),
        paced(limit, status.at)
    ))
}

/// `, via Codex`: the agents that use `account`, where `agent` is known
/// and doesn't.
fn elsewhere(account: &Account, agent: Option<&str>) -> String {
    match agent {
        Some(_) if !super::signed_in(account, agent) => {
            via(&account.agents).map_or(String::new(), |via| format!(", {via}"))
        }
        _ => String::new(),
    }
}

/// A limit in a few words: `5h 86%`, `weekly 25%, out Sat 18:00`, `weekly
/// used up, back Sun 00:00`.
fn short(limit: &Limit, now: i64) -> String {
    let name = limit.short_name();
    let left = match limit.money {
        Some(money) => crate::usage::dollars(money.left_usd),
        None => format!("{}%", limit.left_percent),
    };
    let at = |moment: Moment| match moment.horizon {
        Horizon::Soon => format!("in {}", time::span(moment.at - now)),
        _ => time::clock(moment.at, now),
    };
    match limit.outlook {
        Outlook::UsedUp { back: Some(back) } => format!("{name} used up, back {}", at(back)),
        Outlook::UsedUp { back: None } => format!("{name} used up"),
        Outlook::RunsOut { likely, .. } => format!("{name} {left}, out {}", at(likely)),
        _ => format!("{name} {left}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hours_of_work_fit_or_dont_or_cant_be_told() {
        let example = example();
        let now = example["at"].as_i64().unwrap();
        let claude = &example["accounts"][0];
        // Claude Max's 5 hours with half an hour of work left: said with its
        // fewest and reset, as the verdict says them.
        let mut account = claude.clone();
        account["limits"][0]["work"]["leftMinutes"] = serde_json::json!({"low": 20, "likely": 30});
        let account: Account = serde_json::from_value(account).unwrap();
        let resets = time::ahead(account.limits[0].resets().unwrap(), now);
        assert_eq!(
            fit(&account, 1.0, now),
            format!(
                "1 more hour of work doesn't fit: 5 hours runs out after about 30m of work (as little as 20m); it resets {resets}."
            )
        );
        assert_eq!(
            fit(&account, 2.0, now),
            format!(
                "2 more hours of work don't fit: 5 hours runs out after about 30m of work (as little as 20m); it resets {resets}."
            )
        );
        // Its Weekly with 40 minutes of work left and 5 hours with 1h 20m:
        // the first out decides, by its likely hours, as the verdict says.
        let mut account = claude.clone();
        account["limits"][0]["work"]["leftMinutes"]["likely"] = 80.into();
        account["limits"][1]["work"]["leftMinutes"] = serde_json::json!({"low": 20, "likely": 40});
        let account: Account = serde_json::from_value(account).unwrap();
        let resets = time::ahead(account.limits[1].resets().unwrap(), now);
        assert_eq!(
            fit(&account, 2.0, now),
            format!(
                "2 more hours of work don't fit: Weekly runs out after about 40m of work (as little as 20m); it resets {resets}."
            )
        );
        let said = verdict(&account, now);
        assert!(
            said.starts_with(
                "Claude Max (me@example.com): about 40m of work left (as little as 20m) before Weekly runs out; it resets "
            ),
            "{said}"
        );
        // Its 5 hours reset since it was read, with a new window's 3 hours of
        // work, and its Weekly lasts even working nonstop: 5 hours decides.
        let mut account = claude.clone();
        let five = &mut account["limits"][0];
        five["resetSince"] = (now - 10 * MINUTE).into();
        five["leftPercent"] = 100.into();
        five["outlook"] = serde_json::json!({"kind": "unknown", "reason": "stale"});
        five["work"]["leftMinutes"] = serde_json::json!({"low": 110, "likely": 180});
        account["deciding"] = "seven_day".into();
        let account: Account = serde_json::from_value(account).unwrap();
        assert_eq!(
            fit(&account, 4.0, now),
            "4 more hours of work don't fit: 5 hours runs out after about 3h of work (as little as 1h 50m)."
        );
        assert_eq!(
            verdict(&account, now),
            "Claude Max (me@example.com): about 3h of work left (as little as 1h 50m) before 5 hours runs out."
        );
        // At 10% an hour of work, 6 hours fit, and the new window ends with
        // half left: 5 hours of work count, not 6.
        let mut account = claude.clone();
        let five = &mut account["limits"][0];
        five["resetSince"] = (now - 10 * MINUTE).into();
        five["leftPercent"] = 100.into();
        five["work"] = serde_json::json!({"ratePerHour": 10.0, "leftMinutes": null});
        let lasting: Account = serde_json::from_value(account.clone()).unwrap();
        let said = fit(&lasting, 6.0, now);
        // The new window resets after 5 hours of work, and the one after
        // has one hour's 10% used.
        assert!(said.contains("leaving about 90% of 5 hours"), "{said}");
        // Every limit lasts, the one that reset too: room to keep going.
        assert_eq!(
            verdict(&lasting, now),
            "Claude Max (me@example.com): room to keep going; no limit runs out before it resets, even working nonstop."
        );
        // Too little work measured on it since: no room is claimed.
        account["limits"][0]["work"] = serde_json::Value::Null;
        let unmeasured: Account = serde_json::from_value(account).unwrap();
        let said = verdict(&unmeasured, now);
        assert!(!said.contains("room"), "{said}");
        // Its 5 hours with too little work measured, and its weekly
        // forecast: said, with what the weekly would leave.
        let mut account = claude.clone();
        let five = &mut account["limits"][0];
        five["work"] = serde_json::Value::Null;
        five["outlook"] = serde_json::json!({"kind": "unknown", "reason": "notEnoughData"});
        let unforecast: Account = serde_json::from_value(account.clone()).unwrap();
        let resets = time::ahead(unforecast.limits[0].resets().unwrap(), now);
        assert_eq!(
            fit(&unforecast, 1.0, now),
            format!(
                "Whether 1 more hour of work fits can't be told: 5 hours has 28% left; it resets {resets}, but too little work on it has been measured. At your work rate they'd leave about 59% of Weekly."
            )
        );
        // Deciding the account, it's said over Weekly's hours of work, and
        // over them as of a read since which agents worked.
        account["deciding"] = "five_hour".into();
        let deciding: Account = serde_json::from_value(account.clone()).unwrap();
        let said = verdict(&deciding, now);
        assert!(
            said.starts_with("Claude Max (me@example.com): 5 hours has 28% left; it resets "),
            "{said}"
        );
        account["state"] = serde_json::json!({"kind": "asOf", "readAt": now - 35 * MINUTE, "why": "readFailed", "workedMinutes": 40});
        account["limits"][1]["work"]["leftMinutes"] =
            serde_json::json!({"low": 1200, "likely": 2400});
        let dated: Account = serde_json::from_value(account).unwrap();
        let said = fit(&dated, 1.0, now);
        assert!(
            said.contains("can't be told: 5 hours had 28% left at "),
            "{said}"
        );
    }

    /// 5 hours at 25% an hour of work, with 28% left, resets in 30 minutes:
    /// 6 hours of work run out its next window 4 hours into it, as they do
    /// a minute after the reset.
    #[test]
    fn hours_past_a_reset_count_in_the_window_after() {
        let example = example();
        let now = example["at"].as_i64().unwrap();
        let mut account = example["accounts"][0].clone();
        account["limits"] = serde_json::json!([account["limits"][0].clone()]);
        let five = &mut account["limits"][0];
        five["leftPercent"] = 28.into();
        five["window"]["resets"]["at"] = (now + 30 * MINUTE).into();
        five["window"]["startsAt"] = (now + 30 * MINUTE - 5 * time::HOUR).into();
        five["work"] = serde_json::json!({"ratePerHour": 25.0, "leftMinutes": null});
        let before: Account = serde_json::from_value(account.clone()).unwrap();
        let after_reset = {
            let five = &mut account["limits"][0];
            five["resetSince"] = (now - MINUTE).into();
            five["leftPercent"] = 100.into();
            five["window"]["resets"]["at"] = (now - MINUTE).into();
            five["window"]["startsAt"] = (now - MINUTE - 5 * time::HOUR).into();
            five["work"]["leftMinutes"] = serde_json::json!({"low": 110, "likely": 240});
            serde_json::from_value::<Account>(account).unwrap()
        };
        assert_eq!(
            fit(&before, 6.0, now),
            "6 more hours of work don't fit: 5 hours runs out after about 4h 30m of work."
        );
        assert_eq!(
            fit(&after_reset, 6.0, now),
            "6 more hours of work don't fit: 5 hours runs out after about 4h of work (as little as 1h 50m)."
        );
        // 2 hours fit: an hour and a half of it in the window after.
        assert_eq!(
            fit(&before, 2.0, now),
            "2 more hours of work fit at your work rate, leaving about 62% of 5 hours."
        );
    }

    #[test]
    fn credits_are_used_up_as_the_app_says_where_5_hours_is() {
        let example = example();
        let now = example["at"].as_i64().unwrap();
        // The OpenRouter API key's credits, $45 of $50 left, which decide
        // it, read just now.
        let mut account = example["accounts"][2].clone();
        account["state"] = serde_json::json!({"kind": "live", "readAt": now});
        let mut credits = account["limits"][0].clone();
        credits["key"] = "credits".into();
        credits["name"] = "Credits".into();
        credits["heldBy"] = serde_json::json!([]);
        credits["leftPercent"] = 90.into();
        credits["money"] = serde_json::json!({"sizeUsd": 50.0, "leftUsd": 45.0});
        account["limits"] = serde_json::json!([credits]);
        account["deciding"] = "credits".into();
        let left: Account = serde_json::from_value(account.clone()).unwrap();
        assert_eq!(
            verdict(&left, now),
            "OpenRouter API key: Credits have $45.00 of $50.00 left."
        );
        // Used up, they're plural as the app's notification has them.
        account["limits"][0]["leftPercent"] = 0.into();
        account["limits"][0]["money"]["leftUsd"] = 0.0.into();
        account["limits"][0]["outlook"] = serde_json::json!({"kind": "usedUp", "back": null});
        let used_up: Account = serde_json::from_value(account).unwrap();
        assert_eq!(
            verdict(&used_up, now),
            "OpenRouter API key: Credits are used up."
        );
        assert_eq!(
            fit(&used_up, 1.0, now),
            "1 more hour of work doesn't fit: Credits are used up."
        );
    }

    #[test]
    fn a_least_of_zero_left_at_the_reset_is_not_said() {
        let example = example();
        let now = example["at"].as_i64().unwrap();
        let mut account = example["accounts"][0].clone();
        let weekly: Limit = serde_json::from_value(account["limits"][1].clone()).unwrap();
        let line = limit_text(&weekly, now);
        assert!(
            line.contains("Lasts, with about 43% left at the reset (as little as 2%)."),
            "{line}"
        );
        account["limits"][1]["outlook"]["leftAtReset"]["low"] = 0.into();
        let weekly: Limit = serde_json::from_value(account["limits"][1].clone()).unwrap();
        let line = limit_text(&weekly, now);
        assert!(
            line.contains("Lasts, with about 43% left at the reset."),
            "{line}"
        );
    }

    #[test]
    fn under_ten_minutes_of_work_is_said_once_and_not_about() {
        let example = example();
        let now = example["at"].as_i64().unwrap();
        // Claude Max's 5 hours with 6 minutes of work left.
        let mut account = example["accounts"][0].clone();
        account["limits"][0]["work"]["leftMinutes"] = serde_json::json!({"low": 0, "likely": 6});
        let few: Account = serde_json::from_value(account.clone()).unwrap();
        let line = limit_text(&few.limits[0], now);
        assert!(
            line.ends_with(". Under 10m of work left, at 25% an hour of work."),
            "{line}"
        );
        let verdict = verdict(&few, now);
        assert!(
            verdict.starts_with(
                "Claude Max (me@example.com): under 10m of work left before 5 hours runs out; it resets "
            ),
            "{verdict}"
        );
        let said = fit(&few, 1.0, now);
        assert!(
            said.starts_with(
                "1 more hour of work doesn't fit: 5 hours runs out after under 10m of work; it resets "
            ),
            "{said}"
        );
        // Nor is a fewest under ten minutes, which is no amount.
        account["limits"][0]["work"]["leftMinutes"]["likely"] = 20.into();
        let more: Account = serde_json::from_value(account).unwrap();
        let line = limit_text(&more.limits[0], now);
        assert!(
            line.ends_with(". About 20m of work left, at 25% an hour of work."),
            "{line}"
        );
    }

    #[test]
    fn a_run_out_as_soon_as_it_likely_is_says_its_time_once() {
        let example = example();
        let now = example["at"].as_i64().unwrap();
        // Claude Max's 5 hours, its soonest rounded to its likely time, as
        // near the end of a window.
        let mut account = example["accounts"][0].clone();
        let outlook = &mut account["limits"][0]["outlook"];
        outlook["soonest"] = outlook["likely"].clone();
        let likely = outlook["likely"]["at"].as_i64().unwrap();
        let account: Account = serde_json::from_value(account).unwrap();
        let line = limit_text(&account.limits[0], now);
        let once = format!(
            "At this pace it runs out {}. About",
            time::ahead(likely, now)
        );
        assert!(line.contains(&once), "{line}");
    }

    #[test]
    fn a_time_in_the_minute_of_the_status_reads_as_its_words_say() {
        // The status is as of its minute's start: a limit that runs out or
        // resets in that minute is still to come, and a read in it has gone
        // by.
        let mut example = example();
        let now = example["at"].as_i64().unwrap();
        let accounts = &mut example["accounts"];
        let five = &mut accounts[0]["limits"][0];
        five["window"]["resets"]["at"] = now.into();
        five["outlook"]["likely"]["at"] = now.into();
        accounts[1]["state"]["readAt"] = (now + 10_000).into();
        let status: Status = serde_json::from_value(example.clone()).unwrap();
        let said = text(&status, &[&status.accounts[0], &status.accounts[1]], None);
        let clock = time::clock(now, now);
        for part in [
            format!("resets {clock} (in 0m)"),
            format!("runs out {clock} (in 0m)"),
            format!("As of {clock} (0m ago)"),
        ] {
            assert!(said.contains(&part), "{part} in {said}");
        }
    }

    fn example() -> serde_json::Value {
        serde_json::from_str(include_str!("../../tests/fixtures/status.json")).unwrap()
    }

    #[test]
    fn why_an_account_isnt_read_is_said_the_same_way_everywhere() {
        let status: Status = serde_json::from_value(example()).unwrap();
        // The line under the title, or for one as of an earlier read, what
        // follows that read's time, which is in the local time zone.
        for (title, why, line) in [
            (
                "ChatGPT Pro",
                "no agent here is signed in to it now.",
                " (20m ago): no agent here is signed in to it now.",
            ),
            (
                "OpenRouter API key",
                "it couldn't be read since; it's tried again every 5 to 30 minutes.",
                " (10m ago): it couldn't be read since; it's tried again every 5 to 30 minutes.",
            ),
            (
                "OpenCode Go",
                "its login was refused; open Claude Code to sign in again.",
                "  Its login was refused; open Claude Code to sign in again.",
            ),
            (
                "SuperGrok",
                "its login was refused; open Claude Code to sign in again.",
                " (1h 30m ago): its login was refused; open Claude Code to sign in again.",
            ),
            (
                "Anthropic API key",
                "it couldn't be read yet; it's tried again every 5 to 30 minutes.",
                "  It couldn't be read yet; it's tried again every 5 to 30 minutes.",
            ),
        ] {
            let account = status.accounts.iter().find(|a| a.title == title).unwrap();
            let said = text(&status, &[account], None);
            let under_title = said.lines().nth(1).unwrap();
            match account.state {
                State::Unread { .. } => assert_eq!(under_title, line),
                _ => assert!(under_title.ends_with(line), "{under_title}"),
            }
            let verdict = verdict(account, status.at);
            assert!(verdict.ends_with(&format!(": {why}")), "{verdict}");
        }
        // Its agent renews an expired login when next used: nothing is asked.
        let mut expired = status.accounts[0].clone();
        expired.state = State::Unread {
            why: Stale::Expired,
        };
        assert_eq!(
            verdict(&expired, status.at),
            "Claude Max (me@example.com): its login expired; it's read when Claude Code runs."
        );
        expired.state = State::AsOf {
            read_at: status.at - 45 * MINUTE,
            why: Stale::Expired,
            worked_minutes: 0,
        };
        assert!(
            verdict(&expired, status.at)
                .ends_with("its login expired; it's read again when Claude Code runs."),
        );
        // Signed in by several agents, any of them.
        let mut refused = status.accounts[0].clone();
        refused.agents = ["claude-code", "opencode", "pi"].map(str::to_owned).into();
        refused.state = State::Unread { why: Stale::SignIn };
        assert_eq!(
            verdict(&refused, status.at),
            "Claude Max (me@example.com): its login was refused; open Claude Code, OpenCode or Pi to sign in again."
        );
    }

    #[test]
    fn alternate_note_recommends_live_account_with_room_when_tight() {
        let mut status: Status = serde_json::from_value(example()).unwrap();
        // In the fixture ChatGPT Pro is signed out and OpenRouter failed, so no live alternate.
        assert_eq!(
            alternate_note(&status, &status.accounts[0], Some("claude-code")),
            None
        );
        // Make both ChatGPT Pro (subscription, 97%, Codex) and OpenRouter (API key, 29%, OpenCode) live.
        status.accounts[1].state = State::Live { read_at: status.at };
        status.accounts[1].agents = vec!["codex".to_owned()];
        status.accounts[2].state = State::Live { read_at: status.at };
        // When another agent holds it, say which agent uses it.
        assert_eq!(
            alternate_note(&status, &status.accounts[0], Some("claude-code")).as_deref(),
            Some("Another account has room: ChatGPT Pro has 97% left (Monthly, via Codex).")
        );
        // When an account is signed into the calling agent, prefer it over a higher-left account on another agent.
        status.accounts[2].agents = vec!["claude-code".to_owned()];
        assert_eq!(
            alternate_note(&status, &status.accounts[0], Some("claude-code")).as_deref(),
            Some(
                "Another account has room: OpenRouter API key has $8.70 of $30.00 left (Pi's key limit)."
            )
        );
        // Without a caller agent, subscription is preferred over API key without a via suffix.
        assert_eq!(
            alternate_note(&status, &status.accounts[0], None).as_deref(),
            Some("Another account has room: ChatGPT Pro has 97% left (Monthly).")
        );
        // With under a quarter left, OpenRouter has no room.
        status.accounts[2].limits[0].left_percent = 24;
        assert_eq!(
            alternate_note(&status, &status.accounts[0], Some("claude-code")).as_deref(),
            Some("Another account has room: ChatGPT Pro has 97% left (Monthly, via Codex).")
        );
        // When the current account lasts, no alternate is suggested.
        status.accounts[0].limits[0].outlook = Outlook::Lasts {
            left_at_reset: None,
        };
        assert_eq!(
            alternate_note(&status, &status.accounts[0], Some("claude-code")),
            None
        );
    }

    #[test]
    fn a_key_with_no_limit_read_isnt_said_to_fit_and_one_not_read_says_why() {
        let status: Status = serde_json::from_value(example()).unwrap();
        let mut key = status.accounts[5].clone();
        key.state = State::Live { read_at: status.at };
        assert_eq!(
            fit(&key, 3.0, status.at),
            "Whether 3 more hours of work fit can't be told: no limit on all its usage was read."
        );
        assert_eq!(
            verdict(&key, status.at),
            "Anthropic API key: no limit of it was read; $0.00 this month at list prices."
        );
        // Its login refused since.
        key.state = State::AsOf {
            read_at: status.at,
            why: Stale::SignIn,
            worked_minutes: 0,
        };
        assert_eq!(
            fit(&key, 1.0, status.at),
            "Whether 1 more hour of work fits can't be told: its login was refused."
        );
    }

    #[test]
    fn most_room_is_the_live_account_with_the_most_left() {
        let mut status: Status = serde_json::from_value(example()).unwrap();
        // Claude Max runs out, ChatGPT Pro is signed out and OpenRouter
        // couldn't be read.
        assert_eq!(most_room(&status, None, None, Some("claude-code")), None);
        let read_at = status.at;
        for account in &mut status.accounts[1..3] {
            account.state = State::Live { read_at };
        }
        status.accounts[1].agents = vec!["codex".to_owned()];
        // Claude Code's OpenRouter key has less room than ChatGPT Pro.
        status.accounts[2].agents.push("claude-code".to_owned());
        let room = |yours, agent| most_room(&status, None, yours, agent).unwrap();
        assert!(
            room(None, Some("claude-code"))
                .starts_with("Most room: ChatGPT Pro, via Codex: Monthly has 97% left; it resets ")
        );
        assert!(
            room(Some(status.accounts[1].id.as_str()), Some("codex"))
                .starts_with("Most room: ChatGPT Pro, yours: Monthly has 97% left")
        );
        // Hidden, it isn't named.
        status.accounts[1].hidden = true;
        let room = most_room(&status, None, None, Some("claude-code")).unwrap();
        assert!(room.starts_with("Most room: OpenRouter API key"), "{room}");
    }

    #[test]
    fn given_hours_most_room_is_an_account_they_fit() {
        let mut status: Status = serde_json::from_value(example()).unwrap();
        // Claude Max by its Weekly alone, 60% left at 3.8% an hour of work:
        // 5 hours leave 41%. ChatGPT Pro read now, its Monthly 97% left.
        let claude = &mut status.accounts[0];
        claude.limits.retain(|limit| limit.key == "seven_day");
        claude.deciding = Some("seven_day".to_owned());
        claude.limits[0].work = Some(Work {
            rate_per_hour: 3.8,
            left_minutes: Some(MinutesLeft {
                low: 720,
                likely: 960,
            }),
        });
        let chatgpt = &mut status.accounts[1];
        chatgpt.state = State::Live { read_at: status.at };
        chatgpt.agents = vec!["codex".to_owned()];
        let room = |status: &Status, hours| most_room(status, hours, None, Some("claude-code"));
        let claude = "Most room: Claude Max (me@example.com): Weekly has 60% left";
        // With too little work measured on ChatGPT Pro, 5 hours fit only
        // Claude Max.
        assert!(
            room(&status, None)
                .unwrap()
                .starts_with("Most room: ChatGPT Pro")
        );
        assert!(room(&status, Some(5.0)).unwrap().starts_with(claude));
        // ChatGPT Pro runs out after 2h 20m of work.
        status.accounts[1].limits[1].work = Some(Work {
            rate_per_hour: 41.6,
            left_minutes: Some(MinutesLeft {
                low: 120,
                likely: 140,
            }),
        });
        assert!(room(&status, Some(5.0)).unwrap().starts_with(claude));
        // 50 hours fit neither.
        assert_eq!(room(&status, Some(50.0)), None);
    }

    #[test]
    fn a_key_only_account_stands_by_its_key_limit_in_every_answer() {
        let example = example();
        let now = example["at"].as_i64().unwrap();
        let mut raw = example["accounts"][2].clone();
        raw["state"] = serde_json::json!({"kind": "live", "readAt": now});
        let account: Account = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(line(&account, now), "OpenRouter API key · key limit $8.70");
        assert_eq!(
            fit(&account, 1.0, now),
            "Whether 1 more hour of work fits can't be told: Pi's key limit has $8.70 of $30.00 left, but too little work on it has been measured."
        );

        // Used up with no reset.
        raw["limits"][0]["leftPercent"] = 0.into();
        raw["limits"][0]["money"]["leftUsd"] = 0.0.into();
        raw["limits"][0]["outlook"] = serde_json::json!({"kind": "usedUp", "back": null});
        let used_up: Account = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(
            verdict(&used_up, now),
            "OpenRouter API key: Pi's key limit is used up."
        );
        assert_eq!(
            fit(&used_up, 1.0, now),
            "1 more hour of work doesn't fit: Pi's key limit is used up."
        );

        // Used up with a reset.
        let resets = raw["limits"][1]["window"]["resets"]["at"].as_i64().unwrap();
        raw["deciding"] = "key-fedcba9876543210".into();
        raw["limits"][1]["leftPercent"] = 0.into();
        raw["limits"][1]["money"]["leftUsd"] = 0.0.into();
        raw["limits"][1]["outlook"] = serde_json::json!({
            "kind": "usedUp",
            "back": raw["limits"][1]["window"]["resets"].clone(),
        });
        let used_up_reset: Account = serde_json::from_value(raw).unwrap();
        assert_eq!(
            verdict(&used_up_reset, now),
            format!(
                "OpenRouter API key: OpenCode's daily key limit is used up, back {}.",
                time::ahead(resets, now)
            )
        );
    }

    #[test]
    fn fit_follows_work_left_and_never_says_about_zero_percent() {
        let example = example();
        let now = example["at"].as_i64().unwrap();
        let mut raw = example["accounts"][0].clone();
        // 25% left at 40.9%/h is 36.7m exact, rounded to 40m (as in status.json).
        raw["limits"][0]["leftPercent"] = 25.into();
        raw["limits"][0]["work"] = serde_json::json!({
            "ratePerHour": 40.9,
            "leftMinutes": {"low": 20, "likely": 40},
        });
        raw["limits"][1]["leftPercent"] = 56.into();
        raw["limits"][1]["work"] = serde_json::json!({
            "ratePerHour": 3.0,
            "leftMinutes": {"low": 470, "likely": 1140},
        });
        let account: Account = serde_json::from_value(raw.clone()).unwrap();

        // 0.65h (39m) fits within 40m of work left and says "under 1%", never "about 0%".
        assert_eq!(
            fit(&account, 0.65, now),
            "0.65 more hours of work fit at your work rate, leaving under 1% of 5 hours, about 54% of Weekly."
        );

        // Weekly rates rounded to tenths (0.25 -> 0.3, 0.34 -> 0.3) must still
        // follow work left rather than contradicting it.
        raw["limits"] = serde_json::json!([raw["limits"][1].clone()]);
        raw["deciding"] = "seven_day".into();
        raw["limits"][0]["leftPercent"] = 10.into();
        raw["limits"][0]["work"] = serde_json::json!({
            "ratePerHour": 0.3,
            "leftMinutes": {"low": 1500, "likely": 2400},
        });
        let forty: Account = serde_json::from_value(raw.clone()).unwrap();
        assert!(
            fit(&forty, 34.0, now).starts_with("34 more hours of work fit at your work rate"),
            "{}",
            fit(&forty, 34.0, now)
        );
        raw["limits"][0]["work"] = serde_json::json!({
            "ratePerHour": 0.3,
            "leftMinutes": {"low": 1080, "likely": 1740},
        });
        let twenty_nine: Account = serde_json::from_value(raw.clone()).unwrap();
        assert!(
            fit(&twenty_nine, 31.0, now)
                .starts_with("31 more hours of work don't fit: Weekly runs out after about 29h"),
            "{}",
            fit(&twenty_nine, 31.0, now)
        );

        // Post-reset exact 100% consumption doesn't fit with "about 0%".
        raw = example["accounts"][0].clone();
        raw["limits"] = serde_json::json!([raw["limits"][0].clone()]);
        raw["limits"][0]["window"]["resets"]["at"] = (now + 30 * MINUTE).into();
        raw["limits"][0]["window"]["startsAt"] = (now + 30 * MINUTE - 5 * time::HOUR).into();
        raw["limits"][0]["work"] = serde_json::json!({"ratePerHour": 50.0, "leftMinutes": null});
        let post_reset: Account = serde_json::from_value(raw).unwrap();
        assert_eq!(
            fit(&post_reset, 2.5, now),
            "2.5 more hours of work don't fit: 5 hours runs out after about 2h 30m of work."
        );
    }
}
