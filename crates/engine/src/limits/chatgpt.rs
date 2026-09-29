//! ChatGPT: the limits of a ChatGPT plan, which Codex draws on.
//!
//! Codex, OpenCode and Pi all sign in with the same OpenAI client, so a token
//! from any of them reads the same usage. The token is a JWT naming the
//! workspace it was issued for and its owner's email address. Each limit is a
//! window of `limit_window_seconds`, with `used_percent` and `reset_at` in
//! Unix seconds; limits on particular models arrive in
//! `additional_rate_limits`, each named. A window given is read whole or not
//! at all: one that doesn't read makes the answer one not understood.
//!
//! **Another folder.** Codex keeps one sign-in, in `auth.json` in its folder,
//! `~/.codex` or the one `CODEX_HOME` names, so a second account is kept in a
//! second folder. Codex 0.157.0 (2026-09-29) can also be set to keep it in
//! the Keychain instead (`cli_auth_credentials_store`), which isn't read
//! here; this Mac's is in `auth.json`, with `auth_mode` `chatgpt`.

use serde_json::Value;

use super::{Answer, Identity, LimitProblem, Location, Reader, Reported, Source};
use crate::agent::Agent;
use crate::time::Instant;

const URL: &str = "https://chatgpt.com/backend-api/wham/usage";

/// Where a ChatGPT sign-in can be kept, and how its limits are read.
pub(super) const READER: Reader = Reader {
    sources: &[
        Source {
            agent: Agent::Codex,
            provider: "openai",
            location: Location::File("auth.json"),
            token: "/tokens/access_token",
            expires: None,
            plan: None,
            whose: None,
        },
        Source {
            agent: Agent::OpenCode,
            provider: "openai",
            location: Location::OpenCode("openai"),
            token: "/access",
            expires: Some("/expires"),
            plan: None,
            whose: None,
        },
        // Where OpenCode kept sign-ins before its database did.
        Source {
            agent: Agent::OpenCode,
            provider: "openai",
            location: Location::File("auth.json"),
            token: "/openai/access",
            expires: Some("/openai/expires"),
            plan: None,
            whose: None,
        },
        Source {
            agent: Agent::Pi,
            provider: "openai-codex",
            location: Location::File("auth.json"),
            token: "/openai-codex/access",
            expires: Some("/openai-codex/expires"),
            plan: None,
            whose: None,
        },
    ],
    identity,
    fetch,
};

