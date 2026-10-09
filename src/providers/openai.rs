//! OpenAI: the limits of a ChatGPT plan, and the account OpenAI API keys
//! draw on.
//!
//! Codex, OpenCode and Pi sign in with the same OpenAI client, so a token
//! from any of them reads the same usage. The token is a JWT naming the
//! workspace it was issued for and its owner's email; one person can be in
//! several workspaces, so the request names the token's. Each limit is a
//! window of `limit_window_seconds`, with `used_percent` and `reset_at`;
//! limits on particular models come in `additional_rate_limits`, each named.
//! An API key carries no limit an agent's key can read: OpenAI tells usage
//! only to an admin key.

use serde_json::Value;

use super::{Account, Info, Kind, Limits, Problem, Provider, Reading};
use crate::agents::{Credential, claims};

pub struct OpenAi;

static INFO: Info = Info {
    id: "openai",
    name: "OpenAI",
    subscription: Some("ChatGPT"),
};

impl Provider for OpenAi {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn account(&self, credential: &Credential) -> Option<Account> {
        if credential.key {
            return Some(super::api_account("openai"));
        }
        let (workspace, label) = workspace(credential)?;
        Some(Account {
            id: format!("openai:{workspace}"),
            kind: Kind::Subscription,
            label,
        })
    }

    fn limits(&self, credential: &Credential, now: i64) -> Result<Limits, Problem> {
        if credential.key {
            return Ok(Limits {
                plan: None,
                readings: Vec::new(),
            });
        }
        let (workspace, _) = workspace(credential).ok_or(Problem::SignIn)?;
        let answer = super::ask(
            "https://chatgpt.com/backend-api/wham/usage",
            credential,
            &[&format!("ChatGPT-Account-Id: {workspace}")],
        )?;
        parse(&answer, now).ok_or(Problem::Unavailable)
    }
}

/// The workspace a sign-in is to and its owner's email: as the agent
/// records them, or else as the token says.
fn workspace(credential: &Credential) -> Option<(String, Option<String>)> {
    if let Some(identity) = &credential.identity {
        return Some(identity.clone());
    }
    let claims = claims(credential.secret.expose());
    let workspace = claims["https://api.openai.com/auth"]["chatgpt_account_id"]
        .as_str()
        .filter(|workspace| !workspace.is_empty())?;
    let email = claims["https://api.openai.com/profile"]["email"].as_str();
    Some((workspace.to_owned(), email.map(str::to_owned)))
}

/// `None` for an answer with no `rate_limit` at all, such as an error sent
/// with 200, so it isn't taken for a plan without limits.
fn parse(answer: &Value, now: i64) -> Option<Limits> {
    let mut readings = windows(answer.get("rate_limit")?, None, now)?;
    for extra in answer["additional_rate_limits"]
        .as_array()
        .into_iter()
        .flatten()
    {
        // One without a name can't be told from the plan's own windows.
        let scope = extra["limit_name"].as_str().filter(|name| !name.is_empty());
        if let Some(scope) = scope
            && !super::absent(&extra["rate_limit"])
        {
            readings.extend(windows(&extra["rate_limit"], Some(scope), now)?);
        }
    }
    Some(Limits {
        // `prolite` is Pro Lite.
        plan: answer["plan_type"]
            .as_str()
            .map(|plan| plan.replace("prolite", "pro lite")),
        readings,
    })
}

fn windows(rate: &Value, scope: Option<&str>, now: i64) -> Option<Vec<Reading>> {
    let mut readings = Vec::new();
    for key in ["primary_window", "secondary_window"] {
        let window = &rate[key];
        if super::absent(window) {
            continue;
        }
        let seconds = window["limit_window_seconds"]
            .as_i64()
            .filter(|seconds| *seconds > 0)?;
        // An unknown reset comes as zero; the countdown beside it still says when.
        let resets = match window["reset_at"].as_i64() {
            Some(at) if at > 0 => Some(at * 1000),
            _ => window["reset_after_seconds"]
                .as_i64()
                .map(|after| now + after * 1000),
        };
        readings.push(Reading {
            key: scope.map_or(key.to_owned(), |scope| format!("{key}:{scope}")),
            name: super::window_name(seconds),
            scope: scope.map(str::to_owned),
            used: window["used_percent"].as_f64()?,
            size: None,
            starts: resets.map(|resets| resets - seconds * 1000),
            resets,
        });
    }
    Some(readings)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn a_plans_windows_and_its_model_limits_are_read() {
        let answer = json!({
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": {"used_percent": 20, "limit_window_seconds": 18000, "reset_at": 1_790_000_000},
                "secondary_window": {"used_percent": 5, "limit_window_seconds": 604800, "reset_at": 0, "reset_after_seconds": 3600}
            },
            "additional_rate_limits": [{"limit_name": "GPT-5.3-Codex-Spark", "rate_limit": {"primary_window": {"used_percent": 1, "limit_window_seconds": 18000, "reset_at": 1_790_000_000}}}]
        });
        let limits = super::parse(&answer, 1_789_000_000_000).unwrap();
        assert_eq!(limits.plan.as_deref(), Some("plus"));
        let names: Vec<_> = limits
            .readings
            .iter()
            .map(|reading| (reading.name.as_str(), reading.scope.as_deref()))
            .collect();
        assert_eq!(
            names,
            [
                ("5 hours", None),
                ("Weekly", None),
                ("5 hours", Some("GPT-5.3-Codex-Spark"))
            ]
        );
        assert_eq!(
            limits.readings[1].resets,
            Some(1_789_000_000_000 + 3_600_000)
        );
        assert!(super::parse(&json!({"error": "nope"}), 0).is_none());
    }
}
