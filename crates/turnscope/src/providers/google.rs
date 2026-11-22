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
//! **Model pools.** A plan has peer pools — one for Gemini models and one
//! for Claude and GPT models — each with a 5-hour and a weekly bucket, and
//! no combined cap across all models. Every bucket carries its group's scope
//! (`Gemini`, `Claude and GPT`) so each pool's work rate and "Used most"
//! breakdown count only its own models, and `status::deciding` compares all
//! peer pools equally when an account has no whole-account limit.
//!
//! **Keys.** Gemini buckets keep keys `5h` and `weekly`, and other pools key
//! by window and bucket id (`weekly:3p-weekly`), preserving existing reading
//! history and listing Gemini before third-party pools.
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
//! count Gemini models. A used-up bucket: it should still have
//! `remainingFraction: 0`, as CodexBar reads `agy`'s form of the answer,
//! where a fraction that's set is written even at zero. So a bucket without
//! it says nothing of what's left and is passed over, as OpenUsage does,
//! over reading it as used up.

use std::borrow::Cow;
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

const PLANS: &str = "Google AI";

static INFO: Info = Info {
    id: "google",
    name: "Gemini",
    subscription: Some(PLANS),
    key_limits: false,
};

const PLAN: &str = "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
const QUOTA: &str = "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary";
const TOKEN: &str = "https://oauth2.googleapis.com/token";

const CONSUMER_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const CLIENT_SUFFIX: &[u8] = b".apps.googleusercontent.com";
const SECRET_PREFIX: &[u8] = b"GOCSPX-";
const SECRET_LEN: usize = 35;

/// Bounds on Mach-O reads so a corrupted header can't ask for gigabytes.
const MAX_LOAD_COMMANDS: usize = 16 * 1024 * 1024;
const MAX_SECTION: usize = 64 * 1024 * 1024;
const MAX_PAIRS: usize = 16;

const PROJECT_TTL: i64 = HOUR;
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

    fn owns_model(&self, model: &str) -> bool {
        model.starts_with("gemini-")
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
            return Err(Problem::Expired);
        };
        let fingerprint = refresh.token.fingerprint();
        if let Some(token) = lock_cache().cached_token(&fingerprint, now) {
            match fetch(&token, &account_key, &client_header, now, by) {
                Err(Problem::SignIn) => {
                    lock_cache().tokens.remove(&fingerprint);
                }
                other => return other,
            }
        }
        let (token, expires_at) = refresh_access(refresh, now, by)?;
        let result = fetch(&token, &account_key, &client_header, now, by);
        if !matches!(result, Err(Problem::SignIn)) {
            lock_cache().store_token(fingerprint, token, expires_at, now);
        }
        result
    }
}

fn account_key(credential: &Credential) -> Cow<'_, str> {
    if let Some((id, _)) = &credential.identity {
        return Cow::Borrowed(id);
    }
    Cow::Owned(credential.refresh.as_ref().map_or_else(
        || credential.secret.fingerprint(),
        |r| r.token.fingerprint(),
    ))
}

enum QuotaError {
    /// Cached project was rejected (HTTP 400/403/404); re-fetch via `loadCodeAssist`.
    ProjectRejected(Problem),
    Other(Problem),
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
            Err(QuotaError::ProjectRejected(_)) => {
                lock_cache().projects.remove(account_key);
            }
            Err(QuotaError::Other(problem)) => return Err(problem),
        }
    }
    let client = json!({"metadata": {
        "ideType": "ANTIGRAVITY",
        "platform": "PLATFORM_UNSPECIFIED",
        "pluginType": "GEMINI",
    }});
    let signed_in = super::asked(PLAN, token, &[client_header], Some(&client), &[401], by)?;
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
    fetch_quota(token, project, tier, client_header, by).map_err(|err| match err {
        QuotaError::ProjectRejected(problem) | QuotaError::Other(problem) => problem,
    })
}

