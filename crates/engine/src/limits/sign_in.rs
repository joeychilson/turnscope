//! Where each agent keeps a sign-in to a subscription, whose each is, and
//! telling when any of them changed.

use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread::JoinHandle;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;

use super::{Reader, Subscription, claude, instant_of};
use crate::agent::Agent;
use crate::error::{Error, Result};
use crate::folders::Folder;
use crate::time::Instant;

/// The login Keychain's file, relative to the home directory.
const LOGIN_KEYCHAIN: &str = "Library/Keychains/login.keychain-db";

/// How `security find-generic-password` exits when there is no such item.
const ITEM_NOT_FOUND: i32 = 44;

/// The longest `/usr/bin/security` may take to answer. A locked Keychain
/// can wait on a password no one types, and limits must not wait with it:
/// past this, it counts as a Keychain that won't answer.
const KEYCHAIN_WAIT: Duration = Duration::from_secs(20);

/// How often a process run with [`output_within`] is looked at to see
/// whether it has ended.
const POLL: Duration = Duration::from_millis(10);

/// One place an agent keeps a sign-in to a subscription, in each of its
/// folders.
pub(super) struct Source {
    /// The agent.
    pub(super) agent: Agent,
    /// The provider the agent's usage names when it uses the sign-in, as
    /// Pi's names `openai-codex` for its ChatGPT sign-in: what tells its use
    /// of the subscription from its use of an API key.
    pub(super) provider: &'static str,
    /// Where it keeps it in a folder of its.
    pub(super) location: Location,
    /// JSON pointer to the token.
    pub(super) token: &'static str,
    /// JSON pointer to when the token expires, where the agent records it.
    pub(super) expires: Option<&'static str>,
    /// JSON pointer to the plan, where the agent records it.
    pub(super) plan: Option<&'static str>,
    /// Where the agent records whose sign-in it holds, for tokens that don't
    /// say.
    pub(super) whose: Option<Whose>,
}

/// Where an agent records the account it is signed into, beside the
/// sign-in itself.
pub(super) struct Whose {
    /// The JSON file it records it in for a folder of its, given the home
    /// directory.
    pub(super) file: fn(&Folder, &Path) -> PathBuf,
    /// JSON pointers to what identifies the account, joined in order; any
    /// the file leaves out are left out.
    pub(super) id: &'static [&'static str],
    /// JSON pointer to what tells the account apart, such as its email
    /// address.
    pub(super) label: &'static str,
}

