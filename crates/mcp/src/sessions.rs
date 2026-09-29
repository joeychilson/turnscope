//! Sessions: finding them (`find_sessions`), and reading one
//! (`read_session`).
//!
//! **Finding.** Sessions are found by where they ran, by agent or account,
//! by period, by whether they are running now, and by words said in them.
//! Without words they come most recently active first, or by what they
//! used; with words, by the latest mention, each with the passage that
//! matched, read from its conversation now. Asked of an account, the
//! sessions any of whose usage drew on it, their subagents' counting, as
//! the engine keeps each response to the account signed in where and when
//! it was made; each is given as the account most of its responses drew on
//! ([`crate::accounts::of_session`]). "This session" is the caller's agent's latest in the caller's folder,
//! kept where the caller's agent keeps its history ([`this`]).
//!
//! **Reading.** Every answer fits what an agent takes from a tool, 25,000
//! tokens by default in Claude Code. Full detail at the default 50 entries
//! came to 52,000 to 83,000 characters on large sessions here, and 226,000
//! to 241,000 at 200 (2026-09-26), so a page stops adding entries at
//! [`LONGEST_PAGE`] bytes, always holding one, and says where to continue.
//! A long entry is cut short on a page, never left out, and each cut says
//! how many characters follow and the `entry`, `part` and `from` that read
//! them. Given `entry`, `read_session` answers with a slice of that part
//! instead, in Unicode code points so a slice never splits one, and slices
//! read in turn give the part exactly, nothing missed or repeated. Each is
//! read from the conversation as it then stands, so a caller reading a
//! session still running sees from the part's length whether it changed
//! between slices.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};
use turnscope_engine::{
    AccountLimits, Entry, Filter, Folder, FolderOrigin, Instant, RUNNING, SearchQuery, SessionKey,
    SessionOrder, SessionQuery, SessionRow, Speaker, ToolCall,
};

use crate::accounts;
use crate::prose;
use crate::time;
use crate::tools::{
    self, Answer, Failure, Reply, Server, account_schema, agent_schema, folder_schema,
    moment_schema, object, shape, totals_schema,
};
use crate::usage;

/// The most characters of one entry's text a page shows.
const LONGEST_TEXT: usize = 4_000;

/// The most characters of a tool call's arguments a page shows on the call's
/// one line.
const LONGEST_INPUT: usize = 300;

/// The most characters of a tool call's arguments, and of its result, a full
/// page shows. At full detail what a call was given, such as an edit's text,
/// matters as much as what it returned.
const LONGEST_OUTPUT: usize = 2_000;

/// The most text a page's entries hold, in bytes, before it stops adding
/// entries, as read_session's description says: about 40,000 characters of
/// English, within the 25,000 tokens Claude Code takes from a tool by
/// default. Bytes rather than characters, since a script that takes more
/// tokens a character, such as Chinese, takes more bytes a character too.
/// A slice of one entry holds as much, as its answer writes it.
const LONGEST_PAGE: usize = 40_000;

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
            "usage": totals_schema(),
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

/// The schema of a session's id.
fn session_schema() -> Value {
    json!({
        "type": "string",
        "description": "The session's id, such as claude-code:0f6e3f6a-713c-4bad-8f6d-f04fe41bbd84, from find_sessions.",
    })
}

