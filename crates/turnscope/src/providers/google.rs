//! Google: the limits of a Google AI plan, which only Antigravity signs in
//! to, and the account Gemini API keys draw on, whose keys read no limit.
//!
//! None of it is documented. Measured with this Mac's sign-in on 2026-11-10,
//! asking as OpenUsage and CodexBar do (their code, read that day):
//!
//! - `loadCodeAssist` gives the plan paid for, `paidTier`, and the project
//!   the usage is kept under, `cloudaicompanionProject`, which Google makes
//!   when Antigravity is first used. Asked as `Turnscope`, it answered
//!   `UNSUPPORTED_CLIENT`, so both requests name the client as the app does.
//! - `retrieveUserQuotaSummary`, asked with that project, gives `groups` of
//!   `buckets`, each a `window` (`5h`, `weekly`) with `remainingFraction`
//!   and `resetTime`. It's asked of the `daily-` host: at the same moment,
//!   the other had every bucket full where `daily-` had 2.3% of the Gemini
//!   week used.
//! - A plan has a pool for Gemini models and one for Claude and GPT, each
//!   with a 5-hour and a weekly bucket. Each is scoped by its pool, as
//!   `Weekly (Gemini)`: unscoped, Gemini's work rate counted Claude's usage,
//!   which never draws on it. A limit is keyed by its window, and one not
//!   Gemini's by its bucket too, as `weekly:3p-weekly`, so Gemini's are
//!   listed first.
//! - A pool nothing had drawn on reset a whole window after the request, so
//!   it reads as a window beginning now.
//!
//! **Renewing.** From the app 2.33.1 on, the Keychain item's token expires
//! within an hour of sign-in (`agents::antigravity`). It's renewed as the app
//! does, in memory alone: the item's refresh token goes to Google with the
//! app's OAuth client, and the token given is kept in this process until it
//! expires. A Google refresh token isn't replaced when used, so this takes
//! nothing from the app. Dropped: asking the running app for its limits, as
//! 0.3.1 did, which read nothing while it was closed, nor from 2.34.0 on.
//! The client's secret is read from the app's program: its `__TEXT,__rodata`
//! held 3 `GOCSPX-` secrets in 2.34.0 and in `agy` (read 2026-11-22), none
//! marked as this client's, so each is tried until Google takes one. Google
//! answers a wrong one `invalid_client`, and a refresh token it no longer
//! takes `invalid_grant`: a login to sign in to again.
//!
//! **Not measured.** A refused sign-in: only a 401 is one, as a 403 can mean
//! a client or account the service doesn't serve. An account with no plan. A
//! `disabled` bucket. A used-up bucket: one without `remainingFraction` says
//! nothing of what's left, and is passed over as OpenUsage does. A renewal
//! with a real sign-in.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use serde_json::{Value, json};

use super::{Answered, Info, Limits, Problem, Provider, Reading};
use crate::agents::{self, Client, Credential, Refresh, Secret, non_empty};
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

impl Provider for Google {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn limits(&self, credential: &Credential, now: i64, by: Instant) -> Result<Limits, Problem> {
        let client = agents::client(INFO.id)
            .ok_or_else(|| Problem::unanswered("no agent client for Google"))?;
        match &credential.refresh {
            Some(refresh) if !credential.fresh(now) => renewed(refresh, client, now, by),
            // A token refused before it expires, or with no expiry read, is
            // renewed: one without would be refused until signing in again.
            Some(refresh) => match read(&credential.secret, client, by) {
                Err(Problem::SignIn) => renewed(refresh, client, now, by),
                limits => limits,
            },
            None => read(&credential.secret, client, by),
        }
    }
}

