//! `explain_limit`: what used a limit's window, and why.
//!
//! **Shares.** A limit's readings say how much of it is used and nothing of
//! who used it, so the engine shares each rise between two readings among
//! the responses that could have drawn on it, by what each cost at list
//! prices ([`turnscope_engine::Engine::limit_window`]); a rise while nothing
//! here spent is use elsewhere, as in the provider's apps or on its website,
//! and a rise while only responses with no known price were made here is
//! this Mac's but no one's in particular, said apart from both. Shares are
//! approximate, and say so.
//!
//! **Why.** Beside each session's share is what drove it, as the person can
//! change it: the model; how many responses; how large its context grew,
//! which every response sends again, and how much of its tokens were cache
//! reads, the context read again; its subagents and their share; and how
//! long it ran. Given a session, its share is broken down by prompt and
//! subagent instead ([`turnscope_engine::Engine::session_usage`]).
//!
//! **Which window.** The latest one read: the engine reckons what used a
//! window from its readings, and keeps no reckoning of earlier ones. When
//! that window has reset since and the new one isn't read yet, the answer
//! says it explains the window that ended, and when, rather than give its
//! figures as how the limit stands now.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};
use turnscope_engine::{
    AccountLimits, Agent, Instant, LimitState, LimitWindow, ModelKey, SessionKey, SessionRow,
    Subscription,
};

use crate::accounts;
use crate::limits;
use crate::prose;
use crate::sessions;
use crate::tools::{Answer, Failure, Reply, Server, account_schema, object, rounded, shape};

/// How many parts are told apart; the rest are summed as others.
const PARTS: usize = 5;

/// How many prompts of a session are told apart.
const PROMPTS: usize = 8;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExplainLimit {
    account: Option<String>,
    limit: Option<String>,
    by: Option<String>,
    session: Option<String>,
}

pub(crate) fn schema() -> Value {
    object(
        json!({
            "account": account_schema("Default: yours."),
            "limit": {
                "type": "string",
                "description": "Which limit, as check_limits names them, such as \"week\" or \"5 hours\". Default: the one that runs out first.",
            },
            "by": {
                "type": "string",
                "enum": ["sessions", "projects", "models", "agents"],
                "description": "What to split the window by: sessions (the default), projects, models or agents.",
            },
            "session": {
                "type": "string",
                "description": "A session's id: break its share down by prompt and subagent instead.",
            },
        }),
        &[],
    )
}

pub(crate) fn output_schema() -> Value {
    shape(
        json!({
            "account": shape(json!({"id": {"type": "string"}, "name": {"type": "string"}}), &["id", "name"]),
            "limit": shape(json!({
                "limit": {"type": "string"},
                "key": {"type": "string"},
                "since": {"type": ["string", "null"]},
                "resets_at": {"type": ["string", "null"]},
                "used_percent": {"type": "number"},
                "left_percent": {"type": "number"},
                "ended": {"type": "boolean", "description": "The window reset at resets_at, and the one since isn't read yet: the figures are of the window that ended."},
            }), &["limit", "key", "used_percent", "left_percent"]),
            "by": {"type": "string"},
            "parts": {"type": "array", "items": shape(json!({
                "key": {"type": ["string", "null"]},
                "name": {"type": "string"},
                "share_percent": {"type": "number"},
            }), &["name", "share_percent"])},
            "others": shape(json!({"count": {"type": "integer"}, "share_percent": {"type": "number"}}), &["count", "share_percent"]),
            "elsewhere_percent": {"type": "number"},
            "unpriced_percent": {"type": "number", "description": "Used on this Mac while only responses with no known price were made, which can't be shared among them."},
            "approximate": {"type": "boolean"},
            "session": {"type": "object"},
        }),
        &["account", "limit", "by", "parts", "approximate"],
    )
}

/// What a window is split by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum By {
    Sessions,
    Projects,
    Models,
    Agents,
}

