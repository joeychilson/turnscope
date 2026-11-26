//! Antigravity.
//!
//! Its app and its command line, `agy`, share `~/.gemini`: the app keeps its
//! history in `antigravity/`, `agy` in `antigravity-cli/`, and both their MCP
//! servers in `config/mcp_config.json`. So `~/.gemini` is its one folder, and
//! `connect` runs `agy` once.
//!
//! Antigravity publishes no schema for what it writes as protobuf. Measured
//! 2026-11-10 on 8 conversations with 196 model calls, by `agy` 1.2.11 and
//! the app 2.31.0, at the field numbers CodexBar, tokscale, CodeBurn and
//! agent-walker read (`#1.#4` is field 4 of the message in field 1). In each
//! folder, a conversation `<id>` is:
//!
//! - `conversations/<id>.db`: a row per step in `steps`, a row per model call
//!   in `gen_metadata`, and in `trajectory_metadata_blob` when it began (#2)
//!   and the folder it works in (#1.#1, a `file://` address).
//! - `brain/<id>/.system_generated/logs/transcript_full.jsonl`, a line per
//!   step as the model saw it (`transcript.jsonl` cuts long text short, and
//!   `chunks/` cuts lines in two).
//! - its row of `conversation_summaries.db`, with the title Antigravity gave
//!   it (3 of the 8 had one).
//!
//! **Usage.** A `gen_metadata` row's `data` holds its call's usage at #1.#4:
//! input (#2), output (#3), cache writes (#4), cache reads (#5), reasoning
//! (#9) and the message's id (#7). Input leaves out cache reads (it was the
//! smaller in 173 of the 174 calls that read the cache), and output includes
//! reasoning (in all 193 calls with tokens). The model is #1.#19, as
//! `gemini-3.8-flash`. A call is made when the step it names first (#2) is,
//! at that step's `metadata` #1.
//!
//! **Not read.** The Antigravity IDE's history, which this Mac doesn't have.

use std::borrow::Cow;
use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Agent, Credential, Info, Login, Read, Refresh, Response, Secret, Title, Tokens};
use crate::Result;

pub struct Antigravity;

const APP: &str = "Antigravity.app/Contents/Resources/bin/language_server";

static INFO: Info = Info {
    id: "antigravity",
    name: "Antigravity",
    command: "agy",
    // The app 2.31.0 started `turnscope mcp` from this program (measured
    // 2026-11-11). Not any process of the app, as a shell in its terminal
    // would be one too.
    app: Some(APP),
    mcp_add: &["mcp", "add", "turnscope", "--"],
    mcp_remove: Some(&["mcp", "remove", "turnscope"]),
    folder_var: None,
    charges: false,
    // The app sets no variable naming the conversation.
    session_var: None,
    login_file: Some(FALLBACK),
};

const KEPT: [&str; 2] = ["antigravity", "antigravity-cli"];

const TITLES: &str = "conversation_summaries.db";
const TRANSCRIPT: &str = "transcript_full.jsonl";

const SIGN_IN: (&str, &str) = ("gemini", "antigravity");
/// Where Antigravity keeps its sign-in when the Keychain fails it (2.34.0's
/// `language_server`, read 2026-11-22). None was seen written, so it's taken
/// to hold the Keychain item's JSON without the base64.
const FALLBACK: &str = "jetski-standalone-oauth-token";
const WRAPPED: &str = "go-keyring-base64:";

#[derive(Default, Serialize, Deserialize)]
struct Rows {
    calls: i64,
}

#[derive(Default, Serialize, Deserialize)]
struct Lines {
    offset: u64,
    titled: bool,
}

impl Agent for Antigravity {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn folders(&self, home: &Path) -> Vec<PathBuf> {
        vec![home.join(".gemini")]
    }

    /// The Gemini command line keeps its settings in `~/.gemini` too.
    fn installed(&self, folder: &Path) -> bool {
        KEPT.iter().any(|kept| folder.join(kept).is_dir())
    }

