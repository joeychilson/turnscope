//! SuperGrok: the usage pool of a SuperGrok plan.
//!
//! Grok Build, OpenCode and Pi all sign in with the Grok CLI's xAI client, so
//! a token from any of them reads the same pool, which every Grok product
//! shares. The token is a JWT whose `sub` is the account. The billing endpoint
//! belongs to the proxy the Grok CLI talks to, which refuses a request that
//! does not name a CLI client and version.
//!
//! Its answers are protocol buffers written as JSON, which leave out a number
//! that is zero: a pool none of which is used this period comes with its
//! `currentPeriod` and no `creditUsagePercent`. That is taken as none used
//! only beside a period that reads whole, a start and a later end; without
//! one, the answer is one not understood.
//!
//! The pool is weekly. Measured 2026-09-24: the `format=credits` answer for
//! a unified billing account gave `currentPeriod` as
//! `USAGE_PERIOD_TYPE_WEEKLY`, Sep 22 to 29, with on-demand figures wrapped
//! as `{"val": 0}` and no `creditUsagePercent`, for an account that had
//! spent $0.05 of its week on one response. The Grok CLI 1.0.41 reads the
//! same `BillingConfig`; `format=full` gives an older monthly view, all
//! zeros.

use serde_json::Value;

use super::{Answer, Identity, LimitProblem, Location, Reader, Source};
use crate::agent::Agent;
use crate::time::Instant;

const URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";

/// The headers the Grok CLI sends besides the credential.
const HEADERS: &[&str] = &[
    "X-XAI-Token-Auth: xai-grok-cli",
    "x-grok-client-identifier: grok-shell",
    // A released Grok CLI version.
    "x-grok-client-version: 1.0.30",
    "User-Agent: xai-grok-cli",
];

/// Grok Build keys its sign-in by issuer and client, and the client is fixed.
const GROK_BUILD_TOKEN: &str = "/https:~1~1auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828/key";
const GROK_BUILD_EXPIRES: &str =
    "/https:~1~1auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828/expires_at";

/// Where a SuperGrok sign-in can be kept, and how its pool is read.
pub(super) const READER: Reader = Reader {
    sources: &[
        Source {
            agent: Agent::Grok,
            provider: "xai",
            location: Location::File("auth.json"),
            token: GROK_BUILD_TOKEN,
            expires: Some(GROK_BUILD_EXPIRES),
            plan: None,
            whose: None,
        },
        Source {
            agent: Agent::OpenCode,
            provider: "xai",
            location: Location::OpenCode("xai"),
            token: "/access",
            expires: Some("/expires"),
            plan: None,
            whose: None,
        },
        // Where OpenCode kept sign-ins before its database did.
        Source {
            agent: Agent::OpenCode,
            provider: "xai",
            location: Location::File("auth.json"),
            token: "/xai/access",
            expires: Some("/xai/expires"),
            plan: None,
            whose: None,
        },
        Source {
            agent: Agent::Pi,
            provider: "xai",
            location: Location::File("auth.json"),
            token: "/xai/access",
            expires: Some("/xai/expires"),
            plan: None,
            whose: None,
        },
    ],
    identity,
    fetch,
};

