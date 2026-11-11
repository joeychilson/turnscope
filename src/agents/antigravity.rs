//! Antigravity.
//!
//! Antigravity's app and its command line, `agy`, share `~/.gemini`: the
//! app keeps its history in `antigravity/` there, `agy` in
//! `antigravity-cli/`, and both their MCP servers in
//! `config/mcp_config.json`. So `~/.gemini` is its one folder, over a folder
//! for each: `connect` then runs `agy` once, and a second folder stays what
//! it is for the other agents, a second account's. The Gemini command line
//! keeps its settings in `~/.gemini` too, so Antigravity is installed when
//! one of its own two folders is there, not when `~/.gemini` is.
//!
//! What follows was measured on this Mac's history on 2026-11-10: 8
//! conversations of 405 steps and 196 model calls, written by `agy` 1.2.11
//! in September 2026 and by the app 2.31.0 that day. Antigravity publishes
//! no schema for what it writes as protobuf. The field numbers here are the
//! ones measured, written `#1.#4` for field 4 of the message in field 1, and
//! they're the ones CodexBar, tokscale, CodeBurn and agent-walker read
//! (their code, read 2026-11-10).
//!
//! **Files.** In each of the two folders, a conversation `<id>` is:
//!
//! - `conversations/<id>.db`, a SQLite database: a row per step in `steps`,
//!   a row per model call in `gen_metadata`, and in
//!   `trajectory_metadata_blob` when the conversation began (#2) and the
//!   folder it works in (#1.#1, a `file://` address). One the app begins
//!   with no folder has none.
//! - `brain/<id>/.system_generated/logs/transcript_full.jsonl`, a line per
//!   step as the model saw it. `transcript.jsonl` beside it cuts long text
//!   short, and the files under `chunks/` are the same lines cut every
//!   100 KiB, mid-line too.
//! - its row of `conversation_summaries.db`, with the title Antigravity gave
//!   it: 3 of the 8 had one.
//!
//! **Usage.** A `gen_metadata` row's `data` holds its call's usage at #1.#4:
//! input (#2), output (#3), cache writes (#4), cache reads (#5), reasoning
//! (#9), the reply (#10) and the message's id (#7). Input leaves cache reads
//! out: it was the smaller in 173 of the 174 calls that read the cache.
//! Output includes reasoning: it was reasoning plus the reply in all 193
//! calls with tokens. No call wrote to a cache. The model is #1.#19, as
//! `gemini-3.8-flash`. A call is made when the step it names first (#2) is:
//! that step's `metadata` holds the time (#1, seconds and nanoseconds), and
//! held the same message id in all 193.
//!
//! The 3 calls without tokens were each the last of their conversation, as
//! one still running or one that failed is. So a read goes on from the last
//! row it read, which it reads again. Measured again later that day, with
//! 254 calls, the same 3 were the only ones, each still its conversation's
//! last.
//!
//! **Providers.** Antigravity signs in to Google only, and every call here
//! was to a Gemini model, so a response's provider is `google`. It serves
//! Claude and GPT models from the same plan. None was on this Mac to
//! measure, so they're `google`'s too, where they have no price.
//!
//! **Transcripts.** A line's `type` says what its step is:
//!
//! - `USER_INPUT`, from `USER_EXPLICIT`: what the person typed, in
//!   `<USER_REQUEST>`, then the blocks Antigravity adds, as
//!   `<ADDITIONAL_METADATA>`.
//! - `PLANNER_RESPONSE`: a model's `thinking`, its reply `content`, and its
//!   `tool_calls`, each a `name` and `args`.
//! - `GENERIC`: a tool's output. No line names the call it answers. The
//!   call numbered n of the response at step k is answered by step
//!   k + 1 + n: so were 198 of the 199 calls, and the other's step was in no
//!   transcript.
//! - `CHECKPOINT`: the summary Antigravity wrote when it cut a long
//!   conversation short, after its number, `{{ CHECKPOINT 0 }}`, and a line
//!   saying what it is, and before an instruction to the model not to
//!   answer it. The one here was so.
//! - `SYSTEM_MESSAGE`: what Antigravity told the model itself.
//!
//! A line is written once: the 4 of a command left running in the
//! background stayed `RUNNING`, with what it had printed by then.
//!
//! **Tools.** `agy` and the app name theirs differently. The 288 calls
//! measured later that day were to `run_command` (173), `view_file` (36),
//! `search_web` (25), `write_file` (24), `replace_file_content` (15),
//! `read_file` (5), `list_dir` (3), `manage_task` (2), `edit_file` (2),
//! `write_to_file`, `schedule` and `ask_question` (1 each). A call that
//! writes a file names it in `TargetFile` (`replace_file_content`,
//! `write_to_file`) or in `Path` (`write_file`, `edit_file`), always in
//! full. `args` is an object.
//!
//! **Subagents.** `conversation_summaries.parent_conversation_id` names a
//! conversation's parent. That is from the column's name: no conversation
//! here had one.
//!
//! **Not read.** The Antigravity IDE's history, which this Mac doesn't have.
//!
//! **Logins.** Antigravity keeps its Google sign-in in the Keychain. It
//! isn't read, so its usage is no account's.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    Agent, Conversation, Entry, Info, Kind, Link, Login, Parent, Read, Response, Role, Title,
    Tokens,
};
use crate::Result;

pub struct Antigravity;

static INFO: Info = Info {
    id: "antigravity",
    name: "Antigravity",
    command: "agy",
    resume: "agy --conversation {id}",
    mcp_add: &["mcp", "add", "turnscope", "--"],
    mcp_remove: Some(&["mcp", "remove", "turnscope"]),
    folder_var: None,
    charges: false,
    session_var: None,
    cwd_var: None,
};

/// The folders of `~/.gemini` that keep history: the app's, and `agy`'s.
const KEPT: [&str; 2] = ["antigravity", "antigravity-cli"];

/// A folder's titles, and a conversation's transcript.
const TITLES: &str = "conversation_summaries.db";
const TRANSCRIPT: &str = "transcript_full.jsonl";

