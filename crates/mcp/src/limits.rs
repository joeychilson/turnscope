//! `check_limits`: how much of each limit is left, and whether it lasts.
//!
//! **What is left.** Limits read as what is left, as the person thinks of
//! them: a limit 82% used has 18% left. Each is given with when it resets
//! and where the recent pace leads: when it runs out, if before it resets,
//! or how much will be left at the reset. A pace is how fast the limit rose
//! over its last hour of readings, so it follows what the person is doing
//! now, and with fewer than fifteen minutes of readings it isn't known yet.
//! A window longer than a day is paced over its last day, or, while its
//! readings cover too little of the day, by its average since it began.
//! Beside it, its reserve: how far it is from spending it evenly until it
//! resets, which holds from its first reading; and the fastest it can rise
//! and still last until then.
//!
//! **Unknown is said.** Limits are read every few minutes, so an answer is a
//! guide for pacing, not the provider's enforcement. Where a reading can't
//! say how a limit stands now, the answer says so rather than leave it out,
//! so an agent never reads silence as room to go on. A reading is stale when
//! the account is signed in nowhere, its latest read failed, the limit's
//! window reset since, the latest read left the limit out, or it was read
//! more than fifteen minutes ago.
//!
//! **Below.** Given a percent, each limit is under it, not under it, or
//! unknown, with why. A stale reading is unknown, except one under it in a
//! window whose reset is known and still ahead, since use within a window
//! only rises. A current reading not under it is unknown when its pace says
//! it has gone under since it was read. Together the limits are under it if
//! one is, unknown if one is or none is watched, and not under it only when
//! each is known not to be. Only limits on all of an account's usage are
//! watched unless a limit is named: a limit on one model says nothing of
//! the rest. Only your account's are watched, as a guard watches them,
//! unless accounts are named or which is yours isn't known: another
//! account's limit says nothing of whether you can go on.
//!
//! **Which accounts.** Yours first ([`crate::accounts::yours`]), then the
//! others in use; with `all`, every account; given `account`, the accounts
//! it names. An account the person hid is left out unless named by its id.
//!
//! **API keys.** An API key's use is an account of its provider's, whose
//! limits are those its keys carry of their own, as OpenRouter's can.

use jiff::tz::TimeZone;
use serde::Deserialize;
use serde_json::{Value, json};
use turnscope_engine::{
    AccountLimits, Instant, LimitProblem, LimitState, SessionRow, Standing, Subscription,
};

use crate::accounts;
use crate::prose;
use crate::time;
use crate::tools::{Answer, Failure, Reply, Server, account_schema, object, shape};

/// How long a reading stands as current: three times the five minutes the
/// app, or a server nothing keeps current, waits between reads, so one slow
/// or skipped read doesn't make a limit unknown.
const CURRENT_FOR: i64 = 15 * 60 * 1000;

/// How a limit or an account stands, as the figures say it
/// ([`standing`]), calmest first.
const STANDINGS: [&str; 3] = ["lasts", "running_out", "used_up"];

