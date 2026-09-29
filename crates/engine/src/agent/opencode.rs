//! OpenCode.
//!
//! OpenCode keeps every session in one SQLite database,
//! `~/.local/share/opencode/opencode.db`, in WAL mode. `session_v2` has a row
//! per session, with the session's own totals, and `session_message` a row per
//! message, whose `data` holds an assistant message's tokens, cost, provider
//! and model as JSON. New writes land in `opencode.db-wal` and reach the
//! database file only at a checkpoint, so a change to either is a change to
//! the database.
//!
//! **Usage.** An assistant message's `tokens` keep everything apart: `input`
//! excludes cached input, and `reasoning` is separate from `output`. OpenCode's
//! recorded costs bear this out, charging reasoning at the output rate on top
//! of output: of the 2,076 messages with a cost and a catalog price on
//! 2026-09-23, 2,071 matched only that reading. The other five were three
//! GPT-5.6 prompts between 200K and 272K, which OpenCode charges at the
//! long-context rate from 200K, as models.dev's older `context_over_200k`
//! implies, where its `tiers` put the threshold at 272K, as the catalog
//! does; and two responses OpenRouter charged at its own rates. Output here
//! includes reasoning, as it does for every agent.
//!
//! **Totals.** A session's own totals cover the session alone, since a
//! subagent's session has its own, but they include calls no message records,
//! such as the one that names the session. On 2026-09-23, 73 of 80 root
//! sessions' totals were larger than the sum of their messages, by a few
//! hundred tokens each, and every child session's matched exactly.
//!
//! **Links.** A session names its parent in `parent_id`, and the session it
//! was forked from in `fork_session_id`, with `fork_boundary`. The call
//! that started a subagent names its session in `state.metadata.sessionID`,
//! which marks the call ([`Builder::launched`]): all 33 subagent sessions on
//! 2026-09-27 were named so by a call in their parent. The tool is
//! `subagent` in this version, all 33 such calls on 2026-09-29, and was
//! `task` before it.
//!
//! **Forks.** A fork opens with copies of its parent's messages, with ids of
//! their own but the parent's times, before the fork's own `time_created`.
//! They are the parent's, and are left out as OpenCode leaves them out of its
//! own statistics.
//!
//! **Order.** A session's messages are in the order of `session_message.seq`,
//! a whole number OpenCode requires and keeps unique within a session; by
//! `time_created`, 4 of the 2,793 messages here fell in another order, in 1
//! of 114 sessions (2026-09-27). A conversation is shown in `seq`'s order, so
//! a database without the column is neither read nor shown, as one without
//! any column the reader needs is: taking in its usage while its
//! conversations failed to open would hide that it changed shape.
//!
//! **Deleted sessions.** Deleting a session deletes its row and, with it, its
//! messages, and changes no row that is left: `session_message.session_id`
//! refers to `session_v2` with `ON DELETE CASCADE`. On 2026-09-27, 114
//! sessions held 2,793 messages, none was without messages, and no message
//! was without its session. Each read says which sessions the database still
//! holds, so one deleted is known to be gone: what was read of it is kept,
//! and its conversation can't be opened. A session with no messages is an
//! empty conversation, not a gone one.
//!
//! **Reading on.** The database is read on from the latest `time_updated` seen
//! in each table. OpenCode adds a step's usage to its session's totals without
//! touching the session's `time_updated`, so a session whose messages changed
//! is read again too: in 108 of 114 sessions on 2026-09-26, the session's
//! `time_updated` was older than its latest message. Rows updated in the same
//! millisecond as a mark are read again, which changes nothing, because every
//! record combines idempotently.
//!
//! **Work, for a handoff.** Measured 2026-09-29 over 2,810 messages: of the
//! tools, `shell` made 932 calls, `edit` 359, `write` 72 and `patch` 54,
//! and no `todowrite` call was there.
//!
//! - A command is `shell`'s `command`. Its metadata has its `exit` code
//!   once it finished (900 calls); one still going on in the background has
//!   a metadata `status` of `running` (18) and no outcome yet, and a call
//!   OpenCode marks `error` (7) failed. A command the person ran with `!`
//!   is a `shell` message, with its `exit`.
//! - `edit` and `patch` list each file they changed in `metadata.files`,
//!   with its `additions` and `deletions` and a `status` of `added` (12),
//!   `modified` (467) or `deleted` (2), which give its lines exactly;
//!   without that list, an edit is counted from its `oldString` and
//!   `newString`, and a patch from its envelope. `write` records only the
//!   new `content`, and its result says `Created file successfully` for a
//!   new file (50) or `Wrote file successfully` over one (18), whose old
//!   lines aren't known. A call that didn't complete changed nothing.
//! - The plan is `todowrite`'s whole list of `todos`, read as its tool
//!   describes it, since none was here.
//!
//! **What it writes.** Messages are of 7 types (2026-09-27): `assistant`,
//! `user`, `synthetic`, `idle`, `system`, `shell` and `compaction`, and an
//! assistant message's content is parts of `tool`, `reasoning` and `text`.
//! Session and message metadata beyond what is read is passed over without
//! being listed, since none of it is counted, said or shown.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{
    Agent, AgentReader, ArtifactKind, Batch, Checkpoint, DiagnosticKind, ModelTotal, Observation,
    ReportScope, SessionReport,
};
use crate::error::{Error, Result};
use crate::handoff::{self, Change, ChangeKind, PlanItem, StepStatus, Work};
use crate::session::{LinkKind, SessionKey, SessionLink, Title, TitleSource};
use crate::time::Instant;
use crate::transcript::{Builder, Speaker, Titled, Transcript, compact, text_of};
use crate::usage::{LARGEST_COUNT, Tokens, Usd};

/// OpenCode's reader.
pub(super) struct OpenCode;

/// The database's file name.
const DATABASE: &str = "opencode.db";

/// Where OpenCode keeps its database in its `folder`: its sessions, and its
/// sign-ins, which limits read too.
pub(crate) fn database(folder: &Path) -> PathBuf {
    folder.join(DATABASE)
}

/// Open OpenCode's database at `path` to read it as it stands, waiting a
/// moment for a write OpenCode has under way.
pub(crate) fn open(path: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    Ok(connection)
}

/// The columns this reader reads, by table, to take in usage and to show a
/// conversation, which is in the order of `seq`. A database without one of
/// them has changed shape, and is neither read nor shown until the reader
/// understands it.
const COLUMNS: &[(&str, &[&str])] = &[
    (
        "session_v2",
        &[
            "id",
            "parent_id",
            "fork_session_id",
            "directory",
            "title",
            "version",
            "cost",
            "tokens_input",
            "tokens_output",
            "tokens_reasoning",
            "tokens_cache_read",
            "tokens_cache_write",
            "time_created",
            "time_updated",
        ],
    ),
    (
        "session_message",
        &[
            "id",
            "session_id",
            "type",
            "seq",
            "time_created",
            "time_updated",
            "data",
        ],
    ),
];

