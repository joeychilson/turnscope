//! The feed: everything the menu bar app shows, worked out here and written
//! by `turnscope watch` as one JSON line ([`crate::watch`]). The app only
//! lays it out and says it in words; every rule that decides what it says,
//! how a limit stands, which limit matters, the order accounts are listed
//! in, what used a limit and the advice drawn from that, is here or in the
//! engine, so it is decided once and tested once.
//!
//! **The contract.** `contract/feed.json` at the repository's root is a feed
//! as this module writes it, checked by this module's tests to be exactly
//! what it writes for the accounts it describes, and read by the app's
//! tests. A change to the feed's shape changes that file and [`VERSION`],
//! in the same commit as the app's reading of it.
//!
//! **Reading.** Until the engine has looked through every agent's history
//! once, as on the first launch, which took 10 s on 2026-09-30, the feed
//! says it is still reading: what it has found may be all there is, or not
//! yet, and the app says so rather than that nothing was found.
//!
//! **Accounts.** Every account the engine knows, hidden ones too, marked, so
//! Settings can list them. Those in use come first, the most urgent first:
//! used up, then running out, then the least left. The rest follow: those
//! with a limit read, the most left first, then those whose sign-in or key
//! was refused, then those with no limit to show.
//!
//! **What used it.** For an account in use, the sessions that took most of
//! the window of the limit that matters ([`AccountLimits::deciding`]), three
//! at the most, as the engine shares the window out
//! ([`turnscope_engine::LimitWindow`]); only while the panel is open, which
//! alone shows them ([`crate::watch`]).
//!
//! **Advice.** One at the most, and only what the numbers make true, of the
//! session that took most of the window, a tenth of it or more: that it read
//! a large context again with every reply, one that grew to [`LARGE`] tokens
//! or more with over half its tokens cache reads; or else that its
//! subagents took a quarter or more of its share, on a model that isn't a
//! small one.

use std::cmp::Reverse;
use std::collections::HashMap;

use serde::Serialize;
use turnscope_engine::{
    AccountLimits, Agent, Engine, Instant, LimitProblem, LimitState, LimitTrack, ModelKey,
    SessionKey, SessionRow, Standing, Subscription,
};

use crate::connect::Link;

/// The feed's version, which `contract/feed.json` carries: raised with
/// every change to its shape.
pub(crate) const VERSION: u32 = 10;

/// How many sessions that used a window are named.
const NAMED: usize = 3;

/// The smallest share of a window, in points, a session's use of which is
/// worth advice.
const SIZEABLE: f64 = 10.0;

/// How large a context, in tokens, is large enough that reading it again
/// with every reply is worth advice.
const LARGE: u64 = 100_000;

/// Everything the app shows, as of `at`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Feed {
    pub(crate) version: u32,
    pub(crate) at: String,
    /// Whether the engine is still reading every agent's history for the
    /// first time ([`turnscope_engine::Health::looked`]).
    pub(crate) reading: bool,
    pub(crate) accounts: Vec<Account>,
    /// The agents installed here, and whether each has Turnscope's server.
    pub(crate) agents: Vec<Link>,
    /// Every agent Turnscope reads, installed or not.
    pub(crate) reads: Vec<Named>,
}

/// An agent, wherever the feed names one: its id and what it is called.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub(crate) struct Named {
    /// As the engine names it: "claude-code".
    pub(crate) id: &'static str,
    /// "Claude Code".
    pub(crate) name: &'static str,
}

impl Named {
    fn of(agent: Agent) -> Named {
        Named {
            id: agent.key(),
            name: agent.name(),
        }
    }
}

/// An account, as the app shows it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Account {
    pub(crate) id: String,
    /// What it is called before anything tells it apart: "Claude Max",
    /// "OpenRouter API key".
    pub(crate) title: String,
    /// What tells it apart from others of its kind, as its sign-in's email.
    pub(crate) label: Option<String>,
    /// The logo it is shown with, a provider's id: "anthropic", "openai".
    pub(crate) logo: String,
    /// The agents signed into it, or that keep its key.
    pub(crate) agents: Vec<Named>,
    pub(crate) in_use: bool,
    pub(crate) hidden: bool,
    /// Whether it is an API-key account, its provider's keys, rather than
    /// a subscription.
    pub(crate) api_key: bool,
    /// Why its limits can't be read now, if they can't.
    pub(crate) problem: Option<&'static str>,
    pub(crate) standing: &'static str,
    /// The key of the limit that matters most now.
    pub(crate) deciding: Option<String>,
    pub(crate) limits: Vec<Limit>,
    /// The sessions that took most of the deciding limit's window, most
    /// first; only for an account in use, while the panel is open.
    pub(crate) used_most: Vec<Used>,
    pub(crate) advice: Option<Advice>,
}

