//! xAI: the usage pool of a SuperGrok plan, shared by every Grok product.
//!
//! Grok Build, OpenCode and Pi sign in with the Grok CLI's client, so a token
//! from any of them reads the same pool. The billing endpoint belongs to the proxy the Grok CLI talks
//! to, which refuses a request not naming a CLI client and version. Answers
//! are protocol buffers as JSON, which leave out a zero: a pool with none
//! used comes with its `currentPeriod` and no `creditUsagePercent`. The pool
//! is weekly (measured 2026-09-24).

use std::time::Instant;

use serde_json::Value;

use super::{Account, Info, Limits, Problem, Provider, Reading};
use crate::agents::{Credential, claims, non_empty};
use crate::time::millis;

pub struct Xai;

static INFO: Info = Info {
    id: "xai",
    name: "xAI",
    subscription: Some("SuperGrok"),
    key_limits: false,
};

const BILLING: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";

impl Provider for Xai {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn makes(&self, model: &str) -> bool {
        model.starts_with("grok-")
    }

    fn account(&self, credential: &Credential) -> Option<Account> {
        super::account(INFO.id, credential, || {
            credential.identity.clone().or_else(|| {
                // Where the agent's files don't say, the token's `sub`.
                let claims = claims(credential.secret.expose());
                let sub = non_empty(&claims["sub"])?;
                Some((sub.to_owned(), None))
            })
        })
    }

    fn limits(&self, credential: &Credential, _now: i64, by: Instant) -> Result<Limits, Problem> {
        // Only a 401 is a refused login: the proxy may answer 403 to a client
        // version it no longer takes, which signing in again wouldn't fix.
        let answer = super::asked(
            BILLING,
            &credential.secret,
            &[
                "X-XAI-Token-Auth: xai-grok-cli",
                "x-grok-client-identifier: grok-shell",
                "x-grok-client-version: 1.0.30",
                "User-Agent: xai-grok-cli",
            ],
            None,
            &[401],
            by,
        )?;
        Ok(vec![parse(&answer).ok_or_else(|| Problem::not_understood(BILLING))?].into())
    }
}

/// `None` when the period doesn't read.
fn parse(answer: &Value) -> Option<Reading> {
    let config = answer.get("config")?;
    let period = &config["currentPeriod"];
    let starts = millis(period["start"].as_str()?)?;
    let resets = millis(period["end"].as_str()?)?;
    let used = match config.get("creditUsagePercent") {
        None | Some(Value::Null) => 0.0,
        Some(used) => used.as_f64()?,
    };
    let length = resets.checked_sub(starts).filter(|&length| length > 0)?;
    Some(super::window("pool", None, used, length, Some(resets)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn a_pool_with_nothing_used_leaves_its_percent_out() {
        let answer = json!({"config": {"currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY", "start": "2026-09-22T00:00:00Z", "end": "2026-09-29T00:00:00Z"}}});
        let pool = super::parse(&answer).unwrap();
        assert_eq!((pool.name.as_str(), pool.used), ("Weekly", 0.0));
        let null_used = json!({"config": {"creditUsagePercent": null, "currentPeriod": {"start": "2026-09-22T00:00:00Z", "end": "2026-09-29T00:00:00Z"}}});
        assert_eq!(super::parse(&null_used).unwrap().used, 0.0);
        let reversed = json!({"config": {"currentPeriod": {"start": "2026-09-29T00:00:00Z", "end": "2026-09-22T00:00:00Z"}}});
        assert!(super::parse(&reversed).is_none());
        assert!(super::parse(&json!({"config": {}})).is_none());
    }
}
