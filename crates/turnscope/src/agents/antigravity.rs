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
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use super::{
    Agent, Client, Credential, Info, Login, Read, Refresh, Response, Secret, Title, Tokens,
};
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

/// How Antigravity's app names itself to Google's Cloud Code API.
macro_rules! user_agent {
    ($os:literal, $arch:literal) => {
        concat!(
            "antigravity/2.31.0 (aidev_client; os_type=",
            $os,
            "; arch=",
            $arch,
            ")"
        )
    };
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const USER_AGENT: &str = user_agent!("darwin", "arm64");
#[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
const USER_AGENT: &str = user_agent!("darwin", "amd64");
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
const USER_AGENT: &str = user_agent!("windows", "arm64");
#[cfg(all(target_os = "windows", not(target_arch = "aarch64")))]
const USER_AGENT: &str = user_agent!("windows", "amd64");
#[cfg(all(
    not(target_os = "macos"),
    not(target_os = "windows"),
    target_arch = "aarch64"
))]
const USER_AGENT: &str = user_agent!("linux", "arm64");
#[cfg(all(
    not(target_os = "macos"),
    not(target_os = "windows"),
    not(target_arch = "aarch64")
))]
const USER_AGENT: &str = user_agent!("linux", "amd64");

static CLIENT: Client = Client {
    name: INFO.name,
    user_agent: USER_AGENT,
    id: "ANTIGRAVITY",
};

/// The OAuth client Antigravity's sign-in is renewed with: its id. Its
/// secret is in the app's program.
const CLIENT_ID: &str = "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";

/// How a client secret begins, and how long one is.
const SECRET: (&[u8], usize) = (b"GOCSPX-", 35);

/// The most of a program read for its client, so a program that isn't
/// what it should be can't ask for more: its `__TEXT,__rodata` was 12 MiB
/// and its load commands 4 KiB in 2.34.0, and reading it for the secrets
/// took 34 ms (measured 2026-11-22, a release build).
const MOST: usize = 64 * 1024 * 1024;

const KEPT: [&str; 2] = ["antigravity", "antigravity-cli"];

const TITLES: &str = "conversation_summaries.db";
const TRANSCRIPT: &str = "transcript_full.jsonl";

const SIGN_IN: (&str, &str) = ("gemini", "antigravity");
/// Where Antigravity keeps its sign-in when the Keychain fails it (2.34.0's
/// `language_server`, read 2026-11-22). None was seen written, so it's taken
/// to hold the Keychain item's JSON without the base64.
const FALLBACK: &str = "jetski-standalone-oauth-token";
const WRAPPED: &str = "go-keyring-base64:";

/// Cap on unresolved pending call indices kept in the cursor so a corrupt
/// conversation can't grow the cursor without bound.
const MAX_PENDING: usize = 64;

#[derive(Default, Serialize, Deserialize)]
struct Rows {
    calls: i64,
    #[serde(default)]
    pending: Vec<i64>,
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
        let kept = super::keychain(home, service, Some(account), |kept| signed_in(kept, home))
            .map(Option::flatten);
        let file = super::json_file(&folder.join(FALLBACK)).map(|file| credential(&file, home));
        let credential = super::fresher(kept, file)?;
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
fn signed_in(kept: &[u8], home: &Path) -> std::result::Result<Option<Credential>, String> {
    let kept = std::str::from_utf8(kept).map_err(|error| error.to_string())?;
    let kept = kept
        .trim()
        .strip_prefix(WRAPPED)
        .ok_or("go-keyring didn't write it as base64")?;
    let kept = base64::engine::general_purpose::STANDARD
        .decode(kept)
        .map_err(|error| error.to_string())?;
    let kept: Value = serde_json::from_slice(&kept).map_err(|error| error.to_string())?;
    Ok(credential(&kept, home))
}

/// The sign-in `kept` holds, the Keychain item's JSON or the file's: `token`
/// (`access_token`, `refresh_token`, `expiry`), `auth_method` and
/// `id_token_email`, the account's only name. From the app 2.33.1 on, a
/// renewed token isn't written back (measured 2026-11-21), so the token
/// expires within an hour of sign-in and is renewed in memory
/// (`providers::google`). Only `auth_method` `consumer`, a Google account's,
/// is read: Google answers a token of Antigravity's other client with
/// limits nothing has touched (CodexBar's code, read 2026-11-10).
fn credential(kept: &Value, home: &Path) -> Option<Credential> {
    let (Some(token), Some("consumer")) = (
        super::str_at(kept, "/token/access_token"),
        super::str_at(kept, "/auth_method"),
    ) else {
        return None;
    };
    let refresh = super::str_at(kept, "/token/refresh_token").map(|token| {
        let paths = programs(home);
        Refresh {
            token: Secret::new(token),
            client_id: CLIENT_ID,
            secrets: Box::new(move || {
                paths
                    .iter()
                    .find_map(|program| secrets(program))
                    .ok_or_else(|| "no Antigravity program holds its client".to_owned())
            }),
        }
    });
    Some(Credential {
        provider: "google",
        secret: Secret::new(token),
        key: false,
        expires: super::str_at(kept, "/token/expiry").and_then(crate::time::millis),
        identity: super::str_at(kept, "/id_token_email")
            .map(|email| (email.to_owned(), Some(email.to_owned()))),
        plan: None,
        client: Some(&CLIENT),
        refresh,
    })
}

/// Where Antigravity's programs, which hold the client its sign-in is renewed
/// with, may be: the person's own `~/Applications` and `~/.local/bin/agy`
/// first, then system `/Applications` (measured 2026-11-22).
fn programs(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join("Applications").join(APP),
        home.join(".local/bin").join(INFO.command),
        Path::new("/Applications").join(APP),
    ]
}