/// The session `id` names, with its usage for all time.
pub(crate) fn named(server: &Server, id: &str) -> Result<SessionRow, Failure> {
    let key = tools::session(id)?;
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

/// How much of a conversation a page shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Detail {
    /// What the person and the models said.
    Conversation,
    /// That, and each tool call on one line.
    Actions,
    /// Also the models' thinking, each tool's result, and what the agent
    /// injected.
    Full,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadSession {
    session: String,
    detail: Option<String>,
    find: Option<String>,
    offset: Option<i64>,
    limit: Option<u32>,
    entry: Option<u32>,
    part: Option<String>,
    from: Option<u64>,
}

pub(crate) fn read_schema() -> Value {
    object(
        json!({
            "session": session_schema(),
            "detail": {
                "type": "string",
                "enum": ["conversation", "actions", "full"],
                "description": "conversation: only what the person and the models said. actions (the default): that, and each tool call on one line. full: also the models' thinking, each tool's result, and what the agent injected.",
            },
            "find": {
                "type": "string",
                "description": "Only the entries that contain this, ignoring case, in what the chosen detail shows. The first line lists every entry that does.",
            },
            "offset": {
                "type": "integer",
                "description": "The entry to start at, by its number, or back from the end when negative: -20 shows the last twenty entries this page would show.",
            },
            "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "How many entries to show, 50 unless given."},
            "entry": {
                "type": "integer",
                "minimum": 0,
                "description": "Instead of a page, read this entry's content exactly, by its number, a slice of about 40 KB of text at a time: for what a page cut short, which says where its rest starts. Answers JSON: text is the slice, characters the whole part's length, and while truncated, next_from is where the next slice starts.",
            },
            "part": {
                "type": "string",
                "enum": ["text", "input", "output"],
                "description": "With entry: text (the default) for what was said, thought or injected, or a tool call's input or output.",
            },
            "from": {
                "type": "integer",
                "minimum": 0,
                "description": "With entry: the character to start at, 0 unless given. Characters are Unicode code points.",
            },
        }),
        &["session"],
    )
}

pub(crate) fn read(server: &Server, arguments: ReadSession) -> Answer {
    if let Some(entry) = arguments.entry {
        return read_entry(server, arguments, entry);
    }
    if arguments.part.is_some() || arguments.from.is_some() {
        return Err(Failure(
            "part and from read within one entry, so they come with entry.".into(),
        ));
    }
    let detail = match arguments.detail.as_deref() {
        None | Some("actions") => Detail::Actions,
        Some("conversation") => Detail::Conversation,
        Some("full") => Detail::Full,
        Some(other) => {
            return Err(Failure(format!(
                "detail takes conversation, actions or full; {other:?} is none of those."
            )));
        }
    };
    let limit = tools::limit(arguments.limit, 50, 200)?;
    let row = named(server, &arguments.session)?;
    let entries = server.engine.conversation(&row.key)?;
    let find = arguments
        .find
        .map(|find| find.trim().to_lowercase())
        .filter(|find| !find.is_empty());
    let shown: Vec<&Entry> = entries
        .iter()
        .filter(|entry| shows(entry, detail))
        .filter(|entry| {
            find.as_deref()
                .is_none_or(|find| searched(entry, detail).to_lowercase().contains(find))
        })
        .collect();
    let offset = arguments.offset.unwrap_or(0);
    let from = if offset < 0 {
        // Back over what the page would show, not over every entry.
        let back = usize::try_from(offset.unsigned_abs()).unwrap_or(usize::MAX);
        shown.len().saturating_sub(back)
    } else {
        shown
            .iter()
            .position(|entry| i64::from(entry.index) >= offset)
            .unwrap_or(shown.len())
    };
    let page = paged(&shown[from..], limit, |entry| {
        written(entry, detail, server)
    });
    let end = from + page.len();

    let mut lines = vec![heading(&row, entries.len(), find.as_deref(), &shown)];
    match (page.first(), page.last()) {
        (Some((first, _)), Some((last, _))) => {
            lines.push(format!("Entries {first} to {last}."));
        }
        _ if shown.is_empty() => lines.push("Nothing to show.".to_owned()),
        _ => lines.push(format!("Nothing to show from entry {offset}.")),
    }
    for (_, text) in page {
        lines.push(String::new());
        lines.push(text);
    }
    if let Some(next) = shown.get(end) {
        let after = shown.len() - end;
        lines.push(String::new());
        lines.push(format!(
            "{after} more to show; continue with offset {}.",
            next.index
        ));
    }
    Ok(Reply::Text(lines.join("\n")))
}

/// The first of `entries` a page holds, by number and as `write` writes
/// them: at most `limit`, and no more once their text would pass
/// [`LONGEST_PAGE`], but always the first, however long, so that paging
/// moves on.
fn paged(entries: &[&Entry], limit: usize, write: impl Fn(&Entry) -> String) -> Vec<(u32, String)> {
    let mut page: Vec<(u32, String)> = Vec::new();
    let mut length = 0;
    for entry in entries.iter().take(limit) {
        let text = write(entry);
        if !page.is_empty() && length + text.len() > LONGEST_PAGE {
            break;
        }
        length += text.len();
        page.push((entry.index, text));
    }
    page
}

/// The first line of a page: the session, how long its conversation is, and
/// which entries mention what was looked for.
fn heading(row: &SessionRow, entries: usize, find: Option<&str>, shown: &[&Entry]) -> String {
    let title = row
        .title
        .as_deref()
        .map_or_else(String::new, |title| format!(" \u{b7} {title}"));
    let mut heading = format!("{}{title} \u{b7} {entries} entries", row.key);
    if let Some(find) = find {
        let numbers: Vec<String> = shown
            .iter()
            .take(100)
            .map(|entry| entry.index.to_string())
            .collect();
        let more = if shown.len() > numbers.len() {
            ", \u{2026}"
        } else {
            ""
        };
        heading.push_str(&format!(
            " \u{b7} {} mention {find:?}: {}{more}",
            shown.len(),
            numbers.join(", ")
        ));
    }
    heading
}

/// Whether a page at `detail` shows `entry`.
fn shows(entry: &Entry, detail: Detail) -> bool {
    match entry.speaker {
        Speaker::User | Speaker::Assistant => true,
        Speaker::Tool => detail >= Detail::Actions,
        Speaker::Reasoning | Speaker::System => detail == Detail::Full,
    }
}

/// What `find` looks through in `entry`: what a page at `detail` shows of it.
fn searched(entry: &Entry, detail: Detail) -> String {
    match &entry.tool {
        Some(tool) if detail == Detail::Full => format!(
            "{} {} {}",
            tool.name,
            tool.input,
            tool.output.as_deref().unwrap_or_default()
        ),
        Some(tool) => format!("{} {}", tool.name, tool.input),
        None => entry.text.clone(),
    }
}

/// How a call ended, after an arrow: failed, with no result recorded, or,
/// for a call that started a subagent, which, so that it can be read in turn.
fn outcome(tool: &ToolCall) -> String {
    let mut said = match (&tool.output, tool.failed) {
        (_, true) => " \u{2192} failed".to_owned(),
        (None, false) => " \u{2192} no result recorded".to_owned(),
        (Some(_), false) => String::new(),
    };
    if let Some(subagent) = &tool.subagent {
        said.push_str(&format!(" \u{2192} started subagent {subagent}"));
    }
    said
}

/// One entry of a page, as `detail` shows it.
fn written(entry: &Entry, detail: Detail, server: &Server) -> String {
    let when = entry.at.map_or_else(String::new, |at| {
        format!(" \u{b7} {}", time::short(at, &server.zone))
    });
    let index = entry.index;
    match (&entry.tool, entry.speaker) {
        (Some(tool), _) => {
            let status = outcome(tool);
            let most = if detail == Detail::Full {
                LONGEST_OUTPUT
            } else {
                LONGEST_INPUT
            };
            let mut line = format!(
                "[{index}] tool {}{when}: {}{status}",
                tool.name,
                clipped(&tool.input, most, index, Part::Input)
            );
            if detail == Detail::Full
                && let Some(output) = &tool.output
            {
                line.push('\n');
                line.push_str(&clipped(output, LONGEST_OUTPUT, index, Part::Output));
            }
            line
        }
        (None, speaker) => {
            let who = match speaker {
                Speaker::User => "user".to_owned(),
                Speaker::Assistant => entry.model.as_deref().map_or_else(
                    || "assistant".to_owned(),
                    |model| format!("assistant ({model})"),
                ),
                Speaker::Reasoning => "thinking".to_owned(),
                Speaker::System => "injected by the agent".to_owned(),
                Speaker::Tool => "tool".to_owned(),
            };
            format!(
                "[{index}] {who}{when}\n{}",
                clipped(&entry.text, LONGEST_TEXT, index, Part::Text)
            )
        }
    }
}

/// `text` trimmed and cut to `most` characters, with a note of how many
/// follow and how read_session reads them: as entry `index`'s `part`, from
/// the character the cut falls at in `text` as recorded, before trimming.
pub(crate) fn clipped(text: &str, most: usize, index: u32, part: Part) -> String {
    let trimmed = text.trim();
    let Some((cut, _)) = trimmed.char_indices().nth(most) else {
        return trimmed.to_owned();
    };
    // Where `trimmed` starts in `text`, in bytes.
    let start = text.len() - text.trim_start().len();
    let from = text[..start].chars().count() + most;
    let more = text[start + cut..].chars().count();
    let part = match part {
        Part::Text => String::new(),
        part => format!(", \"part\": \"{}\"", part.key()),
    };
    format!(
        "{}\u{2026} ({more} more characters; read_session reads them with \
         {{\"entry\": {index}{part}, \"from\": {from}}})",
        &trimmed[..cut]
    )
}

/// A part of an entry, which read_session reads exactly, a slice at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Part {
    /// What was said, thought or injected.
    Text,
    /// What a tool call was given.
    Input,
    /// What a tool call returned.
    Output,
}

