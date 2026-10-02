//! Finding sessions (`find_sessions`), and what the other tools say of one.
//!
//! Sessions are found by where they ran, by agent or account,
//! by period, by whether they are running now, and by words said in them.
//! Without words they come most recently active first, or by what they
//! used; with words, by the latest mention, each with the passage that
//! matched, read from its conversation now. Asked of an account, the
//! sessions any of whose usage drew on it, their subagents' counting, as
//! the engine keeps each response to the account signed in where and when
//! it was made; each is given as the account most of its responses drew on
//! ([`crate::accounts::of_session`]). "This session" is the caller's agent's latest in the caller's folder,
//! kept where the caller's agent keeps its history ([`this`]).

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};
use turnscope_engine::{
    AccountLimits, Filter, Folder, FolderOrigin, Instant, RUNNING, SearchQuery, SessionKey,
    SessionOrder, SessionQuery, SessionRow,
};

use crate::accounts;
use crate::prose;
use crate::tools::{
    self, Answer, Failure, Reply, Server, account_schema, agent_schema, folder_schema,
    moment_schema, object, shape,
};
use crate::usage;

/// The most sessions a page of them gives: 50 come to about 29,000
/// characters, within the same budget.
const MOST_SESSIONS: u32 = 50;

/// The most characters of a title a sentence quotes.
const LONGEST_TITLE: usize = 80;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FindSessions {
    folder: Option<String>,
    words: Option<String>,
    agent: Option<String>,
    account: Option<String>,
    since: Option<String>,
    until: Option<String>,
    running: Option<bool>,
    order: Option<String>,
    limit: Option<u32>,
    cursor: Option<String>,
}

pub(crate) fn find_schema() -> Value {
    object(
        json!({
            "folder": folder_schema(),
            "words": {
                "type": "string",
                "description": "Only sessions where you or a model said these words, all in one entry: a short phrase or a few distinctive words, not a sentence. Each matches as typed, as the start of a longer word, or as another form of an English word, ignoring case and accents. Tool output and what agents inject are not searched.",
            },
            "agent": agent_schema(),
            "account": account_schema("Only sessions that drew on it, or on any account it names."),
            "since": moment_schema("Only sessions with usage from then on"),
            "until": moment_schema("Only sessions with usage before then"),
            "running": {"type": "boolean", "description": "Only sessions running now: active, they or their subagents, in the last five minutes."},
            "order": {
                "type": "string",
                "enum": ["recent", "usage"],
                "description": "recent (the default): most recently active first. usage: most tokens first, subagents counted. With words, sessions come by their latest mention.",
            },
            "limit": {"type": "integer", "minimum": 1, "maximum": MOST_SESSIONS, "description": "How many to give, 10 unless given."},
            "cursor": {"type": "string", "description": "Where the previous page ended, as its next_cursor gave it."},
        }),
        &[],
    )
}

pub(crate) fn find_output_schema() -> Value {
    let mut session = figures_schema();
    session["properties"]["passage"] = shape(
        json!({"entry": {"type": "integer"}, "text": {"type": "string"}, "unavailable": {"type": "string"}}),
        &[],
    );
    session["properties"]["mentions"] = json!({"type": "integer"});
    shape(
        json!({
            "sessions": {"type": "array", "items": session},
            "next_cursor": {"type": ["string", "null"]},
        }),
        &["sessions", "next_cursor"],
    )
}

/// The output schema of a session, as [`figures`] gives it.
pub(crate) fn figures_schema() -> Value {
    shape(
        json!({
            "id": {"type": "string"},
            "title": {"type": ["string", "null"]},
            "agent": {"type": "string"},
            "account": {"type": ["string", "null"], "description": "The account most of its responses drew on; null when that isn't known."},
            "account_hidden": {"type": "boolean", "description": "It drew on an account the person hid in Turnscope."},
            "project": {"type": ["string", "null"]},
            "folder": {"type": ["string", "null"]},
            "branch": {"type": ["string", "null"]},
            "started": {"type": ["string", "null"]},
            "last_active": {"type": ["string", "null"]},
            "running": {"type": "boolean"},
            "subagents": {"type": "integer"},
            "usage": usage::totals_schema(),
            "parent": {"type": "string"},
            "transcript_deleted": {"type": "boolean"},
        }),
        &["id", "agent", "running", "usage"],
    )
}