/// The client secrets `program` holds, in the order it holds them, if it's
/// Antigravity's: if it holds its client's id.
fn secrets(program: &Path) -> Option<Vec<Secret>> {
    let strings = rodata(program)?;
    let id = CLIENT_ID.as_bytes();
    strings.windows(id.len()).any(|at| at == id).then_some(())?;
    let (begins, length) = SECRET;
    let mut secrets: Vec<Secret> = Vec::new();
    for at in 0..strings.len().saturating_sub(length - 1) {
        let candidate = &strings[at..at + length];
        let rest = &candidate[begins.len()..];
        if candidate.starts_with(begins)
            && rest
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(byte))
            && let Ok(text) = std::str::from_utf8(candidate)
            && !secrets.iter().any(|secret| secret.expose() == text)
        {
            secrets.push(Secret::new(text));
        }
    }
    (!secrets.is_empty()).then_some(secrets)
}

/// The `__TEXT,__rodata` section of `program`, a 64-bit Mach-O or a
/// universal/fat Mach-O holding one (Antigravity 2.34.0 and `agy`, read
/// 2026-11-22).
fn rodata(program: &Path) -> Option<Vec<u8>> {
    let mut file = File::open(program).ok()?;
    let mut head = [0u8; 8];
    file.read_exact(&mut head).ok()?;
    let le = u32::from_le_bytes(head[..4].try_into().ok()?);
    if le == 0xFEED_FACF {
        return rodata_at(&mut file, 0);
    }
    let be = u32::from_be_bytes(head[..4].try_into().ok()?);
    let count = usize::try_from(u32::from_be_bytes(head[4..8].try_into().ok()?)).ok()?;
    if count == 0 || count > 16 {
        return None;
    }
    let offsets: Vec<u64> = match be {
        // `FAT_MAGIC`: 20-byte big-endian `fat_arch` entries, slice offset at +8.
        0xCAFE_BABE => {
            let mut table = vec![0u8; count * 20];
            file.read_exact(&mut table).ok()?;
            table
                .as_chunks::<20>()
                .0
                .iter()
                .map(|entry| {
                    u64::from(u32::from_be_bytes([
                        entry[8], entry[9], entry[10], entry[11],
                    ]))
                })
                .collect()
        }
        // `FAT_MAGIC_64`: 32-byte big-endian `fat_arch_64` entries, slice offset at +8.
        0xCAFE_BABF => {
            let mut table = vec![0u8; count * 32];
            file.read_exact(&mut table).ok()?;
            table
                .as_chunks::<32>()
                .0
                .iter()
                .map(|entry| {
                    u64::from_be_bytes([
                        entry[8], entry[9], entry[10], entry[11], entry[12], entry[13], entry[14],
                        entry[15],
                    ])
                })
                .collect()
        }
        _ => return None,
    };
    offsets
        .into_iter()
        .find_map(|offset| rodata_at(&mut file, offset))
}

