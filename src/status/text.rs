//! The status in words, as the command line and the MCP server say it:
//! each account and limit as lines, whether more hours of work fit, and the
//! one sentence that leads an answer. The app says it in its own words.

use super::{Account, Horizon, Limit, Moment, Outlook, Spread, Stale, State, Status, Unknown};
use crate::agents;
use crate::sessions::Cite;
use crate::time::{self, HOUR, MINUTE};

/// What under ten minutes of work reads as.
const UNDER: &str = "under 10m";

/// Hours of work as people say them: to ten minutes, `1h 50m`, and from
/// ten hours, to the hour, `26h`.
fn worked(hours: f64) -> String {
    let minutes = (hours * 60.0).round() as i64;
    match minutes {
        ..10 => UNDER.to_owned(),
        10..600 => time::span((minutes + 5) / 10 * 10 * MINUTE),
        _ => format!("{}h", (minutes + 30) / 60),
    }
}

/// The likely hours of work left, as a sentence says them: `about 1h 50m`,
/// or `under 10m` alone, which is already no exact amount.
fn about(hours: f64) -> String {
    match worked(hours) {
        under if under == UNDER => under,
        amount => format!("about {amount}"),
    }
}

/// `text` with a capital first letter, to begin a sentence.
fn capital(text: &str) -> String {
    format!("{}{}", text[..1].to_uppercase(), &text[1..])
}

/// The agents to open to sign in to `account` again: `Codex or Pi`. There's
/// always one: with no agent signed in to it, an account is signed out,
/// whatever its last read said (`status`).
fn login_agents(account: &Account) -> String {
    let names: Vec<&str> = account.agents.iter().map(|id| agents::name(id)).collect();
    names.join(" or ")
}

/// What's said where no account is shown: none is known yet, or every one
/// is hidden, and then how to see one, by `cite`.
pub fn none(status: &Status, cite: Cite) -> String {
    match (status.accounts.is_empty(), cite) {
        (true, _) => "No account's limits have been read yet: no agent here is signed in to one Turnscope reads.",
        (false, Cite::Command) => "Every account is hidden: `turnscope status --all` shows them.",
        (false, Cite::Tool) => "Every account is hidden: name one with account to see its limits.",
    }
    .to_owned()
}

/// Why `account`'s limits aren't read now, and what to do about it: `its
/// login was refused; sign in again in Codex`. Read before, it says since
/// when, or only `since` when the time of that read is `dated` beside it.
pub fn stale(account: &Account, dated: bool, now: i64) -> Option<String> {
    let (why, read_at) = match account.state {
        State::Live { .. } => return None,
        State::AsOf { read_at, why } => (why, Some(read_at)),
        State::Unread { why } => (why, None),
    };
    let agents = login_agents(account);
    Some(match why {
        Stale::SignedOut => "no agent here is signed in to it now".to_owned(),
        Stale::SignIn => format!("its login was refused; sign in again in {agents}"),
        Stale::Expired => format!("its login expired; open {agents} to update"),
        Stale::ReadFailed => format!(
            "it couldn't be read {}; it's tried again every {} minutes",
            match read_at {
                Some(_) if dated => "since".to_owned(),
                Some(read_at) => format!("since {}", time::clock(read_at, now)),
                None => "yet".to_owned(),
            },
            crate::limits::DUE / MINUTE
        ),
    })
}

