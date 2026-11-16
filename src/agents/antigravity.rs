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
//! **Titles.** A conversation Antigravity gave no title is titled by the
//! first thing the person asked: the first `USER_INPUT` line of its
//! transcript from `USER_EXPLICIT`, which holds what they typed in
//! `<USER_REQUEST>`, then the blocks Antigravity adds, as
//! `<ADDITIONAL_METADATA>`. A line is written once. Of the transcript, only
//! that request and each line's time are read.
//!
//! **Subagents.** `conversation_summaries.parent_conversation_id` names a
//! conversation's parent. That is from the column's name: no conversation
//! here had one.
//!
//! **MCP servers.** `agy` starts one itself. The app doesn't run `agy`:
//! measured 2026-11-11 with Turnscope connected, the app (2.31.0) started
//! `turnscope mcp` from `Contents/Resources/bin/language_server`, a child of
//! its own process, `Antigravity`, with nothing named `agy` above it. So
//! that program names the agent too. Only it, over any process of the app:
//! a person's shell in a terminal the app holds would be under the app as
//! well, and what they run there isn't run by the agent. Where such a shell
//! hangs wasn't measured: none ran, and under the app were the language
//! server and the two MCP servers it had started.
//!
//! The app started the server in the folder `/`, and set no variable that
//! names the conversation or the folder it works in. So in the app the
//! folder isn't known, and the caller's own session is Antigravity's latest
//! anywhere, which a conversation begun with no folder can be.
//!
//! **Not read.** The Antigravity IDE's history, which this Mac doesn't have.
//!
//! **Logins.** The app and `agy` keep one Google sign-in, in the Keychain
//! item with service `gemini` and account `antigravity`. `agy` keeps it
//! with go-keyring, which writes through `/usr/bin/security`, so reading it
//! with that raises no prompt, and marks a value it wrote as base64 with
//! `go-keyring-base64:`. Read on this Mac 2026-11-10, the value was so
//! marked, and inside it JSON: the `token`, with its `access_token`, its
//! `expiry` as a written time with the zone's offset, and `expires_in`; the
//! `auth_method`, `consumer`; the account's email, `id_token_email`; and a
//! `writer_id`. Only the email names the account, so it is the account's id
//! as well as its label.
//!
//! The token had 22 minutes left, hours after Antigravity was signed in: it
//! writes each token it renews back to the item. Turnscope never renews one,
//! so once the last has expired the account waits for Antigravity to be
//! used again.
//!
//! Only a sign-in whose `auth_method` is `consumer`, a Google account's, is
//! read. Google answers a token of Antigravity's other client with limits
//! nothing has touched, CodexBar found (its code, read 2026-11-10), which
//! would pass for the plan's.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Agent, Credential, Info, Login, Read, Response, Secret, Title, Tokens};
use crate::{Error, Result};

pub struct Antigravity;

static INFO: Info = Info {
    id: "antigravity",
    name: "Antigravity",
    command: "agy",
    app: Some("Antigravity.app/Contents/Resources/bin/language_server"),
    mcp_add: &["mcp", "add", "turnscope", "--"],
    mcp_remove: Some(&["mcp", "remove", "turnscope"]),
    folder_var: None,
    charges: false,
    session_var: None,
};

/// The folders of `~/.gemini` that keep history: the app's, and `agy`'s.
const KEPT: [&str; 2] = ["antigravity", "antigravity-cli"];

/// A folder's titles, and a conversation's transcript.
const TITLES: &str = "conversation_summaries.db";
const TRANSCRIPT: &str = "transcript_full.jsonl";

/// The Keychain item that holds the sign-in: its service, and its account.
const SIGN_IN: (&str, &str) = ("gemini", "antigravity");
/// What go-keyring puts before a value it wrote as base64.
const WRAPPED: &str = "go-keyring-base64:";

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
fn opened<T>(file: &Path, read: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
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
    // SQLite ends its message with the address it was given, the path again.
    .map_err(|error| {
        let error = error.to_string();
        super::reading(file, error.split(": file:").next().unwrap_or_default())
    })?;
    let read = db
        .busy_timeout(std::time::Duration::from_secs(2))
        .and_then(|()| read(&db))
        .map_err(|error| super::reading(file, error))?;
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
            Some(TITLES) => titles(file),
            Some(TRANSCRIPT) => transcript(file, cursor),
            _ => conversation(file, cursor),
        }
    }

    fn logins(&self, folder: &Path, home: &Path) -> Result<Vec<Login>> {
        // Where it isn't installed, the Keychain isn't asked.
        if !self.installed(folder) {
            return Ok(Vec::new());
        }
        let (service, account) = SIGN_IN;
        let credential = match super::keychain(home, service, Some(account))? {
            Some(kept) => signed_in(&kept).map_err(|why| {
                Error::Failed(format!("reading the Keychain item {service}: {why}"))
            })?,
            None => None,
        };
        Ok(vec![Login {
            provider: "google".to_owned(),
            credential,
        }])
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
}

