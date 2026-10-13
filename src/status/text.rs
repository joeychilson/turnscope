//! The status in words, as the command line and the MCP server say it:
//! each account and limit as lines, whether more hours of work fit, and the
//! one sentence that leads an answer. The app says it in its own words.

use super::{Account, Limit, Outlook, Stale, State, Status, Unknown};
use crate::agents;
use crate::time::{self, HOUR, MINUTE, when};

/// Hours of work as people say them: to ten minutes, `1h 50m`, and from
/// ten hours, to the hour, `26h`.
fn worked(hours: f64) -> String {
    let minutes = (hours * 60.0).round() as i64;
    match minutes {
        ..10 => "under 10m".to_owned(),
        10..600 => time::span((minutes + 5) / 10 * 10 * MINUTE),
        _ => format!("{}h", (minutes + 30) / 60),
    }
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
        let agents = if account.agents.is_empty() {
            "the agent that uses it".to_owned()
        } else {
            account
                .agents
                .iter()
                .map(|agent| agents::name(agent))
                .collect::<Vec<_>>()
                .join(" or ")
        };
        match &account.state {
            State::Unread { why } => {
                out += &match why {
                    Stale::SignedOut => "  No agent here is signed in to it.\n".to_owned(),
                    Stale::SignIn => {
                        format!("  Its login was refused: sign in again in {agents}.\n")
                    }
                    Stale::Expired => {
                        format!("  Its login expired: open {agents} to update.\n")
                    }
                    Stale::ReadFailed => {
                        "  It couldn't be read yet: it's tried again every 5 minutes.\n".to_owned()
                    }
                }
            }
            State::AsOf { read_at, why } => {
                let as_of = format!(
                    "As of {} ({} ago)",
                    time::clock(*read_at, now),
                    time::span(now - read_at)
                );
                out += &match why {
                    Stale::SignedOut => {
                        format!("  {as_of}: no agent here is signed into it now.\n")
                    }
                    Stale::SignIn => {
                        format!("  {as_of}: its login was refused; sign in again in {agents}.\n")
                    }
                    Stale::Expired => {
                        format!("  {as_of}: its login expired; open {agents} to update.\n")
                    }
                    Stale::ReadFailed => format!("  {as_of}: it couldn't be read since.\n"),
                };
            }
            State::Live { .. } => {}
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
            out += &match spend.forecast_usd {
                Some(forecast) => format!(
                    "  This month, across its keys: ${:.2}, on pace for about ${forecast:.2}{partial}.\n",
                    spend.month_usd
                ),
                None => format!(
                    "  This month, across its keys: ${:.2}{partial}.\n",
                    spend.month_usd
                ),
            };
        }
        out += "\n";
    }
    out.trim_end().to_owned()
}

/// One limit as a line: what's left, the reset, the outlook, and the work
/// left at the pace of work.
pub fn limit_text(limit: &Limit, now: i64) -> String {
    let named = limit.named();
    let mut out = match limit.money {
        Some(money) => format!(
            "{named}: ${:.2} of ${:.2} left",
            money.left_usd, money.size_usd
        ),
        None => format!("{named}: {}% left", limit.left_percent),
    };
    match (&limit.window, limit.reset_since) {
        (Some(_), Some(reset)) => out += &format!(", reset {}", when(reset, now)),
        (Some(window), None) if !matches!(limit.outlook, Outlook::UsedUp { .. }) => {
            out += &format!(", resets {}", when(window.resets.at, now))
        }
        _ => {}
    }
    out += ". ";
    out += &match &limit.outlook {
        Outlook::Lasts {
            left_at_reset: Some(spread),
        } if spread.likely == 0 => "At this pace it runs out about when it resets.".to_owned(),
        Outlook::Lasts {
            left_at_reset: Some(spread),
        } if spread.low == spread.high => {
            format!("Lasts, with about {}% left at the reset.", spread.likely)
        }
        Outlook::Lasts {
            left_at_reset: Some(spread),
        } => format!(
            "Lasts, with about {}% left at the reset ({}–{}%).",
            spread.likely, spread.low, spread.high
        ),
        Outlook::Lasts {
            left_at_reset: None,
        } => "No use shows in its reading yet.".to_owned(),
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
                when(likely.at, now),
                match latest {
                    Some(latest) => format!("between {} and {}", near(soonest.at), near(latest.at)),
                    None => format!("as soon as {}", near(soonest.at)),
                }
            )
        }
        Outlook::UsedUp { back: Some(back) } => {
            format!("Used up; back {}.", when(back.at, now))
        }
        Outlook::UsedUp { back: None } => "Used up.".to_owned(),
        Outlook::Unknown {
            reason: Unknown::NotEnoughData,
        } => "No forecast yet.".to_owned(),
        Outlook::Unknown {
            reason: Unknown::Stale,
        } => match limit.reset_since {
            Some(_) => "Full since the reset, unless used on the web or another device.".to_owned(),
            None => format!("Not read since {}.", time::clock(limit.read_at, now)),
        },
        Outlook::Unknown {
            reason: Unknown::NoReset,
        } => "Doesn't reset.".to_owned(),
        Outlook::Unknown {
            reason: Unknown::Rolling,
        } => "Rolling: use drops out as the window moves.".to_owned(),
    };
    if let Some(work) = &limit.work
        && !matches!(limit.outlook, Outlook::UsedUp { .. })
    {
        let rate = work.rate_per_hour;
        out += &match work.left_hours {
            Some(left) => format!(
                " About {} of work left ({}), at {rate}% an hour of work.",
                worked(left.likely),
                range(limit, &left, now)
            ),
            None => {
                format!(" More work left than time until the reset, at {rate}% an hour of work.")
            }
        };
    }
    out
}