/// The plan's limits, asked with `token` as `client`.
fn read(token: &Secret, client: &Client, by: Instant) -> Result<Limits, Problem> {
    let header = format!("User-Agent: {}", client.user_agent);
    let body = json!({"metadata": {
        "ideType": client.ide_type,
        "platform": "PLATFORM_UNSPECIFIED",
        "pluginType": "GEMINI",
    }});
    let signed_in = super::asked(PLAN, token, &[header.as_str()], Some(&body), &[401], by)?;
    let project = non_empty(&signed_in["cloudaicompanionProject"]).ok_or_else(|| {
        Problem::unavailable(format!(
            "no project to ask: {} makes one when first used",
            client.agent
        ))
    })?;
    let answer = super::asked(
        QUOTA,
        token,
        &[header.as_str()],
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
/// renewed before, while it's good and taken, or else a new one. A token
/// that's refused isn't kept.
fn renewed(refresh: &Refresh, client: &Client, now: i64, by: Instant) -> Result<Limits, Problem> {
    let key = refresh.token.fingerprint();
    // Its own statement, so the lock goes before the request.
    let kept = locked(&RENEWED).remove(&key);
    let tried = kept
        .filter(|(_, expires)| *expires > now + MINUTE)
        .map(|(token, expires)| (read(&token, client, by), token, expires));
    let (limits, token, expires) = match tried {
        Some((Err(Problem::SignIn), ..)) | None => {
            let (token, expires) = renew(refresh, client, now, by)?;
            (read(&token, client, by), token, expires)
        }
        Some(tried) => tried,
    };
    if !matches!(limits, Err(Problem::SignIn)) {
        let mut renewed = locked(&RENEWED);
        renewed.retain(|_, (_, expires)| *expires > now);
        renewed.insert(key, (token, expires));
    }
    limits
}

/// A new token for `refresh`'s login, and when it expires, from Google.
fn renew(
    refresh: &Refresh,
    client: &Client,
    now: i64,
    by: Instant,
) -> Result<(Secret, i64), Problem> {
    let mut secrets = refresh
        .programs
        .iter()
        .find_map(|program| (client.secrets)(program))
        .ok_or_else(|| {
            Problem::unanswered(format!("no {} program holds its client", client.agent))
        })?;
    let taken = locked(&TAKEN).clone();
    secrets.sort_by_key(|secret| Some(secret.fingerprint()) != taken);
    for secret in &secrets {
        let answered = super::post_form(
            TOKEN,
            &[
                ("client_id", client.oauth_id),
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
        "{TOKEN}: took none of {}'s {} client secrets",
        client.agent,
        secrets.len()
    )))
}

/// The token in Google's answer to a renewal, and when it expires; `None`
/// when Google didn't take the client.
fn token(answered: &Answered, now: i64) -> Result<Option<(Secret, i64)>, Problem> {
    let answer: Value = serde_json::from_slice(&answered.body).unwrap_or_default();
    match (answered.status, answer["error"].as_str()) {
        (200..=299, _) => {
            let token = non_empty(&answer["access_token"]);
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
        _ => Err(answered.unavailable(TOKEN)),
    }
}

/// The plan in `loadCodeAssist`'s answer: the name of the tier paid for,
/// without the `Google AI` it begins with, over its id (`Google AI Ultra`'s
/// was `g1-ultra-lite-tier`); or else the id of the tier it's on, `free-tier`
/// as `free`.
fn plan(signed_in: &Value) -> Option<String> {
    let plan = match signed_in["paidTier"]["name"].as_str() {
        Some(name) => name.trim_prefix(PLANS).trim(),
        None => {
            let tier = signed_in["currentTier"]["id"].as_str()?;
            tier.trim_suffix("-tier")
        }
    };
    Some(plan.to_owned()).filter(|plan| !plan.is_empty())
}

/// `None` for an answer with no groups, such as an error sent with 200, or
/// one with a bucket that doesn't read.
fn parse(answer: &Value) -> Option<Vec<Reading>> {
    let mut readings = Vec::new();
    for group in answer.get("groups")?.as_array()? {
        let group_scope = group["displayName"]
            .as_str()
            .map(scope)
            .filter(|group| !group.is_empty());
        for bucket in group["buckets"].as_array().into_iter().flatten() {
            // One the plan doesn't have, or with nothing said of what's left.
            let (Some(left), false) = (
                bucket["remainingFraction"].as_f64(),
                bucket["disabled"] == true,
            ) else {
                continue;
            };
            let id = non_empty(&bucket["bucketId"])?;
            let window = bucket["window"].as_str()?;
            let length = match window {
                "weekly" => 7 * DAY,
                hours => hours.strip_suffix('h')?.parse::<i64>().ok()? * HOUR,
            };
            let resets = super::reset(&bucket["resetTime"])?;
            // The Gemini pool's: `gemini-5h`, `gemini-weekly`.
            let own = id.strip_prefix("gemini-") == Some(window);
            let key = match own {
                true => window.to_owned(),
                false => format!("{window}:{id}"),
            };
            let scope = match group_scope {
                Some(group) => group.to_owned(),
                None if own => "Gemini".to_owned(),
                None => super::words(id),
            };
            let used = ((1.0 - left) * 100.0).clamp(0.0, 100.0);
            readings.push(super::windowed(key, Some(scope), used, length, resets));
        }
    }
    Some(readings)
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
}