/// Fields of a message's `tokens` this reader understands. Any other is
/// reported, because a new field may be a new kind of token that is charged.
const TOKEN_FIELDS: &[&str] = &["cache", "input", "output", "reasoning", "total"];

/// Fields of a message's `tokens.cache` this reader understands, for the same
/// reason.
const CACHE_FIELDS: &[&str] = &["read", "write"];

/// Message types that hold nothing counted or said, read and passed over: a
/// compaction's summary, a session going idle, a command the person ran, and
/// notes OpenCode adds itself.
const PASSED_OVER: &[&str] = &["compaction", "idle", "shell", "synthetic", "system"];

/// Kinds of part an assistant message's content holds: its text, its
/// reasoning, and its tool calls with their results. Measured 2026-09-27.
const PARTS: &[&str] = &["reasoning", "text", "tool"];

/// Whether the message `m`, of the session `s`, is the session's own. A
/// fork's copies of its parent's messages keep the times they were first
/// written, before the fork began, and OpenCode leaves them out of its own
/// statistics by that.
const OWN: &str = "(s.fork_session_id IS NULL OR m.time_created >= s.time_created)";

impl AgentReader for OpenCode {
    fn agent(&self) -> Agent {
        Agent::OpenCode
    }

    fn version(&self) -> u32 {
        8
    }

    fn roots(&self, folder: &Path) -> Vec<PathBuf> {
        vec![database(folder)]
    }

    fn classify(&self, root: &Path, path: &Path) -> Option<ArtifactKind> {
        (path == root && path.file_name()?.to_str()? == DATABASE).then_some(ArtifactKind::Database)
    }

    fn read(&self, path: &Path, from: &Checkpoint, batch: &mut Batch) -> Result<Checkpoint> {
        let fail = |error: rusqlite::Error| Error::database(path, error);
        let marks: Marks = from.state("OpenCode reader state")?;
        let connection = open(path).map_err(fail)?;
        // One read transaction, so sessions and messages come from one moment.
        let snapshot = connection.unchecked_transaction().map_err(fail)?;

        let missing = missing_columns(&snapshot).map_err(fail)?;
        if !missing.is_empty() {
            for column in missing {
                batch.note(
                    DiagnosticKind::UnknownRecord,
                    format!("a database without {column}"),
                    0,
                );
            }
            return Ok(from.clone());
        }
        let sessions = read_sessions(&snapshot, &marks, batch).map_err(fail)?;
        let messages = read_messages(&snapshot, marks.messages, batch).map_err(fail)?;
        read_first_prompts(&snapshot, marks.messages, batch).map_err(fail)?;
        // A session OpenCode deleted takes its messages with it, and changes
        // no row that is left; what was read of it is kept, and its
        // conversation is gone.
        batch.hold_only(held(&snapshot).map_err(fail)?);

        // Each is at least the mark it read on from.
        let marks = Marks { sessions, messages };
        Checkpoint::at(0, &marks, "OpenCode reader state")
    }

    fn conversation(&self, session: &SessionKey, artifacts: &[PathBuf]) -> Result<Transcript> {
        let gone = || Error::Gone(session.to_string());
        let path = artifacts
            .iter()
            .find(|path| path.file_name().and_then(|name| name.to_str()) == Some(DATABASE))
            .filter(|path| path.exists())
            .ok_or_else(gone)?;
        let fail = |error: rusqlite::Error| Error::database(path, error);
        let connection = open(path).map_err(fail)?;
        // One read transaction, so the session and its messages come from one
        // moment.
        let snapshot = connection.unchecked_transaction().map_err(fail)?;
        let missing = missing_columns(&snapshot).map_err(fail)?;
        if !missing.is_empty() {
            return Err(Error::Database {
                path: path.clone(),
                detail: format!("a database without {}", missing.join(", ")),
            });
        }
        // A session with no messages is an empty conversation; one whose row
        // OpenCode deleted, with its messages, is gone.
        let kept = snapshot
            .prepare("SELECT 1 FROM session_v2 WHERE id = ?1")
            .and_then(|mut statement| statement.exists([session.native()]))
            .map_err(fail)?;
        if !kept {
            return Err(gone());
        }
        let mut statement = snapshot
            .prepare(&format!(
                "SELECT m.type, m.time_created, m.data
                 FROM session_message m LEFT JOIN session_v2 s ON s.id = m.session_id
                 WHERE m.session_id = ?1 AND {OWN} ORDER BY m.seq"
            ))
            .map_err(fail)?;
        let mut rows = statement.query([session.native()]).map_err(fail)?;
        let mut builder = Builder::default();
        while let Some(row) = rows.next().map_err(fail)? {
            let kind: String = row.get(0).map_err(fail)?;
            let at = Instant::from_millis(row.get(1).map_err(fail)?);
            let data: String = row.get(2).map_err(fail)?;
            let Ok(Value::Object(data)) = serde_json::from_str::<Value>(&data) else {
                continue;
            };
            converse(&kind, at, &data, &mut builder);
        }
        Ok(builder.finish())
    }
}

/// Add one message to a conversation. An assistant message is a list of
/// parts: reasoning, text, and tool calls that carry their own results.
fn converse(kind: &str, at: Option<Instant>, data: &Map<String, Value>, builder: &mut Builder) {
    let text = |field: &str| data.get(field).and_then(Value::as_str).unwrap_or_default();
    match kind {
        "user" => builder.prompt(at, text("text")),
        "assistant" => {
            let model = data
                .get("model")
                .and_then(|model| model.get("id"))
                .and_then(Value::as_str);
            for part in data
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let said = part.get("text").and_then(Value::as_str).unwrap_or_default();
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => builder.say(Speaker::Assistant, at, model, said),
                    Some("reasoning") => builder.say(Speaker::Reasoning, at, model, said),
                    Some("tool") => {
                        let state = part.get("state");
                        let id = part.get("id").and_then(Value::as_str);
                        let input = state
                            .and_then(|state| state.get("input"))
                            .map(compact)
                            .unwrap_or_default();
                        let name = part.get("name").and_then(Value::as_str);
                        let work = state
                            .map(|state| worked(name.unwrap_or_default(), state))
                            .unwrap_or_default();
                        builder.call(id, at, model, name, input);
                        for work in work {
                            builder.worked(at, work);
                        }
                        if matches!(name, Some("task" | "subagent"))
                            && let Some(child) = state
                                .and_then(|state| state.pointer("/metadata/sessionID"))
                                .and_then(Value::as_str)
                        {
                            builder.launched(child);
                        }
                        // A finished call's output is its state's `content`; a
                        // failed call's, its error's message.
                        let failed = state
                            .and_then(|state| state.get("status"))
                            .and_then(Value::as_str)
                            == Some("error");
                        let output = if failed {
                            state.and_then(|state| state.get("error")).map(failure)
                        } else {
                            state.and_then(|state| state.get("content")).map(text_of)
                        };
                        if let (Some(id), Some(output)) = (id, output) {
                            builder.answer(id, output, failed);
                        }
                    }
                    _ => {}
                }
            }
        }
        // A command the person ran themselves, typed after `!`.
        "shell" => {
            let id = data.get("shellID").and_then(Value::as_str);
            builder.call(id, at, None, Some("shell"), text("command").to_owned());
            if let Some(id) = id {
                let output = data
                    .get("output")
                    .and_then(|output| output.get("output"))
                    .map(text_of)
                    .unwrap_or_default();
                let exit = data.get("exit").and_then(Value::as_i64);
                let failed = exit.is_some_and(|exit| exit != 0);
                builder.answer(id, output, failed);
                builder.worked(
                    at,
                    Work::Ran {
                        command: text("command").to_owned(),
                        exit,
                        failed: exit.map(|exit| exit != 0),
                    },
                );
            }
        }
        // What OpenCode adds itself: summaries, compactions, its own notes.
        _ => {
            let said = [text("text"), text("summary")]
                .into_iter()
                .find(|said| !said.is_empty())
                .unwrap_or_default();
            builder.say(Speaker::System, at, None, said);
        }
    }
}