/// A limit, as last read, and where its pace leads.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Limit {
    pub(crate) key: String,
    /// Its provider's name for it, as "5 hours" or "Weekly".
    pub(crate) name: String,
    /// The one model it applies to, when not all.
    pub(crate) scope: Option<String>,
    /// How long its window is, in hours, when known.
    pub(crate) hours: Option<i64>,
    /// Percent left; `None` when its window reset since it was read.
    pub(crate) left: Option<f64>,
    pub(crate) resets_at: Option<String>,
    pub(crate) runs_out_at: Option<String>,
    pub(crate) left_at_reset: Option<f64>,
    /// Points it is in reserve of an even pace through its window, or ahead
    /// of it when below zero ([`LimitState::reserve`]).
    pub(crate) reserve: Option<f64>,
    /// While it runs out, the most it can rise, in points an hour, and
    /// still last until it resets ([`LimitState::lasting_pace`]).
    pub(crate) budget: Option<f64>,
    pub(crate) standing: &'static str,
}

/// A session that used a limit's window.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Used {
    pub(crate) session: String,
    pub(crate) title: Option<String>,
    pub(crate) project: Option<String>,
    pub(crate) agent: Named,
    /// What it took, in points of the limit's percent.
    pub(crate) share: f64,
    /// Whether it did something within the engine's while of now.
    pub(crate) active: bool,
}

/// One piece of advice, and the numbers it rests on.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Advice {
    /// The session read its context, which grew to `context` tokens, again
    /// with every reply.
    Reread { context: u64 },
    /// The session's subagents took `share` points of the window, most of
    /// it on `model`.
    Subagents { share: f64, model: String },
}

/// What the engine says beyond an account's limits, for one in use.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Told {
    pub(crate) used_most: Vec<Used>,
    pub(crate) advice: Option<Advice>,
}

/// Read the feed from `engine`, with the agents' links as they stand, and,
/// when `used` asks, what used each limit.
///
/// # Errors
///
/// Returns the engine's error when the ledger or the cache can't be read.
pub(crate) fn read(
    engine: &Engine,
    agents: Vec<Link>,
    used: bool,
) -> turnscope_engine::Result<Feed> {
    let accounts = engine.limits()?;
    let reading = engine.health()?.looked.is_none();
    let now = Instant::now();
    let mut told = HashMap::new();
    if used {
        for account in accounts
            .iter()
            .filter(|account| account.in_use && !account.hidden)
        {
            if let Some(limit) = account.deciding() {
                told.insert(account.id.clone(), tell(engine, account, limit, now)?);
            }
        }
    }
    Ok(assemble(accounts, told, agents, reading, now))
}

/// The feed of `accounts` as of `now`, with what was `told` of those in use,
/// while history is still `reading` for the first time or not.
pub(crate) fn assemble(
    accounts: Vec<AccountLimits>,
    mut told: HashMap<String, Told>,
    agents: Vec<Link>,
    reading: bool,
    now: Instant,
) -> Feed {
    let accounts = ranked(accounts)
        .into_iter()
        .map(|account| {
            let told = told.remove(&account.id).unwrap_or_default();
            shown(&account, told)
        })
        .collect();
    Feed {
        version: VERSION,
        at: time(now),
        reading,
        accounts,
        agents,
        reads: Agent::ALL.into_iter().map(Named::of).collect(),
    }
}

/// `accounts` in the order the app lists them ([`crate::feed`]).
fn ranked(mut accounts: Vec<AccountLimits>) -> Vec<AccountLimits> {
    accounts.sort_by_cached_key(|account| {
        let least = account
            .deciding()
            .and_then(LimitState::left)
            .map_or(i64::MAX, |left| (left * 10.0).round() as i64);
        let group = if account.in_use {
            0
        } else if account.problem == Some(LimitProblem::SignIn) {
            2
        } else if account.deciding().is_some() {
            1
        } else {
            3
        };
        let urgency = if account.in_use {
            Reverse(account.standing())
        } else {
            Reverse(Standing::Lasts)
        };
        // Those in use with least left first; the rest with most left first.
        let left = if account.in_use { least } else { -least };
        (group, urgency, left, account.id.clone())
    });
    accounts
}