/// Where an app keeps a sign-in, in a folder of its.
pub(super) enum Location {
    /// A JSON file in the folder.
    File(&'static str),
    /// The login Keychain item, its password JSON, Claude Code keeps its
    /// sign-in for the folder in, named as [`claude::service`] names it.
    ClaudeKeychain,
    /// OpenCode's database in the folder, which keeps a row in `credential`
    /// for each sign-in to the provider named, its `value` JSON. Of several
    /// to one provider, OpenCode uses those marked `active`, and so do
    /// limits.
    OpenCode(&'static str),
}

/// A sign-in found on this Mac.
pub(super) struct SignIn {
    pub(super) agent: Agent,
    /// The agent's folder it was found in.
    pub(super) folder: PathBuf,
    /// The provider the agent's usage names when it uses it.
    pub(super) provider: &'static str,
    pub(super) token: String,
    pub(super) expires: Option<Instant>,
    pub(super) plan: Option<String>,
    /// Whose it is, where the agent records it.
    pub(super) whose: Option<Identity>,
}

/// The account a token belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Identity {
    /// The same for every token to the account; empty when tokens do not say.
    pub(super) key: String,
    /// What tells the account apart from others of the subscription.
    pub(super) label: Option<String>,
}

/// The account of a token that doesn't say whose it is: the same for every
/// such token, as for Claude's without Claude Code's record of whose they
/// are, and for a provider's keys, which draw on its one API-key account.
pub(super) fn one_account(_token: &str) -> Identity {
    Identity {
        key: String::new(),
        label: None,
    }
}

/// Where FNV-1a starts.
pub(super) const FNV: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a of `bytes` from `hash`: stable, unlike the standard library's
/// hasher, and written out because account ids made with it are kept across
/// releases of Rust.
pub(super) fn fnv(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Every sign-in to a subscription on this Mac, in any of the agents'
/// `folders`, under `home`.
///
/// # Errors
///
/// Returns an error when a place that keeps sign-ins to it, or whose they
/// are, is there but can't be read: a file being written, a database busy,
/// a Keychain that won't answer. Taking such a place for one that keeps
/// none would sign its accounts out, or give its sign-ins to no one.
pub(super) fn sign_ins(
    subscription: Subscription,
    folders: &[Folder],
    home: &Path,
) -> Result<Vec<SignIn>> {
    let mut found = Vec::new();
    for source in subscription.reader().sources {
        for folder in folders.iter().filter(|folder| folder.agent == source.agent) {
            let kept = kept(&source.location, folder, home)?;
            if kept.is_empty() {
                continue;
            }
            let whose = match &source.whose {
                Some(whose) => whose.read(folder, home)?,
                None => None,
            };
            found.extend(
                kept.iter()
                    .filter_map(|kept| sign_in(source, folder, kept, whose.clone())),
            );
        }
    }
    Ok(found)
}

impl Whose {
    /// The account the agent records for `folder`, when its file names one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file is there but can't be read as
    /// JSON, as while the agent writes it.
    fn read(&self, folder: &Folder, home: &Path) -> Result<Option<Identity>> {
        let Some(kept) = json_file(&(self.file)(folder, home))? else {
            return Ok(None);
        };
        let parts: Vec<&str> = self
            .id
            .iter()
            .filter_map(|pointer| kept.pointer(pointer)?.as_str())
            .filter(|part| !part.is_empty())
            .collect();
        Ok((!parts.is_empty()).then(|| Identity {
            key: parts.join(":"),
            label: kept
                .pointer(self.label)
                .and_then(Value::as_str)
                .map(str::to_owned),
        }))
    }
}

/// The JSON file at `path`, or `None` when there is none.
///
/// # Errors
///
/// Returns [`Error::Io`] when it is there but can't be read, or isn't JSON,
/// as a file is while it is written.
pub(super) fn json_file(path: &Path) -> Result<Option<Value>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::io(path, error)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| Error::io(path, io::Error::new(ErrorKind::InvalidData, error)))
}

/// A stamp of the places `subscription`'s sign-ins are kept in the agents'
/// `folders`, under `home`: each one's size and when it last changed. It
/// changes whenever a sign-in there could have, or a folder is read or no
/// longer, and costs only a look at each place, where finding the sign-ins
/// reads files and databases and asks the Keychain.
pub(crate) fn stamp(subscription: Subscription, folders: &[Folder], home: &Path) -> u64 {
    let mut places = Vec::new();
    for source in subscription.reader().sources {
        for folder in folders.iter().filter(|folder| folder.agent == source.agent) {
            match source.location {
                Location::File(path) => places.push(folder.path.join(path)),
                // The login Keychain's file changes with any item in it.
                Location::ClaudeKeychain => places.push(home.join(LOGIN_KEYCHAIN)),
                Location::OpenCode(_) => {
                    let database = crate::agent::opencode::database(&folder.path);
                    let log = crate::sharing::side_file(&database, "-wal");
                    places.push(database);
                    places.push(log);
                }
            }
        }
        // Where an agent records whose a sign-in is is left out: Claude Code
        // writes to that file all the time, and an account changes only with
        // its sign-in, whose place is stamped.
    }
    places.iter().fold(FNV, |hash, place| {
        let seen = std::fs::metadata(place).ok().map(|metadata| {
            let changed = metadata
                .modified()
                .ok()
                .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_nanos());
            (metadata.len(), changed)
        });
        match seen {
            Some((size, changed)) => fnv(fnv(hash, &size.to_le_bytes()), &changed.to_le_bytes()),
            None => fnv(hash, b"absent"),
        }
    })
}