/// Where a conversation's database was read to: the steps and the model
/// calls numbered below these were read.
#[derive(Default, Serialize, Deserialize)]
struct Rows {
    steps: i64,
    calls: i64,
}

/// Where a transcript was read to, and whether its session has its title.
#[derive(Default, Serialize, Deserialize)]
struct Lines {
    offset: u64,
    titled: bool,
}

/// The latest `last_modified_time` read of a folder's titles.
#[derive(Default, Serialize, Deserialize)]
struct Titles {
    modified: String,
}

/// What `read` finds in one of Antigravity's databases, opened to read.
///
/// Each keeps a write-ahead log, `<file>-wal`, while Antigravity has it
/// open. With no log beside it, all it holds is in the file, which is then
/// read as one that can't change. Read as any other, SQLite makes a log and
/// its index beside it and leaves them: 14 files in Antigravity's folder
/// after reading 7 conversations (measured 2026-11-10). With a log beside
/// it, it's read as any other, log included, and nothing is made.
///
/// SQLite takes no lock on a file it's told can't change. So one that did
/// change while it was read, or that Antigravity opened meanwhile, fails:
/// what was read may be of two moments, and it's read again at the next
/// catch-up.
fn opened<T>(file: &Path, read: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let failed = |error| super::reading(file, error);
    let mut log = file.as_os_str().to_owned();
    log.push("-wal");
    // The file's size and when it was last written, with no log beside it.
    let alone = || {
        let file = std::fs::metadata(file).ok()?;
        (!Path::new(&log).exists()).then_some((file.len(), file.modified().ok()))
    };
    let before = file.to_str().zip(alone());
    let db = match before {
        Some((path, _)) => {
            // What would end the path in an address.
            let path = path
                .replace('%', "%25")
                .replace('?', "%3f")
                .replace('#', "%23");
            Connection::open_with_flags(
                format!("file:{path}?immutable=1"),
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )
        }
        None => Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_ONLY),
    }
    .map_err(failed)?;
    db.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(failed)?;
    let read = read(&db)?;
    match before {
        Some((_, before)) if alone() != Some(before) => {
            Err(super::reading(file, "it changed while it was read"))
        }
        _ => Ok(read),
    }
}

impl Agent for Antigravity {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn folders(&self, home: &Path) -> Vec<PathBuf> {
        vec![home.join(".gemini")]
    }

    fn installed(&self, folder: &Path) -> bool {
        KEPT.iter().any(|kept| folder.join(kept).is_dir())
    }

    fn files(&self, folder: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for kept in KEPT.map(|kept| folder.join(kept)) {
            files.push(kept.join(TITLES));
            for database in super::entries(&kept.join("conversations")) {
                if database
                    .extension()
                    .is_none_or(|extension| extension != "db")
                {
                    continue;
                }
                if let Some(id) = database.file_stem() {
                    files.push(
                        kept.join("brain")
                            .join(id)
                            .join(".system_generated/logs")
                            .join(TRANSCRIPT),
                    );
                }
                files.push(database);
            }
        }
        files
    }

    fn read(&self, file: &Path, cursor: &str) -> Result<Read> {
        match file.file_name().and_then(|name| name.to_str()) {
            Some(TITLES) => titles(file, cursor),
            Some(TRANSCRIPT) => said(file, cursor),
            _ => conversation(file, cursor),
        }
    }

    fn transcript(&self, _native: &str, files: &[PathBuf]) -> Result<Vec<Entry>> {
        let mut conversation = Conversation::default();
        // Its database and its folder's titles hold nothing that was said.
        let Some(file) = files
            .iter()
            .find(|file| file.file_name().is_some_and(|name| name == TRANSCRIPT))
        else {
            return Ok(conversation.entries);
        };
        super::lines(file, 0, |bytes| {
            let Ok(line) = serde_json::from_slice::<Value>(bytes) else {
                return;
            };
            let at = line["created_at"].as_str().and_then(crate::time::millis);
            let step = line["step_index"].as_i64();
            let held = |field: &str| line[field].as_str().unwrap_or_default();
            match line["type"].as_str() {
                Some("USER_INPUT") if line["source"] == "USER_EXPLICIT" => {
                    conversation.say(Kind::User, at, &request(held("content")));
                }
                Some("PLANNER_RESPONSE") => {
                    conversation.say(Kind::Reasoning, at, held("thinking"));
                    conversation.say(Kind::Assistant, at, held("content"));
                    let calls = line["tool_calls"].as_array().into_iter().flatten();
                    for (n, call) in calls.enumerate() {
                        let name = call["name"].as_str().unwrap_or("tool");
                        // The step that answers it, which is all that ties
                        // the two.
                        let answered = step.map(|step| (step + 1 + n as i64).to_string());
                        conversation.call(answered.as_deref(), at, name, call["args"].to_string());
                        // Where a tool that writes a file names it.
                        let named = match name {
                            "replace_file_content" | "write_to_file" => "TargetFile",
                            "write_file" | "edit_file" => "Path",
                            _ => continue,
                        };
                        let file = call["args"][named].as_str();
                        conversation.changed(file.map(str::to_owned));
                    }
                }
                Some("GENERIC") => {
                    if let Some(step) = step {
                        let failed = line["status"] == "ERROR";
                        conversation.answer(&step.to_string(), held("content").to_owned(), failed);
                    }
                }
                Some("CHECKPOINT") => {
                    conversation.say(Kind::Summary, at, summarized(held("content")));
                }
                _ => conversation.say(Kind::System, at, held("content")),
            }
        })?;
        Ok(conversation.entries)
    }

    fn logins(&self, _folder: &Path, _home: &Path) -> Result<Vec<Login>> {
        Ok(Vec::new())
    }

    fn mcp_command(&self, folder: &Path, _home: &Path) -> Option<String> {
        let config = super::json_file(&folder.join("config/mcp_config.json")).ok()??;
        let server = &config["mcpServers"]["turnscope"];
        // One switched off, as `agy mcp disable` does, isn't run.
        if server["disabled"] == true {
            return None;
        }
        Some(server["command"].as_str()?.to_owned())
    }

