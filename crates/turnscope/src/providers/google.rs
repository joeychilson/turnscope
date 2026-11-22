//! Google: the limits of a Google AI plan, which only Antigravity signs in
//! to, and the account Gemini API keys draw on. An API key carries no limit
//! an agent's key can read.
//!
//! A plan's limits are read as Antigravity reads them, with POSTs that change
//! nothing to the service its app and `agy` talk to. None of it is
//! documented: what follows was measured with this Mac's sign-in on
//! 2026-11-10, asking as OpenUsage and CodexBar do (their code, read that
//! day).
//!
//! - `loadCodeAssist` gives the plan paid for, `paidTier` (`currentTier` was
//!   `free-tier` beside it), and the project Google keeps the usage under,
//!   `cloudaicompanionProject`. Google makes the project when Antigravity is
//!   first used, and Turnscope never does: without one there's nothing to
//!   ask. Both project and plan are cached in memory for an hour so routine
//!   reads make one request instead of two.
//! - `retrieveUserQuotaSummary`, asked with that project, gives `groups` of
//!   `buckets`, each a `window` (`5h`, `weekly`) with the fraction left,
//!   `remainingFraction`, and `resetTime`.
//!
//! **The client.** Asked as `Turnscope`, `loadCodeAssist` answered 200 with
//! no tier, no project, and the reason `UNSUPPORTED_CLIENT`. So both
//! requests name the client as Antigravity's app does, taking the installed
//! app's version from `Info.plist` when present.
//!
//! **The host.** Limits are asked of the `daily-` host, over the one that
//! tells the plan: asked at the same moment, that one had every bucket full
//! and resetting a whole window later, where `daily-` had 2.3% of the Gemini
//! week used.
//!
//! **Windows.** A pool nothing had drawn on reset a whole window after the
//! request, so it reads as a window beginning now. In the pool in use, the
//! 5-hour window reset 10 hours after the week began: 5-hour windows follow
//! one another from the week's first use.
//!
//! **The plan's own limits.** A plan has a pool for Gemini models and one
//! for Claude and GPT models, each with a 5-hour and a weekly bucket, and
//! none on all usage. The Gemini pool's are the plan's own, over giving
//! every limit a scope: Antigravity's own models draw on them, and with no
//! limit of its own an account has nothing to stand by in the status line
//! or for `guard`. Every other bucket is scoped by its group, as `Weekly
//! (Claude and GPT)`, and still shows when it runs out.
//!
//! **Keys.** A limit is keyed by its window, and one not the plan's own by
//! its bucket too, as `weekly:3p-weekly`, over the bucket's id alone: the
//! status lists limits by key, and by id the Claude and GPT pool's came
//! before the plan's own.
//!
//! **Refreshing.** From Antigravity 2.33.1 on (verified in 2.34.0,
//! 2026-11-21), each `language_server` process generates a random
//! `writer_id`, saves it to Keychain only on interactive sign-in, and skips
//! saving hourly refreshes whenever `writer_id` differs, leaving Keychain
//! expired after a restart, update, or switch between the app and `agy`.
//! Google's OAuth refresh tokens don't rotate on use, so when the stored
//! access token has expired Turnscope exchanges the refresh token in memory
//! via `https://oauth2.googleapis.com/token` using the OAuth client read
//! from Antigravity's Mach-O `__TEXT.__rodata` section, caches the refreshed
//! access token in memory only, and never writes Antigravity's files or
//! Keychain or stores any token on disk.
//!
//! **Not measured.** A refused sign-in: only a 401 is taken for one, since a
//! 403 from this service can mean a client or an account it doesn't serve.
//! An account that pays for no plan. A group for one model, which OpenUsage
//! reads as `Gemini 4 Argon`. A `disabled` bucket, passed over as OpenUsage
//! does. A Claude or GPT response through Antigravity: the limit on them
//! counts models whose id holds `claude` or `gpt`, and the Gemini limits
//! count every model's. A used-up bucket: it should still have
//! `remainingFraction: 0`, as CodexBar reads `agy`'s form of the answer,
//! where a fraction that's set is written even at zero. So a bucket without
//! it says nothing of what's left and is passed over, as OpenUsage does,
//! over reading it as used up.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use serde_json::{Value, json};

use super::{Account, Answered, Info, Limits, Problem, Provider, Reading};
use crate::agents::{Credential, Refresh, Secret};
use crate::time::{DAY, HOUR, MINUTE};

pub struct Google;

/// What Google's plans are called, before the plan.
const PLANS: &str = "Google AI";

static INFO: Info = Info {
    id: "google",
    // What Google calls a key made for its models: a Gemini API key.
    name: "Gemini",
    subscription: Some(PLANS),
    key_limits: false,
};

const PLAN: &str = "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const QUOTA: &str = "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary";
const TOKEN: &str = "https://oauth2.googleapis.com/token";

/// Antigravity's consumer Google OAuth client id (public; the matching secret
/// is read from Antigravity's binary at refresh time so no secret lives here).
const CONSUMER_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const CLIENT_SUFFIX: &[u8] = b".apps.googleusercontent.com";
const SECRET_PREFIX: &[u8] = b"GOCSPX-";
const SECRET_LEN: usize = 35;

/// Bounds on Mach-O reads so a corrupted header can't ask for gigabytes.
const MAX_LOAD_COMMANDS: usize = 16 * 1024 * 1024;
const MAX_SECTION: usize = 64 * 1024 * 1024;
const MAX_PAIRS: usize = 16;

