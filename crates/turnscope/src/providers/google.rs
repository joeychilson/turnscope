//! Google: the limits of a Google AI plan, which only Antigravity signs in
//! to, and the account Gemini API keys draw on. An API key carries no limit
//! an agent's key can read.
//!
//! A plan's limits are read as Antigravity reads them, with two POSTs, which
//! change nothing, to the service its app and `agy` talk to. None of it is
//! documented: what follows was measured with this Mac's sign-in on
//! 2026-11-10, asking as OpenUsage and CodexBar do (their code, read that
//! day).
//!
//! - `loadCodeAssist` gives the plan paid for, `paidTier` (`currentTier` was
//!   `free-tier` beside it), and the project Google keeps the usage under,
//!   `cloudaicompanionProject`. Google makes the project when Antigravity is
//!   first used, and Turnscope never does: without one there's nothing to
//!   ask.
//! - `retrieveUserQuotaSummary`, asked with that project, gives `groups` of
//!   `buckets`, each a `window` (`5h`, `weekly`) with the fraction left,
//!   `remainingFraction`, and `resetTime`.
//!
//! **The client.** Asked as `Turnscope`, `loadCodeAssist` answered 200 with
//! no tier, no project, and the reason `UNSUPPORTED_CLIENT`. So both
//! requests name the client as Antigravity's app does.
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
//! **Pools.** A plan has a pool for Gemini models and one for Claude and
//! GPT models, each with a 5-hour and a weekly bucket, and none on all
//! usage. Each bucket is scoped by its pool, as `Weekly (Gemini)` and
//! `Weekly (Claude and GPT)`, over leaving the Gemini pool's unscoped as the
//! plan's own: unscoped, its work rate and shares counted Claude's usage,
//! which never draws on it. An account stands by the pool its latest
//! response drew on (`status::pool`), and with none, by Gemini's, which
//! Antigravity's own models draw on.
//!
//! **Keys.** A limit is keyed by its window, and one not Gemini's by its
//! bucket too, as `weekly:3p-weekly`, over the bucket's id alone: the
//! status lists limits by key, so Gemini's come first, and by id the Claude
//! and GPT pool's came before them.
//!
//! **Renewing.** From the app 2.33.1 on, Antigravity doesn't write the
//! token it renews back to its Keychain item (`agents::antigravity`), so
//! the item's token expires within an hour of signing in. Turnscope renews
//! it as the app does, in memory alone: it sends the item's refresh token
//! to Google with the app's OAuth client, and keeps the token Google gives
//! in this process until it expires, written nowhere. A Google refresh
//! token isn't replaced when used, so renewing takes nothing from the app
//! or `agy`. Chosen over asking the running app for its limits, as 0.3.1
//! did: that read nothing while the app was closed, and nothing from 2.34.0
//! on, which stopped passing the token it's asked with on its command line;
//! and over waiting for Antigravity to write the item, which it doesn't.
//!
//! **The app's client.** Its id is below. Its secret is read from the
//! app's program, where Go keeps its strings, in `__TEXT,__rodata`: that
//! held 3 client ids, this one among them, and 3 secrets, each `GOCSPX-`
//! and 28 characters, in the app 2.34.0's `language_server` and in `agy`
//! (read 2026-11-22). Nothing there says which secret is this client's, so
//! each is tried until Google takes one, and the one taken is tried first
//! after. Google answers a wrong secret `invalid_client`, and a refresh
//! token it no longer takes `invalid_grant`: a login to sign in to again.
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
//! over reading it as used up. A renewal with a real sign-in: that the
//! client id is the one consumer sign-ins are made with, and how Google
//! answers a wrong secret, are as the app's program and Google's OAuth
//! documentation say.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

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

/// How Antigravity's app names itself.
#[cfg(target_arch = "aarch64")]
const CLIENT: &str = "User-Agent: antigravity/2.31.0 (aidev_client; os_type=darwin; arch=arm64)";
#[cfg(not(target_arch = "aarch64"))]
const CLIENT: &str = "User-Agent: antigravity/2.31.0 (aidev_client; os_type=darwin; arch=amd64)";

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

