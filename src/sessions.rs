//! Sessions: finding them, showing one, and reading its conversation. Handing
//! one to another agent is `handoff`'s.
//!
//! A list shows each session at the top of its tree, its subagents' usage
//! counted in, most recently active first, a page at a time. A continuation
//! is a session of its own. Conversations aren't kept: they're read from the
//! agent's files when asked, with secrets redacted.

use std::cmp::Reverse;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::agents::{self, Entry, Kind, Tokens};
use crate::redact::redact;
use crate::time::{self, MINUTE};
use crate::usage::count;
use crate::{Error, Result};

mod handoff;
pub use handoff::handoff;

/// How follow-ups are named: as a command for a person, as a tool call for
/// an agent.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Cite {
    Command,
    Tool,
}

/// How recently a session, or one within it, was active to be running.
const RUNNING: i64 = 5 * MINUTE;

/// About how much text a page of a conversation holds, 3,000 tokens: past
/// it the page ends, after one entry at least.
const PAGE: usize = 12_000;

/// The sessions at the top of their trees, each with the sessions in its
/// tree: subagents and forks, of any depth. One whose parent was never read
/// is a top of its own.
///
/// In both trees, `+s.link` keeps SQLite finding children by
/// `session_parent`. Otherwise it builds an index on `link` at every step:
/// 150 ms for TREES over 1,932 sessions, against 2 ms (October 2026).
pub(crate) const TREES: &str = "
WITH RECURSIVE tree (root, id) AS (
    SELECT id, id FROM session s
    WHERE parent IS NULL OR link IN ('continuation', 'fork') OR NOT EXISTS (SELECT 1 FROM session p WHERE p.id = s.parent)
    UNION ALL
    SELECT tree.root, s.id FROM session s JOIN tree ON s.parent = tree.id
    WHERE +s.link = 'subagent'
)";

/// The session `?1` and its subagents, theirs, and so on, as `tree (id)`.
const SUBTREE: &str = "
WITH RECURSIVE tree (id) AS (
    SELECT ?1 UNION ALL
    SELECT s.id FROM session s JOIN tree ON s.parent = tree.id WHERE +s.link = 'subagent'
)";

#[derive(Default)]
pub struct Query {
    /// All these words said in it.
    pub search: Option<String>,
    /// Of this folder's project, or run in or under it.
    pub folder: Option<PathBuf>,
    pub agent: Option<String>,
    /// Active since.
    pub since: Option<i64>,
    pub running: bool,
    /// How many, at most 100.
    pub count: usize,
    /// The last session of the page before.
    pub cursor: Option<String>,
    /// A session to leave out: the caller's own.
    pub except: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub sessions: Vec<Summary>,
    /// How many were found in all.
    pub found: usize,
    /// The cursor for the next page.
    pub next: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub id: String,
    pub agent: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub project: Option<String>,
    pub branch: Option<String>,
    pub started: Option<i64>,
    /// Its latest activity, or that of a session within it.
    pub active: i64,
    pub running: bool,
    /// How many subagents it ran.
    pub subagents: u64,
    pub responses: u64,
    pub tokens: Tokens,
    pub cost_usd: f64,
    pub unpriced: u64,
    /// Found by words: how many of its messages, and its subagents', hold
    /// them all, which tells one that's about them from one where they're
    /// apart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mentions: Option<u64>,
    /// Found by words, among the first on a page: the latest message of its
    /// own that holds them all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub said: Option<Said>,
}

/// A message that holds the words searched for: the subagent's session it's
/// in, when it isn't the one listed, its entry, who said it, and the text
/// around them.
#[derive(Serialize, Clone)]
pub struct Said {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
    pub entry: usize,
    pub kind: Kind,
    pub text: String,
}

/// How many of a page's first sessions found by words show the message that
/// holds them: each is read from its agent's files.
const SAYING: usize = 3;