/// How long a cached `cloudaicompanionProject` and plan stand before
/// `loadCodeAssist` is asked again.
const PROJECT_TTL: i64 = HOUR;

/// Fallback Antigravity version when `Info.plist` isn't found (as with `agy`
/// alone).
const FALLBACK_VERSION: &str = "2.34.0";

#[cfg(target_arch = "aarch64")]
const ARCH: &str = "arm64";
#[cfg(not(target_arch = "aarch64"))]
const ARCH: &str = "amd64";

impl Provider for Google {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn account(&self, credential: &Credential) -> Option<Account> {
        super::account(INFO.id, credential, || credential.identity.clone())
    }

    fn refreshes(&self) -> bool {
        true
    }

    fn limits(&self, credential: &Credential, now: i64, by: Instant) -> Result<Limits, Problem> {
        let binaries = credential
            .refresh
            .as_ref()
            .map_or(&[][..], |refresh| refresh.binaries.as_slice());
        let client_header = user_agent(binaries);
        let account_key = account_key(credential);
        let fresh = credential.expires.is_none_or(|at| at > now + MINUTE);
        if fresh {
            match fetch(&credential.secret, &account_key, &client_header, now, by) {
                Err(Problem::SignIn) if credential.refresh.is_some() => {}
                other => return other,
            }
        }
        let Some(refresh) = &credential.refresh else {
            return Err(if fresh {
                Problem::SignIn
            } else {
                Problem::Expired
            });
        };
        let fingerprint = refresh.token.fingerprint();
        if let Some(token) = lock_cache().cached_token(&fingerprint, now) {
            match fetch(&token, &account_key, &client_header, now, by) {
                Err(Problem::SignIn) => lock_cache().evict_token(&fingerprint),
                other => return other,
            }
        }
        let (token, expires_at) = refresh_access(refresh, now, by)?;
        lock_cache().store_token(fingerprint.clone(), &token, expires_at, now);
        let result = fetch(&token, &account_key, &client_header, now, by);
        if matches!(result, Err(Problem::SignIn)) {
            lock_cache().evict_token(&fingerprint);
        }
        result
    }
}

fn account_key(credential: &Credential) -> String {
    credential
        .identity
        .as_ref()
        .map(|(id, _)| id.clone())
        .or_else(|| {
            credential
                .refresh
                .as_ref()
                .map(|refresh| refresh.token.fingerprint())
        })
        .unwrap_or_else(|| credential.secret.fingerprint())
}

fn fetch(
    token: &Secret,
    account_key: &str,
    client_header: &str,
    now: i64,
    by: Instant,
) -> Result<Limits, Problem> {
    if let Some((project, plan)) = lock_cache().cached_project(account_key, now) {
        match fetch_quota(token, &project, plan, client_header, by) {
            Ok(limits) => return Ok(limits),
            Err(Problem::Unavailable {
                ref why,
                retry_after: None,
                answered: true,
            }) if !why.contains("answered 429") && !why.contains("answered 5") => {
                lock_cache().evict_project(account_key);
            }
            Err(other) => return Err(other),
        }
    }
    let client = json!({"metadata": {
        "ideType": "ANTIGRAVITY",
        "platform": "PLATFORM_UNSPECIFIED",
        "pluginType": "GEMINI",
    }});
    let signed_in = super::asked_bearer(PLAN, token, &[client_header], Some(&client), &[401], by)?;
    let project = signed_in["cloudaicompanionProject"]
        .as_str()
        .filter(|project| !project.is_empty())
        .ok_or_else(|| {
            Problem::unavailable("no project to ask: Antigravity makes one when first used")
        })?;
    let tier = plan(&signed_in);
    lock_cache().store_project(
        account_key.to_owned(),
        project.to_owned(),
        tier.clone(),
        now,
    );
    fetch_quota(token, project, tier, client_header, by)
}