pub(crate) fn find(server: &Server, arguments: FindSessions) -> Answer {
    let order = match arguments.order.as_deref() {
        None | Some("recent") => SessionOrder::Recent,
        Some("usage") => SessionOrder::Tokens,
        Some(other) => {
            return Err(Failure(format!(
                "order takes recent or usage; {other:?} is none of those."
            )));
        }
    };
    let limit = tools::limit(arguments.limit, 10, MOST_SESSIONS)?;
    let mut span = server.span(arguments.since.as_deref(), arguments.until.as_deref())?;
    let agent = tools::agent(arguments.agent.as_deref())?;
    let accounts = server.engine.limits()?;
    let mut filter = Filter {
        projects: tools::folder(server, arguments.folder.as_deref())?,
        ..Filter::default()
    };
    if let Some(asked) = arguments.account.as_deref() {
        filter.accounts = accounts::ids(asked, &accounts)?;
    }
    if let Some(agent) = agent {
        filter.agents = vec![agent];
    }
    // Running now is being active in the last five minutes.
    let running_since = arguments
        .running
        .unwrap_or(false)
        .then(|| {
            let quiet = i64::try_from(RUNNING.as_millis()).unwrap_or(i64::MAX);
            Instant::from_millis(Instant::now().millis().saturating_sub(quiet))
        })
        .flatten();
    let words = arguments
        .words
        .as_deref()
        .map(str::trim)
        .filter(|words| !words.is_empty());
    let Some(words) = words else {
        let page = server.engine.sessions(&SessionQuery {
            span,
            filter,
            order,
            limit,
            after: arguments.cursor,
            active_since: running_since,
            ..SessionQuery::default()
        })?;
        let rows: Vec<_> = page.items.into_iter().map(|row| (row, None)).collect();
        return Ok(found(server, &rows, &accounts, page.next));
    };
    if order != SessionOrder::Recent {
        return Err(Failure(
            "With words, sessions come by their latest mention, so order can't be given too."
                .into(),
        ));
    }
    if !words.chars().any(char::is_alphanumeric) {
        return Err(Failure("words holds no word to look for.".into()));
    }
    // Searched, sessions running now are those with usage since then.
    span.from = span.from.max(running_since);
    let page = server.engine.search(&SearchQuery {
        words: words.to_owned(),
        span,
        filter,
        limit,
        after: arguments.cursor,
    })?;
    let keys: Vec<SessionKey> = page.items.iter().map(|hit| hit.session.clone()).collect();
    let mut rows: HashMap<SessionKey, SessionRow> = rows(server, keys.clone())?
        .into_iter()
        .map(|row| (row.key.clone(), row))
        .collect();
    // The passage around each first match, read from the agents' files now.
    // A file that is gone says so in place of its passage.
    let mentioned: Vec<(SessionRow, Option<(u64, Value)>)> = page
        .items
        .iter()
        .zip(server.engine.excerpts(&keys, words))
        .filter_map(|(hit, excerpt)| {
            let passage = match excerpt {
                Ok(Some(excerpt)) => json!({"entry": excerpt.index, "text": excerpt.text}),
                Ok(None) => json!({}),
                Err(error) => json!({"unavailable": error.to_string()}),
            };
            Some((rows.remove(&hit.session)?, Some((hit.matches, passage))))
        })
        .collect();
    Ok(found(server, &mentioned, &accounts, page.next))
}