    fn role(&self, name: &str) -> Role {
        match name {
            "view_file" | "read_file" | "list_dir" | "search_web" => Role::Read,
            "ask_question" => Role::Ask,
            _ => Role::Work,
        }
    }
}

/// What the person typed, of a `USER_INPUT` line's `content`.
fn request(content: &str) -> String {
    match super::inner(content, "USER_REQUEST") {
        Some(request) => request.trim().to_owned(),
        None => super::without_tags(content, &["ADDITIONAL_METADATA", "USER_SETTINGS_CHANGE"]),
    }
}

/// How Antigravity opens a checkpoint's summary, after its number, and what
/// it tells its model after one.
const BEFORE_SUMMARY: &str = "**The earlier parts of this conversation have been truncated";
const AFTER_SUMMARY: &str = "**IMPORTANT: this summary is just for your reference.";

/// A checkpoint's summary without what Antigravity wraps it in: its number,
/// the line saying what it is, and the instruction that ends it.
fn summarized(content: &str) -> &str {
    let mut text = content.trim();
    if let Some((_, numbered)) = text
        .strip_prefix("{{ CHECKPOINT")
        .and_then(|rest| rest.split_once("}}"))
    {
        text = numbered.trim_start();
    }
    if text.starts_with(BEFORE_SUMMARY) {
        text = text.split_once('\n').map_or("", |(_, summary)| summary);
    }
    let end = text.find(AFTER_SUMMARY).unwrap_or(text.len());
    text[..end].trim()
}

/// Read a conversation's database on from `cursor`: when it worked, where,
/// and its model calls.
fn conversation(file: &Path, cursor: &str) -> Result<Read> {
    opened(file, |db| conversation_in(db, file, cursor))
}

fn conversation_in(db: &Connection, file: &Path, cursor: &str) -> Result<Read> {
    let failed = |error| super::reading(file, error);
    let mut cursor: Rows = serde_json::from_str(cursor).unwrap_or_default();
    let mut read = Read::default();
    let Some(native) = file.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(read);
    };
    let session = INFO.session(native);
    // One read transaction, so steps and calls are of one moment.
    let tx = db.unchecked_transaction().map_err(failed)?;

    let mut steps = tx
        .prepare("SELECT idx, metadata FROM steps WHERE idx >= ?1 ORDER BY idx")
        .map_err(failed)?;
    let mut rows = steps.query([cursor.steps]).map_err(failed)?;
    while let Some(row) = rows.next().map_err(failed)? {
        let idx: i64 = row.get(0).map_err(failed)?;
        let metadata: Option<Vec<u8>> = row.get(1).map_err(failed)?;
        cursor.steps = idx + 1;
        if let Some(at) = metadata.as_deref().and_then(made) {
            read.session(&session).saw(at);
        }
    }
    read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
    // One opened and never used is no session.
    if cursor.steps == 0 {
        return Ok(read);
    }

    let about: Option<Vec<u8>> = tx
        .query_row(
            "SELECT data FROM trajectory_metadata_blob WHERE id = 'main'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(failed)?
        .flatten();
    let facts = read.session(&session);
    if let Some(about) = &about {
        facts.cwd = within(about, &[1, 1]).and_then(folder);
        if let Some(began) = bytes(about, 2).and_then(time) {
            facts.saw(began);
        }
    }

    let mut calls = tx
        .prepare("SELECT idx, data FROM gen_metadata WHERE idx >= ?1 ORDER BY idx")
        .map_err(failed)?;
    let mut step = tx
        .prepare("SELECT metadata FROM steps WHERE idx = ?1")
        .map_err(failed)?;
    // The last call read may have had no tokens yet.
    let seen = cursor.calls;
    let mut rows = calls.query([seen - 1]).map_err(failed)?;
    while let Some(row) = rows.next().map_err(failed)? {
        let idx: i64 = row.get(0).map_err(failed)?;
        let data: Option<Vec<u8>> = row.get(1).map_err(failed)?;
        cursor.calls = idx + 1;
        let data = data.unwrap_or_default();
        let Some(usage) = within(&data, &[1, 4]) else {
            continue;
        };
        let count = |field| number(usage, field).unwrap_or(0);
        let tokens = Tokens {
            input: count(2),
            cache_read: count(5),
            cache_write_5m: count(4),
            cache_write_1h: 0,
            output: count(3),
            reasoning: count(9),
        };
        // A call has no tokens until it ends, and none if it fails.
        if tokens == Tokens::default() {
            continue;
        }
        let named = match field(&data, 2) {
            Some(Field::Bytes(mut steps)) => varint(&mut steps),
            Some(Field::Number(step)) => Some(step),
            None => None,
        };
        let at = match named.and_then(|named| i64::try_from(named).ok()) {
            Some(named) => step
                .query_row([named], |row| row.get::<_, Option<Vec<u8>>>(0))
                .optional()
                .map_err(failed)?
                .flatten()
                .as_deref()
                .and_then(made),
            None => None,
        };
        let model = within(&data, &[1, 19])
            .and_then(|model| std::str::from_utf8(model).ok())
            .filter(|model| !model.is_empty());
        let (Some(at), Some(model)) = (at, model) else {
            // Counted when first read, not each time it's read again.
            read.skipped += u64::from(idx >= seen);
            continue;
        };
        let id = bytes(usage, 7)
            .and_then(|id| std::str::from_utf8(id).ok())
            .filter(|id| !id.is_empty())
            .map_or_else(|| format!("{native}/{idx}"), str::to_owned);
        read.respond(Response {
            id,
            session: session.clone(),
            copy: false,
            at,
            provider: "google".to_owned(),
            model: model.to_owned(),
            prompt: tokens.input + tokens.cache_read + tokens.cache_write_5m,
            tokens,
            web_searches: 0,
            priority: false,
            cost: None,
        });
    }
    read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
    Ok(read)
}