/// The 80% range of the hours of work `limit` has left: `1h 10m–3h`, or
/// `1h 10m or more` when its slow end is held at the reset, as much work as
/// there's time for.
fn range(limit: &Limit, left: &super::HoursLeft, now: i64) -> String {
    let until = limit
        .window
        .filter(|_| limit.reset_since.is_none())
        .map(|window| (window.resets.at - now) as f64 / HOUR as f64);
    match until.is_some_and(|until| left.high >= until - 0.1) {
        true => format!("{} or more", worked(left.low)),
        false => format!("{}–{}", worked(left.low), worked(left.high)),
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
    let mut after = Vec::new();
    let mut unknown = None;
    let mut rolls = None;
    for limit in account.limits.iter().filter(|limit| limit.whole()) {
        match (&limit.outlook, &limit.work) {
            (Outlook::UsedUp { .. }, _) if limit.reset_since.is_none() => {
                return Ok(format!(
                    "{more} of work {dont_fit}: {} is used up.",
                    limit.name
                ));
            }
            (_, Some(work)) => {
                if let Some(left) = work.left_hours.filter(|left| left.likely < hours) {
                    return Ok(format!(
                        "{more} of work {dont_fit}: {} runs out after about {} of work.",
                        limit.name,
                        worked(left.likely)
                    ));
                }
                // At the pace of work, until the reset at most.
                let until = limit
                    .window
                    .filter(|_| limit.reset_since.is_none())
                    .map_or(hours, |window| {
                        ((window.resets.at - now).max(0) as f64 / HOUR as f64).min(hours)
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
            (_, None) => unknown = unknown.or(Some(&limit.name)),
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
        (None, true) => {
            format!(
                "Whether {more} of work {fit} can't be told: no limit on all its usage was read."
            )
        }
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
    match account.state {
        State::Unread { why } => format!(
            "{title}: {}.",
            match why {
                Stale::SignIn => "its login was refused, so its limits can't be read",
                Stale::Expired => "its login expired, so its limits can't be read",
                Stale::SignedOut => "no agent here is signed in to it, so its limits can't be read",
                Stale::ReadFailed => "its limits couldn't be read yet",
            }
        ),
        // What's said rests on an earlier read, and an agent acts on this
        // line first.
        State::AsOf { read_at, why } => format!(
            "{} That's as of {}: {}.",
            standing(account, &title, now),
            time::clock(read_at, now),
            match why {
                Stale::SignIn => "its login was refused since",
                Stale::Expired => "its login expired since",
                Stale::SignedOut => "no agent here is signed in to it now",
                Stale::ReadFailed => "it couldn't be read since",
            }
        ),
        State::Live { .. } => standing(account, &title, now),
    }
}

/// How `account`, read, stands: its limit on all its usage closest to
/// running out, by the work left or else the clock's pace.
fn standing(account: &Account, title: &str, now: i64) -> String {
    let whole: Vec<&Limit> = account
        .limits
        .iter()
        .filter(|limit| limit.whole() && limit.reset_since.is_none())
        .collect();
    for limit in &whole {
        if let Outlook::UsedUp { back } = limit.outlook {
            let back = back.map_or(String::new(), |back| {
                format!(", back {}", when(back.at, now))
            });
            return format!("{title}: {} is used up{back}.", limit.name);
        }
    }
    let first = whole
        .iter()
        .filter_map(|limit| Some((limit, limit.work?.left_hours?)))
        .min_by(|(_, a), (_, b)| a.likely.total_cmp(&b.likely));
    if let Some((limit, left)) = first {
        let resets = limit.window.map_or(String::new(), |window| {
            format!("; it resets {}", when(window.resets.at, now))
        });
        return format!(
            "{title}: about {} of work left ({}) before {} runs out{resets}.",
            worked(left.likely),
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
    let left = match limit.money {
        Some(money) => format!("${:.2} of ${:.2}", money.left_usd, money.size_usd),
        None => format!("{}%", limit.left_percent),
    };
    let resets = limit
        .window
        .filter(|_| limit.reset_since.is_none())
        .map(|window| when(window.resets.at, now));
    let ahead = match (&limit.outlook, resets) {
        (Outlook::RunsOut { likely, .. }, Some(resets)) => format!(
            " and runs out {} at this pace, before it resets {resets}",
            when(likely.at, now)
        ),
        (Outlook::Lasts { .. }, Some(resets)) => {
            format!(" and lasts at this pace until it resets {resets}")
        }
        (_, Some(resets)) => format!("; it resets {resets}"),
        (_, None) => String::new(),
    };
    format!("{title}: {} has {left} left{ahead}.", limit.name)
}

/// `account` as one line, for an agent's status line: its title, its limits
/// on all its usage, and any other running out or used up, as `Claude Max ·
/// 5h 86% · weekly 25%, out Sat 18:00`. Once a limit on all its usage is
/// used up, only that one.
pub fn line(account: &Account, now: i64) -> String {
    let mut parts = vec![account.title.clone()];
    if account.refused() {
        parts.push("sign in again".to_owned());
        return parts.join(" · ");
    }
    let used_up = |limit: &&Limit| matches!(limit.outlook, Outlook::UsedUp { .. });
    let current = |limit: &&Limit| limit.reset_since.is_none();
    let blocking: Vec<&Limit> = account
        .limits
        .iter()
        .filter(|limit| current(limit) && limit.whole() && used_up(limit))
        .collect();
    let shown: Vec<&Limit> = if blocking.is_empty() {
        account
            .limits
            .iter()
            .filter(|limit| {
                limit.whole()
                    || (current(limit)
                        && (used_up(limit) || matches!(limit.outlook, Outlook::RunsOut { .. })))
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
    let name = limit.name.to_lowercase();
    let name = match name.split_once(' ') {
        Some((count, "hours")) if count.parse::<u32>().is_ok() => format!("{count}h"),
        _ => name,
    };
    let name = match &limit.scope {
        Some(scope) => format!("{scope} {name}"),
        None => name,
    };
    let left = match limit.money {
        Some(money) => crate::usage::dollars(money.left_usd),
        None => format!("{}%", limit.left_percent),
    };
    // Soon, as a countdown; later, as the clock says it.
    let at = |at: i64| match at - now < 3 * HOUR {
        true => format!("in {}", time::span(at - now)),
        false => time::clock(at, now),
    };
    match limit.outlook {
        _ if limit.reset_since.is_some() => format!("{name} {left}"),
        Outlook::UsedUp { back: Some(back) } => format!("{name} used up, back {}", at(back.at)),
        Outlook::UsedUp { back: None } => format!("{name} used up"),
        Outlook::RunsOut { likely, .. } => format!("{name} {left}, out {}", at(likely.at)),
        _ => format!("{name} {left}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hours_on_a_window_that_rolls_say_so_rather_than_too_little_work() {
        let example: serde_json::Value =
            serde_json::from_str(include_str!("../../contract/status.notification.json")).unwrap();
        let now = example["params"]["at"].as_i64().unwrap();
        // Claude Max's 5 hours made a window that rolls, as OpenCode Go's
        // is; its weekly limit is forecast.
        let mut account = example["params"]["accounts"][0].clone();
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
    fn one_hour_of_work_doesnt_fit_in_the_singular() {
        let example: serde_json::Value =
            serde_json::from_str(include_str!("../../contract/status.notification.json")).unwrap();
        let now = example["params"]["at"].as_i64().unwrap();
        // Claude Max's 5 hours with half an hour of work left.
        let mut account = example["params"]["accounts"][0].clone();
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
    }
}