/// A page of found sessions as an answer: each's figures, and, when words
/// were looked for, how many entries mention them and the passage that
/// matched.
fn found(
    server: &Server,
    rows: &[(SessionRow, Option<(u64, Value)>)],
    accounts: &[AccountLimits],
    next: Option<String>,
) -> Reply {
    let mut lines = Vec::new();
    let mut sessions = Vec::new();
    for (place, (row, mentioned)) in rows.iter().enumerate() {
        let mut value = figures(server, row, accounts);
        let mut line = format!("{}. {}", place + 1, line(server, row, accounts));
        if let Some((mentions, passage)) = mentioned {
            if let Some(text) = passage.get("text").and_then(Value::as_str) {
                line.push_str(&format!(
                    "\n   Said: \"{}\" (entry {})",
                    prose::line(text, 200),
                    passage["entry"]
                ));
            }
            value["passage"] = passage.clone();
            value["mentions"] = json!(mentions);
        }
        lines.push(line);
        sessions.push(value);
    }
    let mut said = if rows.is_empty() {
        "No session matches.".to_owned()
    } else {
        lines.join("\n")
    };
    if let Some(next) = &next {
        said.push_str(&format!(
            "\n\nMore: pass cursor {next:?} for the next page."
        ));
    }
    Reply::Answer {
        said,
        data: json!({"sessions": sessions, "next_cursor": next}),
    }
}

/// A session's figures, as answers give them.
pub(crate) fn figures(server: &Server, row: &SessionRow, accounts: &[AccountLimits]) -> Value {
    let account = accounts::of_session(row, accounts);
    let mut value = json!({
        "id": row.key.to_string(),
        "title": row.title,
        "agent": row.key.agent().key(),
        "account": account.map(|account| account.id.clone()),
        "project": row.project,
        "folder": row.cwd,
        "branch": row.branch,
        "started": row.started.map(|at| server.time(at)),
        // It or any subagent within it: when any of its work was last done.
        "last_active": row.active.map(|at| server.time(at)),
        "running": running(row),
        "subagents": row.subagents,
        // What it took, its subagents' work with its own.
        "usage": usage::totals(&row.with_subagents),
    });
    if let Some((parent, _)) = &row.parent {
        value["parent"] = json!(parent.to_string());
    }
    if !row.present {
        value["transcript_deleted"] = json!(true);
    }
    if account.is_some_and(|account| account.hidden) {
        value["account_hidden"] = json!(true);
    }
    value
}

/// Whether `row` is running now.
pub(crate) fn running(row: &SessionRow) -> bool {
    row.running_until()
        .is_some_and(|until| until > Instant::now())
}

/// A session on one line, as a sentence says it: `"Title" · Codex · ChatGPT
/// Pro · ~/work/atlas on hillshade · today 11:02 AM to 12:47 PM, ended ·
/// 1.2M tokens`.
fn line(server: &Server, row: &SessionRow, accounts: &[AccountLimits]) -> String {
    let mut parts = vec![title(row), row.key.agent().name().to_owned()];
    if let Some(account) = accounts::of_session(row, accounts).filter(|account| !account.hidden) {
        parts.push(accounts::name(account));
    }
    if let Some(place) = place(server, row) {
        parts.push(place);
    }
    if let Some(when) = when(server, row) {
        parts.push(when);
    }
    let tokens = row.with_subagents.tokens.total();
    if tokens > 0 {
        parts.push(format!("{} tokens", prose::tokens(tokens)));
    }
    parts.join(" \u{b7} ")
}

/// A session's title in quotes, or its id without one.
pub(crate) fn title(row: &SessionRow) -> String {
    match &row.title {
        Some(title) => format!("\"{}\"", prose::line(title, LONGEST_TITLE)),
        None => row.key.to_string(),
    }
}

/// Where a session ran: its folder, from `~`, and on which branch.
pub(crate) fn place(server: &Server, row: &SessionRow) -> Option<String> {
    let folder = row.cwd.as_deref().map(|cwd| server.folder_said(cwd));
    match (folder, &row.branch) {
        (Some(folder), Some(branch)) => Some(format!("{folder} on {branch}")),
        (Some(folder), None) => Some(folder),
        (None, Some(branch)) => Some(format!("branch {branch}")),
        (None, None) => None,
    }
}