/// The work a call of OpenCode's tool `name` did, as its `state` records
/// it: what it was given (`input`), whether it finished (`status`), and
/// what OpenCode worked out of it (`metadata`).
///
/// Commands are `shell`'s, whose metadata has the exit code once one
/// finished and a status of `running` while one goes on in the background.
/// Files are changed by `edit` and `patch`, whose metadata lists each file
/// with the lines it gained and lost (`additions`, `deletions`) and whether
/// it was `added`, `modified` or `deleted`; and by `write`, which records
/// only the new content and whether it created the file. The plan is
/// `todowrite`'s whole list.
fn worked(name: &str, state: &Value) -> Vec<Work> {
    let unclear = || vec![Work::Unclear(name.to_owned())];
    let input = state.get("input");
    let given = |field: &str| {
        input
            .and_then(|input| input.get(field))
            .and_then(Value::as_str)
    };
    let status = state.get("status").and_then(Value::as_str);
    let metadata = state.get("metadata");
    match name {
        "shell" | "bash" => {
            let Some(command) = given("command") else {
                return unclear();
            };
            let exit = metadata
                .and_then(|metadata| metadata.get("exit"))
                .and_then(Value::as_i64);
            let failed = match (exit, status) {
                (Some(exit), _) => Some(exit != 0),
                (None, Some("error")) => Some(true),
                _ => None,
            };
            vec![Work::Ran {
                command: command.to_owned(),
                exit,
                failed,
            }]
        }
        // A change that failed changed nothing.
        "edit" | "patch" | "write" | "multiedit" if status != Some("completed") => Vec::new(),
        "edit" | "patch" | "multiedit" => {
            match metadata
                .and_then(|metadata| metadata.get("files"))
                .and_then(Value::as_array)
            {
                Some(files) => files
                    .iter()
                    .map(|file| {
                        let count = |field: &str| file.get(field).and_then(Value::as_u64);
                        let Some(path) = file.get("file").and_then(Value::as_str) else {
                            return Work::Unclear(name.to_owned());
                        };
                        let kind = match file.get("status").and_then(Value::as_str) {
                            Some("added") => ChangeKind::Created,
                            Some("deleted") => ChangeKind::Deleted,
                            Some("modified") => ChangeKind::Updated,
                            _ => return Work::Unclear(name.to_owned()),
                        };
                        let counted = count("additions").zip(count("deletions"));
                        Work::Changed(Change::new(path, kind, counted))
                    })
                    .collect(),
                // Without OpenCode's account, what the call was given.
                None => match (name, given("filePath").or(given("path"))) {
                    ("patch", _) => match given("patchText").and_then(handoff::envelope) {
                        Some(changes) => changes.into_iter().map(Work::Changed).collect(),
                        None => unclear(),
                    },
                    ("edit", Some(path)) => {
                        let everywhere = input
                            .and_then(|input| input.get("replaceAll"))
                            .and_then(Value::as_bool)
                            == Some(true);
                        let counted = given("oldString")
                            .zip(given("newString"))
                            .filter(|_| !everywhere)
                            .and_then(|(old, new)| handoff::replaced_lines(old, new));
                        vec![Work::Changed(Change::new(
                            path,
                            ChangeKind::Updated,
                            counted,
                        ))]
                    }
                    _ => unclear(),
                },
            }
        }
        "write" => {
            let (Some(path), Some(content)) =
                (given("filePath").or(given("path")), given("content"))
            else {
                return unclear();
            };
            let said = state.get("content").map(text_of).unwrap_or_default();
            let change = if said.starts_with("Created file successfully") {
                Change::new(
                    path,
                    ChangeKind::Created,
                    Some((handoff::lines(content), 0)),
                )
            } else {
                // What the file held before isn't recorded.
                Change {
                    removed: None,
                    ..Change::new(
                        path,
                        ChangeKind::Updated,
                        Some((handoff::lines(content), 0)),
                    )
                }
            };
            vec![Work::Changed(change)]
        }
        "todowrite" if status == Some("completed") => {
            let todos = input
                .and_then(|input| input.get("todos"))
                .and_then(Value::as_array);
            let items: Option<Vec<PlanItem>> = todos.and_then(|todos| {
                todos
                    .iter()
                    .map(|todo| {
                        Some(PlanItem {
                            id: todo.get("id").and_then(Value::as_str).map(str::to_owned),
                            text: Some(todo.get("content")?.as_str()?.to_owned()),
                            status: Some(StepStatus::from_word(todo.get("status")?.as_str()?)?),
                            removed: false,
                        })
                    })
                    .collect()
            });
            match items {
                Some(items) => vec![Work::Planned(items)],
                None => unclear(),
            }
        }
        _ => Vec::new(),
    }
}

/// What a failed call's `error` says: its message, as OpenCode writes one,
/// `{type, message}`, or the error as it stands.
fn failure(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .map_or_else(|| text_of(error), str::to_owned)
}

/// How far each table has been read: the latest `time_updated` seen in it.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Marks {
    sessions: i64,
    messages: i64,
}

/// Every session the database holds.
fn held(connection: &Connection) -> rusqlite::Result<Vec<SessionKey>> {
    let mut statement = connection.prepare("SELECT id FROM session_v2")?;
    let ids = statement.query_map([], |row| row.get::<_, String>(0))?;
    ids.map(|id| id.map(key)).collect()
}

/// The columns this reader needs that the database lacks, as `table.column`.
fn missing_columns(connection: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut missing = Vec::new();
    for (table, columns) in COLUMNS {
        let mut statement =
            connection.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?;
        let present: Vec<String> = statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for column in *columns {
            if !present.iter().any(|name| name == column) {
                missing.push(format!("{table}.{column}"));
            }
        }
    }
    Ok(missing)
}

fn key(id: String) -> SessionKey {
    SessionKey::new(Agent::OpenCode, id)
}

