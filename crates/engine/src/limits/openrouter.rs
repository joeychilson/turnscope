//! OpenRouter: the limit an API key can carry of its own.
//!
//! OpenRouter lets a key be given a spending limit in dollars, which resets
//! each day, week or month, or never. Asked with the key, `GET
//! /api/v1/key` answers with it under `data`: `limit`, in dollars, or null
//! for none; `limit_remaining`, what is left of it; `limit_reset`, `daily`,
//! `weekly` or `monthly`, by UTC days, weeks from Monday and months, or null
//! for a limit that never resets; and what the key used in all, `usage`, and
//! this day, week and month. The answer is shaped as OpenRouter's reference
//! gives it, read on 2026-09-29; no answer was asked for here, which would
//! have sent a key of the person's.
//!
//! A key's limit is a limit of the account `api:openrouter`, keyed `key:`
//! and a hash of the key, as the key itself is never kept: used is what the
//! limit says is gone of it, `limit` less `limit_remaining`, or, where
//! OpenRouter gives no remainder, what the key used in the limit's window, as
//! a share of the limit. A limit read whole or not at all: one whose figures
//! don't read, or say more is left than the limit, makes the answer one not
//! understood. A key with no limit gives none ([`super::api`]).
//!
//! OpenCode keeps an OpenRouter key as `key` in its database's row, as in
//! the `auth.json` it kept before, and Pi as `key`, or, signed in to
//! OpenRouter, as `access`: on 2026-09-29 this Mac's Pi held one of the
//! latter, and OpenCode one of the former.

use serde_json::Value;

use super::sign_in::{FNV, Identity, Location, Source, fnv};
use super::{LimitProblem, PlanLimits};
use crate::agent::Agent;
use crate::time::{Bucket, Instant, Zone, next, start_of};

const URL: &str = "https://openrouter.ai/api/v1/key";

/// The provider OpenRouter's keys are to, as agents' usage names it.
pub(super) const PROVIDER: &str = "openrouter";

/// Where an OpenRouter key can be kept.
pub(super) const SOURCES: &[Source] = &[
    Source {
        agent: Agent::OpenCode,
        provider: PROVIDER,
        location: Location::OpenCode(PROVIDER),
        token: "/key",
        expires: None,
        plan: None,
        whose: None,
    },
    // Where OpenCode kept keys before its database did.
    Source {
        agent: Agent::OpenCode,
        provider: PROVIDER,
        location: Location::File("auth.json"),
        token: "/openrouter/key",
        expires: None,
        plan: None,
        whose: None,
    },
    Source {
        agent: Agent::Pi,
        provider: PROVIDER,
        location: Location::File("auth.json"),
        token: "/openrouter/key",
        expires: None,
        plan: None,
        whose: None,
    },
    // Pi signed in to OpenRouter, which gave it a key.
    Source {
        agent: Agent::Pi,
        provider: PROVIDER,
        location: Location::File("auth.json"),
        token: "/openrouter/access",
        expires: None,
        plan: None,
        whose: None,
    },
];

/// Ask OpenRouter for `key`'s own limit, as of `now`.
pub(super) fn fetch(
    key: &str,
    _identity: &Identity,
    now: Instant,
) -> Result<PlanLimits, LimitProblem> {
    parse(&super::ask(URL, key, &[])?, key, now).ok_or(LimitProblem::Unrecognized)
}