/// Read a transcript on from `cursor`: what the person and the models said,
/// and the session's first request as its title.
fn said(file: &Path, cursor: &str) -> Result<Read> {
    let mut cursor: Lines = serde_json::from_str(cursor).unwrap_or_default();
    let mut read = Read::default();
    // `brain/<id>/.system_generated/logs/transcript_full.jsonl`.
    let native = file
        .ancestors()
        .nth(3)
        .and_then(|folder| folder.file_name())
        .and_then(|name| name.to_str());
    let Some(native) = native else {
        return Ok(read);
    };
    let session = INFO.session(native);
    cursor.offset = super::lines(file, cursor.offset, |bytes| {
        let Ok(line) = serde_json::from_slice::<Value>(bytes) else {
            read.skipped += 1;
            return;
        };
        let facts = read.session(&session);
        if let Some(at) = line["created_at"].as_str().and_then(crate::time::millis) {
            facts.saw(at);
        }
        let content = line["content"].as_str().unwrap_or_default();
        match line["type"].as_str() {
            Some("USER_INPUT") if line["source"] == "USER_EXPLICIT" => {
                let request = request(content);
                if !cursor.titled && !request.is_empty() {
                    facts.title(Title::Prompt, &request);
                    cursor.titled = true;
                }
                read.say(&session, &request);
            }
            Some("PLANNER_RESPONSE") => read.say(&session, content),
            _ => {}
        }
    })?;
    read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
    Ok(read)
}

/// Read a folder's titles on from `cursor`: the title Antigravity gave a
/// conversation, and the conversation that began it.
fn titles(file: &Path, cursor: &str) -> Result<Read> {
    opened(file, |db| titles_in(db, file, cursor))
}

fn titles_in(db: &Connection, file: &Path, cursor: &str) -> Result<Read> {
    let failed = |error| super::reading(file, error);
    let mut cursor: Titles = serde_json::from_str(cursor).unwrap_or_default();
    let mut read = Read::default();
    // A time is written as `2026-09-30 09:00:00.5+00:00`, always in UTC, so
    // a later one is the greater text.
    let mut changed = db
        .prepare(
            "SELECT conversation_id, title, parent_conversation_id, last_modified_time
             FROM conversation_summaries
             WHERE last_modified_time >= ?1 AND step_count > 0 AND conversation_id IS NOT NULL",
        )
        .map_err(failed)?;
    let mut rows = changed.query([&cursor.modified]).map_err(failed)?;
    while let Some(row) = rows.next().map_err(failed)? {
        let (native, title, parent, modified): (String, String, String, String) = (
            row.get(0).map_err(failed)?,
            row.get(1).map_err(failed)?,
            row.get(2).map_err(failed)?,
            row.get(3).map_err(failed)?,
        );
        if modified > cursor.modified {
            cursor.modified = modified;
        }
        if title.trim().is_empty() && parent.is_empty() {
            continue;
        }
        let facts = read.session(&INFO.session(&native));
        facts.title(Title::Generated, &title);
        if !parent.is_empty() {
            facts.parent = Some(Parent {
                id: INFO.session(&parent),
                link: Link::Subagent,
            });
        }
    }
    read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
    Ok(read)
}

/// When a step was made, from its `metadata`.
fn made(metadata: &[u8]) -> Option<i64> {
    time(bytes(metadata, 1)?)
}

/// A protobuf timestamp, seconds (#1) and nanoseconds (#2), in Unix
/// milliseconds.
fn time(stamp: &[u8]) -> Option<i64> {
    let seconds = i64::try_from(number(stamp, 1)?).ok()?;
    let nanos = i64::try_from(number(stamp, 2).unwrap_or(0)).ok()?;
    seconds.checked_mul(1000)?.checked_add(nanos / 1_000_000)
}

/// The folder a `file://` address names, its `%20` a space again.
fn folder(address: &[u8]) -> Option<String> {
    let mut path = Vec::new();
    let mut rest = address.strip_prefix(b"file://")?.iter();
    while let Some(&byte) = rest.next() {
        path.push(match byte {
            b'%' => {
                let digits = [*rest.next()?, *rest.next()?];
                u8::from_str_radix(std::str::from_utf8(&digits).ok()?, 16).ok()?
            }
            byte => byte,
        });
    }
    String::from_utf8(path).ok().filter(|path| !path.is_empty())
}

/// A protobuf field's value: a number, or bytes that are text, a message
/// within, or numbers packed together.
enum Field<'a> {
    Number(u64),
    Bytes(&'a [u8]),
}

/// A protobuf message's fields in the order written, each with its number,
/// read without its schema. It ends at the first thing that isn't a field,
/// so bytes that aren't a message give few fields or none.
///
/// Read by hand, over the `prost` crate: with no schema to generate from,
/// `prost` would have every message written out as a struct, for the ten
/// fields read here.
struct Fields<'a>(&'a [u8]);

impl<'a> Iterator for Fields<'a> {
    type Item = (u64, Field<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        let key = varint(&mut self.0)?;
        let value = match key & 7 {
            0 => Field::Number(varint(&mut self.0)?),
            1 => Field::Number(u64::from_le_bytes(take(&mut self.0, 8)?.try_into().ok()?)),
            2 => {
                let length = usize::try_from(varint(&mut self.0)?).ok()?;
                Field::Bytes(take(&mut self.0, length)?)
            }
            5 => Field::Number(u32::from_le_bytes(take(&mut self.0, 4)?.try_into().ok()?).into()),
            _ => return None,
        };
        Some((key >> 3, value))
    }
}

/// The number that starts `bytes`, seven bits to a byte, low bits first, and
/// `bytes` moved past it.
fn varint(bytes: &mut &[u8]) -> Option<u64> {
    let mut number = 0;
    for (n, &byte) in bytes.iter().enumerate().take(10) {
        number |= u64::from(byte & 0x7f) << (7 * n);
        if byte & 0x80 == 0 {
            *bytes = &bytes[n + 1..];
            return Some(number);
        }
    }
    None
}

/// The first `length` of `bytes`, and `bytes` moved past them.
fn take<'a>(bytes: &mut &'a [u8], length: usize) -> Option<&'a [u8]> {
    let (taken, rest) = bytes.split_at_checked(length)?;
    *bytes = rest;
    Some(taken)
}