/// A stored count, when it is a whole number from zero to [`LARGEST_COUNT`].
fn stored_count(value: i64) -> Option<u64> {
    u64::try_from(value)
        .ok()
        .filter(|count| *count <= LARGEST_COUNT)
}

/// Output with the reasoning OpenCode counts apart from it, when that is at
/// most [`LARGEST_COUNT`], as every count recorded is.
fn with_reasoning(output: u64, reasoning: u64) -> Option<u64> {
    output
        .checked_add(reasoning)
        .filter(|output| *output <= LARGEST_COUNT)
}

/// Read sessions updated since `marks`, or whose messages were: what they
/// are, where they came from, and their own totals. Returns the latest update
/// of a session seen.
fn read_sessions(
    connection: &Connection,
    marks: &Marks,
    batch: &mut Batch,
) -> rusqlite::Result<i64> {
    let mut statement = connection.prepare(
        "SELECT id, parent_id, fork_session_id, directory, title, version, cost, tokens_input,
                tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write,
                time_created, time_updated
         FROM session_v2
         WHERE time_updated >= ?1
            OR id IN (SELECT session_id FROM session_message WHERE time_updated >= ?2)",
    )?;
    let mut rows = statement.query(params![marks.sessions, marks.messages])?;
    // A revert OpenCode commits deletes the messages it took back, and
    // updates the session; what they said is forgotten with them, since the
    // conversation no longer shows it.
    let mut held = connection.prepare(&format!(
        "SELECT m.id FROM session_message m JOIN session_v2 s ON s.id = m.session_id
         WHERE m.session_id = ?1 AND {OWN}"
    ))?;
    let mut latest = marks.sessions;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let updated: i64 = row.get(13)?;
        latest = latest.max(updated);
        let messages = held
            .query_map([&id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        let session = key(id);
        batch.hold_messages(&session, messages);

        let parent: Option<String> = row.get(1)?;
        let fork: Option<String> = row.get(2)?;
        let link = match (parent, fork) {
            (Some(parent), _) if !parent.is_empty() => Some((parent, LinkKind::Subagent)),
            (_, Some(fork)) if !fork.is_empty() => Some((fork, LinkKind::Fork)),
            _ => None,
        };
        if let Some((parent, kind)) = link {
            batch.link(SessionLink {
                child: session.clone(),
                parent: key(parent),
                kind,
                // The `task` call that started a subagent names its session.
                launch: (kind == LinkKind::Subagent).then(|| session.native().to_owned()),
            });
        }

        let facts = batch.session_mut(&session);
        for at in [row.get::<_, i64>(12)?, updated] {
            if let Some(at) = Instant::from_millis(at) {
                facts.saw(at);
            }
        }
        if facts.cwd.is_none() {
            facts.cwd = row
                .get::<_, Option<String>>(3)?
                .filter(|cwd| !cwd.is_empty());
        }
        if let Some(version) = row
            .get::<_, Option<String>>(5)?
            .filter(|version| !version.is_empty())
        {
            facts.version = Some(version);
        }
        let title = row
            .get::<_, Option<String>>(4)?
            .and_then(|title| Title::new(TitleSource::Generated, &title));
        facts.offer_title(title);

        let count = |column| row.get::<_, i64>(column).map(stored_count);
        let (input, output, reasoning, cache_read, cache_write) =
            (count(7)?, count(8)?, count(9)?, count(10)?, count(11)?);
        let cost = Usd::from_dollars(row.get(6)?);
        let total = || {
            Some(ModelTotal {
                model: None,
                input: input?,
                cache_read: cache_read?,
                cache_write: cache_write?,
                output: with_reasoning(output?, reasoning?)?,
                web_searches: 0,
                cost: Some(cost?),
            })
        };
        let Some(total) = total() else {
            batch.note(DiagnosticKind::Invalid, "a session total out of range", 0);
            continue;
        };
        batch.report(SessionReport {
            session,
            scope: ReportScope::Session,
            at: Instant::from_millis(updated),
            models: vec![total],
        });
    }
    Ok(latest)
}

/// Read a session's own messages updated at or after `mark`: the usage
/// assistant messages report, and what the person and the model said.
/// Returns the latest update seen.
fn read_messages(connection: &Connection, mark: i64, batch: &mut Batch) -> rusqlite::Result<i64> {
    let mut statement = connection.prepare(&format!(
        "SELECT m.id, m.session_id, m.time_created, m.time_updated, m.data, m.type
         FROM session_message m LEFT JOIN session_v2 s ON s.id = m.session_id
         WHERE m.time_updated >= ?1 AND {OWN}"
    ))?;
    let mut rows = statement.query(params![mark])?;
    let mut latest = mark;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let session: String = row.get(1)?;
        let created: i64 = row.get(2)?;
        let updated: i64 = row.get(3)?;
        latest = latest.max(updated);
        let kind: String = row.get(5)?;
        match kind.as_str() {
            "assistant" | "user" => {}
            other if PASSED_OVER.contains(&other) => continue,
            other => {
                batch.note_unknown("message type", other, 0);
                continue;
            }
        }
        let data: String = row.get(4)?;
        let Ok(Value::Object(data)) = serde_json::from_str::<Value>(&data) else {
            batch.note(
                DiagnosticKind::Unreadable,
                "a message whose data is not a JSON object",
                0,
            );
            continue;
        };
        let session = key(session);
        let at = Instant::from_millis(created);
        // A message is read again each time it changes, so what it says
        // replaces what it said before, even when it now says nothing.
        if kind == "user" {
            let text = data.get("text").and_then(Value::as_str).unwrap_or_default();
            batch.prompt_in(&id, &session, at, text);
            continue;
        }
        batch.give(&id);
        let parts = match data.get("content") {
            None | Some(Value::Null) => &[][..],
            Some(Value::Array(parts)) => parts.as_slice(),
            Some(_) => {
                batch.note(
                    DiagnosticKind::Unreadable,
                    "a message whose content is not a list of parts",
                    0,
                );
                &[][..]
            }
        };
        for part in parts {
            match part.get("type").and_then(Value::as_str) {
                Some("text") => batch.say_in(
                    &id,
                    &session,
                    at,
                    part.get("text").and_then(Value::as_str).unwrap_or_default(),
                ),
                kind => batch.note_kind(kind, PARTS, "message part", 0),
            }
        }
        observe(id, session, created, &data, batch);
    }
    Ok(latest)
}