/// The limit `key`'s answer gives, as of `now`: none for a key without one,
/// and `None` when it doesn't read.
fn parse(body: &Value, key: &str, now: Instant) -> Option<PlanLimits> {
    let data = body.get("data")?.as_object()?;
    let limit = match data.get("limit") {
        None | Some(Value::Null) => {
            return Some(PlanLimits {
                plan: None,
                limits: Vec::new(),
            });
        }
        Some(limit) => limit
            .as_f64()
            .filter(|limit| limit.is_finite() && *limit > 0.0)?,
    };
    let (every, name, used_in) = match data.get("limit_reset") {
        None | Some(Value::Null) => (None, "Key limit", "usage"),
        Some(Value::String(reset)) => match reset.as_str() {
            "daily" => (Some(Bucket::Day), "Daily key limit", "usage_daily"),
            "weekly" => (Some(Bucket::Week), "Weekly key limit", "usage_weekly"),
            "monthly" => (Some(Bucket::Month), "Monthly key limit", "usage_monthly"),
            _ => return None,
        },
        Some(_) => return None,
    };
    let spent = match data.get("limit_remaining") {
        None | Some(Value::Null) => data.get(used_in)?.as_f64()?,
        Some(remaining) => limit - remaining.as_f64()?,
    };
    let window = match every {
        Some(every) => {
            let utc = Zone::from(jiff::tz::TimeZone::UTC);
            let starts = start_of(every, now, &utc)?;
            (Some(starts), Some(next(every, starts, &utc)?))
        }
        None => (None, None),
    };
    let key = format!("key:{:016x}", fnv(FNV, key.as_bytes()));
    let used = spent / limit * 100.0;
    Some(PlanLimits {
        plan: None,
        limits: vec![super::limit(&key, name, None, Some(used), window)?],
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::parse;
    use crate::limits::Reported;
    use crate::time::Instant;

    /// The limits `key`'s answer `body` gives as of `now`.
    fn limits(body: &Value, key: &str, now: Instant) -> Option<Vec<Reported>> {
        parse(body, key, now).map(|answer| answer.limits)
    }

    fn now() -> Instant {
        // A Tuesday.
        Instant::parse("2026-09-29T12:00:00Z").unwrap()
    }

    /// An answer of the shape OpenRouter's reference gives, with `limit`
    /// and what goes with it.
    fn answer(limit: serde_json::Value) -> serde_json::Value {
        let mut data = json!({"label": "sk-or-v1-abc...xyz", "usage": 40.0, "usage_daily": 2.5,
                              "usage_weekly": 6.0, "usage_monthly": 25.5, "byok_usage": 0.0,
                              "is_free_tier": false, "is_management_key": false,
                              "include_byok_in_limit": false, "expires_at": null,
                              "rate_limit": {"requests": -1, "interval": "10s"}});
        for (field, value) in limit.as_object().unwrap() {
            data[field] = value.clone();
        }
        json!({"data": data})
    }

    #[test]
    fn a_keys_limit_is_used_by_what_is_gone_of_it_in_its_window() {
        // $100 a month with $74.50 left: $25.50 gone, 25.5% used, in
        // September by UTC.
        let monthly = limits(
            &answer(json!({"limit": 100.0, "limit_remaining": 74.5, "limit_reset": "monthly"})),
            "sk-or-v1-key",
            now(),
        )
        .unwrap();
        assert_eq!(monthly.len(), 1);
        let limit = &monthly[0];
        assert_eq!(limit.name, "Monthly key limit");
        assert!((limit.used - 25.5).abs() < 1e-9);
        assert_eq!(limit.starts, Instant::parse("2026-09-01T00:00:00Z"));
        assert_eq!(limit.resets, Instant::parse("2026-10-01T00:00:00Z"));
        assert!(limit.key.starts_with("key:"), "{}", limit.key);
        assert!(!limit.key.contains("sk-or"), "the key itself isn't kept");

        // $10 a day with no remainder given: the day's $2.50 is 25% of it.
        let daily = limits(
            &answer(json!({"limit": 10.0, "limit_remaining": null, "limit_reset": "daily"})),
            "sk-or-v1-key",
            now(),
        )
        .unwrap();
        assert!((daily[0].used - 25.0).abs() < 1e-9);
        assert_eq!(daily[0].starts, Instant::parse("2026-09-29T00:00:00Z"));
        assert_eq!(daily[0].resets, Instant::parse("2026-09-30T00:00:00Z"));

        // $20 a week, $15 left: 25%, the week from Monday the 28th.
        let weekly = limits(
            &answer(json!({"limit": 20.0, "limit_remaining": 15.0, "limit_reset": "weekly"})),
            "sk-or-v1-key",
            now(),
        )
        .unwrap();
        assert!((weekly[0].used - 25.0).abs() < 1e-9);
        assert_eq!(weekly[0].starts, Instant::parse("2026-09-28T00:00:00Z"));
        assert_eq!(weekly[0].resets, Instant::parse("2026-10-05T00:00:00Z"));

        // $50 that never resets, $40 of it used in all: 80%, no window.
        let lifetime = limits(
            &answer(json!({"limit": 50.0, "limit_remaining": null, "limit_reset": null})),
            "sk-or-v1-key",
            now(),
        )
        .unwrap();
        assert!((lifetime[0].used - 80.0).abs() < 1e-9);
        assert_eq!(
            (lifetime[0].name.as_str(), lifetime[0].resets),
            ("Key limit", None)
        );

        // Two keys are two limits.
        let other = limits(
            &answer(json!({"limit": 100.0, "limit_remaining": 74.5, "limit_reset": "monthly"})),
            "sk-or-v1-other",
            now(),
        )
        .unwrap();
        assert_ne!(other[0].key, limit.key);
    }

    #[test]
    fn a_key_with_no_limit_gives_none_and_one_that_doesnt_read_no_answer() {
        assert_eq!(
            limits(&answer(json!({"limit": null})), "k", now()),
            Some(Vec::new())
        );
        for broken in [
            json!({"limit": "100", "limit_remaining": 74.5}),
            json!({"limit": 0.0, "limit_remaining": 0.0}),
            json!({"limit": -5.0, "limit_remaining": 0.0}),
            json!({"limit": 100.0, "limit_remaining": 120.0}),
            json!({"limit": 100.0, "limit_remaining": "74.5"}),
            json!({"limit": 100.0, "limit_remaining": 74.5, "limit_reset": "yearly"}),
        ] {
            assert_eq!(
                limits(&answer(broken.clone()), "k", now()),
                None,
                "{broken}"
            );
        }
        assert_eq!(limits(&json!({"error": "no"}), "k", now()), None);
    }
}