/// `account` as the app shows it, with what was `told` of it.
fn shown(account: &AccountLimits, told: Told) -> Account {
    Account {
        id: account.id.clone(),
        title: account.title(),
        label: account.label.clone(),
        logo: logo(account),
        agents: account.agents.iter().copied().map(Named::of).collect(),
        in_use: account.in_use,
        hidden: account.hidden,
        api_key: account.subscription == Subscription::ApiKey,
        problem: account.problem.map(problem),
        standing: account.standing().key(),
        deciding: account.deciding().map(|limit| limit.key.clone()),
        limits: account.limits.iter().map(limit).collect(),
        used_most: told.used_most,
        advice: told.advice,
    }
}

fn limit(limit: &LimitState) -> Limit {
    let outlook = limit.outlook();
    Limit {
        key: limit.key.clone(),
        name: limit.name.clone(),
        scope: limit.scope.clone(),
        hours: limit
            .starts
            .zip(limit.resets)
            .map(|(starts, resets)| (resets.millis() - starts.millis()) / 3_600_000),
        left: limit.left().map(tenths),
        resets_at: limit.resets.map(time),
        runs_out_at: outlook.runs_out.map(time),
        left_at_reset: outlook.left_at_reset.map(tenths),
        reserve: limit.reserve().map(tenths),
        budget: (limit.standing() == Standing::RunningOut)
            .then(|| limit.lasting_pace())
            .flatten()
            .map(hundredths),
        standing: limit.standing().key(),
    }
}

/// The logo an account is shown with: its provider's id.
fn logo(account: &AccountLimits) -> String {
    match (account.subscription, &account.provider) {
        (Subscription::ApiKey, Some(provider)) => provider.clone(),
        (subscription, _) => subscription
            .providers()
            .first()
            .map_or_else(|| "key".to_owned(), |provider| (*provider).to_owned()),
    }
}

fn problem(problem: LimitProblem) -> &'static str {
    match problem {
        LimitProblem::SignIn => "sign_in",
        LimitProblem::Unavailable => "unavailable",
        LimitProblem::Unrecognized => "unrecognized",
        LimitProblem::Unsent => "unsent",
    }
}

/// An instant as the feed writes it: RFC 3339 in UTC, to the second.
pub(crate) fn time(at: Instant) -> String {
    at.timestamp().strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// A percent to a tenth of a point.
fn tenths(percent: f64) -> f64 {
    (percent * 10.0).round() / 10.0
}

/// A pace to a hundredth of a point an hour, which a day's budget, 24 of
/// them, needs.
fn hundredths(pace: f64) -> f64 {
    (pace * 100.0).round() / 100.0
}

/// What used `account`'s `limit`'s window, and the advice drawn from it.
fn tell(
    engine: &Engine,
    account: &AccountLimits,
    limit: &LimitState,
    now: Instant,
) -> turnscope_engine::Result<Told> {
    let Some(window) = engine.limit_window(&account.id, &limit.key)? else {
        return Ok(Told::default());
    };
    let top: Vec<(SessionKey, f64)> = window.sessions.iter().take(NAMED).cloned().collect();
    let keys: Vec<SessionKey> = top.iter().map(|(key, _)| key.clone()).collect();
    let rows = engine.session_rows(&keys)?;
    let used_most = top
        .iter()
        .filter_map(|(key, share)| {
            let row = rows.get(key)?;
            Some(Used {
                session: key.to_string(),
                title: row.title.clone(),
                project: row.project.clone(),
                agent: Named::of(key.agent()),
                share: tenths(*share),
                active: row.running(now),
            })
        })
        .collect();
    let advice = match top.first() {
        Some((key, share)) if *share >= SIZEABLE => match rows.get(key) {
            Some(row) => advise(engine, account, limit, &window.track, key, row, *share)?,
            None => None,
        },
        _ => None,
    };
    Ok(Told { used_most, advice })
}

/// The advice drawn from the session `key`, whose `row` says what it did,
/// and which took `share` points of `account`'s `limit` in the window
/// `track` draws.
fn advise(
    engine: &Engine,
    account: &AccountLimits,
    limit: &LimitState,
    track: &LimitTrack,
    key: &SessionKey,
    row: &SessionRow,
    share: f64,
) -> turnscope_engine::Result<Option<Advice>> {
    let tokens = row.totals.tokens.total();
    let reads = if tokens == 0 {
        0.0
    } else {
        row.totals.tokens.cache_read as f64 / tokens as f64
    };
    if let Some(context) = engine
        .largest_contexts(std::slice::from_ref(key))?
        .get(key)
        .copied()
        && context >= LARGE
        && reads > 0.5
    {
        return Ok(Some(Advice::Reread { context }));
    }
    if row.subagents == 0 {
        return Ok(None);
    }
    let Some(usage) = engine.session_usage_without_prompts(key)? else {
        return Ok(None);
    };
    let Some(within) = usage.share_in(&account.id, &limit.key, track) else {
        return Ok(None);
    };
    let mut by_model: HashMap<&ModelKey, f64> = HashMap::new();
    for (subagent, part) in usage.subagents.iter().zip(&within.subagents) {
        if let Some(model) = &subagent.model {
            *by_model.entry(model).or_default() += part;
        }
    }
    let total: f64 = within.subagents.iter().sum();
    let Some((model, _)) = by_model.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)) else {
        return Ok(None);
    };
    if total < share / 4.0 || small(model) {
        return Ok(None);
    }
    // Named as a person knows the model, where the catalog names it.
    let name = engine
        .models()?
        .into_iter()
        .find(|info| info.key == *model)
        .and_then(|info| info.name)
        .unwrap_or_else(|| model.as_str().to_owned());
    Ok(Some(Advice::Subagents {
        share: tenths(total),
        model: name,
    }))
}