/// The status of `accounts`, `yours` marked so.
pub fn text(status: &Status, accounts: &[&Account], yours: Option<&str>) -> String {
    let now = status.at;
    let mut out = String::new();
    for account in accounts {
        out += &account.full_title();
        // Which agents draw on it, to choose where to work.
        if let Some((last, rest)) = account.agents.split_last() {
            let names: Vec<&str> = rest.iter().map(|agent| agents::name(agent)).collect();
            out += &match names.is_empty() {
                true => format!(" · via {}", agents::name(last)),
                false => format!(" · via {} and {}", names.join(", "), agents::name(last)),
            };
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
                .map(crate::usage::dollars)
                .filter(|pace| *pace != spent)
                .map_or(String::new(), |pace| format!(", on pace for about {pace}"));
            out += &format!("  This month, across its keys: {spent}{pace}{partial}.\n");
        }
        out += "\n";
    }
    out.trim_end().to_owned()
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
/// left at the pace of work.
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
            Some(Spread { low, likely, high }) if low == high => {
                format!("Lasts, with about {likely}% left at the reset.")
            }
            Some(Spread { low, likely, high }) => {
                format!("Lasts, with about {likely}% left at the reset ({low}–{high}%).")
            }
            None => "No use shows in its reading yet.".to_owned(),
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
            format!(
                "At this pace it runs out {}, {}.",
                time::ahead(likely.at, now),
                match latest {
                    Some(latest) => format!("between {} and {}", near(soonest.at), near(latest.at)),
                    None => format!("as soon as {}", near(soonest.at)),
                }
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
            Unknown::Rolling => "Rolling: use drops out as the window moves.".to_owned(),
        },
    };
    if let Some(work) = &limit.work
        && !matches!(limit.outlook, Outlook::UsedUp { .. })
    {
        let rate = work.rate_per_hour;
        out += &match work.left_hours {
            Some(left) => format!(
                " {} of work left{}, at {rate}% an hour of work.",
                capital(&about(left.likely)),
                range(limit, &left, now)
            ),
            None => {
                format!(" More work left than time until the reset, at {rate}% an hour of work.")
            }
        };
    }
    out
}

/// The 80% range of the hours of work `limit` has left, as it follows the
/// likely hours: ` (1h 10m–3h)`, or ` (1h 10m or more)` when its slow end
/// is held at the reset, as much work as there's time for. Nothing when it
/// says no more than the likely hours do: its ends read the same, as under
/// ten minutes twice, or it's under ten minutes or more, which is any
/// amount.
fn range(limit: &Limit, left: &super::HoursLeft, now: i64) -> String {
    let until = limit
        .resets()
        .map(|resets| (resets - now) as f64 / HOUR as f64);
    let (low, high) = (worked(left.low), worked(left.high));
    match until.is_some_and(|until| left.high >= until - 0.1) {
        true if low == UNDER => String::new(),
        true => format!(" ({low} or more)"),
        false if low == high => String::new(),
        false => format!(" ({low}–{high})"),
    }
}

/// `account`'s limit on all its usage that's used up, and when it's back:
/// the limit that decides it, so of several, the last back.
fn used_up(account: &Account) -> Option<(&Limit, Option<Moment>)> {
    let limit = account.deciding_limit().filter(|limit| limit.whole())?;
    match limit.outlook {
        Outlook::UsedUp { back } => Some((limit, back)),
        _ => None,
    }
}

/// Whether `hours` more of work fit `account`'s limits on all its usage, at
/// the pace of work so far.
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
            "{more} of work {dont_fit}: {} is used up.",
            limit.name
        ));
    }
    let mut after = Vec::new();
    let mut unknown = None;
    let mut rolls = None;
    for limit in account.limits.iter().filter(|limit| limit.whole()) {
        match &limit.work {
            Some(work) => {
                if let Some(left) = work.left_hours.filter(|left| left.likely < hours) {
                    return Ok(format!(
                        "{more} of work {dont_fit}: {} runs out after {} of work.",
                        limit.name,
                        about(left.likely)
                    ));
                }
                // At the pace of work, until the reset at most.
                let until = limit.resets().map_or(hours, |resets| {
                    ((resets - now).max(0) as f64 / HOUR as f64).min(hours)
                });
                let left = f64::from(limit.left_percent) - work.rate_per_hour * until;
                after.push(format!("about {:.0}% of {}", left.max(0.0), limit.name));
            }
            // A window that rolls has no start to measure work from.
            _ if limit
                .window
                .is_some_and(|window| window.starts_at.is_none()) =>
            {
                rolls = rolls.or(Some(&limit.name))
            }
            None => unknown = unknown.or(Some(&limit.name)),
        }
    }
    let rolls = rolls.map_or(String::new(), |name| {
        format!(" {name} is a window that rolls, so it isn't forecast.")
    });
    Ok(match (unknown, after.is_empty()) {
        (Some(name), _) => format!(
            "Whether {more} of work {fit} can't be told yet: too little work on {name} has been measured."
        ),
        (None, true) if !rolls.is_empty() => {
            format!("Whether {more} of work {fit} can't be told.{rolls}")
        }
        (None, true) => format!(
            "Whether {more} of work {fit} can't be told: no limit on all its usage was read."
        ),
        (None, false) => format!(
            "{more} of work {fit} at your working pace, leaving {}.{rolls}",
            after.join(", ")
        ),
    })
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
fn standing(account: &Account, title: &str, now: i64) -> String {
    if let Some((limit, back)) = used_up(account) {
        let back = back.map_or(String::new(), |back| {
            format!(", back {}", time::ahead(back.at, now))
        });
        return format!("{title}: {} is used up{back}.", limit.name);
    }
    let whole: Vec<&Limit> = account
        .limits
        .iter()
        .filter(|limit| limit.whole() && limit.reset_since.is_none())
        .collect();
    let first = whole
        .iter()
        .filter_map(|limit| Some((limit, limit.work?.left_hours?)))
        .min_by(|(_, a), (_, b)| a.likely.total_cmp(&b.likely));
    if let Some((limit, left)) = first {
        let resets = limit.window.map_or(String::new(), |window| {
            format!("; it resets {}", time::ahead(window.resets.at, now))
        });
        return format!(
            "{title}: {} of work left{} before {} runs out{resets}.",
            about(left.likely),
            range(limit, &left, now),
            limit.name
        );
    }
    if !whole.is_empty() && whole.iter().all(|limit| limit.work.is_some()) {
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
            " and runs out {} at this pace, before it resets {resets}",
            time::ahead(likely.at, now)
        ),
        (Outlook::Lasts { .. }, Some(resets)) => {
            format!(" and lasts at this pace until it resets {resets}")
        }
        (_, Some(resets)) => format!("; it resets {resets}"),
        (_, None) => String::new(),
    };
    format!("{title}: {} has {} left{ahead}.", limit.name, left(limit))
}

