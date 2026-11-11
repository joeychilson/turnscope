//! Google: the limits of a Google AI plan, which only Antigravity signs in
//! to, and the account Gemini API keys draw on. An API key carries no limit
//! an agent's key can read.
//!
//! A plan's limits are read the way Antigravity reads them, with two
//! requests to the service its app and `agy` talk to. Both are POSTs that
//! change nothing. None of it is documented: what follows was measured with
//! this Mac's sign-in on 2026-11-10, asking as OpenUsage and CodexBar do
//! (their code, read that day).
//!
//! - `loadCodeAssist` says which plan the sign-in is on and the project
//!   Google keeps its usage under, `cloudaicompanionProject`, a string. The
//!   plan paid for is `paidTier`. `currentTier` was `free-tier` beside it.
//! - `retrieveUserQuotaSummary`, asked with that project, gives the limits:
//!   `groups` of `buckets`, each bucket a `window` (`5h`, `weekly`) with
//!   the fraction of it left, `remainingFraction`, and when it resets,
//!   `resetTime`.
//!
//! **The client.** Asked by a client named `Turnscope`, `loadCodeAssist`
//! still answered 200, with no tier of the account's, no project, and the
//! reason `UNSUPPORTED_CLIENT`. So both requests name the client as
//! Antigravity's app does.
//!
//! **The host.** The limits are asked of the `daily-` host, over the one
//! that tells the plan. Asked at the same moment, that one had every bucket
//! full and resetting a whole window later, where `daily-` had 2.3% of the
//! Gemini week used.
//!
//! **The plan's name.** The tier paid for had the id `g1-ultra-lite-tier`
//! and the name `Google AI Ultra`. So a plan is called what Google names it,
//! over a name from its id, which would have been Ultra Lite.
//!
//! **Windows.** A pool nothing had drawn on reset a whole window after the
//! request, so it reads as a window beginning now. In the pool in use, the
//! 5-hour window reset 10 hours after the week began: 5-hour windows follow
//! one another from the week's first use.
//!
//! **Which limit is the plan's own.** A plan has a pool for Gemini models
//! and one for Claude and GPT models, each with a 5-hour and a weekly
//! bucket. No bucket is on all usage. The Gemini pool's two, `gemini-5h` and
//! `gemini-weekly`, are read as the plan's own, over giving every limit a
//! scope: Antigravity's own models draw on them, and with no limit of its
//! own an account has nothing to stand by, in the status line or for
//! `guard`. Every other bucket is its group's, as `Weekly (Claude and
//! GPT)`, and still shows when it runs out. The group is named `Claude and
//! GPT models`: a scope names models already, and with the word the app's
//! panel cut the limit's reset short.
//!
//! **Keys.** A limit is keyed by its window, and one that isn't the plan's
//! own by its bucket too, as `weekly:3p-weekly`, over the bucket's id alone.
//! The status lists an account's limits by key, and by their ids the
//! Claude and GPT pool's came before the plan's own (seen on this Mac's
//! account, 2026-11-10).
//!
//! **Without a project** there's nothing to ask about. Google makes one when
//! Antigravity is first used, and Turnscope never does.
//!
//! **Not measured.** A refused sign-in: only a 401 is taken for one, as at
//! xAI, since a 403 from this service can mean a client or an account it
//! doesn't serve, which signing in again wouldn't change. An account that
//! pays for no plan, which is named by its tier's id, `free`. A group for
//! one model, which OpenUsage reads as `Gemini 4 Argon`. A bucket marked
//! `disabled`, which is passed over as OpenUsage's reader does. A response
//! of a Claude or GPT model through Antigravity: a breakdown of the limit on
//! them counts models whose id holds `claude` or `gpt`, as their scope
//! names them, and a breakdown of the Gemini limits counts every model's,
//! theirs too.
//!
//! A used-up bucket hasn't been seen either. It should still come with
//! `remainingFraction: 0`: CodexBar reads `agy`'s own form of this answer,
//! where the fraction is one of the cases a bucket's `remaining` can be, and
//! a case that's set is written even at zero. So a bucket without the
//! fraction says nothing of what's left and is passed over, as OpenUsage's
//! reader does, over reading it as used up.

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
        if credential.key {
            return Some(super::api_account(INFO.id));
        }
        Some(super::subscription(INFO.id, credential.identity.clone()?))
    }

    fn limits(&self, credential: &Credential, _now: i64) -> Result<Limits, Problem> {
        if credential.key {
            return Ok(Limits::default());
        }
        let client = json!({"metadata": {
            "ideType": "ANTIGRAVITY",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI",
        }});
        // Both within the time one request gets elsewhere, as `guard` waits
        // on a read.
        let began = std::time::Instant::now();
        let signed_in =
            super::ask_sending(PLAN, credential, &[CLIENT], &client, &[401], super::WAIT)?;
        let project = signed_in["cloudaicompanionProject"]
            .as_str()
            .filter(|project| !project.is_empty())
            .ok_or(Problem::Unavailable)?;
        let spent = u32::try_from(began.elapsed().as_secs()).unwrap_or(super::WAIT);
        let answer = super::ask_sending(
            QUOTA,
            credential,
            &[CLIENT],
            &json!({"project": project}),
            &[401],
            super::WAIT.saturating_sub(spent).max(1),
        )?;
        let readings = parse(&answer)
            .filter(|readings| !readings.is_empty())
            .ok_or(Problem::Unavailable)?;
        Ok(Limits {
            plan: plan(&signed_in),
            readings,
        })
    }
}

/// The plan a sign-in is on: the name of the tier paid for, without the
/// `Google AI` it begins with, or for an account that pays for none its
/// tier's id, `free-tier` as `free`.
fn plan(signed_in: &Value) -> Option<String> {
    let plan = match signed_in["paidTier"]["name"].as_str() {
        Some(name) => name.strip_prefix(PLANS).unwrap_or(name).trim(),
        None => {
            let tier = signed_in["currentTier"]["id"].as_str()?;
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
            let resets = match &bucket["resetTime"] {
                Value::Null => None,
                at => Some(crate::time::millis(at.as_str()?)?),
            };
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
/// and GPT models` is `Claude and GPT`.
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
        // By key, as the status lists them, the plan's own come first on
        // each window.
        let mut listed: Vec<_> = readings
            .iter()
            .map(|reading| (reading.key.as_str(), reading.scope.is_some()))
            .collect();
        listed.sort();
        assert_eq!(
            listed,
            [
                ("5h", false),
                ("5h:3p-5h", true),
                ("weekly", false),
                ("weekly:3p-weekly", true)
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
}