/// What `location` keeps in `folder`, as JSON: one document for a file or a
/// Keychain item, one for each active row of OpenCode's. An app that isn't
/// here keeps no sign-in.
///
/// # Errors
///
/// Returns an error when the place is there but can't be read.
fn kept(location: &Location, folder: &Folder, home: &Path) -> Result<Vec<Value>> {
    match *location {
        Location::File(path) => Ok(json_file(&folder.path.join(path))?.into_iter().collect()),
        Location::ClaudeKeychain => Ok(keychain(&claude::service(folder), home)?
            .into_iter()
            .collect()),
        Location::OpenCode(provider) => {
            opencode(&crate::agent::opencode::database(&folder.path), provider)
        }
    }
}

/// The active sign-ins to `provider` in OpenCode's database at `path`, read
/// only, as their JSON values, as [`credentials`] finds them.
///
/// # Errors
///
/// As [`credentials`].
fn opencode(path: &Path, provider: &str) -> Result<Vec<Value>> {
    Ok(credentials(path)?
        .into_iter()
        .filter(|(to, _)| to == provider)
        .map(|(_, value)| value)
        .collect())
}

/// Every active sign-in and key in OpenCode's database at `path`, read
/// only: the provider each is to, and its JSON value. A database without
/// sign-ins, as OpenCode's was before it kept them there, has none.
///
/// Measured 2026-09-24: OpenCode keeps no `auth.json` any more, but a row of
/// `credential` for each sign-in, 4 here. `integration_id` names the
/// provider (`openai`, `opencode-go` twice, `openrouter`), and `value` is
/// JSON: `{type, key}` for an API key, and `{type, methodID, access,
/// refresh, expires, metadata.accountID}` for the OpenAI sign-in, whose
/// `access` is the same kind of JWT Codex keeps. `active` marks the one
/// OpenCode uses of several to a provider: of the two OpenCode Go keys, one
/// was 0. `account` and `control_account`, for OpenCode's own service, were
/// empty. On 2026-09-29 the same four rows' `type` was `key` for the three
/// keys and `oauth` for the OpenAI sign-in.
///
/// # Errors
///
/// Returns [`Error::Database`] when the database is there but can't be
/// read, as while OpenCode holds it busy, or a sign-in in it isn't JSON, and
/// [`Error::Io`] when whether it is there can't be told.
pub(super) fn credentials(path: &Path) -> Result<Vec<(String, Value)>> {
    if !path.try_exists().map_err(|error| Error::io(path, error))? {
        return Ok(Vec::new());
    }
    let read = || -> rusqlite::Result<Vec<(String, String)>> {
        let connection = crate::agent::opencode::open(path)?;
        let kept: bool = connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'credential')",
            [],
            |row| row.get(0),
        )?;
        if !kept {
            return Ok(Vec::new());
        }
        let mut statement = connection.prepare(
            "SELECT integration_id, value FROM credential
             WHERE integration_id IS NOT NULL AND coalesce(active, 1) != 0",
        )?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect()
    };
    read()
        .map_err(|error| Error::database(path, error))?
        .into_iter()
        .map(|(provider, value)| {
            let value = serde_json::from_str(&value).map_err(|error| Error::Database {
                path: path.to_path_buf(),
                detail: format!("a sign-in to {provider} is not JSON: {error}"),
            })?;
            Ok((provider, value))
        })
        .collect()
}

/// The sign-in `source` describes in what an agent keeps in `folder`,
/// `kept`, if it holds a token, and `whose` it is where the agent records
/// that.
fn sign_in(
    source: &Source,
    folder: &Folder,
    kept: &Value,
    whose: Option<Identity>,
) -> Option<SignIn> {
    let token = kept.pointer(source.token)?.as_str()?.to_owned();
    let expires = source
        .expires
        .and_then(|pointer| kept.pointer(pointer))
        .and_then(instant_of)
        // An app that records no expiry still holds a token that says.
        .or_else(|| {
            claims(&token)["exp"]
                .as_i64()
                .and_then(Instant::from_seconds)
        });
    let plan = source
        .plan
        .and_then(|pointer| kept.pointer(pointer))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some(SignIn {
        agent: source.agent,
        folder: folder.path.clone(),
        provider: source.provider,
        token,
        expires,
        plan,
        whose,
    })
}