impl Provider for Google {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn account(&self, credential: &Credential) -> Option<Account> {
        super::account(INFO.id, credential, || credential.identity.clone())
    }

    fn limits(&self, credential: &Credential, now: i64, by: Instant) -> Result<Limits, Problem> {
        match &credential.refresh {
            Some(refresh) if !credential.fresh(now) => renewed(refresh, now, by),
            _ => read(&credential.secret, by),
        }
    }
}

/// The plan's limits, asked with `token`.
fn read(token: &Secret, by: Instant) -> Result<Limits, Problem> {
    let client = json!({"metadata": {
        "ideType": "ANTIGRAVITY",
        "platform": "PLATFORM_UNSPECIFIED",
        "pluginType": "GEMINI",
    }});
    let signed_in = super::asked(PLAN, token, &[CLIENT], Some(&client), &[401], by)?;
    let project = signed_in["cloudaicompanionProject"]
        .as_str()
        .filter(|project| !project.is_empty())
        .ok_or_else(|| {
            Problem::unavailable("no project to ask: Antigravity makes one when first used")
        })?;
    let answer = super::asked(
        QUOTA,
        token,
        &[CLIENT],
        Some(&json!({"project": project})),
        &[401],
        by,
    )?;
    let readings = parse(&answer)
        .filter(|readings| !readings.is_empty())
        .ok_or_else(|| Problem::not_understood(QUOTA))?;
    Ok(Limits {
        plan: plan(&signed_in),
        readings,
    })
}

/// Tokens renewed in this process, by their refresh token's fingerprint,
/// each with when it expires. A token is taken out while it's used, so no
/// lock is held over a request.
static RENEWED: Mutex<BTreeMap<String, (Secret, i64)>> = Mutex::new(BTreeMap::new());

/// The fingerprint of the client secret Google last took.
static TAKEN: Mutex<Option<String>> = Mutex::new(None);

fn locked<T>(kept: &Mutex<T>) -> MutexGuard<'_, T> {
    kept.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The plan's limits, asked with a token renewed from `refresh`: the one
/// renewed before, while it's good, or else a new one. A token that's
/// refused isn't kept.
fn renewed(refresh: &Refresh, now: i64, by: Instant) -> Result<Limits, Problem> {
    let key = refresh.token.fingerprint();
    let kept = locked(&RENEWED)
        .remove(&key)
        .filter(|(_, expires)| *expires > now + MINUTE);
    let (token, expires) = match kept {
        Some(kept) => kept,
        None => renew(refresh, now, by)?,
    };
    let limits = read(&token, by);
    if !matches!(limits, Err(Problem::SignIn)) {
        let mut renewed = locked(&RENEWED);
        renewed.retain(|_, (_, expires)| *expires > now);
        renewed.insert(key, (token, expires));
    }
    limits
}

/// A new token for `refresh`'s login, and when it expires, from Google.
fn renew(refresh: &Refresh, now: i64, by: Instant) -> Result<(Secret, i64), Problem> {
    let mut secrets = refresh
        .programs
        .iter()
        .find_map(|program| secrets(program))
        .ok_or_else(|| Problem::unanswered("no Antigravity program holds its client"))?;
    let taken = locked(&TAKEN).clone();
    secrets.sort_by_key(|secret| Some(secret.fingerprint()) != taken);
    for secret in &secrets {
        let answered = super::post_form(
            TOKEN,
            &[
                ("client_id", CLIENT_ID),
                ("client_secret", secret.expose()),
                ("refresh_token", refresh.token.expose()),
                ("grant_type", "refresh_token"),
            ],
            by,
        )?;
        if let Some(token) = token(&answered, now)? {
            *locked(&TAKEN) = Some(secret.fingerprint());
            return Ok(token);
        }
    }
    Err(Problem::unavailable(format!(
        "{TOKEN}: took none of Antigravity's {} client secrets",
        secrets.len()
    )))
}