    fn files(&self, folder: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        for kept in KEPT.map(|kept| folder.join(kept)) {
            files.push(kept.join(TITLES));
            let brain = kept.join("brain");
            for database in super::entries(&kept.join("conversations")) {
                if database
                    .extension()
                    .is_none_or(|extension| extension != "db")
                {
                    continue;
                }
                if let Some(id) = database.file_stem() {
                    files.push(
                        brain
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
            Some(TITLES) => super::opened(file, |db| titles(db, file)),
            Some(TRANSCRIPT) => transcript(file, cursor),
            _ => super::opened(file, |db| conversation(db, file, cursor)),
        }
    }

    fn logins(&self, folder: &Path, home: &Path) -> Result<Vec<Login>> {
        let (service, account) = SIGN_IN;
        let kept = super::keychain(home, service, Some(account), signed_in).map(Option::flatten);
        let file = super::json_file(&folder.join(FALLBACK)).map(|file| credential(&file));
        let mut credential = super::fresher(kept, file)?;
        if let Some(refresh) = credential.as_mut().and_then(|c| c.refresh.as_mut()) {
            refresh.programs = programs(home);
        }
        Ok(vec![Login {
            provider: "google".to_owned(),
            credential,
        }])
    }

    fn mcp_command(&self, folder: &Path, _home: &Path) -> Option<String> {
        let config = super::json_file(&folder.join("config/mcp_config.json")).ok()?;
        let server = &config["mcpServers"]["turnscope"];
        // One switched off, as `agy mcp disable` does, isn't run.
        if server["disabled"] == true {
            return None;
        }
        Some(server["command"].as_str()?.to_owned())
    }
}

/// The sign-in the Keychain item's value `kept` holds, base64 JSON as
/// go-keyring writes it (read 2026-11-10). An error for a value that doesn't
/// read: taken for signed out, it would sign its account out.
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
    Ok(credential(&kept))
}

/// The sign-in `kept` holds, the Keychain item's JSON or the file's: `token`
/// (`access_token`, `refresh_token`, `expiry`), `auth_method` and
/// `id_token_email`, the account's only name. From the app 2.33.1 on, a
/// renewed token isn't written back (measured 2026-11-21), so the token
/// expires within an hour of sign-in and is renewed in memory
/// (`providers::google`). Only `auth_method` `consumer`, a Google account's,
/// is read: Google answers a token of Antigravity's other client with
/// limits nothing has touched (CodexBar's code, read 2026-11-10).
fn credential(kept: &Value) -> Option<Credential> {
    let (Some(token), Some("consumer")) = (
        super::str_at(kept, "/token/access_token"),
        super::str_at(kept, "/auth_method"),
    ) else {
        return None;
    };
    Some(Credential {
        provider: "google",
        secret: Secret::new(token),
        key: false,
        expires: super::str_at(kept, "/token/expiry").and_then(crate::time::millis),
        identity: super::str_at(kept, "/id_token_email")
            .map(|email| (email.to_owned(), Some(email.to_owned()))),
        plan: None,
        refresh: super::str_at(kept, "/token/refresh_token").map(|token| Refresh {
            token: Secret::new(token),
            programs: Vec::new(),
        }),
    })
}

/// Where Antigravity's programs, which hold the client its sign-in is renewed
/// with, may be: where the app and `agy` were on this Mac (2026-11-22), and
/// the person's own `~/Applications`.
fn programs(home: &Path) -> Vec<PathBuf> {
    vec![
        Path::new("/Applications").join(APP),
        home.join("Applications").join(APP),
        home.join(".local/bin").join(INFO.command),
    ]
}

/// What a `USER_INPUT` line's `content` holds besides what the person typed
/// in `<USER_REQUEST>`.
const ADDED: &[&str] = &["ADDITIONAL_METADATA", "USER_SETTINGS_CHANGE"];

/// Read a conversation's database on from `cursor`: where it worked, and
/// its model calls.
fn conversation(db: &Connection, file: &Path, cursor: &str) -> rusqlite::Result<Read> {
    let mut cursor: Rows = serde_json::from_str(cursor).unwrap_or_default();
    let mut read = Read::default();
    let Some(native) = file.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(read);
    };
    let session = INFO.session(native);
    // One read transaction, so steps and calls are of one moment.
    let tx = db.unchecked_transaction()?;

    // One opened and never used is no session.
    let used: bool = tx.query_row("SELECT EXISTS (SELECT 1 FROM steps)", [], |row| row.get(0))?;
    if !used {
        return Ok(read);
    }

    read.session(&session).cwd = tx
        .query_row(
            "SELECT data FROM trajectory_metadata_blob WHERE id = 'main'",
            [],
            |row| {
                Ok(row
                    .get_ref(0)?
                    .as_bytes_or_null()?
                    .and_then(|about| bytes(bytes(about, 1)?, 1))
                    .and_then(folder))
            },
        )
        .optional()?
        .flatten();

    let mut calls =
        tx.prepare("SELECT idx, data FROM gen_metadata WHERE idx >= ?1 ORDER BY idx")?;
    let mut step = tx.prepare("SELECT metadata FROM steps WHERE idx = ?1")?;
    // The last call read is read again, as it may have had no tokens yet:
    // the 3 calls without were each their conversation's last, still running
    // or failed, and still the only 3 of 254 later that day.
    let seen = cursor.calls;
    let mut rows = calls.query([seen - 1])?;
    while let Some(row) = rows.next()? {
        let idx: i64 = row.get(0)?;
        let data = row.get_ref(1)?.as_bytes_or_null()?.unwrap_or(&[]);
        cursor.calls = idx + 1;
        let Some(chat) = bytes(data, 1) else { continue };
        let Some(usage) = bytes(chat, 4) else {
            continue;
        };
        let count = |n| number(usage, n).unwrap_or(0);
        let tokens = Tokens {
            input: count(2),
            output: count(3),
            cache_write_5m: count(4),
            cache_read: count(5),
            reasoning: count(9),
            cache_write_1h: 0,
        };
        // Still running, or failed: read again, not skipped.
        if tokens == Tokens::default() {
            continue;
        }
        let (model, message_id) = (
            bytes(chat, 19).and_then(text),
            bytes(usage, 7).and_then(text),
        );
        let named = match field(data, 2) {
            Some(Field::Bytes(mut steps)) => varint(&mut steps),
            Some(Field::Number(step)) => Some(step),
            None => None,
        };
        let at = match named.and_then(|named| i64::try_from(named).ok()) {
            Some(named) => step
                .query_row([named], |row| {
                    Ok(row.get_ref(0)?.as_bytes_or_null()?.and_then(made))
                })
                .optional()?
                .flatten(),
            None => None,
        };
        let (Some(at), Some(model)) = (at, model) else {
            // Counted when first read, not each time it's read again.
            read.skipped += u64::from(idx >= seen);
            continue;
        };
        let id = message_id.map_or_else(|| format!("{native}/{idx}"), str::to_owned);
        read.respond(Response {
            id,
            session: session.clone(),
            copy: false,
            at,
            // Antigravity signs in to Google alone, so the Claude and GPT
            // models its plan serves are `google`'s too. Its 2.34.0 program
            // names Claude's as Anthropic does, which then take Anthropic's
            // list price (`prices`); none was seen in history.
            provider: "google".to_owned(),
            model: model.to_owned(),
            prompt: tokens.prompt(),
            tokens,
            web_searches: 0,
            priority: false,
            cost: None,
        });
    }
    read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
    Ok(read)
}

/// Read a transcript on from `cursor` for its session's title: its first
/// `USER_INPUT` line from `USER_EXPLICIT`. Once it has that, nothing more is
/// read.
fn transcript(file: &Path, cursor: &str) -> Result<Read> {
    let mut read = Read {
        cursor: cursor.to_owned(),
        ..Read::default()
    };
    let mut cursor: Lines = serde_json::from_str(cursor).unwrap_or_default();
    // `brain/<id>/.system_generated/logs/transcript_full.jsonl`.
    let native = file
        .ancestors()
        .nth(3)
        .and_then(|folder| folder.file_name())
        .and_then(|name| name.to_str());
    let (Some(native), false) = (native, cursor.titled) else {
        return Ok(read);
    };
    #[derive(Deserialize)]
    struct Entry<'a> {
        #[serde(rename = "type", borrow)]
        kind: Option<Cow<'a, str>>,
        #[serde(borrow)]
        source: Option<Cow<'a, str>>,
        #[serde(default, deserialize_with = "super::lenient")]
        content: Option<String>,
    }
    let session = INFO.session(native);
    cursor.offset = super::try_lines(file, cursor.offset, |bytes| {
        let Ok(line) = serde_json::from_slice::<Entry<'_>>(bytes) else {
            read.skipped += 1;
            return Ok(ControlFlow::Continue(()));
        };
        if line.kind.as_deref() == Some("USER_INPUT")
            && line.source.as_deref() == Some("USER_EXPLICIT")
            && let Some(content) = line.content
        {
            let request = super::typed_in(&content, "USER_REQUEST", ADDED);
            if !request.is_empty() {
                read.session(&session).title(Title::Prompt, &request);
                cursor.titled = true;
                return Ok(ControlFlow::Break(()));
            }
        }
        Ok(ControlFlow::Continue(()))
    })?;
    read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
    Ok(read)
}