fn rodata_at(file: &mut File, base: u64) -> Option<Vec<u8>> {
    fn word(bytes: &[u8], at: usize) -> Option<usize> {
        let word = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?);
        usize::try_from(word).ok()
    }
    fn name(bytes: &[u8]) -> &[u8] {
        bytes.split(|byte| *byte == 0).next().unwrap_or_default()
    }
    file.seek(SeekFrom::Start(base)).ok()?;
    let mut header = [0; 32];
    file.read_exact(&mut header).ok()?;
    // `MH_MAGIC_64`, and the size of the load commands that follow.
    let size = word(&header, 20).filter(|size| *size <= MOST)?;
    if word(&header, 0)? != 0xFEED_FACF {
        return None;
    }
    let mut commands = vec![0; size];
    file.read_exact(&mut commands).ok()?;
    let mut at = 0;
    while let (Some(kind), Some(cmd_size)) = (word(&commands, at), word(&commands, at + 4)) {
        let end = at.checked_add(cmd_size.max(8))?.min(commands.len());
        // `LC_SEGMENT_64`: 72 bytes, then 80 for each of its sections: its
        // name, its segment's, its address, size and offset.
        if kind == 0x19
            && let Some(sections) = word(&commands, at + 64)
        {
            for index in 0..sections {
                let section = at.checked_add(72)?.checked_add(index.checked_mul(80)?)?;
                if section.checked_add(52)? > end {
                    break;
                }
                let named = commands.get(section..section + 52)?;
                if name(&named[..16]) == b"__rodata" && name(&named[16..32]) == b"__TEXT" {
                    let size = u64::from_le_bytes(named[40..48].try_into().ok()?);
                    let size = usize::try_from(size).ok().filter(|size| *size <= MOST)?;
                    let offset = u64::try_from(word(named, 48)?).ok()?;
                    file.seek(SeekFrom::Start(base.checked_add(offset)?)).ok()?;
                    let mut strings = vec![0; size];
                    file.read_exact(&mut strings).ok()?;
                    return Some(strings);
                }
            }
        }
        at = end;
    }
    None
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

    let mut by_idx = tx.prepare("SELECT data FROM gen_metadata WHERE idx = ?1")?;
    let mut calls =
        tx.prepare("SELECT idx, data FROM gen_metadata WHERE idx >= ?1 ORDER BY idx")?;
    let mut step = tx.prepare("SELECT metadata FROM steps WHERE idx = ?1")?;

    let mut next_pending = Vec::new();
    let mut take_row = |idx: i64, data: &[u8]| -> rusqlite::Result<()> {
        let Some(call) = parse_call(data) else {
            next_pending.push(idx);
            return Ok(());
        };
        // Still running, or failed: read again, not skipped.
        if call.tokens == Tokens::default() {
            next_pending.push(idx);
            return Ok(());
        }
        let step_time = match call.step.and_then(|named| i64::try_from(named).ok()) {
            Some(named) => match step
                .query_row([named], |row| {
                    Ok(match row.get_ref(0)?.as_bytes_or_null()? {
                        None => StepTime::Pending,
                        Some(meta) => made(meta).map_or(StepTime::Missing, StepTime::At),
                    })
                })
                .optional()?
            {
                Some(time) => time,
                None => StepTime::Missing,
            },
            None => StepTime::Missing,
        };
        match (step_time, call.model) {
            (StepTime::At(at), Some(model)) => {
                let id = call
                    .message_id
                    .map_or_else(|| format!("{native}/{idx}"), str::to_owned);
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
                    prompt: call.tokens.prompt(),
                    tokens: call.tokens,
                    web_searches: 0,
                    priority: false,
                    cost: None,
                });
            }
            // Step exists with NULL metadata while tokens already landed: wait
            // for its timestamp instead of permanently counting it as skipped.
            (StepTime::Pending, Some(_)) => next_pending.push(idx),
            _ => read.skipped += 1,
        }
        Ok(())
    };

    for idx in std::mem::take(&mut cursor.pending) {
        if idx >= cursor.calls {
            continue;
        }
        if let Some(data) = by_idx
            .query_row([idx], |row| {
                Ok(row.get_ref(0)?.as_bytes_or_null()?.unwrap_or(&[]).to_vec())
            })
            .optional()?
        {
            take_row(idx, &data)?;
        }
    }

    let mut rows = calls.query([cursor.calls])?;
    while let Some(row) = rows.next()? {
        let idx: i64 = row.get(0)?;
        let data = row.get_ref(1)?.as_bytes_or_null()?.unwrap_or(&[]);
        cursor.calls = idx + 1;
        take_row(idx, data)?;
    }

    if next_pending.len() > MAX_PENDING {
        let drop = next_pending.len() - MAX_PENDING;
        next_pending.drain(..drop);
    }
    cursor.pending = next_pending;
    read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
    Ok(read)
}