/// How a limit or an account stands, by the engine's rule
/// ([`LimitState::standing`]), as the figures say it.
fn standing(standing: Standing) -> &'static str {
    match standing {
        Standing::Lasts => STANDINGS[0],
        Standing::RunningOut => STANDINGS[1],
        Standing::UsedUp => STANDINGS[2],
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckLimits {
    account: Option<String>,
    limit: Option<String>,
    below: Option<f64>,
    all: Option<bool>,
}

pub(crate) fn schema() -> Value {
    object(
        json!({
            "account": account_schema("Default: the one you're signed into, then the others in use."),
            "limit": {
                "type": "string",
                "description": "Which limit, as answers name them: \"5 hours\", \"week\", a model's week such as \"opus week\", or \"month\". Default: every limit.",
            },
            "below": {
                "type": "number",
                "exclusiveMinimum": 0,
                "maximum": 100,
                "description": "A percent left, such as 50. The answer says whether each limit is under it, with why when that isn't known, and whether your account is: under it if a limit on all its usage is, or the limit named.",
            },
            "all": {"type": "boolean", "description": "Include accounts no agent is using now."},
        }),
        &[],
    )
}

pub(crate) fn output_schema() -> Value {
    let limit = shape(
        json!({
            "limit": {"type": "string", "description": "Its name, as the limit argument takes it."},
            "key": {"type": "string"},
            "model": {"type": ["string", "null"]},
            "left_percent": {"type": ["number", "null"], "description": "Null when its window reset since it was read."},
            "resets_at": {"type": ["string", "null"]},
            "runs_out_at": {"type": ["string", "null"], "description": "When it runs out at the recent pace, if before it resets."},
            "left_at_reset": {"type": ["number", "null"], "description": "The percent left at the reset at the recent pace, if it lasts."},
            "rising_per_hour": {"type": ["number", "null"]},
            "lasting_per_hour": {"type": ["number", "null"], "description": "The fastest it can rise, in points an hour, and still last until it resets: what is left over the time to the reset."},
            "reserve": {"type": ["number", "null"], "description": "Points of it to spare against spending it evenly through its window until it resets, as of its reading; below zero, how far ahead of that it is used."},
            "read_at": {"type": "string"},
            "stale": {"type": "boolean"},
            "why_stale": {"type": "string"},
            "under": {"type": ["boolean", "null"]},
            "standing": {"type": "string", "enum": STANDINGS, "description": "used_up; running_out, when at the recent pace it runs out a while before it resets; or lasts."},
        }),
        &[
            "limit",
            "key",
            "left_percent",
            "resets_at",
            "read_at",
            "stale",
            "standing",
        ],
    );
    let account = shape(
        json!({
            "id": {"type": "string"},
            "name": {"type": "string"},
            "subscription": {"type": "string"},
            "plan": {"type": ["string", "null"]},
            "label": {"type": ["string", "null"]},
            "agents": {"type": "array", "items": {"type": "string"}},
            "yours": {"type": "boolean"},
            "signed_in": {"type": "boolean"},
            "in_use": {"type": "boolean"},
            "problem": {"type": ["string", "null"]},
            "standing": {"type": "string", "enum": STANDINGS, "description": "As its most urgent limit stands."},
            "tightest": {"type": ["string", "null"], "description": "The limit that matters most now: one used up, else the one that runs out soonest, else the one with least left."},
            "limits": {"type": "array", "items": limit},
            "provider": {"type": "string", "description": "An API key's provider, as usage names it."},
        }),
        &[
            "id",
            "name",
            "subscription",
            "agents",
            "yours",
            "standing",
            "limits",
        ],
    );
    let you = shape(
        json!({
            "agent": {"type": ["string", "null"]},
            "folder": {"type": ["string", "null"]},
            "account": {"type": ["string", "null"]},
            "account_unknown": {"type": "string"},
            "session": {"type": ["string", "null"]},
        }),
        &["agent", "folder", "account", "session"],
    );
    shape(
        json!({
            "you": you,
            "accounts": {"type": "array", "items": account},
            "below": shape(
                json!({
                    "percent": {"type": "number"},
                    "status": {"type": "string", "enum": ["under", "not_under", "unknown"]},
                    "why": {"type": "string"},
                }),
                &["percent", "status"],
            ),
            "read_failed": {"type": "string"},
        }),
        &["you", "accounts"],
    )
}

pub(crate) fn check(server: &Server, arguments: CheckLimits) -> Answer {
    if let Some(below) = arguments.below
        && !(below > 0.0 && below <= 100.0)
    {
        return Err(Failure("below takes more than 0 and at most 100.".into()));
    }
    let (accounts, read_failed) = server.accounts()?;
    let now = Instant::now();
    let all = arguments.all.unwrap_or(false);
    let (session, yours) = accounts::asking(server, &accounts)?;
    let is_yours =
        |account: &AccountLimits| yours.as_ref().is_ok_and(|yours| yours.id == account.id);
    let chosen: Vec<&AccountLimits> = match arguments.account.as_deref() {
        Some(asked) => {
            let named = accounts::named(asked, &accounts);
            if named.is_empty() {
                return Err(accounts::no_account(asked, &accounts));
            }
            named
        }
        None => {
            let mut chosen: Vec<&AccountLimits> = accounts::shown(&accounts)
                .filter(|account| is_yours(account))
                .chain(
                    accounts::shown(&accounts)
                        .filter(|account| !is_yours(account) && (all || account.in_use)),
                )
                .collect();
            // With none yours and none in use, those signed in, so the
            // answer isn't empty for want of use.
            if chosen.is_empty() {
                chosen = accounts::shown(&accounts)
                    .filter(|account| account.signed_in)
                    .collect();
            }
            chosen
        }
    };
    // Each account with the limits asked for.
    let chosen: Vec<(&AccountLimits, Vec<&LimitState>)> = chosen
        .into_iter()
        .map(|account| {
            let limits: Vec<&LimitState> = account
                .limits
                .iter()
                .filter(|limit| {
                    arguments
                        .limit
                        .as_deref()
                        .is_none_or(|asked| matches(limit, asked))
                })
                .collect();
            (account, limits)
        })
        .filter(|(_, limits)| arguments.limit.is_none() || !limits.is_empty())
        .collect();
    if let Some(asked) = arguments.limit.as_deref()
        && chosen.is_empty()
    {
        return Err(no_limit(asked, &accounts));
    }

    let mut answer = json!({
        "you": you(server, &yours, session.as_ref()),
        "accounts": chosen.iter().map(|(account, limits)| {
            described(server, account, limits, is_yours(account), arguments.below, now)
        }).collect::<Vec<_>>(),
    });
    let mut said = Vec::new();
    match &yours {
        Ok(account) => said.push(format!(
            "You're using {}{}.",
            accounts::name(account),
            server
                .caller
                .agent
                .map_or_else(String::new, |agent| format!(", in {agent}"))
        )),
        Err(why) => said.push(why.clone()),
    }
    if chosen.is_empty() {
        said.push(
            "No agent on this Mac is signed into a subscription Turnscope reads the limits of, \
             and no API key has a limit of its own."
                .to_owned(),
        );
    }
    // Yours, and any named, in full; others a line each, unless all were
    // asked for, when each is a line.
    let mut others = Vec::new();
    for (account, limits) in &chosen {
        if arguments.account.is_none() && (all || !is_yours(account)) {
            others.push(brief(server, account, limits, is_yours(account), now));
        } else if limits.is_empty() {
            said.push(format!(
                "{}: {}",
                accounts::name(account),
                none_read(account)
            ));
        } else {
            let mut lines = Vec::new();
            if !is_yours(account) {
                lines.push(format!("{}:", accounts::name(account)));
            }
            for limit in limits {
                lines.push(sentence(server, account, limit, arguments.below, now));
            }
            said.push(lines.join("\n"));
        }
    }
    if !others.is_empty() {
        if all {
            said.push(others.join("\n"));
        } else {
            said.push(format!("Also in use: {}", others.join(" ")));
        }
    }
    if let Some(below) = arguments.below {
        // Yours alone decide, where it is known and no account was named:
        // another account's limit says nothing of whether you can go on.
        let judged = |account: &AccountLimits| {
            arguments.account.is_some() || yours.is_err() || is_yours(account)
        };
        let statuses: Vec<Status> = chosen
            .iter()
            .filter(|(account, _)| judged(account))
            .flat_map(|(account, limits)| limits.iter().map(move |limit| (*account, *limit)))
            .filter(|(_, limit)| arguments.limit.is_some() || limit.scope.is_none())
            .map(|(account, limit)| status(account, limit, below, now))
            .collect();
        let overall = if statuses.contains(&Status::Under) {
            Status::Under
        } else if statuses.is_empty() || statuses.contains(&Status::Unknown) {
            Status::Unknown
        } else {
            Status::NotUnder
        };
        answer["below"] = json!({"percent": below, "status": overall.key()});
        let percent = prose::percent(below);
        let summary = match overall {
            Status::Under => format!("Under {percent} left."),
            Status::NotUnder => format!("Not under {percent} left."),
            Status::Unknown if statuses.is_empty() => {
                let why =
                    "No limit is watched: no account shown has a limit on all its usage read.";
                answer["below"]["why"] = json!(why);
                format!("Whether it's under {percent} isn't known. {why}")
            }
            Status::Unknown => {
                let why = "At least one limit's reading can't say how it stands now.";
                answer["below"]["why"] = json!(why);
                format!(
                    "Whether it's under {percent} isn't known: {}",
                    prose::capitalized(why)
                )
            }
        };
        said.push(summary);
    }
    if let Some(failed) = read_failed {
        said.push(failed.clone());
        answer["read_failed"] = json!(failed);
    }
    Ok(Reply::Answer {
        said: said.join("\n\n"),
        data: answer,
    })
}

/// Who is asking, as answers give it: the agent, its folder, its account
/// or why that isn't known, and this session.
fn you(
    server: &Server,
    account: &Result<&AccountLimits, String>,
    session: Option<&SessionRow>,
) -> Value {
    let mut you = json!({
        "agent": server.caller.agent.map(|agent| agent.key()),
        "folder": server.caller.folder,
        "account": account.as_ref().ok().map(|account| account.id.clone()),
        "session": session.map(|session| session.key.to_string()),
    });
    if let Err(why) = account {
        you["account_unknown"] = json!(why);
    }
    you
}

/// Why none of `accounts` has a limit `asked` names, with the limits they
/// have.
pub(crate) fn no_limit<'a>(
    asked: &str,
    accounts: impl IntoIterator<Item = &'a AccountLimits>,
) -> Failure {
    let mut names: Vec<String> = accounts
        .into_iter()
        .flat_map(|account| account.limits.iter().map(limit_name))
        .collect();
    names.sort();
    names.dedup();
    Failure(format!(
        "No account has a limit {asked:?}; the limits are {}.",
        if names.is_empty() {
            "none yet".to_owned()
        } else {
            names.join(", ")
        }
    ))
}