/// Read a folder's titles: the title Antigravity gave a conversation, and
/// the conversation that began it, `parent_conversation_id`. That column is
/// read by its name alone: no conversation measured had one.
fn titles(db: &Connection, file: &Path) -> rusqlite::Result<Read> {
    let mut read = Read::default();
    // The app's titles list `agy`'s conversations too, so a conversation's
    // database is in either folder. A row with none in either gives no
    // session, which would have a title and no usage.
    let known: HashSet<String> = file
        .parent()
        .and_then(Path::parent)
        .into_iter()
        .flat_map(|folder| KEPT.map(|kept| folder.join(kept).join("conversations")))
        .flat_map(|dir| super::entries(&dir))
        .filter(|path| path.extension().is_some_and(|ext| ext == "db"))
        .filter_map(|path| path.file_stem()?.to_str().map(str::to_owned))
        .collect();
    if known.is_empty() {
        return Ok(read);
    }
    // Every row is read at each read, over reading on from the latest
    // `last_modified_time`, which rests on that time's meaning, not measured.
    // On a made-up table of 5,000 titled conversations (the real one had 8),
    // a read after one title changed took about 0.1 s (691M instructions,
    // against 21M reading on; release build, 2026-11-14).
    let mut titled = db.prepare(
        "SELECT conversation_id, title, parent_conversation_id FROM conversation_summaries
         WHERE step_count > 0 AND conversation_id IS NOT NULL
           AND (trim(title, ' ' || char(9, 10, 13)) != '' OR parent_conversation_id != '')",
    )?;
    let mut rows = titled.query([])?;
    while let Some(row) = rows.next()? {
        let native = row.get_ref(0)?.as_str()?;
        if !known.contains(native) {
            continue;
        }
        let title = row.get_ref(1)?.as_str()?;
        let parent = row.get_ref(2)?.as_str()?;
        let facts = read.session(&INFO.session(native));
        facts.title(Title::Generated, title);
        if !parent.is_empty() {
            facts.parent = Some(INFO.session(parent));
        }
    }
    Ok(read)
}