/// The sign-in the Keychain item's value `kept` holds, when it's one to a
/// Google account. Why not, for a value that doesn't read: taking that for
/// signed out would sign its account out.
fn signed_in(kept: &[u8]) -> std::result::Result<Option<Credential>, String> {
    let kept = std::str::from_utf8(kept).map_err(|error| error.to_string())?;
    let kept = kept
        .trim()
        .strip_prefix(WRAPPED)
        .ok_or("go-keyring didn't write it as base64")?;
    let kept = base64::engine::general_purpose::STANDARD
        .decode(kept)
        .map_err(|error| error.to_string())?;
    let kept: Value = serde_json::from_slice(&kept).map_err(|error| error.to_string())?;
    let token = kept["token"]["access_token"]
        .as_str()
        .filter(|token| !token.is_empty());
    let (Some(token), Some("consumer")) = (token, kept["auth_method"].as_str()) else {
        return Ok(None);
    };
    let email = kept["id_token_email"]
        .as_str()
        .filter(|email| !email.is_empty());
    Ok(Some(Credential {
        provider: "google",
        secret: Secret::new(token),
        key: false,
        expires: kept["token"]["expiry"]
            .as_str()
            .and_then(crate::time::millis),
        identity: email.map(|email| (email.to_owned(), Some(email.to_owned()))),
        plan: None,
    }))
}

/// What the person typed, of a `USER_INPUT` line's `content`.
fn request(content: &str) -> String {
    match super::inner(content, "USER_REQUEST") {
        Some(request) => request.trim().to_owned(),
        None => super::without_tags(content, &["ADDITIONAL_METADATA", "USER_SETTINGS_CHANGE"]),
    }
}

/// Read a conversation's database on from `cursor`: when it worked, where,
/// and its model calls.
fn conversation(file: &Path, cursor: &str) -> Result<Read> {
    opened(file, |db| conversation_in(db, file, cursor))
}

fn conversation_in(db: &Connection, file: &Path, cursor: &str) -> rusqlite::Result<Read> {
    let mut cursor: Rows = serde_json::from_str(cursor).unwrap_or_default();
    let mut read = Read::default();
    let Some(native) = file.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(read);
    };
    let session = INFO.session(native);
    // One read transaction, so steps and calls are of one moment.
    let tx = db.unchecked_transaction()?;

    let mut steps = tx.prepare("SELECT idx, metadata FROM steps WHERE idx >= ?1 ORDER BY idx")?;
    let mut rows = steps.query([cursor.steps])?;
    while let Some(row) = rows.next()? {
        let idx: i64 = row.get(0)?;
        let metadata: Option<Vec<u8>> = row.get(1)?;
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
        .optional()?
        .flatten();
    let facts = read.session(&session);
    if let Some(about) = &about {
        facts.cwd = within(about, &[1, 1]).and_then(folder);
        if let Some(began) = bytes(about, 2).and_then(time) {
            facts.saw(began);
        }
    }

    let mut calls =
        tx.prepare("SELECT idx, data FROM gen_metadata WHERE idx >= ?1 ORDER BY idx")?;
    let mut step = tx.prepare("SELECT metadata FROM steps WHERE idx = ?1")?;
    // The last call read may have had no tokens yet.
    let seen = cursor.calls;
    let mut rows = calls.query([seen - 1])?;
    while let Some(row) = rows.next()? {
        let idx: i64 = row.get(0)?;
        let data: Option<Vec<u8>> = row.get(1)?;
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
                .optional()?
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

/// Read a transcript on from `cursor`: when the session worked, and its
/// first request as its title.
fn transcript(file: &Path, cursor: &str) -> Result<Read> {
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
        if !cursor.titled && line["type"] == "USER_INPUT" && line["source"] == "USER_EXPLICIT" {
            let request = request(line["content"].as_str().unwrap_or_default());
            if !request.is_empty() {
                facts.title(Title::Prompt, &request);
                cursor.titled = true;
            }
        }
    })?;
    read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
    Ok(read)
}

/// Read a folder's titles: the title Antigravity gave a conversation, and
/// the conversation that began it.
fn titles(file: &Path) -> Result<Read> {
    opened(file, |db| titles_in(db, file))
}