/// The account a token was issued to.
fn identity(token: &str) -> Identity {
    Identity {
        key: super::claims(token)["sub"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        label: None,
    }
}

fn fetch(token: &str, _identity: &Identity, _now: Instant) -> Result<Answer, LimitProblem> {
    super::get(URL, token, HEADERS, parse)
}

/// The pool in a billing answer: how much is used, and when the period ends.
/// `None` when a period it gives doesn't read, or it gives neither a period
/// nor how much is used.
fn parse(body: &Value) -> Option<Answer> {
    let config = body.get("config")?.as_object()?;
    let period = match config.get("currentPeriod") {
        None | Some(Value::Null) => None,
        Some(Value::Object(period)) => {
            let starts = super::instant_of(period.get("start")?)?;
            let resets = super::instant_of(period.get("end")?)?;
            // A period ends after it starts.
            if resets <= starts {
                return None;
            }
            Some((starts, resets))
        }
        Some(_) => return None,
    };
    let name = period.map_or_else(
        || "Usage".to_owned(),
        |(starts, resets)| super::span((resets.millis() - starts.millis()) / 1_000),
    );
    let zero = Value::from(0.0);
    let used = match (config.get("creditUsagePercent"), period) {
        (Some(used), _) => used,
        // Left out because it is zero, where the period it is of is given.
        (None, Some(_)) => &zero,
        (None, None) => return None,
    };
    let window = (
        period.map(|(starts, _)| starts),
        period.map(|(_, resets)| resets),
    );
    let limit = super::limit("pool", &name, None, used, window)?;
    Some(Answer {
        plan: None,
        limits: vec![limit],
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::parse;

    #[test]
    fn a_period_without_a_percentage_has_used_none_of_its_pool() {
        // As the proxy answered on 2026-09-24 for an account that had spent
        // $0.05 of its week: the weekly period, on-demand figures wrapped as
        // `val`, and no `creditUsagePercent`, which is zero and so left out.
        let answer = parse(&json!({"config": {
            "currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY",
                "start": "2026-09-22T19:28:21.395528+00:00", "end": "2026-09-29T19:28:21.395528+00:00"},
            "onDemandCap": {"val": 0}, "onDemandUsed": {"val": 0},
            "isUnifiedBillingUser": true, "prepaidBalance": {"val": 0},
            "topUpMethod": "TOP_UP_METHOD_SAVED_PAYMENT_METHOD",
            "billingPeriodStart": "2026-09-22T19:28:21.395528+00:00",
            "billingPeriodEnd": "2026-09-29T19:28:21.395528+00:00"}}))
        .unwrap();
        let pool = &answer.limits[0];
        assert_eq!((pool.name.as_str(), pool.used), ("Weekly", 0.0));
        assert_eq!(
            pool.resets,
            crate::time::Instant::parse("2026-09-29T19:28:21.395528+00:00")
        );
        // With neither a percentage nor a period, it can't be read.
        assert!(parse(&json!({"config": {"onDemandCap": {"val": 0}}})).is_none());
    }

    #[test]
    fn a_percentage_left_out_is_zero_only_beside_a_period_that_reads() {
        let week = json!({"type": "USAGE_PERIOD_TYPE_WEEKLY",
                          "start": "2026-09-22T19:28:21Z", "end": "2026-09-29T19:28:21Z"});
        let answer = |period: serde_json::Value, used: Option<f64>| {
            let mut config = json!({"onDemandCap": {"val": 0}, "isUnifiedBillingUser": true,
                                    "currentPeriod": period});
            if let Some(used) = used {
                config["creditUsagePercent"] = json!(used);
            }
            parse(&json!({"config": config}))
        };
        assert_eq!(answer(week.clone(), None).unwrap().limits[0].used, 0.0);
        assert_eq!(answer(week.clone(), Some(3.5)).unwrap().limits[0].used, 3.5);
        // A percentage without a period is of no window.
        let pool = answer(json!(null), Some(3.5)).unwrap();
        assert_eq!(
            (pool.limits[0].name.as_str(), pool.limits[0].resets),
            ("Usage", None)
        );

        // An empty period, one with no end, one ending before it starts, and
        // one whose start is no instant, are no evidence that none is used,
        // and no period to put a percentage in.
        for period in [
            json!({}),
            json!({"type": "USAGE_PERIOD_TYPE_WEEKLY", "start": "2026-09-22T19:28:21Z"}),
            json!({"start": "2026-09-29T19:28:21Z", "end": "2026-09-22T19:28:21Z"}),
            json!({"start": "last Monday", "end": "2026-09-29T19:28:21Z"}),
        ] {
            assert!(answer(period.clone(), None).is_none(), "{period}");
            assert!(answer(period.clone(), Some(3.5)).is_none(), "{period}");
        }
    }

    #[test]
    fn the_pool_is_one_limit_over_its_period() {
        let answer = parse(&json!({"config": {"creditUsagePercent": 12.5,
            "currentPeriod": {"start": "2026-09-01T00:00:00Z", "end": "2026-10-01T00:00:00Z"}}}))
        .unwrap();
        assert_eq!(answer.limits.len(), 1);
        assert_eq!(
            (answer.limits[0].name.as_str(), answer.limits[0].used),
            ("Monthly", 12.5)
        );
    }
}