impl Part {
    /// Its name, as read_session's part takes it.
    fn key(self) -> &'static str {
        match self {
            Part::Text => "text",
            Part::Input => "input",
            Part::Output => "output",
        }
    }
}

/// A slice of one entry's `part`, from the character `from`, as read_session
/// reads it given `entry`.
fn read_entry(server: &Server, arguments: ReadSession, index: u32) -> Answer {
    if arguments.detail.is_some()
        || arguments.find.is_some()
        || arguments.offset.is_some()
        || arguments.limit.is_some()
    {
        return Err(Failure(
            "entry reads one entry's content; detail, find, offset and limit choose a page's \
             entries, so they don't come with it."
                .into(),
        ));
    }
    let part = match arguments.part.as_deref() {
        None | Some("text") => Part::Text,
        Some("input") => Part::Input,
        Some("output") => Part::Output,
        Some(other) => {
            return Err(Failure(format!(
                "part takes text, input or output; {other:?} is none of those."
            )));
        }
    };
    let row = named(server, &arguments.session)?;
    let entries = server.engine.conversation(&row.key)?;
    let entry = entries
        .iter()
        .find(|entry| entry.index == index)
        .ok_or_else(|| {
            Failure(format!(
                "{} has {} entries, numbered from 0; there is no entry {index}.",
                row.key,
                entries.len()
            ))
        })?;
    let content = match (part, &entry.tool) {
        (Part::Text, None) => &entry.text,
        (Part::Input, Some(tool)) => &tool.input,
        (
            Part::Output,
            Some(ToolCall {
                output: Some(output),
                ..
            }),
        ) => output,
        (Part::Output, Some(_)) => {
            return Err(Failure(format!(
                "Entry {index} is a tool call with no result recorded."
            )));
        }
        (Part::Text, Some(_)) => {
            return Err(Failure(format!(
                "Entry {index} is a tool call, whose parts are input and output."
            )));
        }
        (Part::Input | Part::Output, None) => {
            return Err(Failure(format!(
                "Entry {index} is not a tool call, so its one part is text."
            )));
        }
    };
    let characters = content.chars().count();
    let from = arguments.from.unwrap_or(0);
    let start = usize::try_from(from)
        .ok()
        .filter(|start| *start <= characters)
        .ok_or_else(|| {
            Failure(format!(
                "Entry {index}'s {} has {characters} characters; from {from} is past its end.",
                part.key()
            ))
        })?;
    let (text, to) = slice(content, start);
    let speaker = match entry.speaker {
        Speaker::User => "user",
        Speaker::Assistant => "assistant",
        Speaker::Reasoning => "thinking",
        Speaker::System => "injected",
        Speaker::Tool => "tool",
    };
    let mut answer = json!({
        "session": row.key.to_string(),
        "entry": index,
        "speaker": speaker,
        "part": part.key(),
        "characters": characters,
        "from": start,
        "to": to,
        "truncated": to < characters,
        "next_from": (to < characters).then_some(to),
        "text": text,
    });
    if let Some(tool) = &entry.tool {
        answer["tool"] = json!(tool.name);
    }
    Ok(Reply::Text(answer.to_string()))
}