fn made(metadata: &[u8]) -> Option<i64> {
    let stamp = bytes(metadata, 1)?;
    let seconds = i64::try_from(number(stamp, 1)?).ok()?;
    let nanos = i64::try_from(number(stamp, 2).unwrap_or(0)).ok()?;
    seconds.checked_mul(1000)?.checked_add(nanos / 1_000_000)
}

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

enum Field<'a> {
    Number(u64),
    Bytes(&'a [u8]),
}

/// Hand-parsed over `prost`: with no schema to generate from, `prost` would
/// still require handwritten structs for the ~10 fields read here.
struct Fields<'a>(&'a [u8]);

impl<'a> Iterator for Fields<'a> {
    type Item = (u64, Field<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        let key = varint(&mut self.0)?;
        let number = key >> 3;
        if number == 0 {
            return None;
        }
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
        Some((number, value))
    }
}

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

fn take<'a>(bytes: &mut &'a [u8], length: usize) -> Option<&'a [u8]> {
    let (taken, rest) = bytes.split_at_checked(length)?;
    *bytes = rest;
    Some(taken)
}

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

fn text(bytes: &[u8]) -> Option<&str> {
    std::str::from_utf8(bytes)
        .ok()
        .filter(|text| !text.is_empty())
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

        let transcript = crate::agents::jsonl(
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
    /// conversation would add. The reader takes what the first request holds.
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
        let again = Antigravity.read(&database, &later.cursor).unwrap();
        assert_eq!(again.responses["bot-3"], later.responses["bot-3"]);
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
    fn a_transcript_is_read_until_its_first_request() {
        let home = tempfile::tempdir().unwrap();
        let (_, transcript, _) = kept(home.path());
        let first = Antigravity.read(&transcript, "").unwrap();
        let mut text = std::fs::read_to_string(&transcript).unwrap();
        for line in &lines()[4..] {
            text.push_str(&format!("{line}\n"));
        }
        std::fs::write(&transcript, &text).unwrap();
        let later = Antigravity.read(&transcript, &first.cursor).unwrap();
        assert!(later.sessions.is_empty());
        assert_eq!(later.cursor, first.cursor);
    }

    #[test]
    fn content_with_no_request_loses_what_antigravity_added() {
        let content = "<USER_SETTINGS_CHANGE>\nThe model is now Gemini 3.8 Flash.\n</USER_SETTINGS_CHANGE>\n<ADDITIONAL_METADATA>\nThe current local time is: 1970-01-01T00:16:40Z.\n</ADDITIONAL_METADATA>\nRun the tests.";
        assert_eq!(
            super::super::typed_in(content, "USER_REQUEST", super::ADDED),
            "Run the tests."
        );
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
        // Text longer than what's left, a number that never ends, and a
        // kind of field there isn't.
        assert_eq!(Fields(&[0x12, 0x05, b'h', b'i']).count(), 0);
        assert_eq!(
            Fields(&[[0x08].as_slice(), &[0xff; 11]].concat()).count(),
            0
        );
        assert_eq!(Fields(&[0x0b, 0x01]).count(), 0);
        // Fixed numbers of four and eight bytes are read past.
        let fixed = [
            &[0x0d][..],
            &7u32.to_le_bytes(),
            &[0x11],
            &9u64.to_le_bytes(),
        ]
        .concat();
        assert_eq!((number(&fixed, 1), number(&fixed, 2)), (Some(7), Some(9)));
        assert_eq!(folder(b"file:///work/%4"), None);
    }

    #[test]
    fn a_sign_in_is_read_from_what_go_keyring_wrote_or_the_file() {
        use base64::engine::general_purpose::STANDARD;
        let home = tempfile::tempdir().unwrap();
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
        let refresh = credential.refresh.as_ref().unwrap();
        assert_eq!(refresh.token.expose(), "made-up-refresh");

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

        // The file is read as the item is, and one half written isn't taken
        // for signed out.
        let folder = Antigravity.folders(home.path()).remove(0);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(FALLBACK), kept.to_string()).unwrap();
        let logins = Antigravity.logins(&folder, home.path()).unwrap();
        let from_file = logins[0].credential.as_ref().unwrap();
        assert_eq!(from_file.secret.expose(), "made-up-token");
        assert_eq!(
            from_file.refresh.as_ref().unwrap().programs,
            programs(home.path())
        );
        std::fs::write(folder.join(FALLBACK), "{").unwrap();
        assert!(Antigravity.logins(&folder, home.path()).is_err());
    }
}
