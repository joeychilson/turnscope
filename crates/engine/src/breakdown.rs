//! What share of each limit a session took, and each of its prompts and
//! subagents' part of it.
//!
//! **Prompts.** Each thing the person said, as the session's conversation
//! shows it, starts a prompt, which lasts until the next. The session's own
//! usage belongs to the prompt it came after. A subagent's belongs, all of
//! it, to the prompt it was started in: by the call that started it, where
//! its agent records one, and by its first response where not. A subagent
//! can go on working after the next prompt is given, and what it does is
//! still the earlier prompt's doing. A subagent started by a subagent is its
//! starter's. Usage before the first prompt, or at no time known, is no
//! prompt's. A session whose files are gone has no prompts, nor does one
//! whose prompts have no times, as Grok Build's: its usage is no prompt's.
//!
//! **Shares of limits.** Every window of every limit it could have drawn on
//! and took a share of, as [`crate::limits::share`] shares them out, with
//! each prompt's and subagent's part of it: parts of one reckoning, they add
//! up to the whole.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};

use crate::error::{Error, Result};
use crate::ledger::{Ledger, optional_instant, unsigned, usd};
use crate::limits::share::{self, Spent};
use crate::model::ModelKey;
use crate::session::SessionKey;
use crate::time::Instant;
use crate::transcript::{Entry, Speaker};
use crate::usage::{Usd, add_counts};

/// Where a session's usage went.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionUsage {
    /// Each prompt, in the order given: none when its files are gone, or
    /// when its prompts have no times.
    pub prompts: Vec<PromptUsage>,
    /// Every subagent it ran, however deep, in the order they started.
    pub subagents: Vec<SubagentUsage>,
    /// Its share of each window of each limit it took one of.
    pub limits: Vec<LimitShare>,
}

/// One prompt of a session.
#[derive(Clone, Debug, PartialEq)]
pub struct PromptUsage {
    /// What the person said.
    pub said: String,
    /// When.
    pub at: Instant,
    /// How many subagents were started in it, however deep.
    pub subagents: u32,
}

/// One subagent of a session.
#[derive(Clone, Debug, PartialEq)]
pub struct SubagentUsage {
    /// Which it is.
    pub key: SessionKey,
    /// Its title, where it has one.
    pub title: Option<String>,
    /// The model it used most, by tokens, the subagents it started apart.
    pub model: Option<ModelKey>,
}

/// A session's share of one window of one limit. It is approximate: a
/// limit's readings say how much of it is used and nothing of who used it,
/// so each rise between two readings is shared out among the responses
/// that could have drawn on it, by what each cost at list prices.
#[derive(Clone, Debug, PartialEq)]
pub struct LimitShare {
    /// The account whose limit it is, as [`crate::AccountLimits::id`].
    pub account: String,
    /// The limit's key within the account.
    pub key: String,
    /// What the limit is called, as `Weekly`.
    pub name: String,
    /// The one model it applies to, when not all.
    pub scope: Option<String>,
    /// When the window started, where known.
    pub starts: Option<Instant>,
    /// When it resets, where known.
    pub resets: Option<Instant>,
    /// The latest reading the share goes up to: what was spent since has no
    /// rise of the limit yet.
    pub through: Instant,
    /// Whether every rise it could have taken a share of is known: false
    /// when some of its usage came before the first reading of a window
    /// whose start isn't known, so the share is at least this.
    pub whole: bool,
    /// Its share, in points of the limit's percent.
    pub share: f64,
    /// Each prompt's part, by its place among the prompts.
    pub prompts: Vec<f64>,
    /// The part of usage no prompt holds.
    pub unprompted: f64,
    /// Each subagent's part of it, by its place among the subagents.
    pub subagents: Vec<f64>,
}

/// One response of the session or a subagent: when, of which model and
/// account, and at what cost, as a limit's shares take it, and its tokens.
struct Response {
    session: i64,
    tokens: u64,
    spent: Spent,
}