fn titles_in(db: &Connection, file: &Path) -> rusqlite::Result<Read> {
    let mut read = Read::default();
    // The app's titles list `agy`'s conversations too, so a conversation's
    // database is in either folder. A row whose conversation has none in
    // either gives no session: a title alone would make one with no times,
    // shown as last active in 1970. Whether Antigravity leaves such a row
    // behind when it deletes a conversation wasn't measured.
    let has_database = |native: &str| {
        let folder = file.parent().and_then(Path::parent);
        folder.is_some_and(|folder| {
            KEPT.iter().any(|kept| {
                let database = format!("conversations/{native}.db");
                folder.join(kept).join(database).is_file()
            })
        })
    };
    // Every row is read at each read, over reading on from the latest
    // `last_modified_time` seen, which rested on what wasn't measured: that
    // a title written later moves the time, that a row is committed at most
    // 10 seconds after one stamped later, and that every time is in UTC.
    // Measured 2026-11-14 with a release build on a made-up table of 5,000
    // conversations, each titled and with a database: a catch-up after one
    // title changed ran 691M instructions more than one with nothing to
    // read (1,185M over 494M), about 0.1 s. Reading on from the latest ran
    // 21M more. Reading the rows took 51 to 56 ms of that, 4 of them the
    // query, and writing 5,000 sessions the rest. The history measured
    // above had 8 conversations.
    let mut titled = db.prepare(
        "SELECT conversation_id, title, parent_conversation_id FROM conversation_summaries
         WHERE step_count > 0 AND conversation_id IS NOT NULL",
    )?;
    let mut rows = titled.query([])?;
    while let Some(row) = rows.next()? {
        let (native, title, parent): (String, String, String) =
            (row.get(0)?, row.get(1)?, row.get(2)?);
        if (title.trim().is_empty() && parent.is_empty()) || !has_database(&native) {
            continue;
        }
        let facts = read.session(&INFO.session(&native));
        facts.title(Title::Generated, &title);
        if !parent.is_empty() {
            facts.parent = Some(INFO.session(&parent));
        }
    }
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
    /// conversation would add. The reader takes each line's time, and what a
    /// request holds.
    fn lines() -> Vec<Value> {
        let line = |step: u64, source: &str, kind: &str, content: &str| {
            json!({"step_index": step, "source": source, "type": kind, "content": content,
                "created_at": format!("1970-01-01T00:16:{}Z", 40 + step)})
        };
        vec![
            line(
                0,
                "USER_EXPLICIT",
                "USER_INPUT",
                "<USER_REQUEST>\nFix the form's validation.\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nThe current local time is: 1970-01-01T00:16:40Z.\n</ADDITIONAL_METADATA>",
            ),
            line(1, "MODEL", "PLANNER_RESPONSE", ""),
            line(2, "MODEL", "GENERIC", "Created file."),
            line(4, "MODEL", "PLANNER_RESPONSE", "Fixed."),
            line(
                5,
                "USER_EXPLICIT",
                "USER_INPUT",
                "<USER_REQUEST>\nRun the tests.\n</USER_REQUEST>",
            ),
            line(8, "MODEL", "PLANNER_RESPONSE", "They pass."),
            line(9, "SYSTEM", "SYSTEM_MESSAGE", "A background task ended."),
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
        assert_eq!(session.last, Some(1_010_500));

        let asked = Antigravity.read(&transcript, "").unwrap();
        let title = |read: &Read| read.sessions["antigravity:c1"].title.clone().unwrap();
        assert!(title(&asked) == (Title::Prompt, "Fix the form's validation.".to_owned()));
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
    fn a_transcript_read_in_two_parts_is_titled_by_its_first_request_alone() {
        let home = tempfile::tempdir().unwrap();
        let (_, transcript, _) = kept(home.path());
        let first = Antigravity.read(&transcript, "").unwrap();
        // A line still being written isn't read until it ends.
        let mut text = std::fs::read_to_string(&transcript).unwrap();
        let more: Vec<String> = lines()[4..].iter().map(Value::to_string).collect();
        text.push_str(&more.join("\n"));
        std::fs::write(&transcript, &text).unwrap();
        let later = Antigravity.read(&transcript, &first.cursor).unwrap();
        // The later part's request isn't the session's title, and its lines
        // say when the session last worked: all but the last, not yet ended.
        let session = &later.sessions["antigravity:c1"];
        assert!(session.title.is_none());
        assert_eq!(session.last, Some(1_008_000));
        text.push('\n');
        std::fs::write(&transcript, &text).unwrap();
        let last = Antigravity.read(&transcript, &later.cursor).unwrap();
        assert_eq!(last.sessions["antigravity:c1"].last, Some(1_009_000));
        assert_eq!(last.skipped, 0);
    }

    #[test]
    fn a_tag_the_person_typed_is_theirs_and_antigravitys_own_are_taken_out() {
        let asked =
            "Log each <USER_SETTINGS_CHANGE> block, then the <ADDITIONAL_METADATA> after it.";
        let added = "<ADDITIONAL_METADATA>\nThe current local time is: 1970-01-01T00:16:40Z.\n</ADDITIONAL_METADATA>";
        // In their request, and in content with none.
        for content in [
            format!("<USER_REQUEST>\n{asked}\n</USER_REQUEST>\n{added}"),
            format!("{added}\n{asked}"),
        ] {
            assert_eq!(request(&content), asked);
        }
        assert_eq!(
            request(&format!(
                "<USER_SETTINGS_CHANGE>\nThe model is now Gemini 3.8 Flash.\n</USER_SETTINGS_CHANGE>\n{added}"
            )),
            ""
        );
    }

    #[test]
    fn a_folder_the_gemini_command_line_made_isnt_antigravity() {
        let home = tempfile::tempdir().unwrap();
        let folder = Antigravity.folders(home.path()).remove(0);
        std::fs::create_dir_all(folder.join("tmp")).unwrap();
        std::fs::write(folder.join("settings.json"), "{}").unwrap();
        assert!(!Antigravity.installed(&folder));
        assert!(Antigravity.logins(&folder, home.path()).unwrap().is_empty());
        std::fs::create_dir(folder.join("antigravity")).unwrap();
        assert!(Antigravity.installed(&folder));
    }

    #[test]
    fn titles_name_a_parent_and_pass_over_a_conversation_with_no_database() {
        let home = tempfile::tempdir().unwrap();
        let (database, _, titles) = kept(home.path());
        // `c3` has a database, in the app's folder; `c4` has none.
        let apps = home.path().join(".gemini/antigravity/conversations");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::copy(&database, apps.join("c3.db")).unwrap();
        Connection::open(&titles)
            .unwrap()
            .execute_batch(
                "INSERT INTO conversation_summaries VALUES
                     ('c3', '', 2, '1970-01-01 00:17:05+00:00', 'c1'),
                     ('c4', 'Since deleted', 2, '1970-01-01 00:17:05+00:00', '');",
            )
            .unwrap();
        let read = Antigravity.read(&titles, "").unwrap();
        assert_eq!(
            read.sessions["antigravity:c3"].parent.as_deref(),
            Some("antigravity:c1")
        );
        assert!(read.sessions["antigravity:c3"].title.is_none());
        assert!(!read.sessions.contains_key("antigravity:c4"));
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

    #[test]
    fn a_sign_in_is_read_from_what_go_keyring_wrote() {
        use base64::engine::general_purpose::STANDARD;
        let wrapped = |kept: &Value| format!("{WRAPPED}{}\n", STANDARD.encode(kept.to_string()));
        let kept = json!({
            "token": {
                "access_token": "made-up-token",
                "token_type": "Bearer",
                "refresh_token": "made-up-refresh",
                "expiry": "2026-10-01T12:00:00.123456-05:00",
                "expires_in": 3599,
            },
            "auth_method": "consumer",
            "id_token_email": "me@example.com",
            "writer_id": "made-up-writer",
        });
        let credential = signed_in(wrapped(&kept).as_bytes()).unwrap().unwrap();
        assert_eq!(
            (
                credential.provider,
                credential.secret.expose(),
                credential.key
            ),
            ("google", "made-up-token", false)
        );
        assert_eq!(
            credential.expires,
            crate::time::millis("2026-10-01T17:00:00.123456Z")
        );
        assert_eq!(
            credential.identity,
            Some((
                "me@example.com".to_owned(),
                Some("me@example.com".to_owned())
            ))
        );

        // Another way of signing in isn't read, nor a sign-in with no token.
        let mut other = kept.clone();
        other["auth_method"] = json!("enterprise");
        assert!(signed_in(wrapped(&other).as_bytes()).unwrap().is_none());
        let mut out = kept.clone();
        out["token"]["access_token"] = json!("");
        assert!(signed_in(wrapped(&out).as_bytes()).unwrap().is_none());

        // A value that doesn't read isn't taken for signed out.
        assert!(signed_in(kept.to_string().as_bytes()).is_err());
        assert!(signed_in(format!("{WRAPPED}not base64").as_bytes()).is_err());
        assert!(signed_in(format!("{WRAPPED}{}", STANDARD.encode("not JSON")).as_bytes()).is_err());
    }
}