/// The workspace a token was issued for, labelled with its owner's email.
fn identity(token: &str) -> Identity {
    let claims = super::claims(token);
    Identity {
        key: claims["https://api.openai.com/auth"]["chatgpt_account_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        label: claims["https://api.openai.com/profile"]["email"]
            .as_str()
            .map(str::to_owned),
    }
}

fn fetch(token: &str, identity: &Identity, now: Instant) -> Result<Answer, LimitProblem> {
    // Picks the workspace when one person belongs to several.
    let workspace = format!("ChatGPT-Account-Id: {}", identity.key);
    let headers: &[&str] = if identity.key.is_empty() {
        &[]
    } else {
        &[&workspace]
    };
    super::get(URL, token, headers, |body| parse(body, now))
}

/// The limits in a usage answer: `None` when a window it gives doesn't
/// read, or a limit on particular models doesn't say which.
fn parse(body: &Value, now: Instant) -> Option<Answer> {
    let mut limits = windows(&body["rate_limit"], None, now)?;
    match &body["additional_rate_limits"] {
        Value::Null => {}
        Value::Array(extras) => {
            for extra in extras {
                let rate = &extra["rate_limit"];
                if super::absent(rate) {
                    continue;
                }
                let scope = extra["limit_name"]
                    .as_str()
                    .filter(|name| !name.is_empty())?;
                limits.extend(windows(rate, Some(scope), now)?);
            }
        }
        _ => return None,
    }
    Some(Answer {
        plan: body["plan_type"].as_str().map(str::to_owned),
        limits,
    })
}

/// The windows of one rate limit, and the model it applies to, if only one:
/// `None` when one it gives doesn't read.
fn windows(rate: &Value, scope: Option<&str>, now: Instant) -> Option<Vec<Reported>> {
    let mut limits = Vec::new();
    for key in ["primary_window", "secondary_window"] {
        let window = &rate[key];
        if super::absent(window) {
            continue;
        }
        let seconds = window["limit_window_seconds"]
            .as_i64()
            .filter(|seconds| *seconds > 0)?;
        // An unknown reset arrives as zero; the countdown beside it still
        // says when.
        let reset_at = match &window["reset_at"] {
            Value::Null => 0,
            at => at.as_i64().filter(|at| *at >= 0)?,
        };
        let resets = if reset_at > 0 {
            Some(Instant::from_seconds(reset_at)?)
        } else {
            match &window["reset_after_seconds"] {
                Value::Null => None,
                after => {
                    let after = after.as_i64().filter(|after| *after >= 0)?;
                    Some(Instant::from_millis(
                        now.millis().checked_add(after.checked_mul(1_000)?)?,
                    )?)
                }
            }
        };
        let stable = match scope {
            Some(scope) => format!("{key}:{scope}"),
            None => key.to_owned(),
        };
        limits.push(super::limit(
            &stable,
            &super::span(seconds),
            scope.map(str::to_owned),
            &window["used_percent"],
            super::ending(resets, seconds.checked_mul(1_000)?),
        )?);
    }
    Some(limits)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::parse;
    use crate::time::Instant;

    #[test]
    fn primary_secondary_and_per_model_windows_are_limits() {
        let now = Instant::parse("2026-09-14T12:00:00Z").unwrap();
        let answer = parse(
            &json!({"plan_type": "pro",
                    "rate_limit": {
                        "primary_window": {"used_percent": 36, "limit_window_seconds": 18_000, "reset_at": 1_789_406_400},
                        "secondary_window": {"used_percent": 67, "limit_window_seconds": 604_800, "reset_at": 0,
                                             "reset_after_seconds": 86_400}},
                    "additional_rate_limits": [{"limit_name": "gpt-6-astra",
                        "rate_limit": {"primary_window": {"used_percent": 5, "limit_window_seconds": 604_800,
                                                          "reset_at": 1_789_747_200}}}]}),
            now,
        )
        .unwrap();
        assert_eq!(answer.plan.as_deref(), Some("pro"));
        let primary = &answer.limits[0];
        assert_eq!(
            (primary.key.as_str(), primary.name.as_str(), primary.used),
            ("primary_window", "5 hours", 36.0)
        );
        // 1,789,406,400 is 2026-09-14T17:20:00Z; the window began five hours before.
        assert_eq!(primary.resets, Instant::parse("2026-09-14T17:20:00Z"));
        assert_eq!(primary.starts, Instant::parse("2026-09-14T12:20:00Z"));
        // A reset given only as a countdown: a day from now.
        assert_eq!(
            answer.limits[1].resets,
            Instant::parse("2026-09-15T12:00:00Z")
        );
        assert_eq!(answer.limits[2].scope.as_deref(), Some("gpt-6-astra"));
        assert_eq!(answer.limits[2].key, "primary_window:gpt-6-astra");
    }

    /// A usage answer of the shape ChatGPT's gives, its weekly window as
    /// `secondary`, and a limit on one model as `extra`.
    fn answer(secondary: serde_json::Value, extra: serde_json::Value) -> serde_json::Value {
        json!({"plan_type": "pro",
               "rate_limit": {"allowed": true, "limit_reached": false,
                   "primary_window": {"used_percent": 36, "limit_window_seconds": 18_000,
                                      "reset_after_seconds": 3_600, "reset_at": 1_789_406_400},
                   "secondary_window": secondary},
               "additional_rate_limits": [extra]})
    }

    fn weekly() -> serde_json::Value {
        json!({"used_percent": 67, "limit_window_seconds": 604_800, "reset_after_seconds": 86_400,
               "reset_at": 1_789_747_200})
    }

    fn model_limit() -> serde_json::Value {
        json!({"limit_name": "gpt-6-astra", "metered_feature": "codex",
               "rate_limit": {"primary_window": weekly(), "secondary_window": null}})
    }

    #[test]
    fn a_window_given_that_does_not_read_is_no_answer() {
        let now = Instant::parse("2026-09-14T12:00:00Z").unwrap();
        let limits = |secondary, extra| parse(&answer(secondary, extra), now);
        assert_eq!(limits(weekly(), model_limit()).unwrap().limits.len(), 3);
        // A window, or a limit on a model, left out is left out.
        assert_eq!(limits(json!(null), model_limit()).unwrap().limits.len(), 2);
        assert_eq!(
            limits(
                weekly(),
                json!({"limit_name": "gpt-6-astra", "rate_limit": null})
            )
            .unwrap()
            .limits
            .len(),
            2
        );

        let mut unread = weekly();
        unread["used_percent"] = json!(null);
        let mut no_length = weekly();
        no_length["limit_window_seconds"] = json!(0);
        let mut bad_reset = weekly();
        bad_reset["reset_at"] = json!("tomorrow");
        for broken in [unread, no_length, bad_reset] {
            assert!(limits(broken.clone(), model_limit()).is_none(), "{broken}");
        }
        // A limit on a model that doesn't say which.
        let mut unnamed = model_limit();
        unnamed["limit_name"] = json!(null);
        assert!(limits(weekly(), unnamed).is_none());
    }
}