impl By {
    fn key(self) -> &'static str {
        match self {
            By::Sessions => "sessions",
            By::Projects => "projects",
            By::Models => "models",
            By::Agents => "agents",
        }
    }
}

pub(crate) fn explain(server: &Server, arguments: ExplainLimit) -> Answer {
    let by = match arguments.by.as_deref() {
        None | Some("sessions") => By::Sessions,
        Some("projects") => By::Projects,
        Some("models") => By::Models,
        Some("agents") => By::Agents,
        Some(other) => {
            return Err(Failure(format!(
                "by takes sessions, projects, models or agents; {other:?} is none of those."
            )));
        }
    };
    if arguments.session.is_some() && arguments.by.is_some() {
        return Err(Failure(
            "session breaks one session down by prompt and subagent, so by doesn't come with it."
                .into(),
        ));
    }
    let (accounts, _) = server.accounts()?;
    let account = match arguments.account.as_deref() {
        Some(asked) => accounts::one(asked, &accounts)?,
        None => accounts::asking(server, &accounts)?.1.map_err(|why| {
            Failure(format!(
                "{why} Name the account with account; check_limits lists them."
            ))
        })?,
    };
    let limit = match arguments.limit.as_deref() {
        Some(asked) => account
            .limits
            .iter()
            .find(|limit| limits::matches(limit, asked))
            .ok_or_else(|| limits::no_limit(asked, [account]))?,
        // With every window reset since it was read, none decides: the
        // first on all of its usage, whose window that ended is explained.
        None => account
            .deciding()
            .or_else(|| account.limits.iter().find(|limit| limit.scope.is_none()))
            .or_else(|| account.limits.first())
            .ok_or_else(|| {
                Failure(format!(
                    "No limit of {} has been read, so none can be explained.",
                    accounts::name(account)
                ))
            })?,
    };
    let window = server
        .engine
        .limit_window(&account.id, &limit.key)?
        .ok_or_else(|| {
            Failure(format!(
                "No reading of {}'s {} is kept, so what used it can't be told.",
                accounts::name(account),
                limits::spoken(limit)
            ))
        })?;
    let since = window
        .track
        .starts
        .or_else(|| window.track.points.first().map(|(at, _)| *at));
    let now = Instant::now();
    let ended = window.track.resets.filter(|resets| *resets <= now);
    // The track holds a point for every reading, so a window read has one.
    let used = window.track.points.last().map_or(0.0, |(_, used)| *used);
    let name = format!("{}, {}", accounts::name(account), limits::spoken(limit));
    let heading = match ended {
        Some(at) => format!(
            "{name}: the window{} reset at {}, {} used by then. Nothing has read the new one \
             yet, so this explains the window that ended",
            since.map_or_else(String::new, |since| format!(
                " from {}",
                server.clock(since)
            )),
            server.clock(at),
            prose::used(used)
        ),
        None => format!(
            "{name}{}: {} used, {} left",
            since.map_or_else(String::new, |since| format!(
                " since {}",
                server.clock(since)
            )),
            prose::used(used),
            prose::left(100.0 - used)
        ),
    };
    let mut said = vec![format!("{heading}.")];
    // The parts after a blank line.
    said.push(String::new());
    let mut data = json!({
        "account": {"id": account.id, "name": accounts::name(account)},
        "limit": {
            "limit": limits::limit_name(limit),
            "key": limit.key,
            "since": since.map(|at| server.time(at)),
            "resets_at": window.track.resets.map(|at| server.time(at)),
            "used_percent": rounded(used, 1),
            "left_percent": rounded((100.0 - used).max(0.0), 1),
        },
        "by": if arguments.session.is_some() { "session" } else { by.key() },
        "parts": [],
        "elsewhere_percent": rounded(window.elsewhere, 2),
        "unpriced_percent": rounded(window.unpriced, 2),
        "approximate": true,
    });
    if ended.is_some() {
        data["limit"]["ended"] = json!(true);
    }
    if let Some(id) = arguments.session.as_deref() {
        let row = sessions::named(server, id)?;
        session(server, account, limit, &window, &row, &mut said, &mut data)?;
    } else {
        let parts = match by {
            By::Sessions => by_sessions(server, account, limit, &window, &accounts, &mut said)?,
            By::Projects => named_parts(
                window
                    .projects
                    .iter()
                    .map(|(root, share)| {
                        let name = root.as_deref().map_or_else(
                            || "no project".to_owned(),
                            |root| {
                                root.rsplit('/')
                                    .find(|part| !part.is_empty())
                                    .unwrap_or(root)
                                    .to_owned()
                            },
                        );
                        (root.clone(), name, *share)
                    })
                    .collect(),
                &mut said,
            ),
            By::Models => {
                let names = server.model_names()?;
                named_parts(
                    window
                        .models
                        .iter()
                        .map(|(model, share)| {
                            let name = if model.as_str().is_empty() {
                                "no model named".to_owned()
                            } else {
                                model_name(&names, model)
                            };
                            (Some(model.as_str().to_owned()), name, *share)
                        })
                        .collect(),
                    &mut said,
                )
            }
            By::Agents => {
                let mut agents: Vec<(Agent, f64)> = Vec::new();
                for (key, share) in &window.sessions {
                    match agents.iter_mut().find(|(agent, _)| *agent == key.agent()) {
                        Some((_, total)) => *total += share,
                        None => agents.push((key.agent(), *share)),
                    }
                }
                agents.sort_by(|a, b| b.1.total_cmp(&a.1));
                named_parts(
                    agents
                        .into_iter()
                        .map(|(agent, share)| {
                            (Some(agent.key().to_owned()), agent.name().to_owned(), share)
                        })
                        .collect(),
                    &mut said,
                )
            }
        };
        data["parts"] = parts.0;
        data["others"] = parts.1;
    }
    said.extend(unshared(&window, account.subscription));
    Ok(Reply::Answer {
        said: said.join("\n"),
        data,
    })
}