enum StepTime {
    At(i64),
    Pending,
    Missing,
}

struct Call<'a> {
    tokens: Tokens,
    model: Option<&'a str>,
    message_id: Option<&'a str>,
    step: Option<u64>,
}

fn parse_call(data: &[u8]) -> Option<Call<'_>> {
    let mut chat = None;
    let mut step = None;
    for (n, field) in Fields(data) {
        match (n, field) {
            (1, Field::Bytes(bytes)) if chat.is_none() => chat = Some(bytes),
            (2, Field::Bytes(mut steps)) if step.is_none() => step = varint(&mut steps),
            (2, Field::Number(first)) if step.is_none() => step = Some(first),
            _ => {}
        }
    }
    let mut usage = None;
    let mut model = None;
    for (n, field) in Fields(chat?) {
        match (n, field) {
            (4, Field::Bytes(bytes)) if usage.is_none() => usage = Some(bytes),
            (19, Field::Bytes(bytes)) if model.is_none() => model = text(bytes),
            _ => {}
        }
    }
    let mut tokens = Tokens::default();
    let mut message_id = None;
    for (n, field) in Fields(usage?) {
        match (n, field) {
            (2, Field::Number(v)) => tokens.input = v,
            (3, Field::Number(v)) => tokens.output = v,
            (4, Field::Number(v)) => tokens.cache_write_5m = v,
            (5, Field::Number(v)) => tokens.cache_read = v,
            (7, Field::Bytes(bytes)) if message_id.is_none() => message_id = text(bytes),
            (9, Field::Number(v)) => tokens.reasoning = v,
            _ => {}
        }
    }
    Some(Call {
        tokens,
        model,
        message_id,
        step,
    })
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
        #[serde(borrow)]
        content: Option<&'a RawValue>,
    }
    let session = INFO.session(native);
    cursor.offset = super::try_lines(file, cursor.offset, |bytes| {
        let Ok(line) = serde_json::from_slice::<Entry<'_>>(bytes) else {
            read.skipped += 1;
            return Ok(ControlFlow::Continue(()));
        };
        if line.kind.as_deref() == Some("USER_INPUT")
            && line.source.as_deref() == Some("USER_EXPLICIT")
            && let Some(content) = super::parsed::<Cow<'_, str>>(line.content)
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
    let (mut seconds, mut nanos) = (None, 0u64);
    for (n, field) in Fields(stamp) {
        match (n, field) {
            (1, Field::Number(v)) if seconds.is_none() => seconds = Some(v),
            (2, Field::Number(v)) if nanos == 0 => nanos = v,
            _ => {}
        }
    }
    let seconds = i64::try_from(seconds?).ok()?;
    let nanos = i64::try_from(nanos).ok()?;
    seconds.checked_mul(1000)?.checked_add(nanos / 1_000_000)
}

