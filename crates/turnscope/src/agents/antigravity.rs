//! Antigravity's app and its command line, `agy`, share `~/.gemini`: the
//! app keeps its history in `antigravity/` there, `agy` in
//! `antigravity-cli/`, and both their MCP servers in
//! `config/mcp_config.json`. So `~/.gemini` is its one folder, over a folder
//! for each: `connect` then runs `agy` once, and a second folder stays what
//! it is for the other agents, a second account's.
//!
//! Antigravity publishes no schema for what it writes as protobuf. What
//! follows was measured on 2026-11-10 on 8 conversations with 196 model
//! calls, written by `agy` 1.2.11 and the app 2.31.0. The field numbers are
//! the ones CodexBar, tokscale, CodeBurn and agent-walker read. `#1.#4` is
//! field 4 of the message in field 1.
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
//!   short, and `chunks/` holds the same lines cut every 100 KiB, mid-line.
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
//! that step's `metadata` holds the time (#1), and held the same message id
//! in all 193.
//!
//! **Not read.** The Antigravity IDE's history, which this Mac doesn't have.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use base64::Engine as _;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Agent, Credential, Info, Login, Read, Response, Secret, Title, Tokens};
use crate::Result;

pub struct Antigravity;

static INFO: Info = Info {
    id: "antigravity",
    name: "Antigravity",
    command: "agy",
    // The app doesn't run `agy`: it started `turnscope mcp` from this
    // program, a child of its process `Antigravity` (2.31.0, measured
    // 2026-11-11). Only this program, over any process of the app: a
    // person's shell in a terminal the app holds would be under the app too
    // (not measured: none ran).
    app: Some("Antigravity.app/Contents/Resources/bin/language_server"),
    mcp_add: &["mcp", "add", "turnscope", "--"],
    mcp_remove: Some(&["mcp", "remove", "turnscope"]),
    folder_var: None,
    charges: false,
    // The app set no variable that names the conversation.
    session_var: None,
};

/// The folders of `~/.gemini` that keep history: the app's, and `agy`'s.
const KEPT: [&str; 2] = ["antigravity", "antigravity-cli"];

const TITLES: &str = "conversation_summaries.db";
const TRANSCRIPT: &str = "transcript_full.jsonl";

/// The Keychain item, service and account, where the app and `agy` keep
/// their one Google sign-in.
const SIGN_IN: (&str, &str) = ("gemini", "antigravity");
/// What go-keyring puts before a value it wrote as base64.
const WRAPPED: &str = "go-keyring-base64:";

/// Where a conversation's database was read to: the model calls numbered
/// below this were read.
#[derive(Default, Serialize, Deserialize)]
struct Rows {
    calls: i64,
}

/// Where a transcript was read to, and whether its session has its title.
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
            Some(TITLES) => super::opened(file, |db| titles(db, file)),
            Some(TRANSCRIPT) => transcript(file, cursor),
            _ => super::opened(file, |db| conversation(db, file, cursor)),
        }
    }

    fn logins(&self, _folder: &Path, home: &Path) -> Result<Vec<Login>> {
        let (service, account) = SIGN_IN;
        let mut credential = super::keychain(home, service, Some(account), signed_in)?.flatten();
        // The Keychain item still says whose account it is.
        if let Some(credential) = &mut credential
            && let Some((token, ports)) = running()
        {
            credential.secret = token;
            credential.expires = None;
            credential.app_ports = ports;
        }
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
///
/// Read 2026-11-10, the value was base64 JSON: `token` with its
/// `access_token` and `expiry`, `auth_method`, and `id_token_email`, the
/// only name of the account, so its id and its label. The app 2.31.0 wrote
/// each token it renewed back to the item (one had 22 minutes left, hours
/// after sign-in); 2.33.1 doesn't, so while it runs it's asked instead
/// ([`running`]). Turnscope never renews a token, so once the last has
/// expired and the app is closed, the account waits for it to be opened.
///
/// Only `auth_method` `consumer`, a Google account's, is read: Google
/// answers a token of Antigravity's other client with limits nothing has
/// touched, which would pass for the plan's (CodexBar's code, read
/// 2026-11-10).
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
        app_ports: Vec::new(),
    }))
}

/// The app's `language_server` while it runs, which answers for the app's
/// sign-in on 127.0.0.1: its token for that, from its command line, and the
/// ports it listens on, from `lsof`. Only the person's own processes are
/// looked at: another's app answers for another's account.
///
/// Asked in the Keychain item's place, as the app 2.33.1 renews its sign-in
/// without writing it back: measured 2026-11-21, the item was last written
/// the day before the app started, and 17 hours on the app still hadn't.
/// Chosen over renewing the expired token as the app does, which takes the
/// app's OAuth client secret from its binary and acts as the app toward
/// Google, where Turnscope only reads.
fn running() -> Option<(Secret, Vec<u16>)> {
    let wait = Duration::from_secs(5);
    let mut ps = Command::new("/bin/ps");
    ps.args(["-x", "-ww", "-o", "pid=,args="]);
    let table = crate::process::output_within(&mut ps, wait).ok()?;
    let (pid, token) = server(&String::from_utf8_lossy(&table.stdout))?;
    let mut lsof = Command::new("/usr/sbin/lsof");
    lsof.args([
        "-nP",
        "-a",
        "-p",
        &pid.to_string(),
        "-i4TCP",
        "-sTCP:LISTEN",
        "-Fn",
    ]);
    let open = crate::process::output_within(&mut lsof, wait).ok()?;
    let ports = listening(&String::from_utf8_lossy(&open.stdout));
    (!ports.is_empty()).then_some((token, ports))
}