/// A limit's name, as the `limit` argument takes it: `5 hours`, `week`,
/// `month`, or a model's, as `opus week`.
pub(crate) fn limit_name(limit: &LimitState) -> String {
    named_as(&limit.name, limit.scope.as_deref())
}

/// The name, as [`limit_name`] gives it, of a limit its provider calls
/// `name`, on the model `scope` alone where it has one.
pub(crate) fn named_as(name: &str, scope: Option<&str>) -> String {
    let base = match name {
        "Weekly" => "week".to_owned(),
        "Monthly" => "month".to_owned(),
        "Daily" => "day".to_owned(),
        "Hourly" => "hour".to_owned(),
        other => {
            let lower = other.to_lowercase();
            match lower.strip_suffix(" weekly") {
                Some(start) => format!("{start} week"),
                None => lower,
            }
        }
    };
    match scope {
        Some(model) => format!("{} {base}", model.to_lowercase()),
        None => base,
    }
}

/// Whether `asked` names `limit`: its name, its key, or its name said
/// another common way, as `weekly`, `5h` or `5-hour`, ignoring case.
pub(crate) fn matches(limit: &LimitState, asked: &str) -> bool {
    let normal = |text: &str| {
        let text = text.trim().to_lowercase().replace(['-', '_'], " ");
        let text = text
            .replace("weekly", "week")
            .replace("monthly", "month")
            .replace("daily", "day")
            .replace("5h", "5 hours")
            .replace("five hours", "5 hours")
            .replace("5 hour ", "5 hours ");
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        match text.as_str() {
            "5 hour" => "5 hours".to_owned(),
            "7 days" | "seven days" => "week".to_owned(),
            _ => text,
        }
    };
    let asked = normal(asked);
    asked == normal(&limit_name(limit)) || asked == normal(&limit.key)
}