/// Record an assistant message's usage.
fn observe(
    id: String,
    session: SessionKey,
    created: i64,
    data: &Map<String, Value>,
    batch: &mut Batch,
) {
    let tokens = match data.get("tokens") {
        Some(Value::Object(tokens)) => tokens,
        // A message has no tokens until its response ends, and is read
        // again each time it changes, so one without them is not noted: one
        // still being written would be noted at every change. Of 2,519
        // responses here, 43 had none (2026-09-28): 39 ended in an error,
        // and 4 with no finish, 3 of them empty.
        None | Some(Value::Null) => return,
        Some(_) => {
            batch.note(
                DiagnosticKind::Unreadable,
                "a message's tokens that are not an object",
                0,
            );
            return;
        }
    };
    batch.note_unknown_fields(tokens, TOKEN_FIELDS, "tokens", 0);
    let cache = tokens.get("cache");
    if let Some(Value::Object(cache)) = cache {
        batch.note_unknown_fields(cache, CACHE_FIELDS, "tokens.cache", 0);
    }
    let count = |field: &str| super::count(tokens.get(field));
    let counted = || {
        let reasoning = count("reasoning")?;
        Some(Tokens {
            input: count("input")?,
            cache_read: super::count(cache?.get("read"))?,
            cache_write_5m: super::count(cache?.get("write"))?,
            cache_write_1h: 0,
            output: with_reasoning(count("output")?, reasoning)?,
            reasoning,
        })
    };
    let Some(tokens) = counted() else {
        batch.note(
            DiagnosticKind::Invalid,
            "a message's tokens missing or out of range",
            0,
        );
        return;
    };
    if tokens.total() == 0 {
        return;
    }
    let model = data.get("model");
    let provider = model
        .and_then(|model| model.get("providerID"))
        .and_then(Value::as_str);
    let name = model
        .and_then(|model| model.get("id"))
        .and_then(Value::as_str);
    let (Some(provider), Some(name)) = (
        provider.filter(|p| !p.is_empty()),
        name.filter(|n| !n.is_empty()),
    ) else {
        batch.note(
            DiagnosticKind::Invalid,
            "a message with usage and no model",
            0,
        );
        return;
    };
    let time = data.get("time");
    let at = time
        .and_then(|time| time.get("completed").or_else(|| time.get("created")))
        .and_then(Value::as_i64)
        .unwrap_or(created);
    let Some(at) = Instant::from_millis(at) else {
        batch.note(DiagnosticKind::Invalid, "a message time out of range", 0);
        return;
    };
    let Some(cost) = super::cost(data.get("cost")) else {
        batch.note(DiagnosticKind::Invalid, "a message cost out of range", 0);
        return;
    };
    batch.observe(Observation {
        response: id,
        session,
        copy: false,
        at,
        provider: provider.to_owned(),
        model: name.to_owned(),
        prompt: tokens.prompt(),
        tokens,
        web_searches: 0,
        cost,
        priority: false,
    });
}