/// The token in Google's answer to a renewal, and when it expires; `None`
/// when Google didn't take the client.
fn token(answered: &Answered, now: i64) -> Result<Option<(Secret, i64)>, Problem> {
    let answer: Value = serde_json::from_slice(&answered.body).unwrap_or_default();
    match (answered.status, answer["error"].as_str()) {
        (200..=299, _) => {
            let token = answer["access_token"]
                .as_str()
                .filter(|token| !token.is_empty());
            let lasts = answer["expires_in"].as_i64().filter(|seconds| *seconds > 0);
            match (token, lasts) {
                (Some(token), Some(lasts)) => Ok(Some((
                    Secret::new(token),
                    now.saturating_add(lasts.saturating_mul(1000)),
                ))),
                _ => Err(Problem::not_understood(TOKEN)),
            }
        }
        (400 | 401, Some("invalid_grant")) => Err(Problem::SignIn),
        (400 | 401, Some("invalid_client" | "unauthorized_client")) => Ok(None),
        (status, _) => Err(Problem::Unavailable {
            why: format!("{TOKEN}: answered {status}"),
            retry_after: answered.retry_after,
            answered: true,
        }),
    }
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

/// The `__TEXT,__rodata` section of `program`, a 64-bit Mach-O, as
/// Antigravity's programs are: built for one architecture, not universal
/// (the app 2.34.0's and `agy`, read 2026-11-22).
fn rodata(program: &Path) -> Option<Vec<u8>> {
    fn word(bytes: &[u8], at: usize) -> Option<usize> {
        let word = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?);
        usize::try_from(word).ok()
    }
    fn name(bytes: &[u8]) -> &[u8] {
        bytes.split(|byte| *byte == 0).next().unwrap_or_default()
    }
    let mut file = File::open(program).ok()?;
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
    while let (Some(kind), Some(size)) = (word(&commands, at), word(&commands, at + 4)) {
        // `LC_SEGMENT_64`: 72 bytes, then 80 for each of its sections: its
        // name, its segment's, its address, size and offset.
        if kind == 0x19 {
            for section in (0..word(&commands, at + 64)?).map(|index| at + 72 + 80 * index) {
                let named = commands.get(section..section + 52)?;
                if name(&named[..16]) == b"__rodata" && name(&named[16..32]) == b"__TEXT" {
                    let size = u64::from_le_bytes(named[40..48].try_into().ok()?);
                    let size = usize::try_from(size).ok().filter(|size| *size <= MOST)?;
                    let offset = u64::try_from(word(named, 48)?).ok()?;
                    file.seek(SeekFrom::Start(offset)).ok()?;
                    let mut strings = vec![0; size];
                    file.read_exact(&mut strings).ok()?;
                    return Some(strings);
                }
            }
        }
        at = at.checked_add(size.max(8))?;
    }
    None
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

/// `None` for an answer with no groups, such as an error sent with 200, or
/// one with a bucket that doesn't read.
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
            let id = bucket["bucketId"].as_str().filter(|id| !id.is_empty())?;
            let window = bucket["window"].as_str()?;
            let length = match window {
                "weekly" => 7 * DAY,
                hours => hours.strip_suffix('h')?.parse::<i64>().ok()? * HOUR,
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
                scope: match group["displayName"].as_str().map(scope) {
                    Some(group) if !group.is_empty() => Some(group.to_owned()),
                    _ if own => Some("Gemini".to_owned()),
                    _ => Some(super::words(id)),
                },
                used: ((1.0 - left) * 100.0).clamp(0.0, 100.0),
                size: None,
                starts: resets.map(|resets| resets - length),
                resets,
            });
        }
    }
    Some(readings)
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

    use crate::agents::{Credential, Refresh, Secret};
    use crate::providers::{Answered, Problem, Provider};
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
    fn each_pool_is_scoped_by_its_group() {
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
        // A bucket that says nothing of what's left is passed over alone.
        let mut unsaid = one("weekly", "2026-10-05T00:00:00Z");
        let buckets = unsaid["groups"][0]["buckets"].as_array_mut().unwrap();
        buckets.push(json!({"bucketId": "gemini-5h", "window": "5h", "remainingFraction": null}));
        assert_eq!(super::parse(&unsaid).map(|read| read.len()), Some(1));
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

    #[test]
    fn a_renewed_token_is_asked_with_again_until_it_expires() {
        let now = 1_790_000_000_000;
        let login = Credential {
            provider: "google",
            secret: Secret::new("made-up-expired-token"),
            key: false,
            expires: Some(now - HOUR),
            identity: None,
            plan: None,
            refresh: Some(Refresh {
                token: Secret::new("made-up-refresh-token"),
                programs: Vec::new(),
            }),
        };
        let key = login.refresh.as_ref().unwrap().token.fingerprint();
        let renewed = |expires| {
            super::locked(&super::RENEWED)
                .insert(key.clone(), (Secret::new("made-up-renewed"), expires))
        };
        // With the read's time spent, nothing is sent: twice, the renewed
        // token is taken, asked with, and kept.
        renewed(now + HOUR);
        let spent = std::time::Instant::now();
        let (done, reads) = std::sync::mpsc::channel();
        let twice = std::thread::spawn(move || {
            let read = || super::Google.limits(&login, now, spent).err();
            done.send([read(), read()]).unwrap();
            login
        });
        let not_asked = Problem::unanswered(format!(
            "{}: not asked, as the read's time was spent",
            super::PLAN
        ));
        let read = reads.recv_timeout(std::time::Duration::from_secs(5));
        assert_eq!(read, Ok([Some(not_asked.clone()), Some(not_asked)]));
        assert!(super::locked(&super::RENEWED).contains_key(&key));
        // One that has expired is renewed: with no program to renew it with,
        // nothing is.
        renewed(now);
        let login = twice.join().unwrap();
        assert_eq!(
            super::Google.limits(&login, now, spent).err(),
            Some(Problem::unanswered(
                "no Antigravity program holds its client"
            ))
        );
    }

    #[test]
    fn a_renewal_is_a_token_a_refused_login_or_a_client_google_doesnt_take() {
        let now = 1_790_000_000_000;
        let answered = |status: u16, body: &str| Answered {
            status,
            retry_after: None,
            body: body.as_bytes().to_vec(),
        };
        let token = |status, body| {
            super::token(&answered(status, body), now)
                .map(|token| token.map(|(token, expires)| (token.expose().to_owned(), expires)))
        };
        assert_eq!(
            token(200, r#"{"access_token": "made-up", "expires_in": 3599}"#),
            Ok(Some(("made-up".to_owned(), now + 3_599_000)))
        );
        assert_eq!(
            token(400, r#"{"error": "invalid_grant"}"#),
            Err(Problem::SignIn)
        );
        assert_eq!(token(401, r#"{"error": "invalid_client"}"#), Ok(None));
        assert_eq!(token(400, r#"{"error": "unauthorized_client"}"#), Ok(None));
        assert_eq!(
            token(200, r#"{"access_token": ""}"#),
            Err(Problem::not_understood(super::TOKEN))
        );
        assert!(matches!(
            token(503, "down"),
            Err(Problem::Unavailable { answered: true, .. })
        ));
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

    #[test]
    fn the_secrets_are_read_from_the_program_that_holds_the_client() {
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (
            format!("GOCSPX-{}", "a".repeat(28)),
            format!("GOCSPX-{}", "B_-9".repeat(7)),
        );
        // Go packs its strings one against the next, with nothing between.
        let strings = format!("{one}gzip{}{two}{one}GOCSPX-short", super::CLIENT_ID);
        let held = dir.path().join("language_server");
        std::fs::write(&held, program(strings.as_bytes())).unwrap();
        let secrets: Vec<String> = super::secrets(&held)
            .unwrap()
            .iter()
            .map(|secret| secret.expose().to_owned())
            .collect();
        assert_eq!(secrets, [one.clone(), two]);

        // Not Antigravity's: no client id, or not a Mach-O, or cut short.
        let other = dir.path().join("other");
        std::fs::write(&other, program(one.as_bytes())).unwrap();
        assert!(super::secrets(&other).is_none());
        std::fs::write(&other, strings.as_bytes()).unwrap();
        assert!(super::secrets(&other).is_none());
        let whole = program(strings.as_bytes());
        std::fs::write(&other, &whole[..whole.len() - 10]).unwrap();
        assert!(super::secrets(&other).is_none());
        assert!(super::secrets(&dir.path().join("missing")).is_none());
    }
}
