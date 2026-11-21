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
//! **Antigravity's app.** While the app runs, Antigravity's login is the
//! app's own (`agents::antigravity`), and the app is asked in Google's
//! place, on 127.0.0.1: it renews its sign-in itself, and the Keychain
//! item's token has expired. Measured 2026-11-21 with the app 2.33.1, each
//! a POST of `{}` answered in under 5 ms:
//!
//! - `GetUserStatus` gives the account's address, `email`, and the tier
//!   paid for, `userTier`, named as `paidTier` is.
//! - `RetrieveUserQuotaSummary` gives Google's answer, in `response`: the
//!   same groups, with as much of the Gemini week left as the app showed.
//! - The app's token goes in `X-Codeium-Csrf-Token`. A wrong one was
//!   answered 401, `invalid CSRF token`, which signing in again wouldn't
//!   fix, so no status is a refused login.
//! - Its `language_server` listened on three ports: the service over HTTP
//!   on one, HTTPS on another (400 over HTTP), and 401 on the third. So
//!   each is asked until one answers.
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

use std::time::Instant;

use serde_json::{Value, json};

use super::{Account, Info, Limits, Problem, Provider, Reading};
use crate::agents::Credential;
use crate::time::{DAY, HOUR};

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

/// How Antigravity's app names itself.
#[cfg(target_arch = "aarch64")]
const CLIENT: &str = "User-Agent: antigravity/2.31.0 (aidev_client; os_type=darwin; arch=arm64)";
#[cfg(not(target_arch = "aarch64"))]
const CLIENT: &str = "User-Agent: antigravity/2.31.0 (aidev_client; os_type=darwin; arch=amd64)";

impl Provider for Google {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn account(&self, credential: &Credential) -> Option<Account> {
        super::account(INFO.id, credential, || credential.identity.clone())
    }

    fn limits(&self, credential: &Credential, _now: i64, by: Instant) -> Result<Limits, Problem> {
        if !credential.app_ports.is_empty() {
            return from_app(credential, by);
        }
        let client = json!({"metadata": {
            "ideType": "ANTIGRAVITY",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI",
        }});
        let signed_in = super::asked(PLAN, credential, &[CLIENT], Some(&client), &[401], by)?;
        let project = signed_in["cloudaicompanionProject"]
            .as_str()
            .filter(|project| !project.is_empty())
            .ok_or_else(|| {
                Problem::unavailable("no project to ask: Antigravity makes one when first used")
            })?;
        let answer = super::asked(
            QUOTA,
            credential,
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
}

/// The service in Antigravity's app.
const APP: &str = "exa.language_server_pb.LanguageServerService";

/// The limits Antigravity's app reads, asked of it on each port it listens
/// on until one answers.
fn from_app(credential: &Credential, by: Instant) -> Result<Limits, Problem> {
    let token = format!("X-Codeium-Csrf-Token: {}", credential.secret.expose());
    let mut problem = None;
    for port in &credential.app_ports {
        let ask = |method: &str| {
            let url = format!("http://127.0.0.1:{port}/{APP}/{method}");
            super::sent(&url, &[&token], Some(&json!({})), &[], by)
        };
        match ask("GetUserStatus") {
            Ok(status) => {
                return app_limits(credential, &status, &ask("RetrieveUserQuotaSummary")?);
            }
            Err(other) => problem = Some(other),
        }
    }
    Err(problem.unwrap_or_else(|| Problem::unavailable("Antigravity's app listens on no port")))
}

/// The limits in the app's answers, `GetUserStatus`'s and
/// `RetrieveUserQuotaSummary`'s, when it's signed in to `credential`'s
/// account.
fn app_limits(credential: &Credential, status: &Value, quota: &Value) -> Result<Limits, Problem> {
    let status = &status["userStatus"];
    let signed_in = status["email"]
        .as_str()
        .ok_or_else(|| Problem::not_understood("Antigravity's app"))?;
    if credential.identity.as_ref().map(|(id, _)| id.as_str()) != Some(signed_in) {
        return Err(Problem::unavailable(
            "Antigravity's app is signed in to another account than its Keychain item",
        ));
    }
    let readings = parse(&quota["response"])
        .filter(|readings| !readings.is_empty())
        .ok_or_else(|| Problem::not_understood("Antigravity's app"))?;
    let tier = &status["userTier"];
    Ok(Limits {
        // Only a Google AI plan's name: a free account's tier isn't
        // measured, and `loadCodeAssist` named the free tier `Antigravity`.
        plan: named(
            tier["name"].as_str().filter(|name| name.starts_with(PLANS)),
            tier["id"].as_str(),
        ),
        readings,
    })
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

    use crate::agents::{Credential, Secret};
    use crate::providers::Problem;
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
    fn the_app_answers_for_its_own_account_as_google_does() {
        let login = Credential {
            provider: "google",
            secret: Secret::new("made-up-token"),
            key: false,
            expires: None,
            identity: Some((
                "me@example.com".to_owned(),
                Some("me@example.com".to_owned()),
            )),
            plan: None,
            app_ports: vec![52353],
        };
        let status = json!({"userStatus": {
            "email": "me@example.com",
            "userTier": {"id": "g1-ultra-lite-tier", "name": "Google AI Ultra"},
        }});
        let quota = json!({"response": {"groups": [{
            "displayName": "Gemini Models",
            "buckets": [bucket("gemini-weekly", "weekly", 0.75, "2026-11-25T02:10:47Z")],
        }]}});
        let limits = super::app_limits(&login, &status, &quota).unwrap();
        assert_eq!(limits.plan.as_deref(), Some("Ultra"));
        let read: Vec<_> = limits
            .readings
            .iter()
            .map(|reading| (reading.key.as_str(), reading.used))
            .collect();
        assert_eq!(read, [("weekly", 25.0)]);

        // A tier that isn't a Google AI plan is named by its id.
        let mut free = status.clone();
        free["userStatus"]["userTier"] = json!({"id": "free-tier", "name": "Antigravity"});
        let limits = super::app_limits(&login, &free, &quota).unwrap();
        assert_eq!(limits.plan.as_deref(), Some("free"));

        // Another account's limits aren't taken for the login's.
        let mut other = status.clone();
        other["userStatus"]["email"] = json!("someone@example.com");
        let error = super::app_limits(&login, &other, &quota).err();
        assert!(
            matches!(error, Some(Problem::Unavailable { .. })),
            "{error:?}"
        );

        // Google's answer, without the app's `response` around it, isn't
        // the app's.
        let error = super::app_limits(&login, &status, &quota["response"]).err();
        assert_eq!(error, Some(Problem::not_understood("Antigravity's app")));
    }
}