/// Title each session with a prompt updated at or after `mark` from its own
/// prompts, in order. OpenCode's own title, when it has made one, outranks
/// it.
fn read_first_prompts(
    connection: &Connection,
    mark: i64,
    batch: &mut Batch,
) -> rusqlite::Result<()> {
    let mut statement = connection.prepare(&format!(
        "SELECT m.session_id, m.data
         FROM session_message m LEFT JOIN session_v2 s ON s.id = m.session_id
         WHERE m.type = 'user' AND {OWN}
           AND m.session_id IN (SELECT session_id FROM session_message
                                WHERE type = 'user' AND time_updated >= ?1)
         ORDER BY m.session_id, m.seq"
    ))?;
    let mut rows = statement.query(params![mark])?;
    let mut titling: Option<(String, Titled)> = None;
    while let Some(row) = rows.next()? {
        let session: String = row.get(0)?;
        let titled = match &mut titling {
            Some((titling, titled)) if *titling == session => titled,
            _ => &mut titling.insert((session.clone(), Titled::default())).1,
        };
        if *titled == Titled::Yes {
            continue;
        }
        let data: String = row.get(1)?;
        let data = serde_json::from_str::<Value>(&data).ok();
        let text = data
            .as_ref()
            .and_then(|data| data.get("text"))
            .and_then(Value::as_str);
        if let Some(title) = text.and_then(|text| titled.offer(text)) {
            batch.session_mut(&key(session)).offer_title(Some(title));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::transcript::Speaker;
    use std::path::{Path, PathBuf};

    use rusqlite::{Connection, params};
    use serde_json::json;

    use super::OpenCode;
    use crate::Agent;
    use crate::agent::tests::assert_said_as_shown;
    use crate::agent::{AgentReader, Batch, Checkpoint, DiagnosticKind, Observation, ReportScope};
    use crate::session::{LinkKind, SessionKey, TitleSource};
    use crate::usage::{Tokens, Usd};

    fn key(id: &str) -> SessionKey {
        SessionKey::new(Agent::OpenCode, id)
    }

    /// An OpenCode database with the columns the reader reads, as OpenCode
    /// defines them.
    fn database(dir: &Path) -> (PathBuf, Connection) {
        let path = dir.join("opencode.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE session_v2 (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT,
                     fork_session_id TEXT, directory TEXT NOT NULL, title TEXT, version TEXT NOT NULL,
                     cost REAL NOT NULL DEFAULT 0, tokens_input INTEGER NOT NULL DEFAULT 0,
                     tokens_output INTEGER NOT NULL DEFAULT 0, tokens_reasoning INTEGER NOT NULL DEFAULT 0,
                     tokens_cache_read INTEGER NOT NULL DEFAULT 0, tokens_cache_write INTEGER NOT NULL DEFAULT 0,
                     time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
                 CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                     type TEXT NOT NULL, seq INTEGER NOT NULL, time_created INTEGER NOT NULL,
                     time_updated INTEGER NOT NULL, data TEXT NOT NULL);
                 CREATE UNIQUE INDEX session_message_session_seq_idx
                     ON session_message (session_id, seq);",
            )
            .unwrap();
        (path, connection)
    }

    fn session(
        connection: &Connection,
        id: &str,
        parent: Option<&str>,
        title: Option<&str>,
        updated: i64,
    ) {
        connection
            .execute(
                "INSERT OR REPLACE INTO session_v2 (id, parent_id, directory, title, version, cost,
                     tokens_input, tokens_output, tokens_reasoning, tokens_cache_read, tokens_cache_write,
                     time_created, time_updated)
                 VALUES (?1, ?2, '/work/turnscope', ?3, '2.0.8', 0.00895912, 562, 164, 129, 199788, 878,
                         1789289400000, ?4)",
                params![id, parent, title, updated],
            )
            .unwrap();
    }

    /// Write the message `id` of `session`, in turn after the session's
    /// others, or where it was when written again.
    fn message(
        connection: &Connection,
        id: &str,
        session: &str,
        kind: &str,
        data: serde_json::Value,
        time: i64,
    ) {
        connection
            .execute(
                "INSERT OR REPLACE INTO session_message
                     (id, session_id, type, seq, time_created, time_updated, data)
                 VALUES (?1, ?2, ?3,
                         coalesce((SELECT seq FROM session_message WHERE id = ?1),
                                  (SELECT count(*) FROM session_message WHERE session_id = ?2)),
                         ?4, ?4, ?5)",
                params![id, session, kind, time, data.to_string()],
            )
            .unwrap();
    }

    fn assistant(
        input: u64,
        output: u64,
        reasoning: u64,
        read: u64,
        write: u64,
        cost: f64,
    ) -> serde_json::Value {
        json!({"time": {"created": 1789289428000u64, "completed": 1789289428586u64}, "agent": "build",
               "model": {"id": "gpt-5.6-luna", "providerID": "opencode-go"}, "finish": "stop", "cost": cost,
               "tokens": {"input": input, "output": output, "reasoning": reasoning,
                          "cache": {"read": read, "write": write}}})
    }

    fn read(path: &Path, from: &Checkpoint) -> (Batch, Checkpoint) {
        let mut batch = Batch::default();
        let checkpoint = OpenCode.read(path, from, &mut batch).unwrap();
        (batch, checkpoint)
    }

    #[test]
    fn a_session_and_its_messages_count_reasoning_as_output_and_keep_their_costs() {
        let dir = tempfile::tempdir().unwrap();
        let (path, connection) = database(dir.path());
        session(
            &connection,
            "ses_a",
            None,
            Some("Planning the scan"),
            1_789_289_430_000,
        );
        message(
            &connection,
            "msg_1",
            "ses_a",
            "assistant",
            assistant(3, 164, 129, 199_788, 878, 0.008_959_12),
            1_789_289_428_000,
        );
        let (batch, _) = read(&path, &Checkpoint::default());

        let seen: Vec<&Observation> = batch.observations().collect();
        assert_eq!(seen.len(), 1);
        // Output 164 and reasoning 129 are separate in OpenCode: 293 in all.
        assert_eq!(
            seen[0].tokens,
            Tokens {
                input: 3,
                cache_read: 199_788,
                cache_write_5m: 878,
                cache_write_1h: 0,
                output: 293,
                reasoning: 129
            }
        );
        assert_eq!(seen[0].prompt, 3 + 199_788 + 878);
        assert_eq!(
            (seen[0].provider.as_str(), seen[0].model.as_str()),
            ("opencode-go", "gpt-5.6-luna")
        );
        assert_eq!(seen[0].cost, Usd::from_nanos(8_959_120));
        assert_eq!(seen[0].at.millis(), 1_789_289_428_586);

        // The session's own totals, covering every model it used, and a
        // call no message records: input 562 where the message has 3.
        let report = batch.reports().next().unwrap();
        assert_eq!(
            (report.session.clone(), report.scope),
            (key("ses_a"), ReportScope::Session)
        );
        let total = &report.models[0];
        assert_eq!(total.model, None);
        assert_eq!(
            (
                total.input,
                total.cache_read,
                total.cache_write,
                total.output
            ),
            (562, 199_788, 878, 164 + 129)
        );
        assert_eq!(total.cost, Usd::from_nanos(8_959_120));
    }

    #[test]
    fn a_count_missing_or_out_of_range_invalidates_its_record_and_no_other() {
        let dir = tempfile::tempdir().unwrap();
        let (path, connection) = database(dir.path());
        session(&connection, "ses_a", None, Some("Planning"), 1_000);
        // A session total below zero.
        connection
            .execute(
                "INSERT INTO session_v2 (id, directory, version, tokens_input, time_created, time_updated)
                 VALUES ('ses_b', '/work', '2.0.8', -5, 1000, 1000)",
                [],
            )
            .unwrap();
        // One more than 2^50 = 1,125,899,906,842,624, and no cache writes,
        // which OpenCode always counts.
        let mut huge = assistant(10, 1, 0, 0, 0, 0.0);
        huge["tokens"]["input"] = json!(1_125_899_906_842_625_u64);
        let mut missing = assistant(10, 1, 0, 0, 0, 0.0);
        missing["tokens"]["cache"]
            .as_object_mut()
            .unwrap()
            .remove("write");
        message(
            &connection,
            "msg_1",
            "ses_a",
            "assistant",
            assistant(10, 1, 0, 0, 0, 0.0),
            1_000,
        );
        // Output 2^49 + 1 and reasoning 2^49, each in range, which with its
        // reasoning comes to 2^50 + 1.
        let over = assistant(10, (1 << 49) + 1, 1 << 49, 0, 0, 0.0);
        message(&connection, "msg_2", "ses_a", "assistant", huge, 1_100);
        message(&connection, "msg_3", "ses_a", "assistant", missing, 1_200);
        message(&connection, "msg_4", "ses_a", "assistant", over, 1_300);
        let (batch, _) = read(&path, &Checkpoint::default());

        let counted: Vec<&str> = batch
            .observations()
            .map(|seen| seen.response.as_str())
            .collect();
        assert_eq!(counted, ["msg_1"]);
        let reported: Vec<&str> = batch
            .reports()
            .map(|report| report.session.native())
            .collect();
        assert_eq!(reported, ["ses_a"]);
        let noted: Vec<(DiagnosticKind, &str)> = batch
            .diagnostics()
            .map(|((kind, detail), _)| (*kind, detail.as_str()))
            .collect();
        assert_eq!(
            noted,
            [
                (
                    DiagnosticKind::Invalid,
                    "a message's tokens missing or out of range"
                ),
                (DiagnosticKind::Invalid, "a session total out of range"),
            ]
        );
    }

    #[test]
    fn subagents_and_forks_are_linked_to_the_sessions_they_came_from() {
        let dir = tempfile::tempdir().unwrap();
        let (path, connection) = database(dir.path());
        session(&connection, "ses_parent", None, Some("Parent"), 1);
        session(
            &connection,
            "ses_child",
            Some("ses_parent"),
            Some("Child"),
            1,
        );
        connection
            .execute(
                "INSERT INTO session_v2 (id, fork_session_id, directory, version, time_created, time_updated)
                 VALUES ('ses_fork', 'ses_parent', '/work', '2.0.8', 1, 1)",
                [],
            )
            .unwrap();
        let (batch, _) = read(&path, &Checkpoint::default());

        let mut links: Vec<(SessionKey, SessionKey, LinkKind, Option<&str>)> = batch
            .links()
            .map(|link| {
                let launch = link.launch.as_deref();
                (link.child.clone(), link.parent.clone(), link.kind, launch)
            })
            .collect();
        links.sort();
        // A subagent is known to the `task` call that started it by its own
        // session's id; a fork was started by no call.
        assert_eq!(
            links,
            [
                (
                    key("ses_child"),
                    key("ses_parent"),
                    LinkKind::Subagent,
                    Some("ses_child")
                ),
                (key("ses_fork"), key("ses_parent"), LinkKind::Fork, None),
            ]
        );
    }

    #[test]
    fn a_task_call_is_marked_by_the_session_it_started() {
        let dir = tempfile::tempdir().unwrap();
        let (path, connection) = database(dir.path());
        session(&connection, "ses_a", None, Some("Audit"), 1_000);
        message(
            &connection,
            "msg_1",
            "ses_a",
            "assistant",
            json!({"model": {"id": "glm-5.3", "providerID": "opencode-go"},
                   "content": [{"type": "tool", "id": "call_1", "name": "task",
                                "state": {"status": "completed", "input": {"description": "Audit the scan"},
                                          "metadata": {"sessionID": "ses_child"},
                                          "content": [{"type": "text", "text": "Done."}]}},
                               {"type": "tool", "id": "call_2", "name": "read",
                                "state": {"status": "completed", "input": {"path": "ingest.rs"},
                                          "metadata": {"sessionID": "ses_other"}}},
                               // As this version names the tool.
                               {"type": "tool", "id": "call_3", "name": "subagent",
                                "state": {"status": "completed",
                                          "input": {"agent": "explore", "description": "Look", "prompt": "…"},
                                          "metadata": {"sessionID": "ses_second", "status": "completed"},
                                          "content": [{"type": "text", "text": "Found it."}]}}]}),
            2_000,
        );
        let transcript = OpenCode
            .conversation(&key("ses_a"), std::slice::from_ref(&path))
            .unwrap();
        // Only a call that starts a subagent is marked, whatever else names
        // a session.
        assert_eq!(
            transcript.launches,
            [(0, "ses_child".to_owned()), (2, "ses_second".to_owned())]
        );
    }

    #[test]
    fn reading_on_takes_what_changed_and_the_totals_of_sessions_whose_messages_did() {
        let dir = tempfile::tempdir().unwrap();
        let (path, connection) = database(dir.path());
        session(&connection, "ses_a", None, Some("Planning"), 1_000);
        message(
            &connection,
            "msg_1",
            "ses_a",
            "assistant",
            assistant(10, 1, 0, 0, 0, 0.0),
            1_000,
        );
        message(
            &connection,
            "msg_2",
            "ses_a",
            "assistant",
            assistant(20, 2, 0, 0, 0, 0.0),
            1_500,
        );
        // Another session, updated since, which the next read goes on from.
        session(&connection, "ses_b", None, Some("Reviewing"), 1_500);
        let (_, checkpoint) = read(&path, &Checkpoint::default());

        // A step, as OpenCode records it: its message, and the session's
        // totals grown by it, leaving the session's time_updated as it was.
        message(
            &connection,
            "msg_3",
            "ses_a",
            "assistant",
            assistant(30, 3, 0, 0, 0, 0.0),
            2_000,
        );
        connection
            .execute(
                "UPDATE session_v2 SET tokens_input = tokens_input + 30,
                     tokens_output = tokens_output + 3 WHERE id = 'ses_a'",
                [],
            )
            .unwrap();
        let (batch, _) = read(&path, &checkpoint);

        // The mark is msg_2's millisecond, which is read again; msg_1, older,
        // is not.
        let mut seen: Vec<&str> = batch
            .observations()
            .map(|seen| seen.response.as_str())
            .collect();
        seen.sort_unstable();
        assert_eq!(seen, ["msg_2", "msg_3"]);
        // Input 562 + 30, and output 164 + 3 with its reasoning, 129.
        let report = batch
            .reports()
            .find(|report| report.session == key("ses_a"))
            .unwrap();
        assert_eq!(
            (report.models[0].input, report.models[0].output),
            (592, 167 + 129)
        );
    }

    #[test]
    fn a_forks_copies_of_its_parents_messages_stay_the_parents() {
        let dir = tempfile::tempdir().unwrap();
        let (path, connection) = database(dir.path());
        let reply = |text: &str| {
            let mut data = assistant(100, 10, 0, 0, 0, 0.001);
            data["content"] = json!([{"type": "text", "text": text}]);
            data
        };
        session(&connection, "ses_p", None, Some("Planning"), 2_000);
        message(
            &connection,
            "msg_p1",
            "ses_p",
            "user",
            json!({"text": "Plan the watcher"}),
            1_000,
        );
        message(
            &connection,
            "msg_p2",
            "ses_p",
            "assistant",
            reply("Watch the folder."),
            1_500,
        );
        // The fork, begun at 5,000, opens with copies of the parent's two
        // messages under ids of their own, at the parent's times.
        connection
            .execute(
                "INSERT INTO session_v2 (id, fork_session_id, directory, version, time_created, time_updated)
                 VALUES ('ses_f', 'ses_p', '/work/turnscope', '2.0.8', 5000, 7000)",
                [],
            )
            .unwrap();
        message(
            &connection,
            "msg_c1",
            "ses_f",
            "user",
            json!({"text": "Plan the watcher"}),
            1_000,
        );
        message(
            &connection,
            "msg_c2",
            "ses_f",
            "assistant",
            reply("Watch the folder."),
            1_500,
        );
        message(
            &connection,
            "msg_f3",
            "ses_f",
            "user",
            json!({"text": "Now write it"}),
            6_000,
        );
        message(
            &connection,
            "msg_f4",
            "ses_f",
            "assistant",
            reply("Written."),
            6_500,
        );
        let (batch, _) = read(&path, &Checkpoint::default());

        let mut counted: Vec<&str> = batch
            .observations()
            .map(|seen| seen.response.as_str())
            .collect();
        counted.sort_unstable();
        assert_eq!(counted, ["msg_f4", "msg_p2"]);
        let said: Vec<&str> = batch
            .said()
            .filter(|said| said.session == key("ses_f"))
            .map(|said| said.text.as_str())
            .collect();
        assert_eq!(said, ["Now write it", "Written."]);
        let (_, facts) = batch
            .sessions()
            .find(|(session, _)| **session == key("ses_f"))
            .unwrap();
        assert_eq!(
            facts.title.as_ref().map(|title| title.text.as_str()),
            Some("Now write it")
        );
        let entries = OpenCode
            .conversation(&key("ses_f"), std::slice::from_ref(&path))
            .unwrap()
            .entries;
        let shown: Vec<(Speaker, &str)> = entries
            .iter()
            .map(|entry| (entry.speaker, entry.text.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                (Speaker::User, "Now write it"),
                (Speaker::Assistant, "Written.")
            ]
        );
        assert_said_as_shown(
            &OpenCode,
            &key("ses_f"),
            std::slice::from_ref(&path),
            std::slice::from_ref(&path),
        );
    }

    #[test]
    fn a_generated_title_outranks_the_first_prompt_and_only_the_first_prompt_titles() {
        let dir = tempfile::tempdir().unwrap();
        let (path, connection) = database(dir.path());
        session(&connection, "ses_untitled", None, None, 1_000);
        message(
            &connection,
            "msg_1",
            "ses_untitled",
            "user",
            json!({"text": "Why is the scan slow?"}),
            1_000,
        );
        // Written second, with an earlier time, as 4 of 2,793 messages here
        // were: the conversation's first prompt, by `seq`, still titles it.
        message(
            &connection,
            "msg_2",
            "ses_untitled",
            "user",
            json!({"text": "And the index?"}),
            500,
        );
        session(
            &connection,
            "ses_titled",
            None,
            Some("Scan performance"),
            1_000,
        );
        message(
            &connection,
            "msg_3",
            "ses_titled",
            "user",
            json!({"text": "Why is the scan slow?"}),
            1_000,
        );
        let (batch, _) = read(&path, &Checkpoint::default());

        let title = |id: &str| {
            let (_, facts) = batch
                .sessions()
                .find(|(session, _)| **session == key(id))
                .unwrap();
            let title = facts.title.clone().unwrap();
            (title.source, title.text)
        };
        assert_eq!(
            title("ses_untitled"),
            (TitleSource::Prompt, "Why is the scan slow?".into())
        );
        assert_eq!(
            title("ses_titled"),
            (TitleSource::Generated, "Scan performance".into())
        );
    }

    #[test]
    fn messages_and_fields_not_understood_are_noted() {
        let dir = tempfile::tempdir().unwrap();
        let (path, connection) = database(dir.path());
        session(&connection, "ses_a", None, Some("Planning"), 1_000);
        let mut cached = assistant(10, 1, 0, 5, 0, 0.0);
        cached["tokens"]["cache"]["audio"] = json!(3);
        message(&connection, "msg_1", "ses_a", "assistant", cached, 1_000);
        message(
            &connection,
            "msg_2",
            "ses_a",
            "idle",
            json!({"outcome": "succeeded", "time": {"created": 1_100}}),
            1_100,
        );
        message(
            &connection,
            "msg_3",
            "ses_a",
            "hologram",
            json!({"text": "?"}),
            1_200,
        );
        // A reply with a part of a kind not known beside its text.
        let mut reply = assistant(20, 2, 0, 0, 0, 0.0);
        reply["content"] = json!([{"type": "text", "text": "The scan reads each file once."},
                                  {"type": "hologram", "data": "?"}]);
        message(&connection, "msg_4", "ses_a", "assistant", reply, 1_300);
        let (batch, _) = read(&path, &Checkpoint::default());

        // Both replies are counted, and the second's text is said.
        assert_eq!(batch.observations().count(), 2);
        let said: Vec<&str> = batch.said().map(|said| said.text.as_str()).collect();
        assert_eq!(said, ["The scan reads each file once."]);
        let noted: Vec<(DiagnosticKind, &str)> = batch
            .diagnostics()
            .map(|((kind, detail), _)| (*kind, detail.as_str()))
            .collect();
        assert_eq!(
            noted,
            [
                (DiagnosticKind::UnknownRecord, "message part hologram"),
                (DiagnosticKind::UnknownRecord, "message type hologram"),
                (DiagnosticKind::UnknownField, "tokens.cache.audio"),
            ]
        );
    }

    #[test]
    fn a_database_of_another_shape_is_noted_and_neither_read_nor_shown() {
        let dir = tempfile::tempdir().unwrap();
        let (path, connection) = database(dir.path());
        session(&connection, "ses_a", None, Some("Planning"), 1_000);
        message(
            &connection,
            "msg_1",
            "ses_a",
            "user",
            json!({"text": "Review the scan"}),
            1_000,
        );
        // An older OpenCode, which kept no `seq` to put a session's messages
        // in order by.
        connection
            .execute_batch(
                "DROP INDEX session_message_session_seq_idx;
                 ALTER TABLE session_message DROP COLUMN seq;",
            )
            .unwrap();
        // Read on from marks of its own, which stay where they were.
        let from = Checkpoint {
            offset: 0,
            state: br#"{"sessions":5,"messages":7}"#.to_vec(),
        };
        let (batch, checkpoint) = read(&path, &from);
        assert_eq!(checkpoint, from);
        assert_eq!(batch.observations().count(), 0);
        assert_eq!(batch.said().count(), 0);
        let noted: Vec<(DiagnosticKind, &str)> = batch
            .diagnostics()
            .map(|((kind, detail), _)| (*kind, detail.as_str()))
            .collect();
        assert_eq!(
            noted,
            [(
                DiagnosticKind::UnknownRecord,
                "a database without session_message.seq"
            )]
        );
        match OpenCode.conversation(&key("ses_a"), std::slice::from_ref(&path)) {
            Err(crate::Error::Database { detail, .. }) => {
                assert_eq!(detail, "a database without session_message.seq");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_assistant_message_holds_its_thinking_reply_and_calls_with_their_results() {
        let dir = tempfile::tempdir().unwrap();
        let (path, connection) = database(dir.path());
        session(&connection, "ses_a", None, Some("Planning"), 1_000);
        message(
            &connection,
            "msg_1",
            "ses_a",
            "user",
            json!({"text": "Review the scan"}),
            1_000,
        );
        message(
            &connection,
            "msg_2",
            "ses_a",
            "assistant",
            json!({"model": {"id": "glm-5.3", "providerID": "opencode-go"},
            "content": [{"type": "reasoning", "text": "Look at ingest first."},
                        {"type": "tool", "id": "call_1", "name": "read",
                         "state": {"status": "completed", "input": {"path": "ingest.rs"}, "content": [{"type": "text", "text": "fn scan()"}]}},
                        {"type": "tool", "id": "call_2", "name": "bash",
                         "state": {"status": "error", "input": {"command": "false"},
                                   "error": {"type": "tool.execution", "message": "exit 1"}}},
                        {"type": "text", "text": "The scan reads every file."}]}),
            2_000,
        );
        // A command the person ran with `!`, as OpenCode records one.
        message(
            &connection,
            "msg_3",
            "ses_a",
            "shell",
            json!({"shellID": "sh_1", "command": "ls", "status": "exited", "exit": 0,
                   "output": {"output": "Cargo.toml\n", "size": 11, "truncated": false, "cursor": 11},
                   "metadata": {"background": false}, "time": {"created": 3_000, "completed": 3_050}}),
            3_000,
        );
        let entries = OpenCode
            .conversation(&key("ses_a"), std::slice::from_ref(&path))
            .unwrap()
            .entries;
        let speakers: Vec<Speaker> = entries.iter().map(|entry| entry.speaker).collect();
        assert_eq!(
            speakers,
            [
                Speaker::User,
                Speaker::Reasoning,
                Speaker::Tool,
                Speaker::Tool,
                Speaker::Assistant,
                Speaker::Tool
            ]
        );
        assert_said_as_shown(
            &OpenCode,
            &key("ses_a"),
            std::slice::from_ref(&path),
            std::slice::from_ref(&path),
        );
        assert_eq!(
            entries[2].tool.as_ref().unwrap().output.as_deref(),
            Some("fn scan()")
        );
        let failed = entries[3].tool.as_ref().unwrap();
        assert_eq!(
            (failed.output.as_deref(), failed.failed),
            (Some("exit 1"), true)
        );
        assert_eq!(entries[4].model.as_deref(), Some("glm-5.3"));
        let shell = entries[5].tool.as_ref().unwrap();
        assert_eq!(
            (
                shell.name.as_str(),
                shell.input.as_str(),
                shell.output.as_deref(),
                shell.failed
            ),
            ("shell", "ls", Some("Cargo.toml\n"), false)
        );
    }
}