/// Whether `model` is one of the small ones, which advice never suggests
/// moving work off: those whose names say so.
fn small(model: &ModelKey) -> bool {
    let name = model.as_str().to_lowercase();
    ["haiku", "mini", "nano", "flash", "lite"]
        .iter()
        .any(|word| name.contains(word))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use turnscope_engine::{AccountLimits, Agent, Instant, LimitProblem, LimitState, Subscription};

    use super::{Advice, Named, Told, Used, assemble, time};
    use crate::connect::{Link, Status};

    fn at(text: &str) -> Instant {
        Instant::parse(text).unwrap()
    }

    #[test]
    fn a_time_is_written_to_the_second_it_falls_in() {
        assert_eq!(time(at("2026-09-14T12:00:00.999Z")), "2026-09-14T12:00:00Z");
        assert_eq!(
            time(at("2026-09-14T07:00:00-05:00")),
            "2026-09-14T12:00:00Z"
        );
    }

    /// A limit read at 12:00, `used` percent used and rising `pace` points
    /// an hour, in a window from `starts` to `resets`.
    fn limit(
        key: &str,
        name: &str,
        used: f64,
        pace: f64,
        starts: &str,
        resets: &str,
    ) -> LimitState {
        LimitState {
            key: key.into(),
            name: name.into(),
            scope: None,
            used: Some(used),
            starts: Some(at(starts)),
            resets: Some(at(resets)),
            read_at: at("2026-09-30T12:00:00Z"),
            pace: Some(pace),
            refilled: false,
        }
    }

    fn account(id: &str, subscription: Subscription, limits: Vec<LimitState>) -> AccountLimits {
        AccountLimits {
            id: id.into(),
            subscription,
            label: None,
            plan: None,
            agents: Vec::new(),
            signed_in: true,
            limits,
            read_at: Some(at("2026-09-30T12:00:00Z")),
            checked_at: Some(at("2026-09-30T12:00:00Z")),
            problem: None,
            in_use: false,
            hidden: false,
            provider: None,
            folders: Vec::new(),
        }
    }

    /// The feed `contract/feed.json` holds: four accounts, as of 12:00 UTC
    /// on 30 September 2026.
    fn contract_feed() -> super::Feed {
        // Claude Max, in use. Five hours, 12:00 read, 80% used, rising 10
        // points an hour: its 20 left last 2 hours, to 14:00, 1 hour before
        // its 15:00 reset, beyond the 15-minute margin: running out. Its
        // week, 40% used, rising 0.2 an hour, 84 hours to its reset: 16.8
        // more, 43.2 left at it.
        let mut claude = account(
            "claude:a",
            Subscription::Claude,
            vec![
                limit(
                    "five_hour",
                    "5 hours",
                    80.0,
                    10.0,
                    "2026-09-30T10:00:00Z",
                    "2026-09-30T15:00:00Z",
                ),
                limit(
                    "seven_day",
                    "Weekly",
                    40.0,
                    0.2,
                    "2026-09-27T00:00:00Z",
                    "2026-10-04T00:00:00Z",
                ),
            ],
        );
        claude.plan = Some("max".into());
        claude.label = Some("joey@example.com".into());
        claude.agents = vec![Agent::ClaudeCode];
        claude.in_use = true;
        // ChatGPT Pro Lite, not in use, its week 34% used and still.
        let mut chatgpt = account(
            "chatgpt:b",
            Subscription::ChatGpt,
            vec![limit(
                "primary_window",
                "Weekly",
                34.0,
                0.0,
                "2026-09-26T19:00:00Z",
                "2026-10-03T19:00:00Z",
            )],
        );
        chatgpt.plan = Some("prolite".into());
        chatgpt.agents = vec![Agent::Codex, Agent::OpenCode];
        // SuperGrok, whose sign-in was refused.
        let mut grok = account("supergrok:c", Subscription::SuperGrok, Vec::new());
        grok.agents = vec![Agent::Grok, Agent::Pi];
        grok.problem = Some(LimitProblem::SignIn);
        // An OpenRouter key with no limit, hidden.
        let mut key = account("api:openrouter", Subscription::ApiKey, Vec::new());
        key.provider = Some("openrouter".into());
        key.agents = vec![Agent::OpenCode, Agent::Pi];
        key.hidden = true;
        let told = HashMap::from([(
            "claude:a".to_owned(),
            Told {
                used_most: vec![Used {
                    session: "claude-code:s1".into(),
                    title: Some("Rethink the architecture".into()),
                    project: Some("atlas".into()),
                    agent: Named::of(Agent::ClaudeCode),
                    share: 12.5,
                    active: true,
                }],
                advice: Some(Advice::Reread { context: 966_000 }),
            },
        )]);
        let agents = vec![
            Link {
                id: "claude-code",
                name: "Claude Code",
                logo: crate::connect::logo(Agent::ClaudeCode),
                status: Status::Connected,
            },
            Link {
                id: "codex",
                name: "Codex",
                logo: crate::connect::logo(Agent::Codex),
                status: Status::Available,
            },
            Link {
                id: "opencode",
                name: "OpenCode",
                logo: crate::connect::logo(Agent::OpenCode),
                status: Status::Outdated,
            },
            Link {
                id: "pi",
                name: "Pi",
                logo: crate::connect::logo(Agent::Pi),
                status: Status::Unsupported,
            },
        ];
        // Given out of order, to be put in order.
        assemble(
            vec![key, grok, chatgpt, claude],
            told,
            agents,
            false,
            at("2026-09-30T12:00:00Z"),
        )
    }

    #[test]
    fn the_feed_is_the_contract() {
        let written = serde_json::to_string_pretty(&contract_feed()).unwrap() + "\n";
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contract/feed.json");
        if std::env::var_os("TURNSCOPE_WRITE_CONTRACT").is_some() {
            std::fs::write(&path, &written).unwrap();
        }
        let contract = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            written, contract,
            "the feed changed: review the change, then write the contract again with \
             TURNSCOPE_WRITE_CONTRACT=1, raise feed::VERSION, and change the app's reading of it"
        );
    }

    #[test]
    fn accounts_in_use_come_first_then_those_with_room_then_those_to_sign_in() {
        let feed = contract_feed();
        let order: Vec<&str> = feed
            .accounts
            .iter()
            .map(|account| account.id.as_str())
            .collect();
        assert_eq!(
            order,
            ["claude:a", "chatgpt:b", "supergrok:c", "api:openrouter"]
        );
        let claude = &feed.accounts[0];
        // Five hours run out at 14:00, an hour before they reset: they are
        // what matters, and the account is running out.
        assert_eq!(claude.deciding.as_deref(), Some("five_hour"));
        assert_eq!(claude.standing, "running_out");
        assert_eq!(
            claude.limits[0].runs_out_at.as_deref(),
            Some("2026-09-30T14:00:00Z")
        );
        assert_eq!(claude.limits[1].left_at_reset, Some(43.2));
        // Its five hours, read two hours in, 40% gone and 80% used, are 40
        // points ahead of an even pace; its week, half gone and 40% used,
        // 10 in reserve.
        assert_eq!(claude.limits[0].reserve, Some(-40.0));
        assert_eq!(claude.limits[1].reserve, Some(10.0));
        // Running out, its five hours' 20 left over the 3 hours to their
        // reset last at 6.67 points an hour; its week, lasting, has no
        // budget to keep to.
        assert_eq!(claude.limits[0].budget, Some(6.67));
        assert_eq!(claude.limits[1].budget, None);
        assert_eq!(claude.title, "Claude Max");
        assert_eq!(feed.accounts[1].title, "ChatGPT Pro Lite");
        assert_eq!(feed.accounts[3].title, "OpenRouter API key");
    }
}
