//! xAI: the usage pool of a SuperGrok plan, shared by every Grok product.
//!
//! Grok Build, OpenCode and Pi sign in with the Grok CLI's client, so a token
//! from any of them reads the same pool. The token is a JWT whose `sub` is
//! the account. The billing endpoint belongs to the proxy the Grok CLI talks
//! to, which refuses a request not naming a CLI client and version. Answers
//! are protocol buffers as JSON, which leave out a zero: a pool with none
//! used comes with its `currentPeriod` and no `creditUsagePercent`. The pool
//! is weekly (measured 2026-09-24).

use serde_json::Value;

use super::{Account, Info, Kind, Limits, Problem, Provider, Reading};
use crate::agents::{Credential, claims};

pub struct Xai;

static INFO: Info = Info {
    id: "xai",
    name: "xAI",
    subscription: Some("SuperGrok"),
};

impl Provider for Xai {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn account(&self, credential: &Credential) -> Option<Account> {
        if credential.key {
            return Some(super::api_account("xai"));
        }
        let (id, label) = match &credential.identity {
            Some(identity) => identity.clone(),
            None => (
                claims(credential.secret.expose())["sub"]
                    .as_str()?
                    .to_owned(),
                None,
            ),
        };
        Some(Account {
            id: format!("xai:{id}"),
            kind: Kind::Subscription,
            label,
        })
    }

    fn limits(&self, credential: &Credential, _now: i64) -> Result<Limits, Problem> {
        if credential.key {
            return Ok(Limits {
                plan: None,
                readings: Vec::new(),
            });
        }
        // Only a 401 is a refused login: the proxy may answer 403 to a client
        // version it no longer takes, which signing in again wouldn't fix.
        let answer = super::ask_refused_by(
            "https://cli-chat-proxy.grok.com/v1/billing?format=credits",
            credential,
            &[
                "X-XAI-Token-Auth: xai-grok-cli",
                "x-grok-client-identifier: grok-shell",
                "x-grok-client-version: 1.0.30",
                "User-Agent: xai-grok-cli",
            ],
            &[401],
        )?;
        Ok(Limits {
            plan: None,
            readings: vec![parse(&answer).ok_or(Problem::Unavailable)?],
        })
    }
}

/// `None` when the period doesn't read, or there's neither a period nor how
/// much is used.
fn parse(answer: &Value) -> Option<Reading> {
    let config = answer.get("config")?;
    let period = match &config["currentPeriod"] {
        Value::Null => None,
        period => {
            let (starts, resets) = (super::time(&period["start"])?, super::time(&period["end"])?);
            (resets > starts).then_some((starts, resets))?;
            Some((starts, resets))
        }
    };
    let used = match (config.get("creditUsagePercent"), period) {
        (Some(used), _) => used.as_f64()?,
        (None, Some(_)) => 0.0,
        (None, None) => return None,
    };
    Some(Reading {
        key: "pool".to_owned(),
        name: period.map_or("Usage".to_owned(), |(starts, resets)| {
            super::window_name((resets - starts) / 1000)
        }),
        scope: None,
        used,
        size: None,
        starts: period.map(|(starts, _)| starts),
        resets: period.map(|(_, resets)| resets),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn a_pool_with_nothing_used_leaves_its_percent_out() {
        let answer = json!({"config": {"currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY", "start": "2026-09-22T00:00:00Z", "end": "2026-09-29T00:00:00Z"}}});
        let pool = super::parse(&answer).unwrap();
        assert_eq!((pool.name.as_str(), pool.used), ("Weekly", 0.0));
        assert!(super::parse(&json!({"config": {}})).is_none());
    }
}