/// A limit's name as a sentence says it: `5-hour limit`, `weekly limit`,
/// `Opus weekly limit`, `monthly key limit`.
pub(crate) fn spoken(limit: &LimitState) -> String {
    spoken_as(&limit.name, limit.scope.as_deref())
}

/// The name, as [`spoken`] says it, of a limit its provider calls `name`,
/// on the model `scope` alone where it has one.
pub(crate) fn spoken_as(name: &str, scope: Option<&str>) -> String {
    let base = match name {
        "Weekly" => "weekly".to_owned(),
        "Monthly" => "monthly".to_owned(),
        "Daily" => "daily".to_owned(),
        "Hourly" => "hourly".to_owned(),
        other => {
            let lower = other.to_lowercase();
            match lower.split_once(' ') {
                Some((count, unit)) if count.parse::<u32>().is_ok() => {
                    format!("{count}-{}", unit.trim_end_matches('s'))
                }
                _ => lower,
            }
        }
    };
    // A key's limit is named as one already.
    let base = if base.ends_with("limit") {
        base
    } else {
        format!("{base} limit")
    };
    match scope {
        Some(model) => format!("{} {base}", prose::capitalized(model)),
        None => base,
    }
}

/// When the account least recently checked of `accounts` signed in somewhere
/// was last read or tried; `None` when there is none, or one never was.
///
/// A try counts though it failed, so an account whose sign-in is refused
/// doesn't call for every provider to be read again at every call. An
/// account signed in nowhere isn't read, so however long ago it was checked
/// calls for no read.
pub(crate) fn least_recently_checked(accounts: &[AccountLimits]) -> Option<Instant> {
    accounts
        .iter()
        .filter(|account| account.signed_in)
        .map(|account| account.checked_at)
        .min()
        .flatten()
}