fn folder(address: &[u8]) -> Option<String> {
    let rest = address.strip_prefix(b"file://")?;
    let rest = match rest.get(..10) {
        Some(head) if head[..9].eq_ignore_ascii_case(b"localhost") && head[9] == b'/' => &rest[9..],
        _ => rest,
    };
    if !rest.starts_with(b"/") {
        return None;
    }
    let mut path = Vec::with_capacity(rest.len());
    let mut rest = rest.iter();
    while let Some(&byte) = rest.next() {
        path.push(match byte {
            b'%' => {
                let digits = [*rest.next()?, *rest.next()?];
                u8::from_str_radix(std::str::from_utf8(&digits).ok()?, 16).ok()?
            }
            byte => byte,
        });
    }
    let path = String::from_utf8(path).ok()?;
    let path = match path.as_bytes() {
        [b'/', drive, b':', b'/', ..] | [b'/', drive, b':'] if drive.is_ascii_alphabetic() => {
            path[1..].to_owned()
        }
        _ => path,
    };
    (!path.is_empty()).then_some(path)
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

#[cfg(test)]
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
        assert!(again.responses.is_empty());
    }

    #[test]
    fn concurrent_pending_calls_and_missing_step_timestamps_are_not_dropped_or_skipped() {
        let home = tempfile::tempdir().unwrap();
        let (database, ..) = kept(home.path());
        let db = Connection::open(&database).unwrap();
        // Call 2 is still running (zero tokens), call 3 already has tokens but
        // its step still has NULL metadata, while call 4 has finished.
        let running = [b(2, &[&[5]]), b(1, &[&b(4, &[&n(1, 1318)])])].concat();
        db.execute_batch(
            "INSERT INTO steps VALUES (5, 15, NULL);
             INSERT INTO steps VALUES (6, 15, NULL);",
        )
        .unwrap();
        db.execute("INSERT INTO steps VALUES (7, 15, ?1)", [step(1030)])
            .unwrap();
        db.execute("INSERT INTO gen_metadata VALUES (2, ?1)", [running])
            .unwrap();
        db.execute(
            "INSERT INTO gen_metadata VALUES (3, ?1)",
            [call(
                &[6],
                "bot-pending-step",
                "gemini-3.8-flash",
                [8, 2, 0, 0],
            )],
        )
        .unwrap();
        db.execute(
            "INSERT INTO gen_metadata VALUES (4, ?1)",
            [call(&[7], "bot-4", "gemini-3.8-flash", [10, 4, 0, 0])],
        )
        .unwrap();

        let first = Antigravity.read(&database, "").unwrap();
        assert_eq!(first.skipped, 0);
        assert!(first.responses.contains_key("bot-4"));
        assert!(!first.responses.contains_key("bot-pending-step"));

        // Now both earlier pending calls complete.
        db.execute("UPDATE steps SET metadata = ?1 WHERE idx = 5", [step(1020)])
            .unwrap();
        db.execute("UPDATE steps SET metadata = ?1 WHERE idx = 6", [step(1025)])
            .unwrap();
        db.execute(
            "UPDATE gen_metadata SET data = ?1 WHERE idx = 2",
            [call(&[5], "bot-2-done", "gemini-3.8-flash", [7, 3, 0, 0])],
        )
        .unwrap();
        let later = Antigravity.read(&database, &first.cursor).unwrap();
        let mut ids: Vec<&str> = later.responses.keys().map(String::as_str).collect();
        ids.sort();
        assert_eq!(ids, ["bot-2-done", "bot-pending-step"]);
        assert_eq!(later.skipped, 0);
        assert_eq!(later.responses["bot-2-done"].at, 1_020_500);
        assert_eq!(later.responses["bot-pending-step"].at, 1_025_500);
        let settled = Antigravity.read(&database, &later.cursor).unwrap();
        assert!(settled.responses.is_empty());
        assert_eq!(settled.skipped, 0);
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
        assert_eq!(
            folder(b"file://localhost/work/my%20app").as_deref(),
            Some("/work/my app")
        );
        assert_eq!(
            folder(b"file:///C:/work/my%20app").as_deref(),
            Some("C:/work/my app")
        );
        assert_eq!(
            folder(b"file:///c%3A/work/app").as_deref(),
            Some("c:/work/app")
        );
        assert_eq!(
            folder(b"file://localhost/C:/work/app").as_deref(),
            Some("C:/work/app")
        );
        assert_eq!(folder(b"file://other-host/share"), None);
    }

    /// A 64-bit Mach-O whose one section, `__TEXT,__rodata`, holds `strings`.
    fn program(strings: &[u8]) -> Vec<u8> {
        let name = |name: &str| {
            let mut padded = [0; 16];
            padded[..name.len()].copy_from_slice(name.as_bytes());
            padded
        };
        let commands: u32 = 72 + 80;
        let mut program = Vec::new();
        for word in [0xFEED_FACF_u32, 0x0100_000C, 0, 2, 1, commands, 0, 0] {
            program.extend(word.to_le_bytes());
        }
        program.extend(0x19_u32.to_le_bytes());
        program.extend(commands.to_le_bytes());
        program.extend(name("__TEXT"));
        program.extend([0; 40]);
        program.extend(1_u32.to_le_bytes());
        program.extend(0_u32.to_le_bytes());
        program.extend(name("__rodata"));
        program.extend(name("__TEXT"));
        program.extend(0_u64.to_le_bytes());
        program.extend((strings.len() as u64).to_le_bytes());
        program.extend((32 + commands).to_le_bytes());
        program.extend([0; 28]);
        program.extend(strings);
        program
    }

    /// Wrap a thin Mach-O `slice` inside a universal/fat Mach-O (`0xCAFEBABE`).
    fn fat_program(slice: &[u8]) -> Vec<u8> {
        let offset: u32 = 8 + 20;
        let mut out = Vec::new();
        out.extend(0xCAFE_BABE_u32.to_be_bytes());
        out.extend(1_u32.to_be_bytes());
        out.extend(0x0100_000C_u32.to_be_bytes());
        out.extend(0_u32.to_be_bytes());
        out.extend(offset.to_be_bytes());
        out.extend((slice.len() as u32).to_be_bytes());
        out.extend(14_u32.to_be_bytes());
        out.extend(slice);
        out
    }

    #[test]
    fn the_secrets_are_read_from_thin_and_universal_programs_holding_the_client() {
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (
            format!("GOCSPX-{}", "a".repeat(28)),
            format!("GOCSPX-{}", "B_-9".repeat(7)),
        );
        // Go packs its strings one against the next, with nothing between.
        let strings = format!("{one}gzip{}{two}{one}GOCSPX-short", CLIENT_ID);
        let thin = program(strings.as_bytes());
        let held = dir.path().join("language_server");
        std::fs::write(&held, &thin).unwrap();
        let extract = |path: &Path| {
            secrets(path)
                .unwrap()
                .iter()
                .map(|secret| secret.expose().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(extract(&held), [one.clone(), two.clone()]);

        let universal = dir.path().join("language_server_universal");
        std::fs::write(&universal, fat_program(&thin)).unwrap();
        assert_eq!(extract(&universal), [one.clone(), two]);

        // Not Antigravity's: no client id, or not a Mach-O, or cut short.
        let other = dir.path().join("other");
        std::fs::write(&other, program(one.as_bytes())).unwrap();
        assert!(secrets(&other).is_none());
        std::fs::write(&other, strings.as_bytes()).unwrap();
        assert!(secrets(&other).is_none());
        std::fs::write(&other, &thin[..thin.len() - 10]).unwrap();
        assert!(secrets(&other).is_none());
        assert!(secrets(&dir.path().join("missing")).is_none());
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
        let credential = signed_in(wrapped(&kept).as_bytes(), home.path())
            .unwrap()
            .unwrap();
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
        assert_eq!(credential.client.unwrap().id, "ANTIGRAVITY");
        let refresh = credential.refresh.as_ref().unwrap();
        assert_eq!(refresh.token.expose(), "made-up-refresh");
        assert_eq!(refresh.client_id, CLIENT_ID);

        // Another way of signing in isn't read, nor a sign-in with no token.
        let mut other = kept.clone();
        other["auth_method"] = json!("enterprise");
        assert!(
            signed_in(wrapped(&other).as_bytes(), home.path())
                .unwrap()
                .is_none()
        );
        let mut out = kept.clone();
        out["token"]["access_token"] = json!("");
        assert!(
            signed_in(wrapped(&out).as_bytes(), home.path())
                .unwrap()
                .is_none()
        );

        // A value that doesn't read isn't taken for signed out.
        assert!(signed_in(kept.to_string().as_bytes(), home.path()).is_err());
        assert!(signed_in(format!("{WRAPPED}not base64").as_bytes(), home.path()).is_err());
        assert!(
            signed_in(
                format!("{WRAPPED}{}", STANDARD.encode("not JSON")).as_bytes(),
                home.path()
            )
            .is_err()
        );

        // The file is read as the item is, and one half written isn't taken
        // for signed out.
        let folder = Antigravity.folders(home.path()).remove(0);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(FALLBACK), kept.to_string()).unwrap();
        let logins = Antigravity.logins(&folder, home.path()).unwrap();
        let from_file = logins[0].credential.as_ref().unwrap();
        assert_eq!(from_file.secret.expose(), "made-up-token");
        assert_eq!(from_file.refresh.as_ref().unwrap().client_id, CLIENT_ID);
        // Secret loader discovers `agy` under `home/.local/bin`.
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let secret = format!("GOCSPX-{}", "z".repeat(28));
        std::fs::write(
            bin.join(INFO.command),
            program(format!("{CLIENT_ID}{secret}").as_bytes()),
        )
        .unwrap();
        let loaded = (from_file.refresh.as_ref().unwrap().secrets)().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].expose(), secret);

        std::fs::write(folder.join(FALLBACK), "{").unwrap();
        assert!(Antigravity.logins(&folder, home.path()).is_err());
    }
}