/// Of `content`, the characters from the `from`th on that [`LONGEST_PAGE`]
/// bytes hold as JSON writes them, and the number of the character after the
/// last. A slice holds at least one, since JSON writes none in more than six
/// bytes, so reading moves on. Characters are Unicode code points, so a
/// slice never splits one's UTF-8. `from` is at most how many `content` has.
fn slice(content: &str, from: usize) -> (&str, usize) {
    let start = content
        .char_indices()
        .nth(from)
        .map_or(content.len(), |(at, _)| at);
    let (mut end, mut to, mut written) = (start, from, 0);
    for character in content[start..].chars() {
        written += written_length(character);
        if written > LONGEST_PAGE {
            break;
        }
        end += character.len_utf8();
        to += 1;
    }
    (&content[start..end], to)
}

/// How many bytes JSON writes `character` in, in a string: two for a
/// quotation mark, a backslash, or a control character with a short escape,
/// six for any other control character, and otherwise its UTF-8.
fn written_length(character: char) -> usize {
    match character {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        '\0'..='\u{1f}' => 6,
        _ => character.len_utf8(),
    }
}

#[cfg(test)]
mod tests {
    use turnscope_engine::{Agent, Entry, SessionKey, Speaker, ToolCall};

    use super::{LONGEST_PAGE, Part, clipped, outcome, paged, slice, written_length};

