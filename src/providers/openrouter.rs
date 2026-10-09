//! OpenRouter: the spending limit a key can carry, and the credits of the
//! account its keys spend.
//!
//! Which key a response used isn't recorded, so every key is one account,
//! `openrouter:api`. `GET /api/v1/key` answers for the key: `limit` in
//! dollars or null, `limit_remaining`, `limit_reset` (`daily`, `weekly` or
//! `monthly` by UTC days, weeks from Monday and months; null for never), and
//! what it used in all and this day, week and month. A key's limit is keyed
//! `key-` and its fingerprint, as the key is never kept. `GET
//! /api/v1/credits` gives the account's `total_credits` and `total_usage`; a
//! key without access to credits still reports its own limit. A limit of 0
//! or less is no limit.

use jiff::civil::Weekday;
use jiff::tz::TimeZone;
use serde_json::Value;

use super::{Account, Info, Limits, Problem, Provider, Reading};
use crate::agents::Credential;

pub struct OpenRouter;

static INFO: Info = Info {
    id: "openrouter",
    name: "OpenRouter",
    subscription: None,
};

impl Provider for OpenRouter {
    fn info(&self) -> &'static Info {
        &INFO
    }

    /// Its sign-in gives an agent a key, so every login is a key.
    fn account(&self, _credential: &Credential) -> Option<Account> {
        Some(super::api_account("openrouter"))
    }

    fn limits(&self, credential: &Credential, now: i64) -> Result<Limits, Problem> {
        let key = super::ask("https://openrouter.ai/api/v1/key", credential, &[])?;
        let name = format!("key-{}", credential.secret.fingerprint());
        let mut readings = own_limit(&key, &name, now).ok_or(Problem::Unavailable)?;
        match super::ask("https://openrouter.ai/api/v1/credits", credential, &[]) {
            Ok(answer) => readings.extend(credits(&answer).ok_or(Problem::Unavailable)?),
            Err(Problem::SignIn) => {}
            Err(problem) => return Err(problem),
        }
        Ok(Limits {
            plan: None,
            readings,
        })
    }
}

fn credits(answer: &Value) -> Option<Option<Reading>> {
    let bought = answer["data"]["total_credits"].as_f64()?;
    let used = answer["data"]["total_usage"].as_f64()?;
    Some((bought > 0.0).then(|| Reading {
        key: "credits".to_owned(),
        name: "Credits".to_owned(),
        scope: None,
        used: used / bought * 100.0,
        size: Some(bought),
        starts: None,
        resets: None,
    }))
}

fn own_limit(answer: &Value, key: &str, now: i64) -> Option<Vec<Reading>> {
    let data = answer.get("data")?;
    let Some(limit) = data["limit"].as_f64().filter(|limit| *limit > 0.0) else {
        return Some(Vec::new());
    };
    // The window this is in, by UTC days, weeks from Monday, and months.
    let today = jiff::Timestamp::from_millisecond(now)
        .ok()?
        .to_zoned(TimeZone::UTC)
        .date();
    let monday = today
        .nth_weekday(-1, Weekday::Monday)
        .ok()
        .filter(|_| today.weekday() != Weekday::Monday)
        .unwrap_or(today);
    let (name, used_in, window) = match data["limit_reset"].as_str() {
        None => ("Key limit", "usage", None),
        Some("daily") => (
            "Daily key limit",
            "usage_daily",
            Some((today, jiff::Span::new().days(1))),
        ),
        Some("weekly") => (
            "Weekly key limit",
            "usage_weekly",
            Some((monday, jiff::Span::new().weeks(1))),
        ),
        Some("monthly") => (
            "Monthly key limit",
            "usage_monthly",
            Some((today.first_of_month(), jiff::Span::new().months(1))),
        ),
        Some(_) => return None,
    };
    let spent = match data["limit_remaining"].as_f64() {
        Some(remaining) => limit - remaining,
        None => data[used_in].as_f64()?,
    };
    let (starts, resets) = match window {
        Some((start, span)) => {
            let start = start.to_zoned(TimeZone::UTC).ok()?;
            let end = start.checked_add(span).ok()?;
            (
                Some(start.timestamp().as_millisecond()),
                Some(end.timestamp().as_millisecond()),
            )
        }
        None => (None, None),
    };
    Some(vec![Reading {
        key: key.to_owned(),
        name: name.to_owned(),
        scope: None,
        used: spent / limit * 100.0,
        size: Some(limit),
        starts,
        resets,
    }])
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn a_weekly_key_limit_runs_monday_to_monday() {
        // Thursday 2026-10-01 12:00 UTC.
        let now = 1_790_856_000_000;
        let answer =
            json!({"data": {"limit": 20.0, "limit_remaining": 11.0, "limit_reset": "weekly"}});
        let limit = &super::own_limit(&answer, "key-abc", now).unwrap()[0];
        assert_eq!(
            (limit.name.as_str(), limit.used, limit.size),
            ("Weekly key limit", 45.0, Some(20.0))
        );
        // Monday 2026-09-28 to Monday 2026-10-05, UTC.
        assert_eq!(
            (limit.starts, limit.resets),
            (Some(1_790_553_600_000), Some(1_791_158_400_000))
        );
        for none in [json!(null), json!(0.0)] {
            assert!(
                super::own_limit(&json!({"data": {"limit": none}}), "key-abc", now)
                    .unwrap()
                    .is_empty()
            );
        }
    }
}