/// An account and the `limits` of it asked for, as answers give them at
/// `now`, `yours` or not, each limit under `below` or not where given.
fn described(
    server: &Server,
    account: &AccountLimits,
    limits: &[&LimitState],
    yours: bool,
    below: Option<f64>,
    now: Instant,
) -> Value {
    let mut described = json!({
        "id": account.id,
        "name": accounts::name(account),
        "subscription": account.subscription.key(),
        "plan": account.plan,
        "label": account.label,
        "agents": account.agents.iter().map(|agent| agent.key()).collect::<Vec<_>>(),
        "yours": yours,
        "signed_in": account.signed_in,
        "in_use": account.in_use,
        "problem": account.problem.map(problem),
        "standing": standing(account.standing()),
        "tightest": account.deciding().map(limit_name),
        "limits": limits.iter().map(|limit| limit_figures(&server.zone, account, limit, below, now)).collect::<Vec<_>>(),
    });
    if let Some(provider) = &account.provider {
        described["provider"] = json!(provider);
    }
    described
}

/// A limit's figures, as answers give them at `now`.
fn limit_figures(
    zone: &TimeZone,
    account: &AccountLimits,
    limit: &LimitState,
    below: Option<f64>,
    now: Instant,
) -> Value {
    let outlook = limit.outlook();
    let stale = stale(account, limit, now);
    let mut value = json!({
        "limit": limit_name(limit),
        "key": limit.key,
        "model": limit.scope,
        "left_percent": limit.left().map(|left| (left * 10.0).round() / 10.0),
        "resets_at": limit.resets.map(|at| time::local(at, zone)),
        "runs_out_at": outlook.runs_out.map(|at| time::local(at, zone)),
        "left_at_reset": outlook.left_at_reset.map(|left| (left * 10.0).round() / 10.0),
        "rising_per_hour": limit.pace.map(|pace| (pace * 100.0).round() / 100.0),
        "reserve": limit.reserve().map(|reserve| (reserve * 10.0).round() / 10.0),
        "lasting_per_hour": limit.lasting_pace().map(|pace| (pace * 100.0).round() / 100.0),
        "read_at": time::local(limit.read_at, zone),
        "stale": stale.is_some(),
        "standing": standing(limit.standing()),
    });
    if let Some(why) = stale {
        value["why_stale"] = json!(why);
    }
    if let Some(below) = below {
        value["under"] = match status(account, limit, below, now) {
            Status::Under => json!(true),
            Status::NotUnder => json!(false),
            Status::Unknown => Value::Null,
        };
    }
    value
}

/// One limit in full, as a sentence says it at `now`.
fn sentence(
    server: &Server,
    account: &AccountLimits,
    limit: &LimitState,
    below: Option<f64>,
    now: Instant,
) -> String {
    let name = prose::capitalized(&spoken(limit));
    let resets = limit.resets.map(|at| server.clock(at));
    let Some(left) = limit.left() else {
        return format!(
            "{name}: its window reset{}, and it hasn't been read since, so how much is used \
             isn't known.",
            resets.map_or_else(String::new, |at| format!(" at {at}"))
        );
    };
    let mut said = if left <= 0.0 {
        format!("{name}: used up")
    } else {
        format!("{name}: {} left", prose::left(left))
    };
    if let Some(below) = below {
        said.push_str(match status(account, limit, below, now) {
            Status::Under => ", under ",
            Status::NotUnder => ", not under ",
            Status::Unknown => ", maybe under ",
        });
        said.push_str(&prose::percent(below));
    }
    let outlook = limit.outlook();
    match (outlook.runs_out, limit.resets) {
        (Some(out), resets) if out <= now => {
            said.push_str(". At the pace it was rising when read, it would have run out by now");
            match resets {
                Some(at) => said.push_str(&format!("; it resets at {}.", server.clock(at))),
                None => said.push('.'),
            }
        }
        (Some(out), Some(resets)) => said.push_str(&format!(
            ". At this pace it runs out around {}, {} before it resets at {}.",
            server.clock(out),
            prose::span(resets.millis() - out.millis()),
            server.clock(resets)
        )),
        (Some(out), None) => said.push_str(&format!(
            ". At this pace it runs out around {}.",
            server.clock(out)
        )),
        (None, _) => {
            if let Some(at) = &resets {
                said.push_str(&format!(", resets {at}"));
            }
            match outlook.left_at_reset {
                _ if left <= 0.0 => said.push('.'),
                Some(spare) => said.push_str(&format!(
                    ". At this pace it lasts, with about {} to spare.",
                    prose::left(spare)
                )),
                None if limit.pace.is_none() => said.push_str(". Its pace isn't known yet."),
                None => said.push('.'),
            }
        }
    }
    if let Some(why) = stale(account, limit, now) {
        said.push_str(&format!(
            " As read at {}: {why}",
            server.clock(limit.read_at)
        ));
    }
    said
}