/// The first field numbered `number` in `message`.
fn field(message: &[u8], number: u64) -> Option<Field<'_>> {
    Fields(message).find_map(|(n, value)| (n == number).then_some(value))
}

fn number(message: &[u8], field_number: u64) -> Option<u64> {
    match field(message, field_number)? {
        Field::Number(number) => Some(number),
        Field::Bytes(_) => None,
    }
}

fn bytes(message: &[u8], field_number: u64) -> Option<&[u8]> {
    match field(message, field_number)? {
        Field::Bytes(bytes) => Some(bytes),
        Field::Number(_) => None,
    }
}

/// The bytes at `path` in `message`: field `path[1]` of the message in
/// field `path[0]`, and so on.
fn within<'a>(message: &'a [u8], path: &[u64]) -> Option<&'a [u8]> {
    path.iter()
        .try_fold(message, |message, &number| bytes(message, number))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A number as protobuf writes one.
    fn written(mut number: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let low = (number & 0x7f) as u8;
            number >>= 7;
            if number == 0 {
                bytes.push(low);
                return bytes;
            }
            bytes.push(low | 0x80);
        }
    }

    /// A field holding a number; none for zero, as protobuf leaves one out.
    fn n(field: u64, value: u64) -> Vec<u8> {
        match value {
            0 => Vec::new(),
            value => [written(field << 3), written(value)].concat(),
        }
    }

    /// A field holding bytes: text, or a message of `parts`.
    fn b(field: u64, parts: &[&[u8]]) -> Vec<u8> {
        let value = parts.concat();
        [written(field << 3 | 2), written(value.len() as u64), value].concat()
    }

    /// A step's `metadata`: made `seconds` and half a second after 1970.
    fn step(seconds: u64) -> Vec<u8> {
        b(1, &[&n(1, seconds), &n(2, 500_000_000)])
    }

    /// A model call's `data`: the steps it names, its message's id, its
    /// model, and its input, output, cache reads and reasoning.
    fn call(steps: &[u8], message: &str, model: &str, tokens: [u64; 4]) -> Vec<u8> {
        let [input, output, cache_read, reasoning] = tokens;
        let usage = [
            n(1, 1318),
            n(2, input),
            n(3, output),
            n(5, cache_read),
            n(6, 24),
            b(7, &[message.as_bytes()]),
            n(9, reasoning),
            n(10, output - reasoning),
        ]
        .concat();
        let chat = [b(4, &[&usage]), b(19, &[model.as_bytes()])].concat();
        [b(2, &[steps]), b(1, &[&chat])].concat()
    }

    /// `agy`'s folder in the home `home`, with the tables its reader reads
    /// and the conversation `c1`: four steps and two model calls, in a
    /// folder whose name has a space.
    fn kept(home: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let kept = home.join(".gemini/antigravity-cli");
        std::fs::create_dir_all(kept.join("conversations")).unwrap();
        let database = kept.join("conversations/c1.db");
        let db = Connection::open(&database).unwrap();
        db.execute_batch(
            "CREATE TABLE steps (idx integer, step_type integer NOT NULL DEFAULT 0, metadata blob,
                 PRIMARY KEY (idx));
             CREATE TABLE gen_metadata (idx integer, data blob, PRIMARY KEY (idx));
             CREATE TABLE trajectory_metadata_blob (id text DEFAULT 'main', data blob,
                 PRIMARY KEY (id));",
        )
        .unwrap();
        for (idx, kind, at) in [(0, 14, 1000), (1, 15, 1001), (2, 132, 1002), (3, 15, 1010)] {
            db.execute(
                "INSERT INTO steps VALUES (?1, ?2, ?3)",
                rusqlite::params![idx, kind, step(at)],
            )
            .unwrap();
        }
        let model = "gemini-3.8-flash";
        for (idx, data) in [
            (0, call(&[1, 2], "bot-1", model, [100, 25, 0, 5])),
            (1, call(&[3], "bot-2", model, [40, 10, 300, 0])),
        ] {
            db.execute(
                "INSERT INTO gen_metadata VALUES (?1, ?2)",
                rusqlite::params![idx, data],
            )
            .unwrap();
        }
        let about = [
            b(1, &[&b(1, &[b"file:///work/my%20app"])]),
            b(2, &[&n(1, 990)]),
        ]
        .concat();
        db.execute(
            "INSERT INTO trajectory_metadata_blob VALUES ('main', ?1)",
            [about],
        )
        .unwrap();

        let transcript = super::super::jsonl(
            kept.join("brain/c1/.system_generated/logs")
                .join(TRANSCRIPT),
            &lines()[..4],
        );

        let titles = kept.join(TITLES);
        Connection::open(&titles)
            .unwrap()
            .execute_batch(
                "CREATE TABLE conversation_summaries (conversation_id text,
                     title text NOT NULL DEFAULT '', step_count integer NOT NULL DEFAULT 0,
                     last_modified_time datetime NOT NULL,
                     parent_conversation_id text NOT NULL DEFAULT '',
                     PRIMARY KEY (conversation_id));
                 INSERT INTO conversation_summaries VALUES
                     ('c1', 'Fix the form', 4, '1970-01-01 00:16:50.5+00:00', ''),
                     ('c2', '', 0, '0001-01-01 00:00:00+00:00', '');",
            )
            .unwrap();
        (database, transcript, titles)
    }

    /// A transcript's lines: the four of `c1`, then three a longer
    /// conversation would add.
    fn lines() -> Vec<Value> {
        let line = |step: u64, source: &str, kind: &str, status: &str, more: Value| {
            let mut line = json!({"step_index": step, "source": source, "type": kind,
                "status": status, "created_at": format!("1970-01-01T00:16:{}Z", 40 + step)});
            line.as_object_mut()
                .unwrap()
                .extend(more.as_object().unwrap().clone());
            line
        };
        vec![
            line(
                0,
                "USER_EXPLICIT",
                "USER_INPUT",
                "DONE",
                json!({"content": "<USER_REQUEST>\nFix the form's validation.\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: 1970-01-01T00:16:40Z.\n</ADDITIONAL_METADATA>"}),
            ),
            line(
                1,
                "MODEL",
                "PLANNER_RESPONSE",
                "DONE",
                json!({"thinking": "The check runs before the field is filled.",
                    "tool_calls": [
                        {"name": "write_to_file", "args": {"TargetFile": "/work/my app/form.ts", "CodeContent": "check()"}},
                        {"name": "run_command", "args": {"CommandLine": "npm test"}}]}),
            ),
            line(
                2,
                "MODEL",
                "GENERIC",
                "DONE",
                json!({"content": "Created file."}),
            ),
            line(
                4,
                "MODEL",
                "PLANNER_RESPONSE",
                "DONE",
                json!({"content": "Fixed: the check runs on submit."}),
            ),
            line(
                5,
                "USER_EXPLICIT",
                "USER_INPUT",
                "DONE",
                json!({"content": "<USER_REQUEST>\nRun the tests.\n</USER_REQUEST>"}),
            ),
            line(
                6,
                "MODEL",
                "PLANNER_RESPONSE",
                "DONE",
                json!({"tool_calls": [{"name": "run_command", "args": {"CommandLine": "npm test"}}]}),
            ),
            line(
                7,
                "MODEL",
                "GENERIC",
                "ERROR",
                json!({"error": "exit 1", "content": "1 test failed."}),
            ),
            line(
                8,
                "SYSTEM",
                "CHECKPOINT",
                "DONE",
                json!({"content": "{{ CHECKPOINT 0 }}\n **The earlier parts of this conversation have been truncated due to its long length. The following content summarizes the truncated context so that you may continue your work. **\n\n\n# User Requests\n1. Fix the form's validation.\n\n**IMPORTANT: this summary is just for your reference. DO NOT ACKNOWLEDGE THIS CHECKPOINT MESSAGE.**"}),
            ),
            line(
                9,
                "SYSTEM",
                "SYSTEM_MESSAGE",
                "DONE",
                json!({"content": "A background task ended."}),
            ),
        ]
    }

    #[test]
    fn a_conversation_is_its_database_its_transcript_and_its_title() {
        let home = tempfile::tempdir().unwrap();
        let (database, transcript, titles) = kept(home.path());
        let folder = Antigravity.folders(home.path()).remove(0);
        // The app's folder isn't there, and its titles are listed unlooked at.
        assert_eq!(
            Antigravity.files(&folder),
            [
                folder.join("antigravity").join(TITLES),
                titles.clone(),
                transcript.clone(),
                database.clone()
            ]
        );

        let read = Antigravity.read(&database, "").unwrap();
        let first = &read.responses["bot-1"];
        assert_eq!(
            first.tokens,
            Tokens {
                input: 100,
                cache_read: 0,
                cache_write_5m: 0,
                cache_write_1h: 0,
                output: 25,
                reasoning: 5
            }
        );
        // Made when the step it names first was.
        assert_eq!(
            (first.at, first.provider.as_str(), first.model.as_str()),
            (1_001_500, "google", "gemini-3.8-flash")
        );
        let second = &read.responses["bot-2"];
        assert_eq!(
            (second.tokens.cache_read, second.prompt, second.at),
            (300, 340, 1_010_500)
        );
        let session = &read.sessions["antigravity:c1"];
        assert_eq!(session.cwd.as_deref(), Some("/work/my app"));
        assert_eq!(
            (session.started, session.last),
            (Some(990_000), Some(1_010_500))
        );

        let said = Antigravity.read(&transcript, "").unwrap();
        assert_eq!(
            said.said,
            [
                (
                    "antigravity:c1".to_owned(),
                    "Fix the form's validation.".to_owned(),
                    None
                ),
                (
                    "antigravity:c1".to_owned(),
                    "Fixed: the check runs on submit.".to_owned(),
                    None
                ),
            ]
        );
        let title = |read: &Read| read.sessions["antigravity:c1"].title.clone().unwrap();
        assert!(title(&said) == (Title::Prompt, "Fix the form's validation.".to_owned()));
        // Antigravity's own title is the better; one it never used has none.
        let titled = Antigravity.read(&titles, "").unwrap();
        assert!(title(&titled) == (Title::Generated, "Fix the form".to_owned()));
        assert_eq!(titled.sessions.len(), 1);
    }

    #[test]
    fn a_call_is_read_again_until_it_has_its_tokens() {
        let home = tempfile::tempdir().unwrap();
        let (database, ..) = kept(home.path());
        let db = Connection::open(&database).unwrap();
        // Still running: its usage has only its model.
        let running = [b(2, &[&[5]]), b(1, &[&b(4, &[&n(1, 1318)])])].concat();
        db.execute_batch("INSERT INTO steps VALUES (5, 15, NULL)")
            .unwrap();
        db.execute("INSERT INTO gen_metadata VALUES (2, ?1)", [running])
            .unwrap();
        let first = Antigravity.read(&database, "").unwrap();
        assert_eq!(first.responses.len(), 2);
        assert_eq!(first.skipped, 0);

        db.execute("UPDATE steps SET metadata = ?1 WHERE idx = 5", [step(1020)])
            .unwrap();
        db.execute(
            "UPDATE gen_metadata SET data = ?1 WHERE idx = 2",
            [call(&[5], "bot-3", "gemini-3.8-flash", [7, 3, 0, 0])],
        )
        .unwrap();
        let later = Antigravity.read(&database, &first.cursor).unwrap();
        assert_eq!(
            later.responses.keys().collect::<Vec<_>>(),
            ["bot-3"],
            "the calls before it aren't read again"
        );
        assert_eq!(later.responses["bot-3"].at, 1_020_500);
        // Read again from there, it's the same response.
        let again = Antigravity.read(&database, &later.cursor).unwrap();
        assert_eq!(again.responses["bot-3"], later.responses["bot-3"]);
    }

    #[test]
    fn reading_leaves_nothing_beside_a_database_and_reads_its_log() {
        // In a folder whose name would end an address.
        let home = tempfile::tempdir().unwrap();
        let home = home.path().join("a #1?");
        let (database, ..) = kept(&home);
        let beside = |ending: &str| {
            let mut path = database.clone().into_os_string();
            path.push(ending);
            PathBuf::from(path)
        };
        // As Antigravity keeps it, and as it leaves it once closed.
        let db = Connection::open(&database).unwrap();
        db.pragma_update(None, "journal_mode", "WAL").unwrap();
        drop(db);
        assert!(!beside("-wal").exists());
        let read = Antigravity.read(&database, "").unwrap();
        assert_eq!(read.responses.len(), 2);
        assert!(!beside("-wal").exists() && !beside("-shm").exists());

        // While Antigravity has it open, what it wrote last is in its log.
        let db = Connection::open(&database).unwrap();
        db.execute("INSERT INTO steps VALUES (5, 15, ?1)", [step(1020)])
            .unwrap();
        db.execute(
            "INSERT INTO gen_metadata VALUES (2, ?1)",
            [call(&[5], "bot-3", "gemini-3.8-flash", [7, 3, 0, 0])],
        )
        .unwrap();
        assert!(beside("-wal").exists());
        let later = Antigravity.read(&database, &read.cursor).unwrap();
        assert!(later.responses.contains_key("bot-3"));

        // Read as one that can't change, one Antigravity opens or writes
        // meanwhile isn't taken for read.
        drop(db);
        assert!(!beside("-wal").exists());
        let opened_meanwhile = opened(&database, |_| {
            std::fs::write(beside("-wal"), "").unwrap();
            Ok(())
        });
        assert!(opened_meanwhile.is_err());
        std::fs::remove_file(beside("-wal")).unwrap();
        let written_meanwhile = opened(&database, |_| {
            let file = std::fs::File::options()
                .write(true)
                .open(&database)
                .unwrap();
            let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
            file.set_modified(later).unwrap();
            Ok(())
        });
        assert!(written_meanwhile.is_err());
        assert!(opened(&database, |_| Ok(())).is_ok());
    }

    #[test]
    fn a_call_with_no_model_or_time_is_counted_once() {
        let home = tempfile::tempdir().unwrap();
        let (database, ..) = kept(home.path());
        let db = Connection::open(&database).unwrap();
        // Tokens, and neither a model nor a step that's there.
        let usage = [n(2, 9), n(3, 1), b(7, &[b"bot-9"])].concat();
        let odd = [b(2, &[&[77]]), b(1, &[&b(4, &[&usage])])].concat();
        db.execute("INSERT INTO gen_metadata VALUES (2, ?1)", [odd])
            .unwrap();
        let first = Antigravity.read(&database, "").unwrap();
        assert_eq!((first.responses.len(), first.skipped), (2, 1));
        assert_eq!(
            Antigravity.read(&database, &first.cursor).unwrap().skipped,
            0
        );
    }

    #[test]
    fn a_conversation_never_used_is_no_session() {
        let home = tempfile::tempdir().unwrap();
        let (database, ..) = kept(home.path());
        Connection::open(&database)
            .unwrap()
            .execute_batch("DELETE FROM steps; DELETE FROM gen_metadata;")
            .unwrap();
        let read = Antigravity.read(&database, "").unwrap();
        assert!(read.sessions.is_empty() && read.responses.is_empty());
    }

    #[test]
    fn a_transcript_read_in_two_parts_says_nothing_twice() {
        let home = tempfile::tempdir().unwrap();
        let (_, transcript, _) = kept(home.path());
        let first = Antigravity.read(&transcript, "").unwrap();
        // A line still being written isn't read until it ends.
        let mut text = std::fs::read_to_string(&transcript).unwrap();
        let more: Vec<String> = lines()[4..].iter().map(Value::to_string).collect();
        text.push_str(&more.join("\n"));
        std::fs::write(&transcript, &text).unwrap();
        let later = Antigravity.read(&transcript, &first.cursor).unwrap();
        // The later part's request is said, and isn't the session's title.
        assert_eq!(later.said.len(), 1);
        assert_eq!(later.said[0].1, "Run the tests.");
        assert!(later.sessions["antigravity:c1"].title.is_none());
        text.push('\n');
        std::fs::write(&transcript, &text).unwrap();
        let whole = Antigravity.read(&transcript, "").unwrap();
        assert_eq!([first.said, later.said].concat(), whole.said);
        let last = Antigravity.read(&transcript, &later.cursor).unwrap();
        assert!(last.said.is_empty() && last.skipped == 0);
    }

    #[test]
    fn a_transcript_puts_each_output_in_the_call_its_step_answers() {
        let home = tempfile::tempdir().unwrap();
        let (database, transcript, titles) = kept(home.path());
        super::super::jsonl(transcript.clone(), &lines());
        // Whichever of a session's files come, and in any order.
        let entries = Antigravity
            .transcript("c1", &[database, titles, transcript])
            .unwrap();
        assert_eq!(
            entries.iter().map(|entry| entry.kind).collect::<Vec<_>>(),
            [
                Kind::User,
                Kind::Reasoning,
                Kind::Tool,
                Kind::Tool,
                Kind::Assistant,
                Kind::User,
                Kind::Tool,
                Kind::Summary,
                Kind::System
            ]
        );
        assert_eq!(entries[0].text, "Fix the form's validation.");
        // The summary, without what Antigravity wraps it in.
        assert_eq!(
            entries[7].text,
            "# User Requests\n1. Fix the form's validation."
        );
        assert_eq!(summarized("Moved the check."), "Moved the check.");
        assert_eq!(entries[0].at, Some(1_000_000));
        let tool = |at: usize| entries[at].tool.as_ref().unwrap();
        assert_eq!(
            (tool(2).name.as_str(), tool(2).output.as_deref()),
            ("write_to_file", Some("Created file."))
        );
        assert_eq!(tool(2).files, ["/work/my app/form.ts"]);
        // The second call's step, 3, is in no line.
        assert_eq!(tool(3).output, None);
        assert_eq!(
            (tool(6).output.as_deref(), tool(6).failed, tool(6).edited),
            (Some("1 test failed."), true, false)
        );
        assert_eq!(Antigravity.role("view_file"), Role::Read);
        assert_eq!(Antigravity.role("run_command"), Role::Work);
    }

    #[test]
    fn a_file_is_changed_by_either_name_its_tools_give_it() {
        let home = tempfile::tempdir().unwrap();
        let (_, transcript, _) = kept(home.path());
        let call = |name: &str, args: Value| json!({"name": name, "args": args});
        let response = json!({
            "step_index": 1,
            "source": "MODEL",
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "created_at": "1970-01-01T00:16:40Z",
            "tool_calls": [
                call("write_file", json!({"Path": "/work/a.md", "Content": "a"})),
                call("edit_file", json!({"Path": "/work/b.md", "Target": "a", "Replacement": "b"})),
                call("replace_file_content", json!({"TargetFile": "/work/c.md"})),
                call("read_file", json!({"Path": "/work/d.md"})),
            ],
        });
        super::super::jsonl(transcript.clone(), &[response]);
        let entries = Antigravity.transcript("c1", &[transcript]).unwrap();
        let changed: Vec<String> = entries
            .iter()
            .filter_map(|entry| entry.tool.as_ref())
            .map(|tool| tool.files.join(" "))
            .collect();
        assert_eq!(changed, ["/work/a.md", "/work/b.md", "/work/c.md", ""]);
        for (name, role) in [
            ("read_file", Role::Read),
            ("ask_question", Role::Ask),
            ("write_file", Role::Work),
        ] {
            assert_eq!(Antigravity.role(name), role);
        }
    }

    #[test]
    fn a_folder_the_gemini_command_line_made_isnt_antigravity() {
        let home = tempfile::tempdir().unwrap();
        let folder = Antigravity.folders(home.path()).remove(0);
        std::fs::create_dir_all(folder.join("tmp")).unwrap();
        std::fs::write(folder.join("settings.json"), "{}").unwrap();
        assert!(!Antigravity.installed(&folder));
        std::fs::create_dir(folder.join("antigravity")).unwrap();
        assert!(Antigravity.installed(&folder));
    }

    #[test]
    fn titles_are_read_on_from_the_latest_change_and_name_a_parent() {
        let home = tempfile::tempdir().unwrap();
        let (.., titles) = kept(home.path());
        let first = Antigravity.read(&titles, "").unwrap();
        Connection::open(&titles)
            .unwrap()
            .execute_batch(
                "INSERT INTO conversation_summaries VALUES
                     ('c3', '', 2, '1970-01-01 00:17:05+00:00', 'c1');",
            )
            .unwrap();
        let later = Antigravity.read(&titles, &first.cursor).unwrap();
        let parent = later.sessions["antigravity:c3"].parent.as_ref().unwrap();
        assert!(parent.id == "antigravity:c1" && parent.link == Link::Subagent);
        assert!(later.sessions["antigravity:c3"].title.is_none());
        // `c1`, the latest read before, is read again, and nothing older.
        assert_eq!(later.sessions.len(), 2);
        let last = Antigravity.read(&titles, &later.cursor).unwrap();
        assert_eq!(last.sessions.keys().collect::<Vec<_>>(), ["antigravity:c3"]);
    }

    #[test]
    fn the_mcp_server_is_the_one_in_the_shared_config_unless_switched_off() {
        let home = tempfile::tempdir().unwrap();
        let folder = Antigravity.folders(home.path()).remove(0);
        assert_eq!(Antigravity.mcp_command(&folder, home.path()), None);
        let config = folder.join("config/mcp_config.json");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        // As `agy mcp add` leaves the file before any server is added.
        std::fs::write(&config, "").unwrap();
        assert_eq!(Antigravity.mcp_command(&folder, home.path()), None);
        let server = |disabled: bool| {
            json!({"mcpServers": {"turnscope":
                {"args": ["mcp"], "command": "/opt/turnscope", "disabled": disabled}}})
            .to_string()
        };
        std::fs::write(&config, server(false)).unwrap();
        assert_eq!(
            Antigravity.mcp_command(&folder, home.path()).as_deref(),
            Some("/opt/turnscope")
        );
        std::fs::write(&config, server(true)).unwrap();
        assert_eq!(Antigravity.mcp_command(&folder, home.path()), None);
        assert!(Antigravity.logins(&folder, home.path()).unwrap().is_empty());
    }

    #[test]
    fn bytes_that_arent_a_message_give_no_fields() {
        // A number, text, and a message within.
        let message = [n(1, 300), b(2, &[b"hi"]), b(3, &[&n(1, 7)])].concat();
        assert_eq!(number(&message, 1), Some(300));
        assert_eq!(bytes(&message, 2), Some(&b"hi"[..]));
        assert_eq!(
            within(&message, &[3]).and_then(|inner| number(inner, 1)),
            Some(7)
        );
        // Asked for as the other kind, or not there.
        assert_eq!((number(&message, 2), bytes(&message, 1)), (None, None));
        assert_eq!(within(&message, &[3, 1]), None);
        assert!(number(&message, 9).is_none());
        // Fixed numbers of four and eight bytes.
        let fixed = [
            &[0x0d][..],
            &7u32.to_le_bytes(),
            &[0x11],
            &9u64.to_le_bytes(),
        ]
        .concat();
        assert_eq!((number(&fixed, 1), number(&fixed, 2)), (Some(7), Some(9)));
        // Text longer than what's left, a number that never ends, and a
        // kind of field there isn't.
        assert_eq!(Fields(&[0x12, 0x05, b'h', b'i']).count(), 0);
        assert_eq!(Fields(&[0x08; 1]).count(), 0);
        assert_eq!(
            Fields(&[
                0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff
            ])
            .count(),
            0
        );
        assert_eq!(Fields(&[0x0b, 0x01]).count(), 0);
        assert_eq!(time(&[n(1, 2), n(2, 999_999_999)].concat()), Some(2_999));
        assert_eq!(
            folder(b"file:///work/a%20b%25").as_deref(),
            Some("/work/a b%")
        );
        assert_eq!(folder(b"https://example.com/a"), None);
        assert_eq!(folder(b"file:///work/%4"), None);
    }
}