/// A session of the tree.
struct Member {
    id: i64,
    key: SessionKey,
    title: Option<String>,
    started: Option<Instant>,
    parent: Option<String>,
}

/// Where the session `key`'s usage went, from the cache and the ledger's
/// limit readings, its prompts from `entries`, its conversation as read now
/// when its files are still there. `None` when the cache holds no such
/// session.
///
/// # Errors
///
/// Returns [`Error::Ledger`] when the cache or the ledger cannot be read,
/// and [`Error::Corrupt`] when a figure in the cache is out of range.
pub(crate) fn session_usage(
    cache: &Connection,
    ledger: &Ledger,
    key: &SessionKey,
    entries: Option<&[Entry]>,
) -> Result<Option<SessionUsage>> {
    let Some(root) = cache
        .prepare_cached("SELECT id FROM session WHERE key = ?1")?
        .query_row([key.to_string()], |row| row.get::<_, i64>(0))
        .optional()?
    else {
        return Ok(None);
    };
    let members = members(cache, root)?;
    let responses = responses(cache, root)?;

    // Prompts, in the order given, those with a time.
    let mut prompts: Vec<PromptUsage> = entries
        .unwrap_or_default()
        .iter()
        .filter(|entry| entry.speaker == Speaker::User && entry.tool.is_none())
        .filter_map(|entry| {
            Some(PromptUsage {
                said: entry.text.clone(),
                at: entry.at?,
                subagents: 0,
            })
        })
        .collect();
    prompts.sort_by_key(|prompt| prompt.at);
    let said: Vec<Instant> = prompts.iter().map(|prompt| prompt.at).collect();
    let prompt_at = |at: Instant| said.partition_point(|said| *said <= at).checked_sub(1);

    // Each subagent's prompt: its starter's, up to the one the session
    // started, and that one's by the call that started it.
    let launched: HashMap<&SessionKey, Instant> = entries
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| Some((entry.tool.as_ref()?.subagent.as_ref()?, entry.at?)))
        .collect();
    let by_key: HashMap<String, &Member> = members
        .iter()
        .map(|member| (member.key.to_string(), member))
        .collect();
    let first_at: HashMap<i64, Instant> = responses
        .iter()
        .rev()
        .filter_map(|r| Some((r.session, r.spent.at?)))
        .collect();
    let root_key = key.to_string();
    let mut prompt_of: HashMap<i64, Option<usize>> = HashMap::new();
    for member in members.iter().filter(|member| member.id != root) {
        let mut top = member;
        // A tree holds each member once, so this many steps up reach the
        // top whatever the links say.
        for _ in 0..members.len() {
            match top.parent.as_deref() {
                Some(parent) if parent != root_key => match by_key.get(parent) {
                    Some(up) => top = up,
                    None => break,
                },
                _ => break,
            }
        }
        let started = launched
            .get(&top.key)
            .copied()
            .or_else(|| first_at.get(&top.id).copied())
            .or_else(|| first_at.get(&member.id).copied());
        prompt_of.insert(member.id, started.and_then(prompt_at));
    }

    let mut usage = SessionUsage::default();
    let place = |response: &Response| -> Option<usize> {
        if response.session == root {
            response.spent.at.and_then(prompt_at)
        } else {
            prompt_of.get(&response.session).copied().flatten()
        }
    };
    // Each subagent's own tokens by model.
    let mut own: HashMap<i64, HashMap<&str, u64>> = HashMap::new();
    for response in responses
        .iter()
        .filter(|response| response.session != root && !response.spent.model_key.is_empty())
    {
        let tokens = own
            .entry(response.session)
            .or_default()
            .entry(response.spent.model_key.as_str())
            .or_default();
        *tokens = add_counts(*tokens, response.tokens);
    }

    let mut subagents: Vec<&Member> = members.iter().filter(|member| member.id != root).collect();
    subagents.sort_by_key(|member| {
        (
            member.started.or_else(|| first_at.get(&member.id).copied()),
            member.id,
        )
    });
    for member in &subagents {
        let prompt = prompt_of.get(&member.id).copied().flatten();
        if let Some(prompt) = prompt.and_then(|index| prompts.get_mut(index)) {
            prompt.subagents = prompt.subagents.saturating_add(1);
        }
        let model = own
            .remove(&member.id)
            .unwrap_or_default()
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0)))
            .map(|(model, _)| ModelKey::stored(model.to_owned()));
        usage.subagents.push(SubagentUsage {
            key: member.key.clone(),
            title: member.title.clone(),
            model,
        });
    }

    // Shares of limits, each response's part summed as its usage was.
    let spent: Vec<Spent> = responses
        .iter()
        .map(|response| response.spent.clone())
        .collect();
    let subagent_place: HashMap<i64, usize> = subagents
        .iter()
        .enumerate()
        .map(|(place, member)| (member.id, place))
        .collect();
    for shared in share::shares(ledger, cache, &spent)? {
        let mut limit = LimitShare {
            account: shared.account,
            key: shared.key,
            name: shared.name,
            scope: shared.scope,
            starts: shared.starts,
            resets: shared.resets,
            through: shared.through,
            whole: shared.whole,
            share: 0.,
            prompts: vec![0.; prompts.len()],
            unprompted: 0.,
            subagents: vec![0.; subagents.len()],
        };
        for (response, part) in responses.iter().zip(shared.each) {
            if part <= 0. {
                continue;
            }
            limit.share += part;
            let prompt = place(response);
            match prompt.and_then(|index| limit.prompts.get_mut(index)) {
                Some(prompt) => *prompt += part,
                None => limit.unprompted += part,
            }
            if let Some(place) = subagent_place.get(&response.session) {
                limit.subagents[*place] += part;
            }
        }
        usage.limits.push(limit);
    }
    usage.prompts = prompts;
    Ok(Some(usage))
}