/// When a session ran, and whether it still is: `today 11:02 AM to 12:47
/// PM, ended`, or `since 9:40 AM, running`.
fn when(server: &Server, row: &SessionRow) -> Option<String> {
    let now = Instant::now();
    let start = row.started.or(row.active)?;
    let from = format!(
        "{} {}",
        prose::day(start, now, &server.zone),
        prose::hour(start, &server.zone)
    );
    if running(row) {
        return Some(format!("{from}, running now"));
    }
    let until = row.active.filter(|active| *active > start);
    Some(match until {
        Some(until) => {
            let same_day =
                prose::day(until, now, &server.zone) == prose::day(start, now, &server.zone);
            // Its day said too when it ended on another, as `yesterday 2:28
            // PM to today 3:26 AM`.
            let end = if same_day {
                prose::hour(until, &server.zone)
            } else {
                format!(
                    "{} {}",
                    prose::day(until, now, &server.zone),
                    prose::hour(until, &server.zone)
                )
            };
            // Within the minute it began, it began and ended at once.
            if end == prose::hour(start, &server.zone) {
                format!("{from}, ended")
            } else {
                format!("{from} to {end}, ended")
            }
        }
        None => format!("{from}, ended"),
    })
}

/// The sessions `keys` name, subagents among them, with their usage for all
/// time: none for none.
pub(crate) fn rows(server: &Server, keys: Vec<SessionKey>) -> Result<Vec<SessionRow>, Failure> {
    // A question naming no sessions would admit every one.
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let limit = keys.len();
    let question = SessionQuery {
        filter: Filter {
            sessions: keys,
            ..Filter::default()
        },
        subagents: true,
        empty: true,
        limit,
        ..SessionQuery::default()
    };
    Ok(server.engine.sessions(&question)?.items)
}

/// The session `id` names, as answers give ids: `agent:id`.
fn session(id: &str) -> Result<SessionKey, Failure> {
    SessionKey::parse(id)
        .filter(|key| !key.native().is_empty())
        .ok_or_else(|| {
            Failure(format!(
                "{id:?} is not a session id; ids look like claude-code:0f6e3f6a-713c-4bad-8f6d-f04fe41bbd84, \
                 as find_sessions gives them."
            ))
        })
}

/// The session `id` names, with its usage for all time.
pub(crate) fn named(server: &Server, id: &str) -> Result<SessionRow, Failure> {
    let key = session(id)?;
    rows(server, vec![key])?.into_iter().next().ok_or_else(|| {
        Failure(format!(
            "No session has the id {id:?}; find_sessions gives ids."
        ))
    })
}

/// The latest session, subagents apart, in the project `folder` stands
/// for, from any agent, passing over `except`, the caller's own, which an
/// agent looking for work to pick up doesn't mean.
pub(crate) fn latest_in(
    server: &Server,
    folder: &str,
    except: Option<&SessionKey>,
) -> Result<Option<SessionRow>, Failure> {
    let page = server.engine.sessions(&SessionQuery {
        filter: Filter {
            projects: tools::folder(server, Some(folder))?,
            ..Filter::default()
        },
        limit: 2,
        ..SessionQuery::default()
    })?;
    Ok(page.items.into_iter().find(|row| Some(&row.key) != except))
}

/// This session: the caller's agent's latest session in the caller's
/// folder, kept in `kept_in`, the folder its agent keeps its history in,
/// or, of those running there, the one the agent said it started the
/// server in. So a second account's agent, pointed at a folder of its own,
/// isn't given the first's session in the same project. `None` when who is
/// asking, where, or where its agent keeps its history, isn't known.
pub(crate) fn this(
    server: &Server,
    kept_in: Option<&Folder>,
) -> Result<Option<SessionRow>, Failure> {
    let (Some(agent), Some(folder), Some(kept_in)) = (
        server.caller.agent,
        server.caller.folder.as_deref(),
        kept_in,
    ) else {
        return Ok(None);
    };
    let page = server.engine.sessions(&SessionQuery {
        filter: Filter {
            projects: tools::folder(server, Some(folder))?,
            agents: vec![agent],
            ..Filter::default()
        },
        limit: 20,
        ..SessionQuery::default()
    })?;
    // A session in the agent's own folder has none named.
    let kept = |row: &&SessionRow| match &row.folder {
        None => kept_in.origin == FolderOrigin::Own,
        Some(path) => *path == kept_in.path,
    };
    let theirs: Vec<&SessionRow> = page.items.iter().filter(kept).collect();
    let said = theirs
        .iter()
        .find(|row| running(row) && server.caller.session.as_deref() == Some(row.key.native()));
    Ok(said.or(theirs.first()).map(|row| (*row).clone()))
}