/// An account in brief, as a line says it: its name, agents, and its
/// tightest limit's share left, or, for `yours`, as marked.
fn brief(
    server: &Server,
    account: &AccountLimits,
    limits: &[&LimitState],
    yours: bool,
    now: Instant,
) -> String {
    let agents: Vec<&str> = account.agents.iter().map(|agent| agent.name()).collect();
    let mut said = accounts::name(account);
    if yours {
        said.push_str(" (yours)");
    } else if !agents.is_empty() {
        said.push_str(&format!(" ({})", agents.join(", ")));
    }
    let parts: Vec<String> = limits
        .iter()
        .filter(|limit| limit.scope.is_none())
        .map(|limit| {
            let name = spoken(limit);
            match (limit.left(), limit.resets) {
                (None, _) => format!("{name} reset, not read since"),
                (Some(left), resets) if left >= 100.0 && limit.pace.is_none_or(|p| p <= 0.0) => {
                    match resets {
                        Some(at) => format!("{name} not started, resets {}", server.clock(at)),
                        None => format!("{name} not started"),
                    }
                }
                (Some(left), _) => {
                    let mut part = format!("{name} {} left", prose::left(left));
                    if let Some(out) = limit.outlook().runs_out.filter(|out| *out > now) {
                        part.push_str(&format!(", runs out around {}", server.clock(out)));
                    }
                    part
                }
            }
        })
        .collect();
    if parts.is_empty() {
        said.push_str(&format!(": {}", none_read(account)));
    } else {
        said.push_str(&format!(": {}.", parts.join("; ")));
        if let Some(problem) = account.problem {
            said.push_str(&format!(" As last read, since {}.", failed(problem)));
        }
    }
    said
}

/// What a sentence says of an account with no limit read: why, when its
/// latest read failed; and of an API key's, that its keys carry none, as
/// most don't: its use is paid for as it goes.
fn none_read(account: &AccountLimits) -> String {
    match account.problem {
        Some(problem) => format!("no limit read, since {}.", failed(problem)),
        None if account.subscription == Subscription::ApiKey => {
            "no limit, as its keys carry none of their own.".to_owned()
        }
        None => "no limit read yet.".to_owned(),
    }
}

/// Why the latest read failed, as a clause of a sentence.
fn failed(problem: LimitProblem) -> &'static str {
    match problem {
        LimitProblem::SignIn => "its sign-in was refused, or has expired until its agent renews it",
        LimitProblem::Unavailable => "its provider couldn't be reached or was busy",
        LimitProblem::Unrecognized => {
            "its provider's reply wasn't what this version of Turnscope understands"
        }
        LimitProblem::Unsent => "/usr/bin/curl couldn't be run to read it",
    }
}

/// Why the latest read failed, for the agent.
fn problem(problem: LimitProblem) -> &'static str {
    match problem {
        LimitProblem::SignIn => {
            "Its sign-ins were refused, or have expired until their agent renews them. The limits shown are the last read."
        }
        LimitProblem::Unavailable => {
            "The provider could not be reached or was busy. The limits shown are the last read."
        }
        LimitProblem::Unrecognized => {
            "The provider's reply was not what this version of Turnscope expects, so the usage could not be read. The limits shown are the last read."
        }
        LimitProblem::Unsent => {
            "The request could not be made on this Mac, since /usr/bin/curl could not be run. The limits shown are the last read."
        }
    }
}

/// Why the latest reading of `account`'s `limit` may not be how it stands at
/// `now`; `None` while it is current.
fn stale(account: &AccountLimits, limit: &LimitState, now: Instant) -> Option<String> {
    if !account.signed_in {
        Some(
            "No agent on this Mac is signed into its account now, so it isn't read; this is its \
             last reading."
                .to_owned(),
        )
    } else if let Some(problem) = account.problem {
        Some(format!(
            "The latest read of its account failed: {}.",
            failed(problem)
        ))
    } else if limit.refilled {
        Some("Its window has reset since it was read.".to_owned())
    } else if account.read_at.is_some_and(|at| limit.read_at < at) {
        Some("The latest read of its account left it out.".to_owned())
    } else if now.millis().saturating_sub(limit.read_at.millis()) > CURRENT_FOR {
        Some("It was read more than 15 minutes ago.".to_owned())
    } else {
        None
    }
}