/// What of `window` no part took, each a sentence when it is at least a
/// tenth of a point as rounded: use off this Mac, and use on it with no
/// known price to share it out by.
fn unshared(window: &LimitWindow, subscription: Subscription) -> Vec<String> {
    let mut said = Vec::new();
    if window.elsewhere >= 0.05 {
        said.push(format!(
            "Not on this Mac ({}): {}.",
            elsewhere(subscription),
            prose::share(window.elsewhere)
        ));
    }
    if window.unpriced >= 0.05 {
        said.push(format!(
            "On this Mac, by models with no known price, so not shared out: {}.",
            prose::share(window.unpriced)
        ));
    }
    said
}

/// Where use off this Mac comes from, for a subscription.
fn elsewhere(subscription: Subscription) -> &'static str {
    match subscription {
        Subscription::Claude => "Claude apps and the web",
        Subscription::ChatGpt => "ChatGPT's apps, the web and Codex elsewhere",
        Subscription::SuperGrok => "Grok's apps and the web",
        Subscription::OpenCodeGo => "OpenCode elsewhere",
        Subscription::ApiKey => "the same key used elsewhere",
    }
}

/// What `names` calls `model`, or its key where they don't name it.
fn model_name(names: &HashMap<String, String>, model: &ModelKey) -> String {
    names
        .get(model.as_str())
        .cloned()
        .unwrap_or_else(|| model.as_str().to_owned())
}