fn fetch_quota(
    token: &Secret,
    project: &str,
    plan: Option<String>,
    client_header: &str,
    by: Instant,
) -> Result<Limits, Problem> {
    let answer = super::asked_bearer(
        QUOTA,
        token,
        &[client_header],
        Some(&json!({"project": project})),
        &[401],
        by,
    )?;
    let readings = parse(&answer)
        .filter(|readings| !readings.is_empty())
        .ok_or_else(|| Problem::not_understood(QUOTA))?;
    Ok(Limits { plan, readings })
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl FileStamp {
    fn of(path: &Path) -> Option<FileStamp> {
        let meta = std::fs::metadata(path).ok()?;
        Some(FileStamp {
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }
}

#[derive(Default)]
struct Cache {
    /// Extracted OAuth client pairs per binary file, invalidated when its size
    /// or modification time changes.
    binaries: HashMap<PathBuf, (FileStamp, Vec<(String, Secret)>)>,
    /// Cached Antigravity `Info.plist` version, invalidated on app updates.
    versions: HashMap<PathBuf, (FileStamp, Option<String>)>,
    /// The last OAuth client pair accepted by Google's token endpoint.
    client: Option<(String, Secret)>,
    /// In-memory refreshed access tokens, keyed by `refresh_token.fingerprint()`.
    tokens: HashMap<String, (Secret, i64)>,
    /// Cached `(project, plan, fetched_at)` per account, to skip
    /// `loadCodeAssist` on routine reads.
    projects: HashMap<String, (String, Option<String>, i64)>,
}

impl Cache {
    fn cached_token(&self, fingerprint: &str, now: i64) -> Option<Secret> {
        let (token, expires_at) = self.tokens.get(fingerprint)?;
        (*expires_at > now + MINUTE).then(|| Secret::new(token.expose()))
    }

    fn store_token(&mut self, fingerprint: String, token: &Secret, expires_at: i64, now: i64) {
        if self.tokens.len() >= 16 {
            self.tokens.retain(|_, (_, exp)| *exp > now + MINUTE);
        }
        self.tokens
            .insert(fingerprint, (Secret::new(token.expose()), expires_at));
    }

    fn evict_token(&mut self, fingerprint: &str) {
        self.tokens.remove(fingerprint);
    }

    fn cached_project(&self, account: &str, now: i64) -> Option<(String, Option<String>)> {
        let (project, plan, at) = self.projects.get(account)?;
        (now - *at < PROJECT_TTL && *at <= now + MINUTE).then(|| (project.clone(), plan.clone()))
    }

    fn store_project(&mut self, account: String, project: String, plan: Option<String>, now: i64) {
        if self.projects.len() >= 16 {
            self.projects
                .retain(|_, (_, _, at)| now - *at < PROJECT_TTL);
        }
        self.projects.insert(account, (project, plan, now));
    }

    fn evict_project(&mut self, account: &str) {
        self.projects.remove(account);
    }

    fn remember_client(&mut self, id: &str, secret: &Secret) {
        self.client = Some((id.to_owned(), Secret::new(secret.expose())));
    }

    fn forget_client(&mut self, id: &str, secret: &Secret) {
        if self.client.as_ref().is_some_and(|(kept_id, kept_secret)| {
            kept_id == id && kept_secret.expose() == secret.expose()
        }) {
            self.client = None;
        }
    }

    fn cached_binaries(&mut self, binaries: &[PathBuf]) -> Vec<(String, Secret)> {
        let mut pairs: Vec<(String, Secret)> = Vec::new();
        let mut push = |id: &str, secret: &Secret| {
            if !pairs.iter().any(|(seen_id, seen_secret)| {
                seen_id == id && seen_secret.expose() == secret.expose()
            }) {
                pairs.push((id.to_owned(), Secret::new(secret.expose())));
            }
        };
        if let Some((id, secret)) = &self.client {
            push(id, secret);
        }
        for path in binaries {
            let Some(stamp) = FileStamp::of(path) else {
                self.binaries.remove(path);
                continue;
            };
            let known = self
                .binaries
                .get(path)
                .filter(|(kept, _)| *kept == stamp)
                .map(|(_, found)| found);
            if let Some(found) = known {
                for (id, secret) in found {
                    push(id, secret);
                }
            }
        }
        pairs
    }
}

fn lock_cache() -> std::sync::MutexGuard<'static, Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(Cache::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The `User-Agent` header Antigravity's app sends, taking the installed app's
/// version from `Info.plist` when one of `binaries` lives inside an app bundle.
fn user_agent(binaries: &[PathBuf]) -> String {
    let version = binaries
        .iter()
        .find_map(|binary| app_version(binary))
        .unwrap_or_else(|| FALLBACK_VERSION.to_owned());
    format!("User-Agent: antigravity/{version} (aidev_client; os_type=darwin; arch={ARCH})")
}

/// Read `CFBundleShortVersionString` from `Antigravity.app/Contents/Info.plist`
/// beside `binary` (`.../Contents/Resources/bin/language_server`), cached by
/// file stamp.
fn app_version(binary: &Path) -> Option<String> {
    // .../Contents/Resources/bin/language_server -> .../Contents/Info.plist
    let contents = binary.parent()?.parent()?.parent()?;
    if contents.file_name().and_then(|name| name.to_str()) != Some("Contents") {
        return None;
    }
    let plist = contents.join("Info.plist");
    let stamp = FileStamp::of(&plist)?;
    if let Some((kept, version)) = lock_cache().versions.get(&plist)
        && *kept == stamp
    {
        return version.clone();
    }
    let version = std::fs::read_to_string(&plist)
        .ok()
        .and_then(|text| plist_version(&text));
    lock_cache()
        .versions
        .insert(plist, (stamp, version.clone()));
    version
}

/// Extract `CFBundleShortVersionString` from an XML `Info.plist`, checking
/// that it holds only safe version characters so it can never inject headers.
fn plist_version(xml: &str) -> Option<String> {
    let after_key = xml.split_once("<key>CFBundleShortVersionString</key>")?.1;
    let after_open = after_key.split_once("<string>")?.1;
    let value = after_open.split_once("</string>")?.0.trim();
    let valid = !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));
    valid.then(|| value.to_owned())
}

/// Candidate `(client_id, client_secret)` pairs for `binaries`: the last
/// working client first, then pairs extracted from each binary (reading a
/// binary outside the cache lock when it's new or changed on disk).
fn client_pairs(binaries: &[PathBuf]) -> Vec<(String, Secret)> {
    let mut missing = Vec::new();
    {
        let cache = lock_cache();
        for path in binaries {
            let Some(stamp) = FileStamp::of(path) else {
                continue;
            };
            let current = cache
                .binaries
                .get(path)
                .is_some_and(|(kept, _)| *kept == stamp);
            if !current {
                missing.push((path.clone(), stamp));
            }
        }
    }
    if !missing.is_empty() {
        let read: Vec<_> = missing
            .into_iter()
            .map(|(path, stamp)| {
                let found = read_clients(&path);
                (path, stamp, found)
            })
            .collect();
        let mut cache = lock_cache();
        for (path, stamp, found) in read {
            cache.binaries.insert(path, (stamp, found));
        }
    }
    lock_cache().cached_binaries(binaries)
}

enum Exchange {
    Token(Secret, i64),
    WrongClient,
    Failed(Problem),
}

fn refresh_access(refresh: &Refresh, now: i64, by: Instant) -> Result<(Secret, i64), Problem> {
    if refresh.binaries.is_empty() {
        return Err(Problem::unanswered(
            "Antigravity wasn't found to refresh its Google login".to_owned(),
        ));
    }
    let pairs = client_pairs(&refresh.binaries);
    if pairs.is_empty() {
        return Err(Problem::unanswered(
            "no Google OAuth client was found in Antigravity".to_owned(),
        ));
    }
    for (id, secret) in pairs {
        let answered = super::post_form(
            TOKEN,
            &[
                ("client_id", id.as_str()),
                ("client_secret", secret.expose()),
                ("refresh_token", refresh.token.expose()),
                ("grant_type", "refresh_token"),
            ],
            by,
        )?;
        match parse_refresh(&answered, now) {
            Exchange::Token(token, expires_at) => {
                lock_cache().remember_client(&id, &secret);
                return Ok((token, expires_at));
            }
            Exchange::WrongClient => {
                lock_cache().forget_client(&id, &secret);
            }
            Exchange::Failed(problem) => return Err(problem),
        }
    }
    Err(Problem::unavailable(format!(
        "{TOKEN}: OAuth client refused"
    )))
}

fn parse_refresh(answered: &Answered, now: i64) -> Exchange {
    match answered.status {
        200..=299 => {
            let Ok(body) = serde_json::from_slice::<Value>(&answered.body) else {
                return Exchange::Failed(Problem::not_understood(TOKEN));
            };
            let token = body["access_token"]
                .as_str()
                .filter(|token| !token.is_empty());
            let expires_in = body["expires_in"].as_i64().filter(|seconds| *seconds > 0);
            match (token, expires_in) {
                (Some(token), Some(seconds)) => Exchange::Token(
                    Secret::new(token),
                    now.saturating_add(seconds.saturating_mul(1000)),
                ),
                _ => Exchange::Failed(Problem::not_understood(TOKEN)),
            }
        }
        400 | 401 => {
            let error = serde_json::from_slice::<Value>(&answered.body)
                .ok()
                .and_then(|body| body["error"].as_str().map(str::to_owned));
            match error.as_deref() {
                Some("invalid_grant") => Exchange::Failed(Problem::SignIn),
                Some("invalid_client" | "unauthorized_client") => Exchange::WrongClient,
                _ => Exchange::Failed(Problem::Unavailable {
                    why: format!("{TOKEN}: answered {}", answered.status),
                    retry_after: answered.retry_after,
                    answered: true,
                }),
            }
        }
        status => Exchange::Failed(Problem::Unavailable {
            why: format!("{TOKEN}: answered {status}"),
            retry_after: answered.retry_after,
            answered: true,
        }),
    }
}

/// Read Google OAuth `(client_id, client_secret)` pairs from the Mach-O
/// `__TEXT.__rodata` (or `__cstring`) section of `path`.
fn read_clients(path: &Path) -> Vec<(String, Secret)> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let Some((slice_offset, sections)) = macho_text_sections(&mut file) else {
        return Vec::new();
    };
    for (offset, size) in sections {
        let Ok(size) = usize::try_from(size) else {
            continue;
        };
        if size == 0 || size > MAX_SECTION {
            continue;
        }
        let Some(at) = slice_offset.checked_add(offset) else {
            continue;
        };
        if file.seek(SeekFrom::Start(at)).is_err() {
            continue;
        }
        let mut bytes = vec![0u8; size];
        if file.read_exact(&mut bytes).is_err() {
            continue;
        }
        let found = extract_clients(&bytes);
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

/// Locate `__TEXT.__rodata` (first) and `__TEXT.__cstring` sections in a
/// 64-bit Mach-O or universal/fat binary. Returns `(slice_offset, sections)`
/// where each section is `(offset_in_slice, size)`.
fn macho_text_sections(file: &mut File) -> Option<(u64, Vec<(u64, u64)>)> {
    let mut magic_bytes = [0u8; 4];
    file.read_exact(&mut magic_bytes).ok()?;
    let magic_be = u32::from_be_bytes(magic_bytes);
    let slice_offset = match magic_be {
        // FAT_MAGIC / FAT_MAGIC_64 (big-endian universal binary headers).
        0xCAFE_BABE | 0xCAFE_BABF => fat_slice_offset(file, magic_be == 0xCAFE_BABF)?,
        _ => 0,
    };
    file.seek(SeekFrom::Start(slice_offset)).ok()?;
    let mut header = [0u8; 32];
    file.read_exact(&mut header).ok()?;
    // MH_MAGIC_64 (little-endian 64-bit Mach-O).
    if le_u32(&header, 0)? != 0xFEED_FACF {
        return None;
    }
    let ncmds = le_u32(&header, 16)? as usize;
    let sizeofcmds = le_u32(&header, 20)? as usize;
    if sizeofcmds > MAX_LOAD_COMMANDS {
        return None;
    }
    let mut commands = vec![0u8; sizeofcmds];
    file.read_exact(&mut commands).ok()?;
    let mut rodata = Vec::new();
    let mut cstring = Vec::new();
    let mut pos = 0usize;
    for _ in 0..ncmds {
        let cmd = le_u32(&commands, pos)?;
        let cmdsize = le_u32(&commands, pos + 4)? as usize;
        if cmdsize < 8 || pos.checked_add(cmdsize)? > commands.len() {
            break;
        }
        // LC_SEGMENT_64 = 0x19; header is 72 bytes, each section_64 is 80 bytes.
        if cmd == 0x19 && cmdsize >= 72 {
            let seg = &commands[pos..pos + cmdsize];
            if cstr(&seg[8..24]) == b"__TEXT" {
                let nsects = le_u32(seg, 64)? as usize;
                for index in 0..nsects {
                    let start = 72usize.checked_add(index.checked_mul(80)?)?;
                    let end = start.checked_add(80)?;
                    if end > seg.len() {
                        break;
                    }
                    let sect = &seg[start..end];
                    let name = cstr(&sect[0..16]);
                    let size = le_u64(sect, 40)?;
                    let offset = u64::from(le_u32(sect, 48)?);
                    if name == b"__rodata" {
                        rodata.push((offset, size));
                    } else if name == b"__cstring" {
                        cstring.push((offset, size));
                    }
                }
            }
        }
        pos += cmdsize;
    }
    rodata.extend(cstring);
    Some((slice_offset, rodata))
}

fn fat_slice_offset(file: &mut File, is_64: bool) -> Option<u64> {
    #[cfg(target_arch = "aarch64")]
    const TARGET_CPU: u32 = 0x0100_000C; // CPU_TYPE_ARM64
    #[cfg(not(target_arch = "aarch64"))]
    const TARGET_CPU: u32 = 0x0100_0007; // CPU_TYPE_X86_64

    let mut count = [0u8; 4];
    file.read_exact(&mut count).ok()?;
    let nfat = u32::from_be_bytes(count).min(32) as usize;
    let entry_len = if is_64 { 32 } else { 20 };
    let mut table = vec![0u8; nfat * entry_len];
    file.read_exact(&mut table).ok()?;
    let mut first = None;
    for index in 0..nfat {
        let entry = &table[index * entry_len..(index + 1) * entry_len];
        let cputype = u32::from_be_bytes(entry[0..4].try_into().ok()?);
        let offset = if is_64 {
            u64::from_be_bytes(entry[8..16].try_into().ok()?)
        } else {
            u64::from(u32::from_be_bytes(entry[8..12].try_into().ok()?))
        };
        if first.is_none() {
            first = Some(offset);
        }
        if cputype == TARGET_CPU {
            return Some(offset);
        }
    }
    first
}

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn le_u64(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

fn cstr(bytes: &[u8]) -> &[u8] {
    match bytes.iter().position(|&byte| byte == 0) {
        Some(end) => &bytes[..end],
        None => bytes,
    }
}

/// Extract Google OAuth `(client_id, client_secret)` pairs from packed Go
/// string data (`__TEXT.__rodata`), prioritizing Antigravity's consumer client
/// and its matching-index secret.
fn extract_clients(bytes: &[u8]) -> Vec<(String, Secret)> {
    let mut ids: Vec<String> = Vec::new();
    let mut pos = 0;
    while let Some(found) = find_bytes(&bytes[pos..], CLIENT_SUFFIX) {
        let suffix_at = pos + found;
        let end = suffix_at + CLIENT_SUFFIX.len();
        pos = end;
        let id = if bytes[..end].ends_with(CONSUMER_CLIENT_ID.as_bytes()) {
            Some(CONSUMER_CLIENT_ID.to_owned())
        } else {
            client_id_before(bytes, suffix_at)
        };
        if let Some(id) = id
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }

    let mut secrets: Vec<Secret> = Vec::new();
    let mut pos = 0;
    while let Some(found) = find_bytes(&bytes[pos..], SECRET_PREFIX) {
        let start = pos + found;
        pos = start + SECRET_PREFIX.len();
        let Some(slice) = bytes.get(start..start + SECRET_LEN) else {
            break;
        };
        // Go packs string literals without NUL terminators, so check exact
        // length (prefix + 28 token chars) rather than scanning to a delimiter.
        let valid = slice[SECRET_PREFIX.len()..]
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if valid
            && let Ok(text) = std::str::from_utf8(slice)
            && !secrets.iter().any(|seen| seen.expose() == text)
        {
            secrets.push(Secret::new(text));
        }
    }

    let mut pairs: Vec<(String, Secret)> = Vec::new();
    let mut add = |id: &str, secret: &Secret| {
        if pairs.len() < MAX_PAIRS
            && !pairs.iter().any(|(seen_id, seen_secret)| {
                seen_id == id && seen_secret.expose() == secret.expose()
            })
        {
            pairs.push((id.to_owned(), Secret::new(secret.expose())));
        }
    };
    if let Some(consumer_index) = ids.iter().position(|id| id == CONSUMER_CLIENT_ID) {
        if let Some(secret) = secrets.get(consumer_index) {
            add(CONSUMER_CLIENT_ID, secret);
        }
        for secret in &secrets {
            add(CONSUMER_CLIENT_ID, secret);
        }
    }
    for (id, secret) in ids.iter().zip(&secrets) {
        add(id, secret);
    }
    for id in &ids {
        for secret in &secrets {
            add(id, secret);
        }
    }
    pairs
}

fn client_id_before(bytes: &[u8], suffix_at: usize) -> Option<String> {
    let mut hash_start = suffix_at;
    while hash_start > 0
        && matches!(
            bytes[hash_start - 1],
            b'a'..=b'z' | b'0'..=b'9' | b'_'
        )
    {
        hash_start -= 1;
    }
    if suffix_at - hash_start < 8 || hash_start == 0 || bytes[hash_start - 1] != b'-' {
        return None;
    }
    let dash = hash_start - 1;
    let mut digits_start = dash;
    while digits_start > 0 && bytes[digits_start - 1].is_ascii_digit() {
        digits_start -= 1;
    }
    if dash - digits_start < 6 {
        return None;
    }
    std::str::from_utf8(&bytes[digits_start..suffix_at + CLIENT_SUFFIX.len()])
        .ok()
        .map(str::to_owned)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The plan in `loadCodeAssist`'s answer: the tier paid for, or for an
/// account that pays for none, the tier it's on.
fn plan(signed_in: &Value) -> Option<String> {
    named(
        signed_in["paidTier"]["name"].as_str(),
        signed_in["currentTier"]["id"].as_str(),
    )
}

/// A plan by the name of the tier paid for, `paid`, without the `Google AI`
/// it begins with, over a name from its id: `Google AI Ultra`'s was
/// `g1-ultra-lite-tier`. Without one, by the id of the `tier` it's on,
/// `free-tier` as `free`.
fn named(paid: Option<&str>, tier: Option<&str>) -> Option<String> {
    let plan = match paid {
        Some(name) => name.strip_prefix(PLANS).unwrap_or(name).trim(),
        None => {
            let tier = tier?;
            tier.strip_suffix("-tier").unwrap_or(tier)
        }
    };
    Some(plan.to_owned()).filter(|plan| !plan.is_empty())
}

/// `None` for an answer with no groups (such as an error sent with 200), a
/// known bucket whose reset time doesn't parse, or no readable buckets at all.
/// Individual buckets with an unknown window shape (such as a new future
/// cadence) are passed over so they don't hide the known windows beside them.
fn parse(answer: &Value) -> Option<Vec<Reading>> {
    let mut readings = Vec::new();
    for group in answer.get("groups")?.as_array()? {
        for bucket in group["buckets"].as_array().into_iter().flatten() {
            // One the plan doesn't have, or with nothing said of what's left.
            let (Some(left), false) = (
                bucket["remainingFraction"].as_f64(),
                bucket["disabled"] == true,
            ) else {
                continue;
            };
            let Some(id) = bucket["bucketId"].as_str().filter(|id| !id.is_empty()) else {
                continue;
            };
            let Some(window) = bucket["window"].as_str() else {
                continue;
            };
            let Some(length) = window_length(window) else {
                continue;
            };
            let resets = super::reset(&bucket["resetTime"])?;
            // The Gemini pool's: `gemini-5h`, `gemini-weekly`.
            let own = id.strip_prefix("gemini-") == Some(window);
            readings.push(Reading {
                key: match own {
                    true => window.to_owned(),
                    false => format!("{window}:{id}"),
                },
                name: super::window_name(length),
                scope: match (own, group["displayName"].as_str().map(scope)) {
                    (true, _) => None,
                    (false, Some(group)) if !group.is_empty() => Some(group.to_owned()),
                    (false, _) => Some(super::words(id)),
                },
                used: ((1.0 - left) * 100.0).clamp(0.0, 100.0),
                size: None,
                starts: resets.map(|resets| resets - length),
                resets,
            });
        }
    }
    (!readings.is_empty()).then_some(readings)
}

fn window_length(window: &str) -> Option<i64> {
    match window {
        "weekly" => Some(7 * DAY),
        hours => hours
            .strip_suffix('h')?
            .parse::<i64>()
            .ok()
            .filter(|&n| n > 0)
            .and_then(|n| n.checked_mul(HOUR)),
    }
}

/// A group's name as a limit's scope, without a last word `models`: `Claude
/// and GPT models` is `Claude and GPT`. A scope names models already, and
/// with the word the app's panel cut the limit's reset short.
fn scope(group: &str) -> &str {
    let group = group.trim();
    match group.rsplit_once(' ') {
        Some((named, last)) if last.eq_ignore_ascii_case("models") => named.trim_end(),
        _ => group,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::time::{DAY, HOUR, millis};

    /// A bucket as the service writes one.
    fn bucket(id: &str, window: &str, left: f64, reset: &str) -> Value {
        json!({
            "bucketId": id,
            "displayName": "Limit Remaining",
            "window": window,
            "resetTime": reset,
            "remainingFraction": left,
        })
    }

    #[test]
    fn the_gemini_pool_is_the_plans_own_and_the_others_are_their_groups() {
        let answer = json!({
            "groups": [
                {
                    "buckets": [
                        bucket("gemini-weekly", "weekly", 0.9770981, "2026-10-08T21:53:07Z"),
                        bucket("gemini-5h", "5h", 1.0, "2026-10-02T07:53:07Z"),
                    ],
                    "displayName": "Gemini Models",
                    "description": "Models within this group: Gemini Flash, Gemini Pro",
                },
                {
                    "buckets": [
                        bucket("3p-weekly", "weekly", 0.0, "2026-10-09T03:04:49Z"),
                        bucket("3p-5h", "5h", 0.25, "2026-10-02T08:04:49Z"),
                        // Passed over: switched off, and nothing said of what's left.
                        json!({"bucketId": "3p-daily", "window": "24h", "remainingFraction": 1, "disabled": true}),
                        json!({"bucketId": "3p-monthly", "window": "720h"}),
                    ],
                    "displayName": "Claude and GPT models",
                },
            ],
            "description": "Within each group, models share a weekly limit and a 5-hour limit.",
        });
        let readings = super::parse(&answer).unwrap();
        let read: Vec<_> = readings
            .iter()
            .map(|reading| {
                (
                    reading.key.as_str(),
                    reading.name.as_str(),
                    reading.scope.as_deref(),
                    (reading.used * 10.0).round() / 10.0,
                )
            })
            .collect();
        assert_eq!(
            read,
            [
                ("weekly", "Weekly", None, 2.3),
                ("5h", "5 hours", None, 0.0),
                ("weekly:3p-weekly", "Weekly", Some("Claude and GPT"), 100.0),
                ("5h:3p-5h", "5 hours", Some("Claude and GPT"), 75.0),
            ]
        );
        let week = millis("2026-10-08T21:53:07Z").unwrap();
        assert_eq!(
            (readings[0].starts, readings[0].resets),
            (Some(week - 7 * DAY), Some(week))
        );
        let hours = millis("2026-10-02T07:53:07Z").unwrap();
        assert_eq!(
            (readings[1].starts, readings[1].resets),
            (Some(hours - 5 * HOUR), Some(hours))
        );
    }

    #[test]
    fn a_scope_is_its_groups_name_without_the_word_models() {
        assert_eq!(super::scope("Claude and GPT models"), "Claude and GPT");
        assert_eq!(super::scope("Gemini Models"), "Gemini");
        assert_eq!(super::scope("Gemini 4 Argon"), "Gemini 4 Argon");
        assert_eq!(super::scope("Models"), "Models");
    }

    #[test]
    fn an_answer_that_doesnt_read_is_no_answer() {
        let one = |window: &str, reset: &str| {
            json!({"groups": [{"displayName": "Gemini Models", "buckets": [
                bucket("gemini-weekly", window, 0.5, reset),
            ]}]})
        };
        assert!(super::parse(&one("weekly", "2026-10-05T00:00:00Z")).is_some());
        assert!(super::parse(&one("monthly", "2026-10-05T00:00:00Z")).is_none());
        assert!(super::parse(&one("weekly", "next Monday")).is_none());
        assert!(super::parse(&json!({"error": {"code": 403}})).is_none());
        // A bucket that says nothing of what's left, or carries an unknown
        // future window shape, is passed over alone when known buckets exist.
        let mut mixed = one("weekly", "2026-10-05T00:00:00Z");
        let buckets = mixed["groups"][0]["buckets"].as_array_mut().unwrap();
        buckets.push(json!({"bucketId": "gemini-5h", "window": "5h", "remainingFraction": null}));
        buckets.push(bucket(
            "gemini-monthly",
            "monthly",
            0.9,
            "2026-11-01T00:00:00Z",
        ));
        assert_eq!(super::parse(&mixed).map(|read| read.len()), Some(1));
    }

    #[test]
    fn a_plan_is_what_google_names_the_tier_paid_for() {
        let paid = json!({
            "currentTier": {"id": "free-tier", "name": "Antigravity"},
            "paidTier": {"id": "g1-ultra-lite-tier", "name": "Google AI Ultra"},
            "cloudaicompanionProject": "made-up-project",
        });
        assert_eq!(super::plan(&paid).as_deref(), Some("Ultra"));
        let free = json!({"currentTier": {"id": "free-tier", "name": "Antigravity"}});
        assert_eq!(super::plan(&free).as_deref(), Some("free"));
        // What a client the service doesn't know gets.
        let unknown = json!({"allowedTiers": [{"id": "standard-tier"}], "ineligibleTiers": [{"reasonCode": "UNSUPPORTED_CLIENT"}]});
        assert_eq!(super::plan(&unknown), None);
    }

    /// Build a minimal 64-bit Mach-O binary holding `rodata` in `__TEXT.__rodata`.
    fn macho64(rodata: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let cmdsize: u32 = 72 + 80;
        let sect_offset: u32 = 32 + cmdsize;
        // Mach-O 64-bit header (32 bytes).
        out.extend_from_slice(&0xFEED_FACFu32.to_le_bytes()); // MH_MAGIC_64
        out.extend_from_slice(&0x0100_000Cu32.to_le_bytes()); // CPU_TYPE_ARM64
        out.extend_from_slice(&0u32.to_le_bytes()); // cpusubtype
        out.extend_from_slice(&2u32.to_le_bytes()); // MH_EXECUTE
        out.extend_from_slice(&1u32.to_le_bytes()); // ncmds
        out.extend_from_slice(&cmdsize.to_le_bytes()); // sizeofcmds
        out.extend_from_slice(&0u32.to_le_bytes()); // flags
        out.extend_from_slice(&0u32.to_le_bytes()); // reserved
        // LC_SEGMENT_64 (72 bytes).
        out.extend_from_slice(&0x19u32.to_le_bytes());
        out.extend_from_slice(&cmdsize.to_le_bytes());
        let mut segname = [0u8; 16];
        segname[..6].copy_from_slice(b"__TEXT");
        out.extend_from_slice(&segname);
        out.extend_from_slice(&[0u8; 40]); // vmaddr..initprot
        out.extend_from_slice(&1u32.to_le_bytes()); // nsects
        out.extend_from_slice(&0u32.to_le_bytes()); // flags
        // section_64 (80 bytes).
        let mut sectname = [0u8; 16];
        sectname[..8].copy_from_slice(b"__rodata");
        out.extend_from_slice(&sectname);
        out.extend_from_slice(&segname);
        out.extend_from_slice(&0u64.to_le_bytes()); // addr
        out.extend_from_slice(&(rodata.len() as u64).to_le_bytes()); // size
        out.extend_from_slice(&sect_offset.to_le_bytes()); // offset
        out.extend_from_slice(&[0u8; 28]); // align..reserved3
        out.extend_from_slice(rodata);
        out
    }

    #[test]
    fn oauth_clients_are_read_from_macho_rodata_and_prioritize_the_consumer_client() {
        let dir = tempfile::tempdir().unwrap();
        let secret_a = format!("GOCSPX-{}", "A".repeat(28));
        let secret_b = format!("GOCSPX-{}", "B".repeat(28));
        let other_id = "222222222222-otherclient000000000000000000000.apps.googleusercontent.com";
        // Packed without NUL terminators, as Go writes `__TEXT.__rodata`.
        let packed = format!(
            "prefix{other_id}mid{}tail{secret_a}decoding{secret_b}more",
            super::CONSUMER_CLIENT_ID
        );

        let thin = dir.path().join("thin");
        std::fs::write(&thin, macho64(packed.as_bytes())).unwrap();
        let pairs: Vec<(String, String)> = super::read_clients(&thin)
            .into_iter()
            .map(|(id, secret)| (id, secret.expose().to_owned()))
            .collect();
        // Consumer id is index 1, paired first with index 1 secret (`secret_b`).
        assert_eq!(
            pairs.first(),
            Some(&(super::CONSUMER_CLIENT_ID.to_owned(), secret_b.clone()))
        );
        assert!(pairs.contains(&(other_id.to_owned(), secret_a.clone())));

        // Universal (fat) binary wrapping the thin slice at offset 64.
        let slice = macho64(packed.as_bytes());
        let mut fat = Vec::new();
        fat.extend_from_slice(&0xCAFE_BABEu32.to_be_bytes());
        fat.extend_from_slice(&1u32.to_be_bytes()); // nfat_arch
        fat.extend_from_slice(&0x0100_000Cu32.to_be_bytes()); // arm64
        fat.extend_from_slice(&0u32.to_be_bytes());
        fat.extend_from_slice(&64u32.to_be_bytes()); // offset
        fat.extend_from_slice(&(slice.len() as u32).to_be_bytes());
        fat.extend_from_slice(&14u32.to_be_bytes()); // align
        fat.resize(64, 0);
        fat.extend_from_slice(&slice);
        let universal = dir.path().join("fat");
        std::fs::write(&universal, &fat).unwrap();
        let pairs: Vec<(String, String)> = super::read_clients(&universal)
            .into_iter()
            .map(|(id, secret)| (id, secret.expose().to_owned()))
            .collect();
        assert_eq!(
            pairs.first(),
            Some(&(super::CONSUMER_CLIENT_ID.to_owned(), secret_b))
        );
    }

    #[test]
    fn refresh_answers_map_revoked_grants_wrong_clients_and_outages() {
        let answered = |status: u16, body: &str, retry_after: Option<i64>| super::Answered {
            status,
            retry_after,
            body: body.as_bytes().to_vec(),
        };
        let now = 1_790_000_000_000;
        match super::parse_refresh(
            &answered(
                200,
                r#"{"access_token":"ya29.made-up","expires_in":3599}"#,
                None,
            ),
            now,
        ) {
            super::Exchange::Token(token, exp) => {
                assert_eq!(token.expose(), "ya29.made-up");
                assert_eq!(exp, now + 3_599_000);
            }
            _ => panic!("expected refreshed token"),
        }
        assert!(matches!(
            super::parse_refresh(&answered(400, r#"{"error":"invalid_grant"}"#, None), now),
            super::Exchange::Failed(super::Problem::SignIn)
        ));
        assert!(matches!(
            super::parse_refresh(&answered(401, r#"{"error":"invalid_client"}"#, None), now),
            super::Exchange::WrongClient
        ));
        assert!(matches!(
            super::parse_refresh(
                &answered(400, r#"{"error":"unauthorized_client"}"#, None),
                now
            ),
            super::Exchange::WrongClient
        ));
        assert!(matches!(
            super::parse_refresh(&answered(429, "slow down", Some(60_000)), now),
            super::Exchange::Failed(super::Problem::Unavailable {
                retry_after: Some(60_000),
                answered: true,
                ..
            })
        ));
        assert!(matches!(
            super::parse_refresh(&answered(200, r#"{"access_token":""}"#, None), now),
            super::Exchange::Failed(super::Problem::Unavailable { answered: true, .. })
        ));
    }

    #[test]
    fn app_version_is_read_from_info_plist_with_a_safe_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir
            .path()
            .join("Antigravity.app/Contents/Resources/bin/language_server");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"").unwrap();
        let plist = dir.path().join("Antigravity.app/Contents/Info.plist");
        std::fs::write(
            &plist,
            "<plist><dict><key>CFBundleShortVersionString</key><string>2.99.0</string></dict></plist>",
        )
        .unwrap();
        assert!(super::user_agent(&[binary]).starts_with("User-Agent: antigravity/2.99.0 "));
        assert!(super::user_agent(&[]).starts_with("User-Agent: antigravity/2.34.0 "));
        assert_eq!(
            super::plist_version(
                "<key>CFBundleShortVersionString</key><string>1.0\r\nX: y</string>"
            ),
            None
        );
    }
}