/// Sessions as `query` asks, most recently active first.
pub fn find(db: &Connection, home: &Path, query: &Query, now: i64) -> Result<Page> {
    if let Some(agent) = &query.agent {
        agents::find(agent)?;
    }
    let project = query
        .folder
        .as_ref()
        .map(|folder| crate::ingest::project(&folder.to_string_lossy(), home));
    let folder = query
        .folder
        .as_ref()
        .map(|folder| folder.to_string_lossy().trim_end_matches('/').to_owned());
    // Each word as a phrase, so punctuation can't make a query of it.
    let words: Vec<String> = query
        .search
        .iter()
        .flat_map(|search| search.split_whitespace())
        .map(|word| word.replace('"', ""))
        // A word of punctuation alone isn't indexed, so it would match
        // nothing.
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .map(|word| format!("\"{word}\""))
        .collect();
    // A session matches when it said every word, in any of its messages.
    let mut matched: Option<std::collections::HashSet<String>> = None;
    let mut found = db.prepare("SELECT DISTINCT session FROM said WHERE said MATCH ?1")?;
    for word in &words {
        let these: std::collections::HashSet<String> = found
            .query_map([word], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        matched = Some(match matched {
            Some(was) => was.intersection(&these).cloned().collect(),
            None => these,
        });
    }
    let together: std::collections::HashMap<String, u64> = match words.is_empty() {
        true => Default::default(),
        false => db
            .prepare("SELECT session, count(*) FROM said WHERE said MATCH ?1 GROUP BY session")?
            .query_map([words.join(" AND ")], |row| {
                Ok((row.get(0)?, row.get::<_, i64>(1)? as u64))
            })?
            .collect::<rusqlite::Result<_>>()?,
    };
    let mut statement = db.prepare(&format!(
        "{TREES}
         SELECT r.id, r.agent, r.title, r.cwd, r.project, r.branch, r.started, max(s.last), count(*) - 1,
             group_concat(s.id, char(31))
         FROM tree JOIN session r ON r.id = tree.root JOIN session s ON s.id = tree.id
         WHERE (?1 IS NULL OR r.agent = ?1)
           AND (?2 IS NULL OR r.project = ?2 OR r.cwd = ?3
                OR substr(r.cwd, 1, length(?3) + 1) = ?3 || '/')
         GROUP BY r.id HAVING max(s.last) IS NOT NULL AND max(s.last) >= ?4
             -- One opened and left, that asked for nothing, isn't one to find.
             AND (r.title IS NOT NULL OR EXISTS (SELECT 1 FROM response x WHERE x.session = r.id))
         ORDER BY max(s.last) DESC, r.id"
    ))?;
    let since = match query.running {
        true => query.since.unwrap_or(0).max(now - RUNNING),
        false => query.since.unwrap_or(0),
    };
    let mut rows = statement.query(params![query.agent, project, folder, since])?;
    let mut found = Vec::new();
    // For each session found by words, the one of its tree that holds them
    // together most: itself where it does as much.
    let mut holding: std::collections::HashMap<String, String> = Default::default();
    while let Some(row) = rows.next()? {
        let tree: String = row.get(9)?;
        if let Some(matched) = &matched
            && !tree.split('\u{1f}').any(|id| matched.contains(id))
        {
            continue;
        }
        let mentions = matched
            .as_ref()
            .map(|_| tree.split('\u{1f}').filter_map(|id| together.get(id)).sum());
        let id: String = row.get(0)?;
        if query.except.as_ref() == Some(&id) {
            continue;
        }
        let most = tree
            .split('\u{1f}')
            .filter_map(|held| Some((*together.get(held)?, held == id, held)))
            .max_by_key(|(count, own, _)| (*count, *own));
        if let Some((_, _, held)) = most {
            holding.insert(id.clone(), held.to_owned());
        }
        let active: i64 = row.get(7)?;
        found.push(Summary {
            id,
            agent: row.get(1)?,
            title: row.get(2)?,
            cwd: row.get(3)?,
            project: row.get(4)?,
            branch: row.get(5)?,
            started: row.get(6)?,
            active,
            running: now - active <= RUNNING,
            subagents: row.get::<_, i64>(8)? as u64,
            responses: 0,
            tokens: Tokens::default(),
            cost_usd: 0.0,
            unpriced: 0,
            mentions,
            said: None,
        });
    }
    // Found by words, those with them together most often first.
    if !words.is_empty() {
        found.sort_by_key(|session| Reverse(session.mentions));
    }
    let total = found.len();
    // A page starts after the session the page before ended at.
    let start = match &query.cursor {
        Some(cursor) => {
            found
                .iter()
                .position(|session| &session.id == cursor)
                .ok_or_else(|| {
                    Error::Usage(format!(
                        "the cursor {cursor} isn't among these sessions: ask again without it"
                    ))
                })?
                + 1
        }
        None => 0,
    };
    let most = query.count.clamp(1, 100);
    let mut sessions: Vec<Summary> = found.into_iter().skip(start).take(most + 1).collect();
    let next = (sessions.len() > most).then(|| {
        sessions.truncate(most);
        sessions.last().map(|session| session.id.clone())
    });
    for session in &mut sessions {
        used(db, session, now)?;
    }
    let wanted: Vec<String> = words
        .iter()
        .map(|word| word.trim_matches('"').to_lowercase())
        .collect();
    for session in sessions
        .iter_mut()
        .filter(|session| session.mentions.is_some_and(|mentions| mentions > 0))
        .take(SAYING)
    {
        let held = holding.get(&session.id).unwrap_or(&session.id);
        session.said = said(db, held, &wanted).map(|said| Said {
            subagent: (held != &session.id).then(|| held.clone()),
            ..said
        });
    }
    Ok(Page {
        sessions,
        found: total,
        next: next.flatten(),
    })
}

/// Add up what a session and the sessions in its tree used.
fn used(db: &Connection, session: &mut Summary, now: i64) -> Result<()> {
    let mut statement = db.prepare_cached(&format!(
        "{SUBTREE}
         SELECT count(*), sum(input), sum(cache_read), sum(cache_write_5m), sum(cache_write_1h), sum(output),
             sum(reasoning), total(cost), count(*) - count(cost),
             (SELECT count(*) - 1 FROM tree), (SELECT max(last) FROM session WHERE id IN tree)
         FROM tree CROSS JOIN response ON response.session = tree.id"
    ))?;
    statement.query_row([&session.id], |row| {
        let count = |at: usize| {
            row.get::<_, Option<i64>>(at)
                .map(|count| count.unwrap_or(0) as u64)
        };
        session.responses = count(0)?;
        session.tokens = Tokens {
            input: count(1)?,
            cache_read: count(2)?,
            cache_write_5m: count(3)?,
            cache_write_1h: count(4)?,
            output: count(5)?,
            reasoning: count(6)?,
        };
        session.cost_usd = row.get(7)?;
        session.unpriced = count(8)?;
        session.subagents = count(9)?;
        // Active as its latest subagent was.
        if let Some(last) = row.get::<_, Option<i64>>(10)? {
            session.active = session.active.max(last);
            session.running = now - session.active <= RUNNING;
        }
        Ok(())
    })?;
    Ok(())
}

/// One session in full.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Detail {
    #[serde(flatten)]
    pub summary: Summary,
    pub parent: Option<String>,
    pub models: Vec<String>,
    /// The ids of the accounts it drew on, the most used first.
    pub accounts: Vec<String>,
    /// The same accounts by their titles, for text.
    #[serde(skip)]
    pub account_titles: Vec<String>,
    /// The command that resumes it.
    pub resume: String,
}