fn fetch_quota(
    token: &Secret,
    project: &str,
    plan: Option<String>,
    client_header: &str,
    by: Instant,
) -> Result<Limits, QuotaError> {
    let answered = super::asked_raw(
        QUOTA,
        token,
        &[client_header],
        Some(&json!({"project": project})),
        by,
    )
    .map_err(QuotaError::Other)?;
    if matches!(answered.status, 400 | 403 | 404) {
        return Err(QuotaError::ProjectRejected(answered.unavailable(QUOTA)));
    }
    let answer = super::answer(QUOTA, &answered, &[401]).map_err(QuotaError::Other)?;
    let readings = parse(&answer)
        .filter(|readings| !readings.is_empty())
        .ok_or_else(|| QuotaError::Other(Problem::not_understood(QUOTA)))?;
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
    binaries: HashMap<PathBuf, (FileStamp, Vec<(String, Secret)>)>,
    versions: HashMap<PathBuf, (FileStamp, Option<String>)>,
    client: Option<(String, Secret)>,
    tokens: HashMap<String, (Secret, i64)>,
    projects: HashMap<String, (String, Option<String>, i64)>,
}

impl Cache {
    fn cached_token(&self, fingerprint: &str, now: i64) -> Option<Secret> {
        let (token, expires_at) = self.tokens.get(fingerprint)?;
        (*expires_at > now + MINUTE).then(|| Secret::new(token.expose()))
    }

    fn store_token(&mut self, fingerprint: String, token: Secret, expires_at: i64, now: i64) {
        if self.tokens.len() >= 16 {
            self.tokens.retain(|_, (_, exp)| *exp > now + MINUTE);
        }
        self.tokens.insert(fingerprint, (token, expires_at));
    }

    fn cached_project(&self, account: &str, now: i64) -> Option<(String, Option<String>)> {
        let (project, plan, at) = self.projects.get(account)?;
        (now - *at < PROJECT_TTL && *at <= now + MINUTE).then(|| (project.clone(), plan.clone()))
    }

    fn store_project(&mut self, account: String, project: String, plan: Option<String>, now: i64) {
        if self.projects.len() >= 16 {
            self.projects
                .retain(|_, (_, _, at)| now - *at < PROJECT_TTL && *at <= now + MINUTE);
        }
        self.projects.insert(account, (project, plan, now));
    }

    fn collect_pairs(&self, stamped: &[(&PathBuf, FileStamp)]) -> Vec<(String, Secret)> {
        let mut pairs = Vec::new();
        if let Some((id, secret)) = &self.client {
            push_unique_pair(&mut pairs, id, secret, usize::MAX);
        }
        for (path, stamp) in stamped {
            if let Some((kept, found)) = self.binaries.get(*path)
                && kept == stamp
            {
                for (id, secret) in found {
                    push_unique_pair(&mut pairs, id, secret, usize::MAX);
                }
            }
        }
        pairs
    }
}

fn push_unique_pair(pairs: &mut Vec<(String, Secret)>, id: &str, secret: &Secret, max: usize) {
    if pairs.len() < max
        && !pairs
            .iter()
            .any(|(seen_id, seen_secret)| seen_id == id && seen_secret.expose() == secret.expose())
    {
        pairs.push((id.to_owned(), Secret::new(secret.expose())));
    }
}

fn lock_cache() -> std::sync::MutexGuard<'static, Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(Cache::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn user_agent(binaries: &[PathBuf]) -> String {
    let version = binaries
        .iter()
        .find_map(|binary| app_version(binary))
        .unwrap_or_else(|| FALLBACK_VERSION.to_owned());
    format!("User-Agent: antigravity/{version} (aidev_client; os_type=darwin; arch={ARCH})")
}

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

/// Validates version characters so the `Info.plist` value can't inject headers.
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