/// The session `root` and every session within it, however deep.
fn members(cache: &Connection, root: i64) -> Result<Vec<Member>> {
    let mut statement = cache.prepare_cached(
        "SELECT s.id, s.key, s.title, s.started, s.parent
         FROM lineage l JOIN session s ON s.id = l.session
         WHERE l.ancestor = ?1 ORDER BY s.id",
    )?;
    let mut rows = statement.query([root])?;
    let mut members = Vec::new();
    while let Some(row) = rows.next()? {
        let key: String = row.get(1)?;
        let key =
            SessionKey::parse(&key).ok_or_else(|| Error::corrupt("session key", key.clone()))?;
        members.push(Member {
            id: row.get(0)?,
            key,
            title: row.get(2)?,
            started: optional_instant(row.get(3)?, "session start")?,
            parent: row.get(4)?,
        });
    }
    Ok(members)
}

/// Every response of the session `root` and of every session within it,
/// oldest first, those at no time known last.
fn responses(cache: &Connection, root: i64) -> Result<Vec<Response>> {
    let mut statement = cache.prepare_cached(
        "SELECT u.session, u.at, u.account, u.model_key, u.cost,
                u.input, u.cache_read, u.cache_write_5m, u.cache_write_1h, u.output
         FROM usage u
         WHERE u.session IN (SELECT session FROM lineage WHERE ancestor = ?1)
         ORDER BY u.at IS NULL, u.at, u.agent, u.response",
    )?;
    let mut rows = statement.query([root])?;
    let mut responses = Vec::new();
    while let Some(row) = rows.next()? {
        // Its tokens, reasoning within its output, as a total counts them.
        let mut tokens = 0;
        for column in 5..10 {
            tokens = add_counts(tokens, unsigned(row.get(column)?, "tokens")?);
        }
        responses.push(Response {
            session: row.get(0)?,
            tokens,
            spent: Spent {
                at: optional_instant(row.get(1)?, "usage time")?,
                account: row.get(2)?,
                model_key: row.get(3)?,
                cost: usd(row.get(4)?, "cost")?.map(Usd::nanos),
            },
        });
    }
    Ok(responses)
}