/// The title, project and agent of the session `id`, and whether it or a
/// subagent of it runs now: what the app lists of a session that used a
/// limit. `show` adds up the whole tree besides, 140 ms for three large
/// sessions, which the app waited for as an account opened.
pub fn brief(
    db: &Connection,
    id: &str,
    now: i64,
) -> Result<(Option<String>, Option<String>, String, bool)> {
    db.prepare_cached(&format!(
        "{SUBTREE} SELECT title, project, agent, (SELECT max(last) FROM session WHERE id IN tree)
         FROM session WHERE id = ?1"
    ))?
    .query_row([id], |row| {
        let last: Option<i64> = row.get(3)?;
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            last.is_some_and(|last| now - last <= RUNNING),
        ))
    })
    .optional()?
    .ok_or_else(|| missing(id))
}

pub fn show(db: &Connection, home: &Path, id: &str, now: i64) -> Result<Detail> {
    let (agent, native) = parse(id)?;
    let (folder, parent, mut summary) = db
        .query_row(
            "SELECT agent, title, cwd, project, branch, started, last, folder, parent FROM session WHERE id = ?1",
            [id],
            |row| {
            let last: Option<i64> = row.get(6)?;
            let summary = Summary {
                id: id.to_owned(),
                agent: row.get(0)?,
                title: row.get(1)?,
                cwd: row.get(2)?,
                project: row.get(3)?,
                branch: row.get(4)?,
                started: row.get(5)?,
                active: last.unwrap_or_default(),
                running: last.is_some_and(|last| now - last <= RUNNING),
                subagents: 0,
                responses: 0,
                tokens: Tokens::default(),
                cost_usd: 0.0,
                unpriced: 0,
                mentions: None,
                said: None,
            };
            Ok((row.get::<_, String>(7)?, row.get::<_, Option<String>>(8)?, summary))
        },
        )
        .optional()?
        .ok_or_else(|| missing(id))?;
    used(db, &mut summary, now)?;
    let models = db
        .prepare(&format!(
            "{SUBTREE} SELECT model FROM response WHERE session IN tree AND model != ''
             GROUP BY model ORDER BY count(*) DESC"
        ))?
        .query_map([id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let drawn: Vec<(String, String)> = db
        .prepare(&format!(
            "{SUBTREE} SELECT r.account, coalesce(a.title || coalesce(' (' || a.label || ')', ''), r.account)
             FROM response r LEFT JOIN account a ON a.id = r.account
             WHERE r.session IN tree AND r.account IS NOT NULL GROUP BY r.account ORDER BY count(*) DESC"
        ))?
        .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // A subagent can't be resumed on its own: the session it ran in is.
    let (mut root, mut folder, mut cwd) = (id.to_owned(), folder, summary.cwd.clone());
    while let Some((parent, parent_folder, parent_cwd)) = db
        .query_row(
            "SELECT p.id, p.folder, p.cwd FROM session s JOIN session p ON p.id = s.parent
             WHERE s.id = ?1 AND s.link = 'subagent'",
            [&root],
            |row| Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
    {
        (root, folder, cwd) = (parent, parent_folder, parent_cwd);
    }
    let native = root.split_once(':').map_or(native, |(_, native)| native);
    // A session kept in a folder other than its agent's own resumes with
    // the variable that points the agent there.
    let info = agent.info();
    let mut resume = info.resume.replace("{id}", native);
    if let Some(variable) = info.folder_var
        && agent
            .folders(home)
            .first()
            .is_some_and(|own| own.to_string_lossy() != folder)
    {
        resume = format!("{variable}={} {resume}", shell(&folder));
    }
    if let Some(cwd) = &cwd {
        resume = format!("cd {} && {resume}", shell(cwd));
    }
    Ok(Detail {
        summary,
        parent,
        models,
        accounts: drawn.iter().map(|(id, _)| id.clone()).collect(),
        account_titles: drawn.into_iter().map(|(_, title)| title).collect(),
        resume,
    })
}

/// `word` as a shell reads it back: as it is when it's plain, quoted when
/// it holds a space or anything else a shell would act on.
fn shell(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|char| char.is_ascii_alphanumeric() || "/._-~+=:,@%".contains(char));
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// The agent and its own id of the session `id`, `claude-code:0f6e3f6a-713c`.
fn parse(id: &str) -> Result<(&'static dyn agents::Agent, &str)> {
    id.split_once(':')
        .and_then(|(agent, native)| Some((agents::by_id(agent)?, native)))
        .filter(|(_, native)| !native.is_empty())
        .ok_or_else(|| missing(id))
}

/// The error for a session `id` that names none.
fn missing(id: &str) -> Error {
    Error::NotFound(format!(
        "no session is {id}. Session ids look like `claude-code:0f6e3f6a-…`; find_sessions (`turnscope sessions`) lists them."
    ))
}

/// The session `id` names: its whole id, the start of one, or the start of
/// an agent's own id, as long as only one session's matches.
pub fn resolve(db: &Connection, id: &str) -> Result<String> {
    let id = id.trim();
    let exact: Option<String> = db
        .query_row("SELECT id FROM session WHERE id = ?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    if let Some(exact) = exact {
        return Ok(exact);
    }
    if id.len() < 4 {
        return Err(missing(id));
    }
    // A prefix, with the characters LIKE treats as patterns escaped.
    let prefix = id
        .replace('\\', r"\\")
        .replace('%', r"\%")
        .replace('_', r"\_");
    let found: Vec<String> = db
        .prepare(
            "SELECT id FROM session WHERE id LIKE ?1 || '%' ESCAPE '\\'
                 OR substr(id, instr(id, ':') + 1) LIKE ?1 || '%' ESCAPE '\\'
             ORDER BY last DESC LIMIT 6",
        )?
        .query_map([&prefix], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    match found.as_slice() {
        [] => Err(missing(id)),
        [one] => Ok(one.clone()),
        many => Err(Error::Usage(format!(
            "{id} starts more than one session's id, such as {}: give more of it",
            many.iter()
                .take(5)
                .map(|id| format!("`{id}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// A session's conversation, read from its agent's files now.
pub fn transcript(db: &Connection, id: &str) -> Result<Vec<Entry>> {
    let (agent, native) = parse(id)?;
    let mut statement = db.prepare("SELECT f.path FROM session_file s JOIN file f ON f.id = s.file WHERE s.session = ?1 ORDER BY f.path")?;
    // A file its agent has since moved or deleted is left out: Codex moves
    // an archived thread to `archived_sessions/`, which is read as a file of
    // the session too.
    let files: Vec<PathBuf> = statement
        .query_map([id], |row| Ok(PathBuf::from(row.get::<_, String>(0)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|path| path.exists())
        .collect();
    if files.is_empty() {
        return Err(Error::NotFound(format!(
            "{id}'s files are gone: {} deleted or moved them, so its conversation can't be read",
            agent.info().name
        )));
    }
    let mut entries = agent.transcript(native, &files)?;
    for entry in &mut entries {
        entry.text = shown(&entry.text);
        if let Some(tool) = &mut entry.tool {
            tool.input = shown(&tool.input);
            tool.output = tool.output.as_deref().map(shown);
            tool.role = agent.role(&tool.name);
        }
    }
    Ok(entries)
}

/// `text` as it's shown: secrets redacted, and without the escape codes
/// that color a terminal, which only take up room.
fn shown(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(char) = chars.next() {
        if char == '\u{1b}' {
            // `ESC [`, parameters, and the letter that ends it.
            if chars.next() == Some('[') {
                for code in chars.by_ref() {
                    if code.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            plain.push(char);
        }
    }
    redact(&plain)
}

/// The latest session in `folder` other than `except`, as a handoff
/// continues from, and the two before it. The latest can be a quick
/// question beside the work meant, so a handoff names those too.
pub fn latest(
    db: &Connection,
    home: &Path,
    folder: &Path,
    except: Option<&str>,
) -> Result<(String, Vec<Summary>)> {
    let query = Query {
        folder: Some(folder.to_path_buf()),
        count: 4,
        ..Query::default()
    };
    let page = find(db, home, &query, crate::now())?;
    let mut others = page
        .sessions
        .into_iter()
        .filter(|session| Some(session.id.as_str()) != except);
    let latest = others.next().ok_or_else(|| {
        let other = if except.is_some() {
            "other than yours "
        } else {
            ""
        };
        Error::NotFound(format!(
            "no session {other}in {} to hand off",
            folder.display()
        ))
    })?;
    Ok((latest.id, others.take(2).collect()))
}

/// What a handoff taken by default says before it: that it's the latest
/// other session in the folder, and the sessions before it there.
pub fn also_here(others: &[Summary], now: i64) -> String {
    if others.is_empty() {
        return String::new();
    }
    let mut out = "This is the latest other session in this folder. Before it here:".to_owned();
    for session in others {
        out += &line(session, None, now);
    }
    out + "\n\n"
}

#[derive(Default)]
pub struct Reading {
    /// The first entry, by number; when it's negative, counted back from the
    /// end over the entries wanted, as -10 for the last ten of them.
    pub from: i64,
    /// How many entries, at most 100.
    pub count: usize,
    /// Only entries of these kinds.
    pub kinds: Vec<Kind>,
    /// Only entries holding all these words.
    pub search: Option<String>,
    /// Only tool calls that failed.
    pub failed: bool,
}

/// The latest message of session `id` that holds all of `words`, matched
/// as the search index matches them, a whole word at a time, with about 240
/// characters around the longest of them, on one line.
fn said(db: &Connection, id: &str, words: &[String]) -> Option<Said> {
    // Text as the index reads it: letters and digits, in lower case, the
    // rest spaces, one character for each, so places in it are the text's.
    let plain = |text: &str| -> Vec<char> {
        text.chars()
            .map(|char| match char.is_alphanumeric() {
                true => char.to_lowercase().next().unwrap_or(char),
                false => ' ',
            })
            .collect()
    };
    // A word as a whole word: spaces around it, the text with spaces at its
    // ends.
    let words: Vec<Vec<char>> = words
        .iter()
        .map(|word| {
            let word: String = plain(word).into_iter().collect();
            format!(
                " {} ",
                word.split_whitespace().collect::<Vec<_>>().join(" ")
            )
            .chars()
            .collect()
        })
        .collect();
    let find =
        |text: &[char], word: &[char]| text.windows(word.len()).position(|window| window == word);
    let entries = transcript(db, id).ok()?;
    let (entry, found, padded) = entries
        .iter()
        .enumerate()
        .rev()
        .find_map(|(entry, found)| {
            if found.tool.is_some() || found.kind == Kind::System {
                return None;
            }
            let padded: Vec<char> = std::iter::once(' ')
                .chain(plain(&found.text))
                .chain(std::iter::once(' '))
                .collect();
            words
                .iter()
                .all(|word| find(&padded, word).is_some())
                .then_some((entry, found, padded))
        })?;
    // Around the longest word, the one that says most of what's looked
    // for, as a common one may be anywhere.
    let longest = words.iter().max_by_key(|word| word.len())?;
    let at = find(&padded, longest).unwrap_or(0);
    let text: Vec<char> = found.text.chars().collect();
    let (start, end) = (at.saturating_sub(80), (at + 160).min(text.len()));
    let around: String = text[start..end].iter().collect();
    let around = around.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(Said {
        subagent: None,
        entry,
        kind: found.kind,
        text: format!(
            "{}{around}{}",
            if start > 0 { "…" } else { "" },
            if end < text.len() { "…" } else { "" }
        ),
    })
}

/// A page of a conversation: its entries with their numbers, and the number
/// the next page starts from. A page ends at `count` entries, or once it
/// holds about `PAGE` characters.
pub fn page<'a>(
    entries: &'a [Entry],
    reading: &Reading,
) -> (Vec<(usize, &'a Entry)>, Option<usize>) {
    let words: Vec<String> = reading
        .search
        .iter()
        .flat_map(|words| words.split_whitespace())
        .map(str::to_lowercase)
        .collect();
    let matching = entries.iter().enumerate().filter(|(_, entry)| {
        (reading.kinds.is_empty() || reading.kinds.contains(&entry.kind))
            && (!reading.failed || entry.tool.as_ref().is_some_and(|tool| tool.failed))
            && {
                let text = match &entry.tool {
                    Some(tool) => format!(
                        "{} {} {}",
                        tool.name,
                        tool.input,
                        tool.output.as_deref().unwrap_or_default()
                    ),
                    None => entry.text.clone(),
                }
                .to_lowercase();
                words.iter().all(|word| text.contains(word))
            }
    });
    let wanted: Vec<(usize, &Entry)> = match usize::try_from(reading.from) {
        Ok(from) => matching.filter(|(at, _)| *at >= from).collect(),
        Err(_) => {
            let matching: Vec<_> = matching.collect();
            let back = usize::try_from(reading.from.unsigned_abs()).unwrap_or(usize::MAX);
            matching[matching.len().saturating_sub(back)..].to_vec()
        }
    };
    let mut wanted = wanted.into_iter().peekable();
    let most = reading.count.clamp(1, 100);
    let mut shown: Vec<(usize, &Entry)> = Vec::new();
    let mut held = 0;
    while let Some(&(at, entry)) = wanted.peek() {
        let size = render(at, entry, &WHOLE, 0).len();
        if shown.len() == most || (!shown.is_empty() && held + size > PAGE) {
            break;
        }
        held += size;
        shown.push((at, entry));
        wanted.next();
    }
    (shown, wanted.next().map(|(at, _)| at))
}

/// `text` cut to `most` characters, saying how many were left out.
fn cut(text: &str, most: usize) -> String {
    let count = text.chars().count();
    if count <= most {
        return text.to_owned();
    }
    let kept: String = text.chars().take(most).collect();
    format!("{kept}… ({} more characters)", count - most)
}

/// `text` cut to `most` characters by leaving out its middle, saying how
/// many were: what a command printed last, as a test's summary or an error,
/// is often what matters.
fn cut_middle(text: &str, most: usize) -> String {
    let count = text.chars().count();
    if count <= most {
        return text.to_owned();
    }
    let head = most / 3;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(count - (most - head)).collect();
    format!("{start}… ({} characters left out) …{end}", count - most)
}

/// `text` on one line, cut to `most` characters with an ellipsis.
pub fn short(text: &str, most: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= most {
        return line;
    }
    let kept: String = line.chars().take(most).collect();
    format!("{}…", kept.trim_end())
}

/// A session's title as a list or heading gives it, cut to about 80
/// characters.
fn title(title: Option<&str>) -> String {
    title.map_or("(untitled)".to_owned(), |title| short(title, 80))
}

/// How long each part of an entry may run before it's cut; `None` is in
/// full.
struct Room {
    /// What the person, the agent or a subagent's brief said, and summaries.
    said: Option<usize>,
    reasoning: usize,
    /// System notes, as a hook's output.
    system: usize,
    tool_in: usize,
    tool_out: usize,
    /// Whether what it says is quoted, `> ` a line, so a heading of its own
    /// can't pass for one around it.
    quote: bool,
}

/// As read_session gives an entry: what was said in full.
const WHOLE: Room = Room {
    said: None,
    reasoning: 2000,
    system: 2000,
    tool_in: 2000,
    tool_out: 2000,
    quote: false,
};

/// As read_session gives the one entry asked for: everything but a part
/// longer than about 10,000 tokens, well under what a client takes from a
/// tool.
const ONE: Room = Room {
    said: None,
    reasoning: 40_000,
    system: 40_000,
    tool_in: 40_000,
    tool_out: 40_000,
    quote: false,
};

/// One entry as text: its number, kind and time, then what it says, each
/// part cut as `room` allows.
fn render(at: usize, entry: &Entry, room: &Room, now: i64) -> String {
    let when = when(entry, now);
    let (head, body) = match &entry.tool {
        Some(tool) => {
            let failed = if tool.failed { " (failed)" } else { "" };
            let output = tool
                .output
                .as_deref()
                .filter(|output| !output.is_empty())
                .map_or(String::new(), |output| {
                    format!("\nout: {}", cut_middle(output, room.tool_out))
                });
            (
                format!("#{at} tool `{}`{failed}{when}", tool.name),
                format!("{}{output}", cut(&input(&tool.input), room.tool_in)),
            )
        }
        None => {
            let most = match entry.kind {
                Kind::Reasoning => Some(room.reasoning),
                Kind::System => Some(room.system),
                _ => room.said,
            };
            let said = entry.text.trim_end();
            let text = most.map_or(said.to_owned(), |most| cut(said, most));
            (format!("#{at} {}{when}", kind(entry.kind)), text)
        }
    };
    match room.quote {
        true => format!("{head}\n{}", quote(&body)),
        false => format!("{head}\n{body}"),
    }
}

/// A tool call's input, a field at a time: one-line fields first and the
/// shortest first, so a path or a command is never what's cut, then text of
/// several lines as it is, indented, rather than escaped as JSON.
fn input(raw: &str) -> String {
    use serde_json::Value;
    let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(raw) else {
        return format!("in: {raw}");
    };
    if fields.is_empty() {
        return "in: (nothing)".to_owned();
    }
    let mut fields: Vec<(&String, String)> = fields
        .iter()
        .map(|(key, value)| match value {
            Value::String(text) => (key, text.clone()),
            other => (key, other.to_string()),
        })
        .collect();
    fields.sort_by_key(|(_, text)| (text.contains('\n'), text.len()));
    let mut lines = Vec::new();
    for (key, text) in fields {
        match text.contains('\n') {
            false => lines.push(format!("{key}: {text}")),
            true => {
                lines.push(format!("{key}:"));
                lines.extend(text.lines().map(|line| format!("  {line}")));
            }
        }
    }
    lines.join("\n")
}

/// `text` quoted, `> ` a line.
fn quote(text: &str) -> String {
    text.lines()
        .map(|line| format!("> {line}").trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A page of a conversation as text.
pub fn page_text(id: &str, entries: &[Entry], reading: &Reading, cite: Cite, now: i64) -> String {
    let (shown, next) = page(entries, reading);
    let mut out = format!(
        "{} of {} entries of `{id}`, from entry {}. Times are local; it's now {}.\n",
        shown.len(),
        entries.len(),
        shown
            .first()
            .map_or(reading.from.max(0), |(at, _)| *at as i64),
        clock_now(now)
    );
    let room = if reading.count == 1 { &ONE } else { &WHOLE };
    for (at, item) in shown {
        out += "\n";
        out += &render(at, item, room, now);
        out += "\n";
    }
    if let Some(next) = next {
        out += &match cite {
            Cite::Command => format!("\nMore from entry {next}: add `--from {next}`."),
            Cite::Tool => format!("\nMore from entry {next}: call again with \"from\": {next}."),
        };
    }
    out.trim_end().to_owned()
}

/// Now, as a person reads it: `Thu Oct 1 12:00`.
fn clock_now(now: i64) -> String {
    time::local(now).strftime("%a %b %-d %H:%M").to_string()
}

/// `count` of `word`, as said: "1 request", "2 requests".
fn plural(count: usize, word: &str) -> String {
    format!("{count} {word}{}", if count == 1 { "" } else { "s" })
}

/// An entry's kind, as it's written.
fn kind(kind: Kind) -> &'static str {
    match kind {
        Kind::User => "user",
        Kind::Assistant => "assistant",
        Kind::Reasoning => "reasoning",
        Kind::Tool => "tool",
        Kind::System => "system",
        Kind::Summary => "summary",
        Kind::Task => "task",
    }
}

/// When an entry was, as a clock says it: `· 22:02` today, `· Wed 09:00`
/// within a week, `· Sep 12 09:30` before.
fn when(entry: &Entry, now: i64) -> String {
    entry
        .at
        .map_or(String::new(), |at| format!(" · {}", time::clock(at, now)))
}

/// Whether a session runs now, or how long ago it was last active:
/// `running`, `active 2h ago`.
fn activity(session: &Summary, now: i64) -> String {
    if session.running {
        "running".to_owned()
    } else {
        format!("active {} ago", time::span(now - session.active))
    }
}

/// A page of sessions as text.
/// `page` as text; the session `yours`, the caller's own, is marked.
pub fn text(page: &Page, cite: Cite, yours: Option<&str>, now: i64) -> String {
    if page.sessions.is_empty() {
        return "No sessions found.".to_owned();
    }
    let order = match page
        .sessions
        .iter()
        .any(|session| session.mentions.is_some())
    {
        true => "those with all the words in the most messages first",
        false => "most recently active first",
    };
    let mut out = format!("{} found, {order}.\n", page.found);
    for session in &page.sessions {
        out += &line(session, yours, now);
        if let Some(said) = &session.said {
            let within = said.subagent.as_ref().map_or(String::new(), |subagent| {
                format!(" in its subagent `{subagent}`")
            });
            out += &format!(
                "\n  #{} {}{within}: \"{}\"",
                said.entry,
                kind(said.kind),
                said.text
            );
        }
    }
    if let Some(next) = &page.next {
        out += &match cite {
            Cite::Command => format!("\n\nNext page: `--cursor {next}`."),
            Cite::Tool => format!("\n\nNext page: cursor \"{next}\"."),
        };
    }
    out
}

/// A session as a list shows it, on a line of its own; `yours`, the
/// caller's own, is marked.
fn line(session: &Summary, yours: Option<&str>, now: i64) -> String {
    let place = match (&session.cwd, &session.branch) {
        (Some(cwd), Some(branch)) => format!(" · {cwd} ({branch})"),
        (Some(cwd), None) => format!(" · {cwd}"),
        _ => String::new(),
    };
    let subagents = match session.subagents {
        0 => String::new(),
        count => format!(" · {}", plural(count as usize, "subagent")),
    };
    let mine = match Some(session.id.as_str()) == yours {
        true => " · yours",
        false => "",
    };
    let mentions = match session.mentions {
        None | Some(0) => String::new(),
        Some(1) => " · all the words in 1 message".to_owned(),
        Some(many) => format!(" · all the words in {} messages", count(many)),
    };
    format!(
        "\n- `{}` \"{}\"{place} · {} · {} · {}{subagents}{mentions}{mine}",
        session.id,
        title(session.title.as_deref()),
        activity(session, now),
        plural_count(session.responses, "response"),
        crate::usage::cost(session.cost_usd, session.unpriced)
    )
}

/// `count` of `word` with its thousands marked: "1 response", "1,204
/// responses".
fn plural_count(number: u64, word: &str) -> String {
    format!(
        "{} {word}{}",
        count(number),
        if number == 1 { "" } else { "s" }
    )
}

pub fn detail_text(detail: &Detail, now: i64) -> String {
    let session = &detail.summary;
    let mut out = format!(
        "{}  {}\n  {} · {} · {}\n",
        session.id,
        title(session.title.as_deref()),
        activity(session, now),
        plural_count(session.responses, "response"),
        crate::usage::cost(session.cost_usd, session.unpriced)
    );
    for (label, value) in [
        ("folder", session.cwd.clone()),
        ("project", session.project.clone()),
        ("branch", session.branch.clone()),
        ("started", session.started.map(|at| time::clock(at, now))),
        ("parent", detail.parent.clone()),
        (
            "models",
            Some(detail.models.join(", ")).filter(|models| !models.is_empty()),
        ),
        (
            "account",
            Some(detail.account_titles.join(", ")).filter(|accounts| !accounts.is_empty()),
        ),
        ("resume", Some(detail.resume.clone())),
    ] {
        if let Some(value) = value {
            out += &format!("  {label}: {value}\n");
        }
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::{Entry, Kind, Reading, page};

    #[test]
    fn counted_back_from_the_end_it_counts_the_entries_wanted() {
        // A person's message, then two tool calls, five times over.
        let entries: Vec<Entry> = (0..15)
            .map(|at| Entry {
                kind: if at % 3 == 0 { Kind::User } else { Kind::Tool },
                at: None,
                text: format!("entry {at}"),
                tool: None,
            })
            .collect();
        let shown = |reading: Reading| -> Vec<usize> {
            page(&entries, &reading)
                .0
                .into_iter()
                .map(|(at, _)| at)
                .collect()
        };
        let reading = |from: i64, kinds: Vec<Kind>| Reading {
            from,
            count: 30,
            kinds,
            search: None,
            failed: false,
        };
        assert_eq!(shown(reading(12, vec![])), [12, 13, 14]);
        assert_eq!(shown(reading(-2, vec![])), [13, 14]);
        // The last two the person said, not those among the last two.
        assert_eq!(shown(reading(-2, vec![Kind::User])), [9, 12]);
        assert_eq!(shown(reading(-40, vec![Kind::User])), [0, 3, 6, 9, 12]);
    }
}