    #[test]
    fn a_call_that_started_a_subagent_says_which() {
        let call = ToolCall {
            name: "Agent".to_owned(),
            input: "{}".to_owned(),
            output: Some("Done.".to_owned()),
            failed: false,
            subagent: Some(SessionKey::new(Agent::ClaudeCode, "a1")),
        };
        assert_eq!(outcome(&call), " \u{2192} started subagent claude-code:a1");
        let failed = ToolCall {
            failed: true,
            subagent: None,
            ..call
        };
        assert_eq!(outcome(&failed), " \u{2192} failed");
    }

    #[test]
    fn clipping_counts_characters_and_says_where_the_rest_starts() {
        assert_eq!(clipped("  café  ", 10, 0, Part::Text), "café");
        // "héllo wörld" is 11 characters after the two spaces before it. Cut
        // at 5, it goes on from character 2 + 5 = 7 of the text as recorded,
        // " wörld  ": 8 characters, counting the spaces after it.
        assert_eq!(
            clipped("  héllo wörld  ", 5, 3, Part::Text),
            "héllo\u{2026} (8 more characters; read_session reads them with \
             {\"entry\": 3, \"from\": 7})"
        );
        assert_eq!(
            clipped("abcdef", 4, 9, Part::Output),
            "abcd\u{2026} (2 more characters; read_session reads them with \
             {\"entry\": 9, \"part\": \"output\", \"from\": 4})"
        );
    }

    #[test]
    fn a_character_takes_the_bytes_json_writes_it_in() {
        let characters = (0..0x80)
            .filter_map(char::from_u32)
            .chain(['é', '日', '🙂']);
        for character in characters {
            // serde_json writes the string between two quotation marks.
            let written = serde_json::to_string(&character.to_string()).unwrap().len() - 2;
            assert_eq!(written_length(character), written, "{character:?}");
        }
    }

    #[test]
    fn a_slice_holds_what_fits_in_a_page_as_json_writes_it() {
        // 39,999 bytes of a, and é's 2 would make 40,001: the first slice
        // stops before é, and the next holds é and b.
        let text = format!("{}éb", "a".repeat(LONGEST_PAGE - 1));
        assert_eq!(
            slice(&text, 0),
            (&text[..LONGEST_PAGE - 1], LONGEST_PAGE - 1)
        );
        assert_eq!(slice(&text, LONGEST_PAGE - 1), ("éb", LONGEST_PAGE + 1));
        assert_eq!(slice(&text, LONGEST_PAGE + 1), ("", LONGEST_PAGE + 1));
        // JSON writes a quotation mark in 2 bytes, so 20,000 fill a page; a
        // control character without a short escape in 6, so 6,666 do
        // (39,996 bytes), and not 6,667 (40,002).
        let quotes = "\"".repeat(20_001);
        assert_eq!(slice(&quotes, 0).1, 20_000);
        let controls = "\u{1}".repeat(7_000);
        assert_eq!(slice(&controls, 0).1, 6_666);
    }

    #[test]
    fn a_page_stops_short_of_its_budget_but_holds_at_least_one_entry() {
        let entries: Vec<Entry> = (0..10)
            .map(|index| Entry {
                index,
                speaker: Speaker::User,
                at: None,
                model: None,
                text: String::new(),
                tool: None,
            })
            .collect();
        let entries: Vec<&Entry> = entries.iter().collect();
        let numbers = |page: Vec<(u32, String)>| -> Vec<u32> {
            page.into_iter().map(|(index, _)| index).collect()
        };
        // Two entries of 15,000 bytes come to 30,000; a third would pass
        // 40,000.
        let page = paged(&entries, 50, |_| "x".repeat(15_000));
        assert_eq!(numbers(page), [0, 1]);
        let page = paged(&entries[4..], 50, |_| "é".repeat(30_000));
        assert_eq!(numbers(page), [4]);
        let page = paged(&entries, 3, |_| "x".to_owned());
        assert_eq!(numbers(page), [0, 1, 2]);
    }
}