/// The process and token of the app's `language_server` in `table`, rows of
/// `pid args` as `ps` lists them. The token is the one after
/// `--csrf_token`, as 2.33.1 passes it.
fn server(table: &str) -> Option<(u32, Secret)> {
    let app = INFO.app?;
    table.lines().find_map(|row| {
        let mut words = row.split_whitespace();
        let pid = words.next()?.parse().ok()?;
        if !Path::new(words.next()?).ends_with(app) {
            return None;
        }
        let token = words.skip_while(|word| *word != "--csrf_token").nth(1)?;
        Some((pid, Secret::new(token)))
    })
}

/// The ports `lsof -F n` lists that 127.0.0.1 reaches, in lines such as
/// `n127.0.0.1:52353`: the token is sent to no other. The server listens on
/// several, and only one is the service.
fn listening(lsof: &str) -> Vec<u16> {
    lsof.lines()
        .filter_map(|line| {
            let port = line
                .strip_prefix("n127.0.0.1:")
                .or_else(|| line.strip_prefix("n*:"))?;
            port.parse().ok()
        })
        .collect()
}

/// What the person typed, of a `USER_INPUT` line's `content`: it's in
/// `<USER_REQUEST>`, then come the blocks Antigravity adds, as
/// `<ADDITIONAL_METADATA>`.
fn request(content: &str) -> String {
    match super::inner(content, "USER_REQUEST") {
        Some(request) => request.trim().to_owned(),
        None => super::without_tags(content, &["ADDITIONAL_METADATA", "USER_SETTINGS_CHANGE"]),
    }
}

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

    let about: Option<Vec<u8>> = tx
        .query_row(
            "SELECT data FROM trajectory_metadata_blob WHERE id = 'main'",
            [],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    read.session(&session).cwd = about
        .as_deref()
        .and_then(|about| within(about, &[1, 1]))
        .and_then(folder);

    let mut calls =
        tx.prepare("SELECT idx, data FROM gen_metadata WHERE idx >= ?1 ORDER BY idx")?;
    let mut step = tx.prepare("SELECT metadata FROM steps WHERE idx = ?1")?;
    // The last call read may have had no tokens yet, so it's read again: the
    // 3 calls without tokens were each their conversation's last, as one
    // still running or one that failed is, and were still the only 3 of 254
    // later that day.
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
        // Still running, or failed: it's read again, as said above, and not
        // skipped for having no model or time yet.
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
            // Antigravity signs in to Google only. The Claude and GPT models
            // it serves from the same plan weren't measured: they're
            // `google`'s too, where they have no price.
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

/// Read a transcript on from `cursor` for its session's title: the first
/// thing the person asked, its first `USER_INPUT` line from
/// `USER_EXPLICIT`. A line is written once. Once it has that, nothing more
/// is read.
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
    let session = INFO.session(native);
    cursor.offset = super::lines(file, cursor.offset, |bytes| {
        if cursor.titled {
            return;
        }
        let Ok(line) = serde_json::from_slice::<Value>(bytes) else {
            read.skipped += 1;
            return;
        };
        if line["type"] == "USER_INPUT" && line["source"] == "USER_EXPLICIT" {
            let request = request(line["content"].as_str().unwrap_or_default());
            if !request.is_empty() {
                read.session(&session).title(Title::Prompt, &request);
                cursor.titled = true;
            }
        }
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
    // database is in either folder. A row whose conversation has none in
    // either gives no session, which would have a title and no usage.
    // Whether Antigravity leaves such a row behind when it deletes a
    // conversation wasn't measured.
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
    // `last_modified_time` seen, which rested on three things not measured:
    // that a later title moves the time, that a row is committed at most
    // 10 s after one stamped later, and that every time is UTC. Measured
    // 2026-11-14, release build, on a made-up table of 5,000 titled
    // conversations (the real one had 8): a catch-up after one title changed
    // ran 691M instructions more than one with nothing to read, about 0.1 s;
    // reading on from the latest ran 21M more.
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
        assert_eq!(request(content), "Run the tests.");
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

    #[test]
    fn the_running_app_is_its_language_server_with_its_token_and_ports() {
        let table = "\
              310 /Applications/Antigravity.app/Contents/MacOS/Antigravity
              315 /opt/Other.app/Contents/Resources/bin/language_server --csrf_token other-token
              320 /Applications/Antigravity.app/Contents/Resources/bin/language_server --standalone --csrf_token made-up-token --app_data_dir antigravity
        ";
        let (pid, token) = server(table).unwrap();
        assert_eq!((pid, token.expose()), (320, "made-up-token"));
        // Without its token, it can't be asked.
        let tokenless = "320 /Applications/Antigravity.app/Contents/Resources/bin/language_server";
        assert!(server(tokenless).is_none());

        // Only ports 127.0.0.1 reaches are asked: not one on another
        // address, where another program may hold the same port at
        // 127.0.0.1.
        let lsof = "p320\nf7\nn127.0.0.1:52353\nf8\nn*:52360\nf9\nn192.168.1.5:52370\n";
        assert_eq!(listening(lsof), [52353, 52360]);
    }
}