/// `account` as one line, for an agent's status line: its title, its limits
/// on all its usage, and any other running out or used up, as `Claude Max ·
/// 5h 86% · weekly 25%, out Sat 18:00`. Once a limit on all its usage is
/// used up, only that one.
pub fn line(account: &Account, now: i64) -> String {
    if account.refused() {
        return format!("{} · sign in again", account.title);
    }
    let mut parts = vec![account.title.clone()];
    let used_up = |limit: &&Limit| matches!(limit.outlook, Outlook::UsedUp { .. });
    let blocking: Vec<&Limit> = account
        .limits
        .iter()
        .filter(|limit| limit.whole() && used_up(limit))
        .collect();
    let shown: Vec<&Limit> = if blocking.is_empty() {
        account
            .limits
            .iter()
            .filter(|limit| {
                limit.whole() || used_up(limit) || matches!(limit.outlook, Outlook::RunsOut { .. })
            })
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

/// A limit in a few words: `5h 86%`, `weekly 25%, out Sat 18:00`, `weekly
/// used up, back Sun 00:00`.
fn short(limit: &Limit, now: i64) -> String {
    let name = limit.lowercase("h");
    let left = match limit.money {
        Some(money) => crate::usage::dollars(money.left_usd),
        None => format!("{}%", limit.left_percent),
    };
    // Soon, as a countdown; later, as the clock says it.
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
        let now = example["params"]["at"].as_i64().unwrap();
        let claude = &example["params"]["accounts"][0];
        // Claude Max's 5 hours with half an hour of work left.
        let mut account = claude.clone();
        account["limits"][0]["work"]["leftHours"]["likely"] = 0.5.into();
        let account: Account = serde_json::from_value(account).unwrap();
        assert_eq!(
            fit(&account, 1.0, now).unwrap(),
            "1 more hour of work doesn't fit: 5 hours runs out after about 30m of work."
        );
        assert_eq!(
            fit(&account, 2.0, now).unwrap(),
            "2 more hours of work don't fit: 5 hours runs out after about 30m of work."
        );
        // Its 5 hours made a window that rolls, as OpenCode Go's is: said,
        // rather than taken for too little work. Its weekly is forecast.
        let mut account = claude.clone();
        let five = &mut account["limits"][0];
        five["window"]["startsAt"] = serde_json::Value::Null;
        five["work"] = serde_json::Value::Null;
        five["outlook"] = serde_json::json!({"kind": "unknown", "reason": "rolling"});
        let account: Account = serde_json::from_value(account).unwrap();
        assert_eq!(
            fit(&account, 1.0, now).unwrap(),
            "1 more hour of work fits at your working pace, leaving about 59% of Weekly. 5 hours is a window that rolls, so it isn't forecast."
        );
    }

    #[test]
    fn an_account_with_two_limits_used_up_is_back_when_the_last_is() {
        let example = example();
        let now = example["params"]["at"].as_i64().unwrap();
        // Claude Max's 5 hours back in an hour and its Weekly in three
        // days, which decides it.
        let (soon, later) = (now + HOUR, now + 72 * HOUR);
        let mut account = example["params"]["accounts"][0].clone();
        account["limits"][0]["outlook"] =
            serde_json::json!({"kind": "usedUp", "back": {"at": soon, "horizon": "soon"}});
        account["limits"][1]["outlook"] =
            serde_json::json!({"kind": "usedUp", "back": {"at": later, "horizon": "thisWeek"}});
        account["deciding"] = "seven_day".into();
        let account: Account = serde_json::from_value(account).unwrap();
        assert_eq!(
            verdict(&account, now),
            format!(
                "Claude Max (me@example.com): Weekly is used up, back {}.",
                time::ahead(later, now)
            )
        );
        assert_eq!(
            fit(&account, 1.0, now).unwrap(),
            "1 more hour of work doesn't fit: Weekly is used up."
        );
    }

    #[test]
    fn under_ten_minutes_of_work_is_said_once_and_not_about() {
        let example = example();
        let now = example["params"]["at"].as_i64().unwrap();
        // Claude Max's 5 hours with 6 minutes of work left, and as much at
        // the slow end.
        let mut account = example["params"]["accounts"][0].clone();
        let hours = &mut account["limits"][0]["work"]["leftHours"];
        *hours = serde_json::json!({"low": 0.0, "likely": 0.1, "high": 0.1});
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
        assert_eq!(
            fit(&few, 1.0, now).unwrap(),
            "1 more hour of work doesn't fit: 5 hours runs out after under 10m of work."
        );
        // A slow end of ten minutes or more is still said.
        account["limits"][0]["work"]["leftHours"]["high"] = 0.3.into();
        let more: Account = serde_json::from_value(account.clone()).unwrap();
        let line = limit_text(&more.limits[0], now);
        assert!(
            line.ends_with(". Under 10m of work left (under 10m–20m), at 25% an hour of work."),
            "{line}"
        );
        // Under ten minutes or as much as there's time for is any amount.
        account["limits"][0]["window"]["resets"]["at"] = (now + 20 * MINUTE).into();
        let any: Account = serde_json::from_value(account).unwrap();
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
        let mut example = example();
        let now = example["params"]["at"].as_i64().unwrap();
        let accounts = &mut example["params"]["accounts"];
        let five = &mut accounts[0]["limits"][0];
        five["window"]["resets"]["at"] = now.into();
        five["outlook"]["likely"]["at"] = now.into();
        accounts[1]["state"]["readAt"] = (now + 10_000).into();
        let status: Status = serde_json::from_value(example["params"].clone()).unwrap();
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
        serde_json::from_str(include_str!("../../contract/status.notification.json")).unwrap()
    }

    #[test]
    fn dollars_from_a_thousand_have_a_comma_as_on_the_status_line() {
        let mut example = example();
        let key = &mut example["params"]["accounts"][2];
        key["spend"] =
            serde_json::json!({"monthUsd": 1234.5, "forecastUsd": 2469.0, "partial": false});
        key["limits"][0]["money"] = serde_json::json!({"leftUsd": 1234.5, "sizeUsd": 2000.0});
        let status: Status = serde_json::from_value(example["params"].clone()).unwrap();
        let said = text(&status, &[&status.accounts[2]], None);
        assert!(
            said.contains("Key limit: $1,234.50 of $2,000.00 left"),
            "{said}"
        );
        assert!(
            said.contains("This month, across its keys: $1,234.50, on pace for about $2,469.00."),
            "{said}"
        );
    }

    #[test]
    fn why_an_account_isnt_read_is_said_the_same_way_everywhere() {
        let status: Status = serde_json::from_value(example()["params"].clone()).unwrap();
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
                "it couldn't be read since; it's tried again every 5 minutes.",
                " (10m ago): it couldn't be read since; it's tried again every 5 minutes.",
            ),
            (
                "OpenCode Go",
                "its login was refused; sign in again in Claude Code.",
                "  Its login was refused; sign in again in Claude Code.",
            ),
            (
                "SuperGrok",
                "its login was refused; sign in again in Claude Code.",
                " (1h 30m ago): its login was refused; sign in again in Claude Code.",
            ),
            (
                "Anthropic API key",
                "it couldn't be read yet; it's tried again every 5 minutes.",
                "  It couldn't be read yet; it's tried again every 5 minutes.",
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
    }
}