/// Parts, each its key, name and share, most first: the first [`PARTS`] said
/// a line each and given, the rest summed as others. The figures of the
/// parts and of the others.
fn named_parts(
    parts: Vec<(Option<String>, String, f64)>,
    said: &mut Vec<String>,
) -> (Value, Value) {
    for (place, (_, name, share)) in parts.iter().take(PARTS).enumerate() {
        said.push(format!("{}. {name}: {}.", place + 1, prose::share(*share)));
    }
    let shares: Vec<f64> = parts.iter().map(|(_, _, share)| *share).collect();
    let (count, share) = rest(&shares);
    if count > 0 {
        said.push(format!("Others: {count} more, {}.", prose::share(share)));
    }
    (
        parts
            .iter()
            .take(PARTS)
            .map(|(key, name, share)| json!({"key": key, "name": name, "share_percent": rounded(*share, 2)}))
            .collect(),
        others(&shares),
    )
}

/// How many of `shares` come after the first [`PARTS`], and what they come
/// to together.
fn rest(shares: &[f64]) -> (usize, f64) {
    let rest = &shares[shares.len().min(PARTS)..];
    (rest.len(), rest.iter().sum())
}

/// The figures of the parts after the first [`PARTS`] of `shares`.
fn others(shares: &[f64]) -> Value {
    let (count, share) = rest(shares);
    json!({"count": count, "share_percent": rounded(share, 2)})
}

/// The window's sessions, most first, each with what drove its share.
fn by_sessions(
    server: &Server,
    account: &AccountLimits,
    limit: &LimitState,
    window: &LimitWindow,
    accounts: &[AccountLimits],
    said: &mut Vec<String>,
) -> Result<(Value, Value), Failure> {
    let top: Vec<&(SessionKey, f64)> = window.sessions.iter().take(PARTS).collect();
    let keys: Vec<SessionKey> = top.iter().map(|(key, _)| key.clone()).collect();
    let rows = server.engine.session_rows(&keys)?;
    let contexts = server.engine.largest_contexts(&keys)?;
    let names = server.model_names()?;
    let now = Instant::now();
    let mut parts = Vec::new();
    for (place, (key, share)) in top.iter().enumerate() {
        let row = rows.get(key);
        let breakdown = server.engine.session_usage_without_prompts(key)?;
        let within = breakdown
            .as_ref()
            .and_then(|breakdown| breakdown.share_in(&account.id, &limit.key, &window.track));
        let subagents_share: Option<f64> = within.map(|share| share.subagents.iter().sum());
        let model_name = |model: &ModelKey| model_name(&names, model);
        let mut heading = format!(
            "{}. {}",
            place + 1,
            row.map_or_else(|| key.to_string(), sessions::title)
        );
        let mut about = Vec::new();
        if let Some(row) = row {
            if let Some(project) = &row.project {
                about.push(project.clone());
            }
            about.push(key.agent().name().to_owned());
            if sessions::running(row) {
                about.push("running".to_owned());
            } else if let Some(active) = row.active {
                about.push(prose::day(active, now, &server.zone));
            }
        }
        if !about.is_empty() {
            heading.push_str(&format!(" ({})", about.join(", ")));
        }
        heading.push_str(&format!(": {}.", prose::share(*share)));
        let mut why = Vec::new();
        let mut figures = json!({
            "key": key.to_string(),
            "name": row.and_then(|row| row.title.clone()).unwrap_or_else(|| key.to_string()),
            "share_percent": rounded(*share, 2),
        });
        if let Some(row) = row {
            let models: Vec<String> = row.models.iter().take(2).map(model_name).collect();
            let responses = row.totals.responses;
            let duration = row
                .started
                .zip(row.active)
                .map(|(start, end)| end.millis() - start.millis())
                .filter(|millis| *millis > 0);
            let mut first = prose::list(&models);
            if let Some(duration) = duration {
                first.push_str(&format!(" for {}", prose::span(duration)));
            }
            first.push_str(&format!(", {}.", prose::count(responses, "response")));
            why.push(first.trim_start_matches(", ").to_owned());
            let tokens = row.totals.tokens.total();
            let reads = (tokens > 0).then(|| row.totals.tokens.cache_read as f64 / tokens as f64);
            if let Some(context) = contexts.get(key) {
                let mut grew = format!("Its context grew to {} tokens", prose::tokens(*context));
                match reads {
                    Some(reads) if reads >= 0.5 => grew.push_str(&format!(
                        " and was read again on every response: {} of its tokens were cache reads.",
                        prose::percent(reads * 100.0)
                    )),
                    Some(reads) if reads >= 0.05 => grew.push_str(&format!(
                        "; {} of its tokens were cache reads.",
                        prose::percent(reads * 100.0)
                    )),
                    _ => grew.push('.'),
                }
                why.push(grew);
            }
            if row.subagents > 0 {
                let mut subagents = prose::count(row.subagents.into(), "subagent");
                let mut subagent_models: Vec<String> = breakdown
                    .as_ref()
                    .map(|breakdown| {
                        breakdown
                            .subagents
                            .iter()
                            .filter_map(|subagent| subagent.model.as_ref().map(model_name))
                            .collect()
                    })
                    .unwrap_or_default();
                subagent_models.sort();
                subagent_models.dedup();
                if !subagent_models.is_empty() {
                    subagents.push_str(&format!(", on {},", prose::list(&subagent_models)));
                }
                match subagents_share {
                    Some(part) => {
                        subagents.push_str(&format!(" took {} of it.", prose::share(part)))
                    }
                    None => subagents.push_str(" ran within it."),
                }
                why.push(subagents);
            }
            figures["models"] = json!(row.models.iter().map(ModelKey::as_str).collect::<Vec<_>>());
            figures["responses"] = json!(responses);
            figures["duration_minutes"] = json!(duration.map(|millis| millis / 60_000));
            figures["largest_context_tokens"] = json!(contexts.get(key));
            figures["cache_read_share"] = json!(reads.map(|reads| rounded(reads, 3)));
            figures["subagents"] = json!(row.subagents);
            figures["subagents_share_percent"] =
                json!(subagents_share.map(|share| rounded(share, 2)));
            figures["running"] = json!(sessions::running(row));
            figures["account"] =
                json!(accounts::of_session(row, accounts).map(|account| account.id.clone()));
        }
        said.push(heading);
        if !why.is_empty() {
            said.push(format!("   {}", why.join(" ")));
        }
        parts.push(figures);
    }
    let shares: Vec<f64> = window.sessions.iter().map(|(_, share)| *share).collect();
    let (count, share) = rest(&shares);
    if count > 0 {
        said.push(format!(
            "Others: {count} sessions, {}.",
            prose::share(share)
        ));
    }
    if window.sessions.is_empty() {
        said.push("Nothing on this Mac took a share of it yet.".to_owned());
    }
    Ok((Value::Array(parts), others(&shares)))
}