fn client_pairs(binaries: &[PathBuf]) -> Vec<(String, Secret)> {
    let stamped: Vec<(&PathBuf, Option<FileStamp>)> = binaries
        .iter()
        .map(|path| (path, FileStamp::of(path)))
        .collect();
    let existing: Vec<(&PathBuf, FileStamp)> = stamped
        .iter()
        .filter_map(|(path, stamp)| Some((*path, (*stamp)?)))
        .collect();
    let missing: Vec<(&PathBuf, FileStamp)> = {
        let mut cache = lock_cache();
        for (path, stamp) in &stamped {
            if stamp.is_none() {
                cache.binaries.remove(*path);
            }
        }
        let missing: Vec<_> = existing
            .iter()
            .copied()
            .filter(|(path, stamp)| {
                !cache
                    .binaries
                    .get(*path)
                    .is_some_and(|(kept, _)| *kept == *stamp)
            })
            .collect();
        if missing.is_empty() {
            return cache.collect_pairs(&existing);
        }
        missing
    };
    let read: Vec<_> = missing
        .into_iter()
        .map(|(path, stamp)| (path.clone(), stamp, read_clients(path)))
        .collect();
    let mut cache = lock_cache();
    for (path, stamp, found) in read {
        cache.binaries.insert(path, (stamp, found));
    }
    cache.collect_pairs(&existing)
}

enum Exchange {
    Token(Secret, i64),
    WrongClient,
    Failed(Problem),
}