/// Where a limit stands against a percent left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    /// Less than the percent is left.
    Under,
    /// Its reading is current, at or above the percent, and not falling
    /// fast enough to have gone under it since.
    NotUnder,
    /// No reading says how it stands now.
    Unknown,
}

impl Status {
    fn key(self) -> &'static str {
        match self {
            Status::Under => "under",
            Status::NotUnder => "not_under",
            Status::Unknown => "unknown",
        }
    }
}

/// Where `account`'s `limit` stands against `below` percent left at `now`.
///
/// Within a window use only rises, so a reading under it stays under,
/// however old, until its window resets; one whose reset isn't known can't
/// say so. A current reading not under it is not under only while its pace
/// doesn't say it has gone under since: one that does is unknown, not
/// under, since a pace is a projection and the rise may have stopped.
pub(crate) fn status(
    account: &AccountLimits,
    limit: &LimitState,
    below: f64,
    now: Instant,
) -> Status {
    let used_at = 100.0 - below;
    let under = limit.used.is_some_and(|used| used > used_at);
    let stale = stale(account, limit, now).is_some();
    // When it goes under, at its recent pace, if before its window resets.
    let goes_under = limit
        .reaches(used_at)
        .filter(|at| limit.resets.is_none_or(|resets| *at < resets));
    match stale {
        false if under => Status::Under,
        false if limit.used.is_none() => Status::Unknown,
        false if goes_under.is_some_and(|at| at <= now) => Status::Unknown,
        false => Status::NotUnder,
        true if under && limit.resets.is_some_and(|resets| resets > now) => Status::Under,
        true => Status::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use turnscope_engine::{AccountLimits, Instant, LimitProblem, LimitState, Subscription};

    use super::{Status, limit_name, matches, none_read, spoken, stale, status};

    /// A limit named `name`, `used` percent as read at `read`, rising at
    /// `pace`, resetting at `resets`.
    fn reading(name: &str, used: f64, read: &str, pace: Option<f64>, resets: &str) -> LimitState {
        LimitState {
            key: name.to_lowercase().replace(' ', "_"),
            name: name.into(),
            scope: None,
            used: Some(used),
            starts: None,
            resets: Instant::parse(resets),
            read_at: Instant::parse(read).unwrap(),
            pace,
            runs_out: None,
            refilled: false,
        }
    }

    /// An account signed in and in use, last read at `read`.
    fn account(read: &str, limits: Vec<LimitState>) -> AccountLimits {
        AccountLimits {
            id: "claude:acc-1:org-9".into(),
            subscription: Subscription::Claude,
            label: None,
            plan: Some("max".into()),
            agents: Vec::new(),
            signed_in: true,
            limits,
            read_at: Instant::parse(read),
            checked_at: Instant::parse(read),
            problem: None,
            in_use: true,
            hidden: false,
            provider: None,
            folders: Vec::new(),
        }
    }

    #[test]
    fn limits_are_named_as_the_argument_takes_them() {
        let five = reading("5 hours", 10.0, "2026-09-29T12:00:00Z", None, "");
        // As Claude's answer keys a week on one model.
        let mut opus = reading("Weekly", 10.0, "2026-09-29T12:00:00Z", None, "");
        opus.key = "seven_day_opus".into();
        opus.scope = Some("Opus".into());
        let apps = reading("Other apps weekly", 1.0, "2026-09-29T12:00:00Z", None, "");
        assert_eq!(limit_name(&five), "5 hours");
        assert_eq!(limit_name(&opus), "opus week");
        assert_eq!(limit_name(&apps), "other apps week");
        assert_eq!(spoken(&five), "5-hour limit");
        assert_eq!(spoken(&opus), "Opus weekly limit");
        for asked in ["5 hours", "5h", "5-hour", "five hours", "5_hours"] {
            assert!(matches(&five, asked), "{asked}");
        }
        assert!(matches(&opus, "Opus weekly"));
        assert!(!matches(&opus, "week"));
    }

    #[test]
    fn a_pace_says_when_a_limit_runs_out_or_what_is_left_at_its_reset() {
        let read = "2026-09-29T12:00:00Z";
        // 82% used, rising 12 points an hour: the 18 left last 1.5 hours,
        // to 13:30, before the 15:00 reset.
        let five = reading("5 hours", 82.0, read, Some(12.0), "2026-09-29T15:00:00Z");
        assert_eq!(
            five.outlook().runs_out,
            Instant::parse("2026-09-29T13:30:00Z")
        );
        // 38% used, rising half a point an hour, 40 hours to its reset: 20
        // more points, so 42% left at it.
        let week = reading("Weekly", 38.0, read, Some(0.5), "2026-10-01T04:00:00Z");
        assert_eq!(week.outlook().left_at_reset, Some(42.0));
        assert_eq!(week.outlook().runs_out, None);
        // Not rising: what is left now is left at the reset.
        let flat = reading("Weekly", 38.0, read, Some(0.0), "2026-10-01T04:00:00Z");
        assert_eq!(flat.outlook().left_at_reset, Some(62.0));
        // No pace yet: neither.
        let new = reading("Weekly", 38.0, read, None, "2026-10-01T04:00:00Z");
        assert_eq!(new.outlook().left_at_reset, None);
        // The tightest is the one that runs out first.
        let both = account(read, vec![week, five]);
        assert_eq!(both.deciding().unwrap().name, "5 hours");
    }

    #[test]
    fn a_limit_is_under_a_percent_left_not_under_it_or_unknown() {
        let now = Instant::parse("2026-09-29T12:00:00Z").unwrap();
        let resets = "2026-09-29T15:00:00Z";
        let read = "2026-09-29T11:58:00Z";
        let at = |limit: LimitState| {
            let account = account(read, vec![limit.clone()]);
            status(&account, &limit, 50.0, now)
        };
        // 58% used is 42% left: under 50.
        assert_eq!(
            at(reading("5 hours", 58.0, read, None, resets)),
            Status::Under
        );
        // 42% used is 58% left, rising 8 points an hour: 50% left at 50%
        // used, an hour after 11:58, still ahead.
        assert_eq!(
            at(reading("5 hours", 42.0, read, Some(8.0), resets)),
            Status::NotUnder
        );
        // 45% used at 11:50, rising 60 points an hour: 50% used 5 minutes
        // later, at 11:55, before now: unknown.
        let passed = reading("5 hours", 45.0, "2026-09-29T11:50:00Z", Some(60.0), resets);
        let account_passed = account("2026-09-29T11:50:00Z", vec![passed.clone()]);
        assert_eq!(status(&account_passed, &passed, 50.0, now), Status::Unknown);
        // Exactly 50% left is not under 50.
        assert_eq!(
            at(reading("5 hours", 50.0, read, None, resets)),
            Status::NotUnder
        );
        // A stale reading under it, in a window still ahead, stays under;
        // one not under it is unknown.
        let old = "2026-09-29T10:00:00Z";
        let mut refused = account(old, Vec::new());
        refused.problem = Some(LimitProblem::SignIn);
        let low = reading("5 hours", 70.0, old, None, resets);
        let high = reading("5 hours", 30.0, old, None, resets);
        assert_eq!(status(&refused, &low, 50.0, now), Status::Under);
        assert_eq!(status(&refused, &high, 50.0, now), Status::Unknown);
        // Under with no reset known may have reset since: unknown.
        let unbounded = reading("5 hours", 70.0, old, None, "");
        assert_eq!(status(&refused, &unbounded, 50.0, now), Status::Unknown);
    }

    #[test]
    fn a_read_that_failed_says_why() {
        let now = Instant::parse("2026-09-29T12:00:00Z").unwrap();
        let read = "2026-09-29T11:58:00Z";
        let mut refused = account(read, Vec::new());
        assert_eq!(none_read(&refused), "no limit read yet.");
        let key = AccountLimits {
            subscription: Subscription::ApiKey,
            provider: Some("openrouter".into()),
            ..refused.clone()
        };
        assert_eq!(
            none_read(&key),
            "no limit, as its keys carry none of their own."
        );
        refused.problem = Some(LimitProblem::SignIn);
        assert_eq!(
            none_read(&refused),
            "no limit read, since its sign-in was refused, or has expired until its agent \
             renews it."
        );
        let five = reading("5 hours", 10.0, read, None, "2026-09-29T15:00:00Z");
        assert_eq!(
            stale(&refused, &five, now).as_deref(),
            Some(
                "The latest read of its account failed: its sign-in was refused, or has \
                 expired until its agent renews it."
            )
        );
    }
}