/// One session's share of the window, by prompt and subagent.
fn session(
    server: &Server,
    account: &AccountLimits,
    limit: &LimitState,
    window: &LimitWindow,
    row: &SessionRow,
    said: &mut Vec<String>,
    data: &mut Value,
) -> Result<(), Failure> {
    let breakdown = server
        .engine
        .session_usage(&row.key)?
        .ok_or_else(|| Failure(format!("No session has the id {:?}.", row.key.to_string())))?;
    let Some(share) = breakdown.share_in(&account.id, &limit.key, &window.track) else {
        said.push(format!(
            "{} took no share of this window.",
            sessions::title(row)
        ));
        data["session"] = json!({"id": row.key.to_string(), "share_percent": 0.0});
        return Ok(());
    };
    said.push(format!(
        "{} took {}{}.",
        sessions::title(row),
        if share.whole { "" } else { "at least " },
        prose::share(share.share)
    ));
    let mut prompts: Vec<(usize, f64)> = share
        .prompts
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, part)| *part > 0.0)
        .collect();
    prompts.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (place, part) in prompts.iter().take(PROMPTS) {
        if let Some(prompt) = breakdown.prompts.get(*place) {
            let subagents = if prompt.subagents > 0 {
                format!(", {}", prose::count(prompt.subagents.into(), "subagent"))
            } else {
                String::new()
            };
            said.push(format!(
                "- \"{}\" ({}{subagents}): {}.",
                prose::line(&prompt.said, 100),
                server.clock(prompt.at),
                prose::share(*part)
            ));
        }
    }
    if prompts.len() > PROMPTS {
        let rest: f64 = prompts[PROMPTS..].iter().map(|(_, part)| part).sum();
        said.push(format!(
            "- {} other prompts: {}.",
            prompts.len() - PROMPTS,
            prose::share(rest)
        ));
    }
    if share.unprompted >= 0.05 {
        said.push(format!(
            "- Before any prompt, or at no time known: {}.",
            prose::share(share.unprompted)
        ));
    }
    let mut subagents: Vec<(usize, f64)> = share
        .subagents
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, part)| *part > 0.0)
        .collect();
    subagents.sort_by(|a, b| b.1.total_cmp(&a.1));
    if !subagents.is_empty() {
        let named: Vec<String> = subagents
            .iter()
            .take(PARTS)
            .filter_map(|(place, part)| {
                let subagent = breakdown.subagents.get(*place)?;
                let title = subagent.title.as_deref().map_or_else(
                    || subagent.key.to_string(),
                    |title| format!("\"{}\"", prose::line(title, 60)),
                );
                let model = subagent
                    .model
                    .as_ref()
                    .map_or_else(String::new, |model| format!(", {model}"));
                Some(format!("{title}{model}: {}", prose::share(*part)))
            })
            .collect();
        let total: f64 = subagents.iter().map(|(_, part)| part).sum();
        said.push(format!(
            "Its subagents took {} of it: {}.",
            prose::share(total),
            named.join("; ")
        ));
    }
    data["session"] = json!({
        "id": row.key.to_string(),
        "share_percent": rounded(share.share, 2),
        "whole": share.whole,
        "prompts": prompts.iter().filter_map(|(place, part)| {
            let prompt = breakdown.prompts.get(*place)?;
            Some(json!({
                "said": prose::line(&prompt.said, 300),
                "at": server.time(prompt.at),
                "subagents": prompt.subagents,
                "share_percent": rounded(*part, 2),
            }))
        }).collect::<Vec<_>>(),
        "unprompted_percent": rounded(share.unprompted, 2),
        "subagents": subagents.iter().filter_map(|(place, part)| {
            let subagent = breakdown.subagents.get(*place)?;
            Some(json!({
                "id": subagent.key.to_string(),
                "title": subagent.title,
                "model": subagent.model.as_ref().map(ModelKey::as_str),
                "share_percent": rounded(*part, 2),
            }))
        }).collect::<Vec<_>>(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use turnscope_engine::{LimitTrack, LimitWindow, Subscription};

    use super::unshared;

    /// A window nothing here took, with `elsewhere` and `unpriced`.
    fn window(elsewhere: f64, unpriced: f64) -> LimitWindow {
        LimitWindow {
            track: LimitTrack {
                starts: None,
                resets: None,
                points: Vec::new(),
            },
            sessions: Vec::new(),
            projects: Vec::new(),
            models: Vec::new(),
            elsewhere,
            unpriced,
        }
    }

    #[test]
    fn what_no_part_took_is_said_apart_where_it_is_more_than_rounding() {
        assert_eq!(
            unshared(&window(12.0, 3.0), Subscription::Claude),
            [
                "Not on this Mac (Claude apps and the web): 12.0%.",
                "On this Mac, by models with no known price, so not shared out: 3.0%."
            ]
        );
        // Under 0.05 is not said.
        assert!(unshared(&window(0.04, 0.04), Subscription::Claude).is_empty());
    }
}