fn refresh_access(refresh: &Refresh, now: i64, by: Instant) -> Result<(Secret, i64), Problem> {
    if refresh.binaries.is_empty() {
        return Err(Problem::unanswered(
            "Antigravity wasn't found to refresh its Google login",
        ));
    }
    let pairs = client_pairs(&refresh.binaries);
    if pairs.is_empty() {
        return Err(Problem::unanswered(
            "no Google OAuth client was found in Antigravity",
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
                lock_cache().client = Some((id, secret));
                return Ok((token, expires_at));
            }
            Exchange::WrongClient => {
                let mut cache = lock_cache();
                if cache.client.as_ref().is_some_and(|(kept_id, kept_secret)| {
                    *kept_id == id && kept_secret.expose() == secret.expose()
                }) {
                    cache.client = None;
                }
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
            let body = serde_json::from_slice::<Value>(&answered.body).ok();
            match body.as_ref().and_then(|body| body["error"].as_str()) {
                Some("invalid_grant") => Exchange::Failed(Problem::SignIn),
                Some("invalid_client" | "unauthorized_client") => Exchange::WrongClient,
                _ => Exchange::Failed(answered.unavailable(TOKEN)),
            }
        }
        _ => Exchange::Failed(answered.unavailable(TOKEN)),
    }
}

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
        let (Some(cmd), Some(cmdsize)) = (le_u32(&commands, pos), le_u32(&commands, pos + 4))
        else {
            break;
        };
        let cmdsize = cmdsize as usize;
        let Some(end) = pos.checked_add(cmdsize) else {
            break;
        };
        if cmdsize < 8 || end > commands.len() {
            break;
        }
        // LC_SEGMENT_64 = 0x19; header is 72 bytes, each section_64 is 80 bytes.
        if cmd == 0x19 && cmdsize >= 72 {
            let seg = &commands[pos..end];
            if cstr(&seg[8..24]) == b"__TEXT"
                && let Some(nsects) = le_u32(seg, 64)
            {
                for sect in seg[72..].as_chunks::<80>().0.iter().take(nsects as usize) {
                    let (Some(size), Some(offset)) = (le_u64(sect, 40), le_u32(sect, 48)) else {
                        break;
                    };
                    match cstr(&sect[0..16]) {
                        b"__rodata" => rodata.push((u64::from(offset), size)),
                        b"__cstring" => cstring.push((u64::from(offset), size)),
                        _ => {}
                    }
                }
            }
        }
        pos = end;
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
    for entry in table.chunks_exact(entry_len) {
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

fn extract_clients(bytes: &[u8]) -> Vec<(String, Secret)> {
    let mut ids: Vec<String> = Vec::new();
    let mut pos = 0;
    while let Some(found) = find_bytes(&bytes[pos..], CLIENT_SUFFIX) {
        let suffix_at = pos + found;
        let end = suffix_at + CLIENT_SUFFIX.len();
        pos = end;
        // Go packs string literals without NUL terminators, so check the known
        // consumer client id directly before scanning back through digits.
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
    if let Some(consumer_index) = ids.iter().position(|id| id == CONSUMER_CLIENT_ID) {
        if let Some(secret) = secrets.get(consumer_index) {
            push_unique_pair(&mut pairs, CONSUMER_CLIENT_ID, secret, MAX_PAIRS);
        }
        for secret in &secrets {
            push_unique_pair(&mut pairs, CONSUMER_CLIENT_ID, secret, MAX_PAIRS);
        }
    }
    for (id, secret) in ids.iter().zip(&secrets) {
        push_unique_pair(&mut pairs, id, secret, MAX_PAIRS);
    }
    for id in &ids {
        for secret in &secrets {
            push_unique_pair(&mut pairs, id, secret, MAX_PAIRS);
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
    let (&first, rest) = needle.split_first()?;
    if haystack.len() < needle.len() {
        return None;
    }
    let last_start = haystack.len() - needle.len();
    let mut at = 0;
    while at <= last_start {
        let found = haystack[at..=last_start]
            .iter()
            .position(|&byte| byte == first)?;
        let pos = at + found;
        if haystack[pos + 1..pos + needle.len()] == *rest {
            return Some(pos);
        }
        at = pos + 1;
    }
    None
}

fn plan(signed_in: &Value) -> Option<String> {
    let paid = signed_in["paidTier"]["name"]
        .as_str()
        .map(|name| name.strip_prefix(PLANS).unwrap_or(name).trim())
        .filter(|name| !name.is_empty());
    let plan = paid.or_else(|| {
        signed_in["currentTier"]["id"]
            .as_str()
            .map(|tier| tier.strip_suffix("-tier").unwrap_or(tier))
            .filter(|tier| !tier.is_empty())
    })?;
    Some(plan.to_owned())
}

fn parse(answer: &Value) -> Option<Vec<Reading>> {
    let mut readings = Vec::new();
    for group in answer.get("groups")?.as_array()? {
        let group_scope = group["displayName"]
            .as_str()
            .map(scope)
            .filter(|group| !group.is_empty());
        for bucket in group["buckets"].as_array().into_iter().flatten() {
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
            let own = id.strip_prefix("gemini-") == Some(window);
            readings.push(Reading {
                key: match own {
                    true => window.to_owned(),
                    false => format!("{window}:{id}"),
                },
                name: super::window_name(length),
                scope: match group_scope {
                    Some(group) => Some(group.to_owned()),
                    None if own => Some("Gemini".to_owned()),
                    None => Some(super::words(id)),
                },
                used: ((1.0 - left) * 100.0).clamp(0.0, 100.0),
                size: None,
                starts: resets.and_then(|resets| resets.checked_sub(length)),
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

/// Drops a trailing `models` so the panel's reset line fits without clipping.
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
    fn each_model_pool_keeps_its_group_scope() {
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
                ("weekly", "Weekly", Some("Gemini"), 2.3),
                ("5h", "5 hours", Some("Gemini"), 0.0),
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
        // Empty paid tier name falls back to `currentTier.id`.
        let empty_paid = json!({
            "currentTier": {"id": "free-tier"},
            "paidTier": {"name": "Google AI"},
        });
        assert_eq!(super::plan(&empty_paid).as_deref(), Some("free"));
        // What a client the service doesn't know gets.
        let unknown = json!({"allowedTiers": [{"id": "standard-tier"}], "ineligibleTiers": [{"reasonCode": "UNSUPPORTED_CLIENT"}]});
        assert_eq!(super::plan(&unknown), None);
    }

    fn macho64(rodata: &[u8]) -> Vec<u8> {
        let cmdsize: u32 = 72 + 80;
        let sect_offset: u32 = 32 + cmdsize;
        let mut segname = [0u8; 16];
        segname[..6].copy_from_slice(b"__TEXT");
        let mut sectname = [0u8; 16];
        sectname[..8].copy_from_slice(b"__rodata");
        let mut out = Vec::with_capacity(sect_offset as usize + rodata.len());
        for word in [
            0xFEED_FACF,
            0x0100_000C,
            0,
            2,
            1,
            cmdsize,
            0,
            0,
            0x19,
            cmdsize,
        ] {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.extend_from_slice(&segname);
        out.extend_from_slice(&[0u8; 40]);
        for word in [1u32, 0] {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out.extend_from_slice(&sectname);
        out.extend_from_slice(&segname);
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&(rodata.len() as u64).to_le_bytes());
        out.extend_from_slice(&sect_offset.to_le_bytes());
        out.extend_from_slice(&[0u8; 28]);
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
