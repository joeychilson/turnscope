//! The status in words, as the command line and the MCP server say it:
//! each account and limit as lines, whether more hours of work fit, and the
//! one sentence that leads an answer. The app says it in its own words.

use std::fmt::Write as _;

use super::{
    Account, Horizon, HoursLeft, Limit, Moment, Outlook, Spread, Stale, State, Status, Unknown,
};
use crate::agents;
use crate::time::{self, MINUTE};

const UNDER: &str = "under 10m";

fn worked(hours: f64) -> String {
    let minutes = (hours * 60.0).round() as i64;
    match minutes {
        ..10 => UNDER.to_owned(),
        10..600 => time::span((minutes + 5) / 10 * 10 * MINUTE),
        _ => format!("{}h", (minutes + 30) / 60),
    }
}

fn about(hours: f64) -> String {
    match worked(hours) {
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

fn agree<'a>(limit: &Limit, one: &'a str, many: &'a str) -> &'a str {
    match limit.plural() {
        true => many,
        false => one,
    }
}

fn login_agents(account: &Account) -> String {
    let names: Vec<&str> = account.agents.iter().map(|id| agents::name(id)).collect();
    names.join(" or ")
}

pub(super) const NONE_READ: &str =
    "No account's limits have been read yet: no agent here is signed in to one Turnscope reads.";

pub fn none(status: &Status, show: &str) -> String {
    match status.accounts.is_empty() {
        true => NONE_READ.to_owned(),
        false => format!("Every account is hidden: {show}."),
    }
}

pub fn stale(account: &Account, dated: bool, now: i64) -> Option<String> {
    let (why, read_at) = match account.state {
        State::Live { .. } => return None,
        State::AsOf { read_at, why } => (why, Some(read_at)),
        State::Unread { why } => (why, None),
    };
    let agents = login_agents(account);
    Some(match why {
        Stale::SignedOut => "no agent here is signed in to it now".to_owned(),
        Stale::SignIn => format!("its login was refused; open {agents} to sign in again"),
        Stale::Expired => {
            format!("its cached token expired; updates automatically when {agents} runs")
        }
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

/// The status of `accounts`, `yours` marked so.
pub fn text(status: &Status, accounts: &[&Account], yours: Option<&str>) -> String {
    let now = status.at;
    let mut out = String::new();
    for account in accounts {
        out.push_str(&account.full_title());
        // Which agents draw on it, to choose where to work.
        if let Some((last, rest)) = account.agents.split_last() {
            let names: Vec<&str> = rest.iter().map(|agent| agents::name(agent)).collect();
            if names.is_empty() {
                let _ = write!(out, " · via {}", agents::name(last));
            } else {
                let _ = write!(
                    out,
                    " · via {} and {}",
                    names.join(", "),
                    agents::name(last)
                );
            }
        }
        for (on, mark) in [
            (Some(account.id.as_str()) == yours, "yours"),
            (account.in_use, "in use"),
            (account.hidden, "hidden"),
        ] {
            if on {
                let _ = write!(out, " · {mark}");
            }
        }
        out.push('\n');
        if let Some(stale) = stale(account, true, now) {
            match account.state {
                State::AsOf { read_at, .. } => {
                    let _ = writeln!(out, "  As of {}: {stale}.", time::ago(read_at, now));
                }
                _ => {
                    let _ = writeln!(out, "  {}.", capital(&stale));
                }
            }
        }
        for limit in &account.limits {
            let _ = writeln!(out, "  {}", limit_text(limit, now));
        }
        if let Some(spend) = &account.spend {
            let partial = if spend.partial {
                ", and some usage has no known price"
            } else {
                ""
            };
            let spent = crate::usage::dollars(spend.month_usd);
            // A pace that reads as what was spent says nothing more, as for
            // a key that spent nothing: "$0.00, on pace for about $0.00".
            let pace = spend
                .forecast_usd
                .map(forecast_dollars)
                .filter(|pace| *pace != forecast_dollars(super::two_figures(spend.month_usd)));
            let _ = write!(out, "  This month, across its keys: {spent}");
            if let Some(pace) = pace {
                let _ = write!(out, ", on pace for about {pace}");
            }
            let _ = writeln!(out, "{partial}.");
        }
        out.push('\n');
    }
    out.trim_end().to_owned()
}

fn forecast_dollars(usd: f64) -> String {
    if usd < 10.0 {
        crate::usage::dollars(usd)
    } else {
        format!("${}", crate::usage::count(usd.round() as u64))
    }
}

fn percent_left(limit: &Limit) -> String {
    if limit.estimated {
        format!("~{}%", limit.left_percent)
    } else {
        format!("{}%", limit.left_percent)
    }
}

fn left(limit: &Limit) -> String {
    match limit.money {
        Some(money) => format!(
            "{} of {}",
            crate::usage::dollars(money.left_usd),
            crate::usage::dollars(money.size_usd)
        ),
        None => percent_left(limit),
    }
}

pub fn limit_text(limit: &Limit, now: i64) -> String {
    let mut out = format!("{}: {} left", limit.named(), left(limit));
    match (&limit.window, limit.reset_since) {
        (Some(_), Some(reset)) => {
            let _ = write!(out, ", reset {}", time::ago(reset, now));
        }
        (Some(window), None) if !matches!(limit.outlook, Outlook::UsedUp { .. }) => {
            let _ = write!(out, ", resets {}", time::ahead(window.resets.at, now));
        }
        _ => {}
    }
    out.push_str(". ");
    match &limit.outlook {
        Outlook::Lasts { left_at_reset } => match left_at_reset {
            Some(Spread { likely: 0, .. }) => {
                out.push_str("At this pace it runs out about when it resets.");
            }
            Some(Spread { low, likely, high }) if low == high => {
                let _ = write!(out, "Lasts, with about {likely}% left at the reset.");
            }
            Some(Spread { low, likely, high }) => {
                let _ = write!(
                    out,
                    "Lasts, with about {likely}% left at the reset ({low}–{high}%)."
                );
            }
            None => out.push_str("No use shows in its reading yet."),
        },
        Outlook::RunsOut {
            likely,
            soonest,
            latest,
        } => {
            // Today's day said where the likely time says one, as "13:30"
            // beside "Sat 18:00" would read as Saturday's.
            let near = |at: i64| match time::today(at, now) && !time::today(likely.at, now) {
                true => format!("today {}", time::clock(at, now)),
                false => time::clock(at, now),
            };
            let _ = write!(
                out,
                "At this pace it runs out {}, ",
                time::ahead(likely.at, now)
            );
            match latest {
                Some(latest) => {
                    let _ = write!(out, "between {} and {}.", near(soonest.at), near(latest.at));
                }
                None => {
                    let _ = write!(out, "as soon as {}.", near(soonest.at));
                }
            }
        }
        Outlook::UsedUp { back: Some(back) } => {
            let _ = write!(out, "Used up; back {}.", time::ahead(back.at, now));
        }
        Outlook::UsedUp { back: None } => out.push_str("Used up."),
        Outlook::Unknown { reason } => match reason {
            Unknown::NotEnoughData => out.push_str("No forecast yet."),
            Unknown::Stale if limit.reset_since.is_some() => {
                out.push_str("Full since the reset, unless used on the web or another device.");
            }
            Unknown::Stale if limit.estimated => {
                let _ = write!(
                    out,
                    "Estimated from local usage since {}.",
                    time::clock(limit.read_at, now)
                );
            }
            Unknown::Stale => {
                let _ = write!(out, "Not read since {}.", time::clock(limit.read_at, now));
            }
            Unknown::NoReset => out.push_str("Doesn't reset."),
            Unknown::Rolling => out.push_str("Rolling: use drops out as the window moves."),
        },
    }
    if let Some(work) = &limit.work
        && !matches!(limit.outlook, Outlook::UsedUp { .. })
    {
        let rate = work.rate_per_hour;
        match work.left_hours {
            Some(left) => {
                let _ = write!(
                    out,
                    " {} of work left{}, at {rate}% an hour of work.",
                    capital(&about(left.likely)),
                    range(limit, &left, now)
                );
            }
            None => {
                let _ = write!(
                    out,
                    " More work left than time until the reset, at {rate}% an hour of work."
                );
            }
        }
    }
    out
}

fn range(limit: &Limit, left: &HoursLeft, now: i64) -> String {
    let until = limit.hours_to_reset(now);
    let (low, high) = (worked(left.low), worked(left.high));
    match until.is_some_and(|until| left.high >= until - 0.1) {
        true if low == UNDER => String::new(),
        true => format!(" ({low} or more)"),
        false if low == high => String::new(),
        false => format!(" ({low}–{high})"),
    }
}

fn is_governing(limit: &Limit, has_whole: bool) -> bool {
    if has_whole {
        limit.whole()
    } else {
        limit.scope.is_some() && !limit.is_key()
    }
}

fn governing(account: &Account) -> impl Iterator<Item = &Limit> {
    let has_whole = account.limits.iter().any(Limit::whole);
    account
        .limits
        .iter()
        .filter(move |limit| is_governing(limit, has_whole))
}

fn used_up(account: &Account) -> Option<(&Limit, Option<Moment>)> {
    let has_whole = account.limits.iter().any(Limit::whole);
    let limit = account
        .deciding_limit()
        .filter(|limit| is_governing(limit, has_whole))?;
    match limit.outlook {
        Outlook::UsedUp { back } => Some((limit, back)),
        _ => None,
    }
}

fn first_out(account: &Account) -> Option<(&Limit, HoursLeft)> {
    governing(account)
        .filter_map(|limit| Some((limit, limit.work?.left_hours?)))
        .min_by(|(_, a), (_, b)| a.likely.total_cmp(&b.likely))
}

/// Whether `hours` more of work fit `account`'s governing limits, at the work
/// rate so far.
///
/// A limit with no work rate measured, too little work or a window that
/// rolls, says it can't be told, naming it and what's left of it: said to
/// fit with a note after, OpenCode Go's rolling 5 hours at 4% left read as
/// room for hours of work.
pub fn fit(account: &Account, hours: f64, now: i64) -> crate::Result<String> {
    if !(hours.is_finite() && hours >= 0.0) {
        return Err(crate::Error::Usage(format!(
            "{hours} isn't a number of hours: give one of zero or more"
        )));
    }
    let (more, fit, dont_fit) = match hours == 1.0 {
        true => ("1 more hour".to_owned(), "fits", "doesn't fit"),
        false => (format!("{hours} more hours"), "fit", "don't fit"),
    };
    if let Some((limit, _)) = used_up(account) {
        return Ok(format!(
            "{more} of work {dont_fit}: {} {} used up.",
            limit.named(),
            agree(limit, "is", "are")
        ));
    }
    let mut out: Option<(&Limit, f64)> = None;
    let mut after = Vec::new();
    let mut untold = None;
    for limit in governing(account) {
        let Some(work) = &limit.work else {
            if untold.is_none() {
                // A window that rolls has no start to measure work from.
                let rolls = limit
                    .window
                    .is_some_and(|window| window.starts_at.is_none());
                let why = match rolls {
                    true => format!(
                        "{} a window that rolls, so it isn't forecast",
                        agree(limit, "is", "are")
                    ),
                    false => format!(
                        "too little work on {} has been measured",
                        agree(limit, "it", "them")
                    ),
                };
                untold = Some(format!(
                    "{} {} {} left, and {why}",
                    limit.named(),
                    agree(limit, "has", "have"),
                    left(limit)
                ));
            }
            continue;
        };
        match ends(limit, work, hours, now) {
            End::Out(at) if out.is_none_or(|(_, first)| at < first) => out = Some((limit, at)),
            End::Out(_) => {}
            End::Leaves(left) => after.push(format!("about {left:.0}% of {}", limit.named())),
        }
    }
    if let Some((limit, at)) = out {
        return Ok(format!(
            "{more} of work {dont_fit}: {} {} out after {} of work.",
            limit.named(),
            agree(limit, "runs", "run"),
            about(at)
        ));
    }
    let after = after.join(", ");
    Ok(match (untold, after.is_empty()) {
        (Some(why), true) => format!("Whether {more} of work {fit} can't be told: {why}."),
        (Some(why), false) => format!(
            "Whether {more} of work {fit} can't be told: {why}. At your work rate they'd leave {after}."
        ),
        (None, true) => format!(
            "Whether {more} of work {fit} can't be told: no limit on all its usage was read."
        ),
        (None, false) => format!("{more} of work {fit} at your work rate, leaving {after}."),
    })
}

enum End {
    Out(f64),
    Leaves(f64),
}

/// Accounts for resets occurring within `hours` by rolling the remaining work
/// into subsequent windows of the same length.
fn ends(limit: &Limit, work: &crate::status::Work, hours: f64, now: i64) -> End {
    let rate = work.rate_per_hour;
    // Out before its reset, as forecast with the spread of work's pace.
    if let Some(left) = work.left_hours
        && left.likely < hours
    {
        return End::Out(left.likely);
    }
    let left = f64::from(limit.left_percent);
    let Some(until) = limit.hours_to_reset(now).filter(|until| *until < hours) else {
        return End::Leaves((left - rate * hours).max(0.0));
    };
    let length = limit
        .window
        .and_then(|window| Some((window.resets.at - window.starts_at?) as f64 / time::HOUR as f64));
    let past = hours - until;
    // The work in the first window after, and in the last.
    let (first, last) = match length.filter(|length| *length > 0.0) {
        Some(length) => (
            past.min(length),
            past - ((past / length).ceil() - 1.0).max(0.0) * length,
        ),
        None => (past, past),
    };
    match first * rate > 100.0 {
        true => End::Out(until + 100.0 / rate),
        false => End::Leaves((100.0 - rate * last).max(0.0)),
    }
}

/// The one thing to know of `account`: whether there's room to keep going,
/// and for how much work, by the limit on all its usage that runs out first.
pub fn verdict(account: &Account, now: i64) -> String {
    let title = account.full_title();
    let Some(stale) = stale(account, true, now) else {
        return standing(account, &title, now);
    };
    match account.state {
        // What's said rests on an earlier read, and an agent acts on this
        // line first.
        State::AsOf { read_at, .. } => format!(
            "{} That's as of {}: {stale}.",
            standing(account, &title, now),
            time::clock(read_at, now)
        ),
        _ => format!("{title}: {stale}."),
    }
}

/// How `account`, read, stands: used up until the limit that decides it is
/// back, or else by its limit on all its usage closest to running out, by
/// the work left or else the clock's pace.
///
/// The limit that decides it, with no work rate measured, is said by the
/// clock's pace, over another's work left: OpenCode Go's rolling 5 hours at
/// 4% left was passed over for "about 12h of work left before Weekly runs
/// out".
fn standing(account: &Account, title: &str, now: i64) -> String {
    if let Some((limit, back)) = used_up(account) {
        let back = back.map_or(String::new(), |back| {
            format!(", back {}", time::ahead(back.at, now))
        });
        let is = agree(limit, "is", "are");
        return format!("{title}: {} {is} used up{back}.", limit.named());
    }
    let has_whole = account.limits.iter().any(Limit::whole);
    let unmeasured = account
        .deciding_limit()
        .is_some_and(|limit| is_governing(limit, has_whole) && limit.work.is_none());
    if !unmeasured && let Some((limit, left)) = first_out(account) {
        let resets = limit.resets().map_or(String::new(), |resets| {
            format!("; it resets {}", time::ahead(resets, now))
        });
        return format!(
            "{title}: {} of work left{} before {} {} out{resets}.",
            about(left.likely),
            range(limit, &left, now),
            limit.named(),
            agree(limit, "runs", "run")
        );
    }
    let mut gov = governing(account).peekable();
    if gov.peek().is_some() && gov.all(|limit| limit.work.is_some()) {
        return format!(
            "{title}: room to keep going; no limit runs out before it resets, even working nonstop."
        );
    }
    // Until an hour of work is measured, the clock's pace says it.
    let Some(limit) = account.deciding_limit() else {
        return format!("{title}: no limit of it was read.");
    };
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
    format!(
        "{title}: {} {has} {} left{ahead}.",
        limit.named(),
        left(limit)
    )
}

/// `account` as one line, for an agent's status line: its title, its governing
/// limits, and any other running out or used up, as `Claude Max · 5h 86% ·
/// weekly 25%, out Sat 18:00`. Once a limit on all its usage is used up, only
/// that one.
pub fn line(account: &Account, now: i64) -> String {
    if account.refused() {
        return format!("{} · sign in again", account.title);
    }
    let mut parts = vec![account.title.clone()];
    let is_used_up = |limit: &&Limit| matches!(limit.outlook, Outlook::UsedUp { .. });
    let has_whole = account.limits.iter().any(Limit::whole);
    let has_blocking = has_whole
        && account
            .limits
            .iter()
            .any(|limit| limit.whole() && is_used_up(&limit));
    parts.extend(
        account
            .limits
            .iter()
            .filter(|limit| {
                if has_blocking {
                    limit.whole() && is_used_up(limit)
                } else {
                    is_governing(limit, has_whole)
                        || is_used_up(limit)
                        || matches!(limit.outlook, Outlook::RunsOut { .. })
                }
            })
            .map(|limit| short(limit, now)),
    );
    match (&account.state, &account.spend) {
        (State::Unread { why }, None) => parts.push(
            match why {
                Stale::Expired => "updates on next use",
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

/// A limit in a few words: `5h 86%`, `weekly 25%, out Sat 18:00`, `weekly
/// used up, back Sun 00:00`.
fn short(limit: &Limit, now: i64) -> String {
    let name = limit.lowercase("h");
    let left = match limit.money {
        Some(money) => crate::usage::dollars(money.left_usd),
        None => percent_left(limit),
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
    fn hours_of_work_fit_or_dont_and_a_window_that_rolls_says_so() {
        let example = example();
        let now = example.at;
        let claude = &example.accounts[0];
        // Claude Max's 5 hours with half an hour of work left.
        let mut account = claude.clone();
        account.limits[0]
            .work
            .as_mut()
            .unwrap()
            .left_hours
            .as_mut()
            .unwrap()
            .likely = 0.5;
        assert_eq!(
            fit(&account, 1.0, now).unwrap(),
            "1 more hour of work doesn't fit: 5 hours runs out after about 30m of work."
        );
        assert_eq!(
            fit(&account, 2.0, now).unwrap(),
            "2 more hours of work don't fit: 5 hours runs out after about 30m of work."
        );
        // Its Weekly with 40 minutes of work left and 5 hours with 1h 15m:
        // the first out decides, by its likely hours, as the verdict says.
        let mut account = claude.clone();
        account.limits[0]
            .work
            .as_mut()
            .unwrap()
            .left_hours
            .as_mut()
            .unwrap()
            .likely = 1.25;
        account.limits[1].work.as_mut().unwrap().left_hours = Some(HoursLeft {
            low: 0.3,
            likely: 0.67,
            high: 1.5,
        });
        assert_eq!(
            fit(&account, 2.0, now).unwrap(),
            "2 more hours of work don't fit: Weekly runs out after about 40m of work."
        );
        let said = verdict(&account, now);
        assert!(
            said.starts_with(
                "Claude Max (me@example.com): about 40m of work left (20m–1h 30m) before Weekly runs out; it resets "
            ),
            "{said}"
        );
        // Its 5 hours reset since it was read, with a new window's 3 hours of
        // work, and its Weekly lasts even working nonstop: 5 hours decides.
        let mut account = claude.clone();
        let five = &mut account.limits[0];
        five.reset_since = Some(now - 10 * MINUTE);
        five.left_percent = 100;
        five.outlook = Outlook::Unknown {
            reason: Unknown::Stale,
        };
        five.work.as_mut().unwrap().left_hours = Some(HoursLeft {
            low: 1.9,
            likely: 3.0,
            high: 5.0,
        });
        account.deciding = Some("seven_day".to_owned());
        assert_eq!(
            fit(&account, 4.0, now).unwrap(),
            "4 more hours of work don't fit: 5 hours runs out after about 3h of work."
        );
        // Its slow end lasts the new window, as long as the one read.
        assert_eq!(
            verdict(&account, now),
            "Claude Max (me@example.com): about 3h of work left (1h 50m or more) before 5 hours runs out."
        );
        // At 10% an hour of work, 6 hours fit, and the new window ends with
        // half left: 5 hours of work count, not 6.
        let mut lasting = claude.clone();
        let five = &mut lasting.limits[0];
        five.reset_since = Some(now - 10 * MINUTE);
        five.left_percent = 100;
        five.work = Some(crate::status::Work {
            rate_per_hour: 10.0,
            left_hours: None,
        });
        let said = fit(&lasting, 6.0, now).unwrap();
        // The new window resets after 5 hours of work, and the one after
        // has one hour's 10% used.
        assert!(said.contains("leaving about 90% of 5 hours"), "{said}");
        // Every limit lasts, the one that reset too: room to keep going.
        assert_eq!(
            verdict(&lasting, now),
            "Claude Max (me@example.com): room to keep going; no limit runs out before it resets, even working nonstop."
        );
        // Too little work measured on it since: no room is claimed.
        let mut unmeasured = lasting;
        unmeasured.limits[0].work = None;
        let said = verdict(&unmeasured, now);
        assert!(!said.contains("room"), "{said}");
        // Its 5 hours made a window that rolls, as OpenCode Go's is: said,
        // rather than taken for too little work. Its weekly is forecast.
        let mut rolling = claude.clone();
        let five = &mut rolling.limits[0];
        five.window.as_mut().unwrap().starts_at = None;
        five.work = None;
        five.outlook = Outlook::Unknown {
            reason: Unknown::Rolling,
        };
        assert_eq!(
            fit(&rolling, 1.0, now).unwrap(),
            "Whether 1 more hour of work fits can't be told: 5 hours has 28% left, and is a window that rolls, so it isn't forecast. At your work rate they'd leave about 59% of Weekly."
        );
        // Deciding the account, it's said over Weekly's hours of work.
        let mut deciding = rolling;
        deciding.deciding = Some("five_hour".to_owned());
        let said = verdict(&deciding, now);
        assert!(
            said.starts_with("Claude Max (me@example.com): 5 hours has 28% left; it resets "),
            "{said}"
        );
    }

    /// 5 hours at 25% an hour of work, with 28% left, resets in 30 minutes:
    /// 6 hours of work run out its next window 4 hours into it, as they do
    /// a minute after the reset.
    #[test]
    fn hours_past_a_reset_count_in_the_window_after() {
        let example = example();
        let now = example.at;
        let mut before = example.accounts[0].clone();
        before.limits.truncate(1);
        let five = &mut before.limits[0];
        five.left_percent = 28;
        five.window.as_mut().unwrap().resets.at = now + 30 * MINUTE;
        five.window.as_mut().unwrap().starts_at = Some(now + 30 * MINUTE - 5 * time::HOUR);
        five.work = Some(crate::status::Work {
            rate_per_hour: 25.0,
            left_hours: None,
        });
        let mut after_reset = before.clone();
        let five = &mut after_reset.limits[0];
        five.reset_since = Some(now - MINUTE);
        five.left_percent = 100;
        five.window.as_mut().unwrap().resets.at = now - MINUTE;
        five.window.as_mut().unwrap().starts_at = Some(now - MINUTE - 5 * time::HOUR);
        five.work.as_mut().unwrap().left_hours = Some(HoursLeft {
            low: 4.0,
            likely: 4.0,
            high: 4.0,
        });
        assert_eq!(
            fit(&before, 6.0, now).unwrap(),
            "6 more hours of work don't fit: 5 hours runs out after about 4h 30m of work."
        );
        assert_eq!(
            fit(&after_reset, 6.0, now).unwrap(),
            "6 more hours of work don't fit: 5 hours runs out after about 4h of work."
        );
        // 2 hours fit: an hour and a half of it in the window after.
        assert_eq!(
            fit(&before, 2.0, now).unwrap(),
            "2 more hours of work fit at your work rate, leaving about 62% of 5 hours."
        );
    }

    #[test]
    fn credits_are_used_up_as_the_app_says_where_5_hours_is() {
        let example = example();
        let now = example.at;
        // The OpenRouter API key's credits, $45 of $50 left, which decide
        // it, read just now.
        let mut left = example.accounts[2].clone();
        left.state = State::Live { read_at: now };
        left.limits.truncate(1);
        let credits = &mut left.limits[0];
        credits.key = "credits".to_owned();
        credits.name = "Credits".to_owned();
        credits.held_by.clear();
        credits.left_percent = 90;
        credits.money = Some(crate::status::Money {
            size_usd: 50.0,
            left_usd: 45.0,
        });
        left.deciding = Some("credits".to_owned());
        assert_eq!(
            verdict(&left, now),
            "OpenRouter API key: Credits have $45.00 of $50.00 left."
        );
        // Used up, they're plural as the app's notification has them.
        let mut used_up = left;
        let credits = &mut used_up.limits[0];
        credits.left_percent = 0;
        credits.money.as_mut().unwrap().left_usd = 0.0;
        credits.outlook = Outlook::UsedUp { back: None };
        assert_eq!(
            verdict(&used_up, now),
            "OpenRouter API key: Credits are used up."
        );
        assert_eq!(
            fit(&used_up, 1.0, now).unwrap(),
            "1 more hour of work doesn't fit: Credits are used up."
        );
    }

    #[test]
    fn under_ten_minutes_of_work_is_said_once_and_not_about() {
        let example = example();
        let now = example.at;
        // Claude Max's 5 hours with 6 minutes of work left, and as much at
        // the slow end.
        let mut few = example.accounts[0].clone();
        few.limits[0].work.as_mut().unwrap().left_hours = Some(HoursLeft {
            low: 0.0,
            likely: 0.1,
            high: 0.1,
        });
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
        assert_eq!(
            fit(&few, 1.0, now).unwrap(),
            "1 more hour of work doesn't fit: 5 hours runs out after under 10m of work."
        );
        // A slow end of ten minutes or more is still said.
        let mut more = few;
        more.limits[0]
            .work
            .as_mut()
            .unwrap()
            .left_hours
            .as_mut()
            .unwrap()
            .high = 0.3;
        let line = limit_text(&more.limits[0], now);
        assert!(
            line.ends_with(". Under 10m of work left (under 10m–20m), at 25% an hour of work."),
            "{line}"
        );
        // Under ten minutes or as much as there's time for is any amount.
        let mut any = more;
        any.limits[0].window.as_mut().unwrap().resets.at = now + 20 * MINUTE;
        let line = limit_text(&any.limits[0], now);
        assert!(
            line.ends_with(". Under 10m of work left, at 25% an hour of work."),
            "{line}"
        );
    }

    #[test]
    fn a_time_in_the_minute_of_the_status_reads_as_its_words_say() {
        // The status is as of its minute's start: a limit that runs out or
        // resets in that minute is still to come, and a read in it has gone
        // by.
        let mut status = example();
        let now = status.at;
        let five = &mut status.accounts[0].limits[0];
        five.window.as_mut().unwrap().resets.at = now;
        if let Outlook::RunsOut { likely, .. } = &mut five.outlook {
            likely.at = now;
        }
        if let State::AsOf { read_at, .. } = &mut status.accounts[1].state {
            *read_at = now + 10_000;
        }
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

    fn example() -> Status {
        serde_json::from_str(include_str!("../../tests/fixtures/status.json")).unwrap()
    }

    #[test]
    fn why_an_account_isnt_read_is_said_the_same_way_everywhere() {
        let status = example();
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
        // Its agent renews an expired login when next used.
        let mut expired = status.accounts[0].clone();
        expired.state = State::Unread {
            why: Stale::Expired,
        };
        assert_eq!(
            verdict(&expired, status.at),
            "Claude Max (me@example.com): its cached token expired; updates automatically when Claude Code runs."
        );
    }
}