/// A login Keychain item's JSON password, or `None` when there is no such
/// item.
///
/// Read with `/usr/bin/security`, the tool Claude Code writes its sign-in
/// with, so the item already trusts the reader and no permission prompt
/// appears.
///
/// # Errors
///
/// Returns [`Error::Database`], naming the login Keychain under `home`, when
/// `security` fails for any reason but the item's absence, gives no answer
/// within [`KEYCHAIN_WAIT`], or the password isn't JSON.
fn keychain(service: &str, home: &Path) -> Result<Option<Value>> {
    let failed = |detail: String| Error::Database {
        path: home.join(LOGIN_KEYCHAIN),
        detail: format!("{service}: {detail}"),
    };
    let output = output_within(
        Command::new("/usr/bin/security").args(["find-generic-password", "-s", service, "-w"]),
        KEYCHAIN_WAIT,
    )
    .map_err(|error| failed(format!("security gave no answer: {error}")))?;
    match output.status.code() {
        Some(0) => serde_json::from_slice(&output.stdout)
            .map(Some)
            .map_err(|error| failed(format!("the item is not JSON: {error}"))),
        Some(ITEM_NOT_FOUND) => Ok(None),
        code => Err(failed(format!(
            "security failed ({}): {}",
            code.map_or_else(|| "a signal".to_owned(), |code| code.to_string()),
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

/// Run `command` to its end with nothing on its standard input, and give
/// what it wrote and how it ended, as [`Command::output`] does, but wait no
/// longer than `limit`: past it, the process is killed.
///
/// # Errors
///
/// Returns the error of running it or reading what it wrote, and one of
/// [`ErrorKind::TimedOut`] when it did not end in time.
fn output_within(command: &mut Command, limit: Duration) -> io::Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Each pipe is read as the process writes to it, so one that fills
    // never holds it up.
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            // Not yet waited for, so the process is there to be killed even
            // if it has just ended; waiting leaves nothing of it behind, and
            // closes the pipes its readers are reading.
            child.kill()?;
            child.wait()?;
            return Err(io::Error::new(
                ErrorKind::TimedOut,
                format!("it did not end within {} s", limit.as_secs()),
            ));
        }
        std::thread::sleep(POLL);
    };
    Ok(Output {
        status,
        stdout: drained(stdout)?,
        stderr: drained(stderr)?,
    })
}

/// Read `pipe` to its end on a thread of its own.
fn drain(mut pipe: impl io::Read + Send + 'static) -> JoinHandle<io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

/// What a pipe [`drain`] read held: nothing for a pipe there wasn't.
fn drained(reader: Option<JoinHandle<io::Result<Vec<u8>>>>) -> io::Result<Vec<u8>> {
    match reader {
        Some(reader) => reader
            .join()
            .map_err(|_| io::Error::other("reading what it wrote stopped"))?,
        None => Ok(Vec::new()),
    }
}

/// The claims a token carries, or null when it is not a JWT. Read without
/// checking the signature: claims only group sign-ins and label accounts, and
/// the provider checks the token itself.
pub(super) fn claims(token: &str) -> Value {
    token
        .split('.')
        .nth(1)
        .and_then(|payload| URL_SAFE_NO_PAD.decode(payload).ok())
        .and_then(|json| serde_json::from_slice(&json).ok())
        .unwrap_or(Value::Null)
}

impl SignIn {
    /// Whose it is: as its agent records it, or as `reader` tells from its
    /// token.
    pub(super) fn identity(&self, reader: &Reader) -> Identity {
        self.whose
            .clone()
            .unwrap_or_else(|| (reader.identity)(&self.token))
    }
}

/// `sign_ins` grouped by the account each belongs to, whose the agent says
/// it is or as `reader` tells accounts apart, in the order they were found.
pub(super) fn accounts(reader: &Reader, sign_ins: Vec<SignIn>) -> Vec<(Identity, Vec<SignIn>)> {
    let mut accounts: Vec<(Identity, Vec<SignIn>)> = Vec::new();
    for sign_in in sign_ins {
        let identity = sign_in.identity(reader);
        match accounts
            .iter_mut()
            .find(|(known, _)| known.key == identity.key)
        {
            Some((known, held)) => {
                known.label = known.label.take().or(identity.label);
                held.push(sign_in);
            }
            None => accounts.push((identity, vec![sign_in])),
        }
    }
    accounts
}

/// The fingerprint of `subscription`'s sign-ins in the agents' `folders`,
/// which changes when one is added, renewed, moved or removed.
///
/// # Errors
///
/// As [`sign_ins`].
pub(crate) fn fingerprint(
    subscription: Subscription,
    folders: &[Folder],
    home: &Path,
) -> Result<u64> {
    // Over the tokens, where each is, and whose each is: never kept, so a
    // hash that could be reversed would reveal nothing the files do not.
    Ok(sign_ins(subscription, folders, home)?
        .iter()
        .fold(FNV, |hash, sign_in| {
            let hash = fnv(hash, sign_in.folder.as_os_str().as_encoded_bytes());
            let hash = fnv(hash, sign_in.token.as_bytes());
            fnv(
                hash,
                sign_in
                    .whose
                    .as_ref()
                    .map_or(&[][..], |whose| whose.key.as_bytes()),
            )
        }))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::{Identity, SignIn, Whose, accounts, claims, output_within, sign_ins, stamp};
    use crate::agent::Agent;
    use crate::folders::{self, Folder, FolderOrigin};
    use crate::limits::Subscription;

    /// Write `text` to `path` under `home`, making its folders.
    fn write(home: &Path, path: &str, text: impl AsRef<[u8]>) {
        let path = home.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// The folders read in `home` with no choice made.
    fn folders(home: &Path) -> Vec<Folder> {
        folders::read(home)
    }

    /// Claude Code's own folder in `home`.
    fn claude_code(home: &Path) -> Folder {
        Folder {
            agent: Agent::ClaudeCode,
            path: home.join(".claude"),
            origin: FolderOrigin::Own,
        }
    }

    /// Where Claude Code records whose its sign-in is.
    fn claude_json() -> &'static Whose {
        Subscription::Claude.reader().sources[0]
            .whose
            .as_ref()
            .unwrap()
    }

    #[test]
    fn a_command_that_does_not_end_in_time_is_killed() {
        // What a command writes, to either stream, and how it ends, as
        // `Command::output` gives them.
        let ended = output_within(
            Command::new("/bin/sh").args(["-c", "echo found; echo refused >&2; exit 44"]),
            Duration::from_secs(20),
        )
        .unwrap();
        assert_eq!(
            (
                ended.status.code(),
                ended.stdout.as_slice(),
                ended.stderr.as_slice()
            ),
            (Some(44), &b"found\n"[..], &b"refused\n"[..])
        );
        // One that waits, as `security` does on a locked Keychain, is
        // stopped at the limit, not after it has had its minute.
        let started = Instant::now();
        let waited = output_within(
            Command::new("/bin/sleep").arg("60"),
            Duration::from_millis(200),
        );
        assert_eq!(
            waited.map_err(|error| error.kind()).err(),
            Some(std::io::ErrorKind::TimedOut)
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    /// A JWT whose claims are `claims`, unsigned, as the claims are all
    /// that is read of one.
    fn jwt(claims: serde_json::Value) -> String {
        use base64::Engine as _;
        let encode = |json: serde_json::Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string())
        };
        format!("{}.{}.sig", encode(json!({"alg": "none"})), encode(claims))
    }

    #[test]
    fn every_app_a_subscription_is_signed_into_is_found_and_grouped_by_account() {
        let home = tempfile::tempdir().unwrap();
        let chatgpt = jwt(json!({
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct-1"},
            "https://api.openai.com/profile": {"email": "joey@example.com"},
            "exp": 4_102_444_800_i64,
        }));
        // Codex keeps its sign-in in a file.
        write(
            home.path(),
            ".codex/auth.json",
            json!({"tokens": {"access_token": chatgpt}}).to_string(),
        );
        // OpenCode keeps a row for each sign-in in its database, the one in
        // use of several to a provider marked active: an OpenCode Go key it
        // replaced, the one it uses, and a ChatGPT sign-in as JSON.
        let database = home.path().join(".local/share/opencode/opencode.db");
        std::fs::create_dir_all(database.parent().unwrap()).unwrap();
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE credential (id text PRIMARY KEY, integration_id text, label text NOT NULL,
                 value text NOT NULL, connector_id text, method_id text, active integer,
                 time_created integer NOT NULL, time_updated integer NOT NULL);",
            )
            .unwrap();
        for (id, provider, value, active) in [
            (
                "1",
                "opencode-go",
                json!({"type": "api", "key": "go-old"}),
                0,
            ),
            (
                "2",
                "opencode-go",
                json!({"type": "api", "key": "go-new"}),
                1,
            ),
            (
                "3",
                "openai",
                json!({"type": "oauth", "access": chatgpt, "refresh": "r",
                       "expires": 4_102_444_800_000_i64, "metadata": {"accountID": "acct-1"}}),
                1,
            ),
            ("4", "openrouter", json!({"type": "api", "key": "or"}), 1),
        ] {
            connection
                .execute(
                    "INSERT INTO credential VALUES (?1, ?2, ?2, ?3, NULL, NULL, ?4, 0, 0)",
                    rusqlite::params![id, provider, value.to_string(), active],
                )
                .unwrap();
        }
        drop(connection);
        // Pi keeps the same OpenCode Go key in its file.
        write(
            home.path(),
            ".pi/agent/auth.json",
            json!({"opencode-go": {"type": "api", "key": "go-new"}}).to_string(),
        );

        let grouped = |subscription: Subscription| -> Vec<Vec<Agent>> {
            let found = sign_ins(subscription, &folders(home.path()), home.path()).unwrap();
            accounts(&subscription.reader(), found)
                .into_iter()
                .map(|(_, held)| held.iter().map(|sign_in| sign_in.agent).collect())
                .collect()
        };
        // ChatGPT from Codex and OpenCode is one account, the workspace both
        // tokens name.
        assert_eq!(
            grouped(Subscription::ChatGpt),
            [[Agent::Codex, Agent::OpenCode]]
        );
        // OpenCode Go from OpenCode's active key and Pi's same key is one
        // account; the key OpenCode no longer uses isn't read.
        assert_eq!(
            grouped(Subscription::OpenCodeGo),
            [[Agent::OpenCode, Agent::Pi]]
        );
        // No one here is signed into SuperGrok.
        assert!(grouped(Subscription::SuperGrok).is_empty());

        // A new sign-in in a place already looked at changes its stamp; a
        // look without a change leaves it as it was.
        let stamped = || stamp(Subscription::SuperGrok, &folders(home.path()), home.path());
        let before = stamped();
        assert_eq!(stamped(), before);
        write(
            home.path(),
            ".grok/auth.json",
            json!({"https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828":
                   {"key": jwt(json!({"sub": "grok-1"}))}})
            .to_string(),
        );
        assert_ne!(stamped(), before);
        assert_eq!(grouped(Subscription::SuperGrok), [[Agent::Grok]]);
    }

    #[test]
    fn a_second_folder_signed_into_another_account_is_another_account() {
        let home = tempfile::tempdir().unwrap();
        let signed = |account: &str| {
            json!({"auth_mode": "chatgpt", "tokens": {"access_token": jwt(json!({
                "https://api.openai.com/auth": {"chatgpt_account_id": account}}))}})
            .to_string()
        };
        // Codex in its own folder signed into one workspace, and pointed by
        // CODEX_HOME at ~/.codex-personal, signed into another.
        write(home.path(), ".codex/auth.json", signed("acct-work"));
        write(
            home.path(),
            ".codex-personal/auth.json",
            signed("acct-personal"),
        );
        let stamped = |folders: &[Folder]| stamp(Subscription::ChatGpt, folders, home.path());
        let own_only: Vec<Folder> = folders(home.path())
            .into_iter()
            .filter(|folder| folder.origin == FolderOrigin::Own)
            .collect();
        let found = sign_ins(Subscription::ChatGpt, &folders(home.path()), home.path()).unwrap();
        let grouped: Vec<(String, Vec<PathBuf>)> = accounts(&Subscription::ChatGpt.reader(), found)
            .into_iter()
            .map(|(identity, held)| {
                (
                    identity.key,
                    held.into_iter().map(|sign_in| sign_in.folder).collect(),
                )
            })
            .collect();
        assert_eq!(
            grouped,
            [
                ("acct-work".to_owned(), vec![home.path().join(".codex")]),
                (
                    "acct-personal".to_owned(),
                    vec![home.path().join(".codex-personal")]
                ),
            ]
        );
        // A folder read or no longer changes what is stamped.
        assert_ne!(stamped(&own_only), stamped(&folders(home.path())));
    }

    #[test]
    fn accounts_an_agent_says_whose_they_are_stand_apart() {
        let home = tempfile::tempdir().unwrap();
        // As Claude Code records the account it is signed into, beside its
        // sign-in in the Keychain.
        let whose = claude_json();
        let folder = claude_code(home.path());
        assert_eq!(
            whose.read(&folder, home.path()).unwrap(),
            None,
            "no record, no account"
        );
        std::fs::write(
            home.path().join(".claude.json"),
            json!({"oauthAccount": {"accountUuid": "acc-1", "organizationUuid": "org-9",
                                    "emailAddress": "joey@example.com"}})
            .to_string(),
        )
        .unwrap();
        let first = whose.read(&folder, home.path()).unwrap().unwrap();
        assert_eq!(
            first,
            Identity {
                key: "acc-1:org-9".into(),
                label: Some("joey@example.com".into()),
            }
        );
        let sign_in = |token: &str, whose: Option<Identity>| SignIn {
            agent: Agent::ClaudeCode,
            folder: folder.path.clone(),
            provider: "anthropic",
            token: token.into(),
            expires: None,
            plan: None,
            whose,
        };
        let other = Identity {
            key: "acc-2:org-9".into(),
            label: None,
        };
        // Two sign-ins to one account are one; another account stands apart,
        // though Claude's tokens say nothing of whose they are.
        let grouped = accounts(
            &Subscription::Claude.reader(),
            vec![
                sign_in("a", Some(first.clone())),
                sign_in("b", Some(other)),
                sign_in("c", Some(first)),
            ],
        );
        let keys: Vec<(&str, usize)> = grouped
            .iter()
            .map(|(identity, held)| (identity.key.as_str(), held.len()))
            .collect();
        assert_eq!(keys, [("acc-1:org-9", 2), ("acc-2:org-9", 1)]);
    }

    #[test]
    fn a_place_that_cant_be_read_is_not_taken_to_keep_no_sign_in() {
        let home = tempfile::tempdir().unwrap();
        // Places that aren't there, or keep none, keep no sign-in: here
        // OpenCode's database from before it kept sign-ins in it.
        let database = home.path().join(".local/share/opencode/opencode.db");
        std::fs::create_dir_all(database.parent().unwrap()).unwrap();
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute_batch("CREATE TABLE session_v2 (id text PRIMARY KEY);")
            .unwrap();
        let read = |subscription| sign_ins(subscription, &folders(home.path()), home.path());
        assert!(read(Subscription::ChatGpt).unwrap().is_empty());
        assert!(read(Subscription::OpenCodeGo).unwrap().is_empty());

        // Codex part way through writing its sign-in.
        write(home.path(), ".codex/auth.json", r#"{"tokens": {"access_to"#);
        assert!(read(Subscription::ChatGpt).is_err());
        // OpenCode's database, unreadable.
        write(
            home.path(),
            ".local/share/opencode/opencode.db",
            "not a database",
        );
        assert!(read(Subscription::OpenCodeGo).is_err());
        // Claude Code part way through writing whose sign-in it holds.
        write(
            home.path(),
            ".claude.json",
            r#"{"oauthAccount": {"accountUu"#,
        );
        assert!(
            claude_json()
                .read(&claude_code(home.path()), home.path())
                .is_err()
        );
    }

    #[test]
    fn a_tokens_claims_are_read_without_its_signature() {
        // {"sub":"user-1"} in base64url, between a header and a signature.
        assert_eq!(
            claims("eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1c2VyLTEifQ.signature")["sub"],
            "user-1"
        );
        assert!(claims("not-a-jwt").is_null());
    }
}
