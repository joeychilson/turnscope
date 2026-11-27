//! OpenRouter: the spending limit a key can carry and the budgets it's held
//! to, and the credits and free models' requests of the account its keys
//! spend.
//!
//! Which key a response used isn't recorded, so every key is one account,
//! `openrouter:api`. A key's limit is keyed by the key's fingerprint, as the
//! key is never kept. Credits are the account's, so they're asked once a
//! read, of the first key with access to them; a key without still reports
//! its own limit.
//!
//! `total_credits` is taken for all the account ever bought and
//! `total_usage` for all it ever used, as they're named: no purchase was
//! made to measure it. So buying credits makes the limit's size larger,
//! which is how an alert tells credits used up again from the time before
//! (`alerts::window`). A key's limit that never resets counts all the key
//! ever used, and is given more by raising `limit`. OpenRouter's docs say
//! `/credits` needs a management key, but a regular key
//! (`is_management_key` false) was answered with its credits on 2026-11-26.
//!
//! A key's answer lists the budgets its requests are held to (`budgets`):
//! guardrails on what the key or its member spends, the workspace's, and
//! the key's own limit again (`key_limit`), which is read from `limit`. It
//! also gives the account's requests to free models today, the same from
//! every key, so they're kept as one limit. By OpenRouter's docs, every
//! window is a UTC day, a week from Monday or a month, and free models'
//! requests reset at UTC midnight.

use std::time::Instant;

use jiff::tz::TimeZone;
use serde_json::Value;

use super::{Info, Limits, Problem, Provider, Reading};
use crate::agents::Credential;
use crate::time::{self, Period};

pub struct OpenRouter;

static INFO: Info = Info {
    id: "openrouter",
    name: "OpenRouter",
    subscription: None,
    key_limits: true,
};

const KEY: &str = "https://openrouter.ai/api/v1/key";
const CREDITS: &str = "https://openrouter.ai/api/v1/credits";

impl Provider for OpenRouter {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn limits(&self, credential: &Credential, now: i64, by: Instant) -> Result<Limits, Problem> {
        let answer = super::ask(KEY, credential, &[], by)?;
        let readings = key_limits(&answer, &credential.secret.fingerprint(), now)
            .ok_or_else(|| Problem::not_understood(KEY))?;
        Ok(readings.into())
    }

    fn account_limits(
        &self,
        credential: &Credential,
        by: Instant,
    ) -> Result<Vec<Reading>, Problem> {
        let answer = super::ask(CREDITS, credential, &[], by)?;
        Ok(credits(&answer)
            .ok_or_else(|| Problem::not_understood(CREDITS))?
            .into_iter()
            .collect())
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

/// What a key's answer gives: its own limit, its budgets, and the account's
/// requests to free models today.
fn key_limits(answer: &Value, fingerprint: &str, now: i64) -> Option<Vec<Reading>> {
    let own = own_limit(answer, &super::key_limit(fingerprint), now)?;
    let data = &answer["data"];
    let budgets = data["budgets"].as_array().into_iter().flatten();
    Some(
        own.into_iter()
            .chain(budgets.filter_map(|entry| budget(entry, fingerprint, now)))
            .chain(free_requests(data, now))
            .collect(),
    )
}

fn own_limit(answer: &Value, key: &str, now: i64) -> Option<Option<Reading>> {
    let data = answer.get("data").filter(|data| data.is_object())?;
    let limit = match &data["limit"] {
        Value::Null => return Some(None),
        value => value.as_f64()?,
    };
    if limit <= 0.0 {
        return Some(None);
    }
    let period = period(&data["limit_reset"])?;
    let used_in = match period {
        None => "usage",
        Some(Period::Day) => "usage_daily",
        Some(Period::Week) => "usage_weekly",
        Some(Period::Month) => "usage_monthly",
    };
    let spent = match data["limit_remaining"].as_f64() {
        Some(remaining) => limit - remaining,
        None => data[used_in].as_f64()?,
    };
    let name = named("key limit", period);
    Some(Some(money(key.to_owned(), name, period, spent, limit, now)))
}

/// A budget besides the key's own limit: a guardrail's or a workspace's.
/// One of a scope or interval not known is passed over.
fn budget(budget: &Value, fingerprint: &str, now: i64) -> Option<Reading> {
    let id = |field: &str| budget[field].as_str().filter(|id| !id.is_empty());
    let period = period(&budget["reset_interval"])?;
    // A guardrail on keys holds each key to its own spend, so it's the
    // key's limit: "Pi's weekly guardrail". One on members holds every
    // key of theirs to their spend together.
    let (key, name) = match budget["scope"].as_str()? {
        "api_key" | "workspace_default_key" => (
            super::key_limit_on(fingerprint, &format!("guardrail-{}", id("guardrail_id")?)),
            named("guardrail", period),
        ),
        "member" | "workspace_default_member" => (
            format!("guardrail-{}", id("guardrail_id")?),
            named("guardrail", period),
        ),
        "workspace" => (
            format!("budget-{}", id("budget_id")?),
            named("workspace budget", period),
        ),
        _ => return None,
    };
    let limit = budget["limit_usd"].as_f64().filter(|limit| *limit > 0.0)?;
    let spent = budget["spend_usd"].as_f64()?;
    Some(money(key, name, period, spent, limit, now))
}

/// The account's requests to free models today: a count, not money, so the
/// limit has no size. It's read with none made too, as the day after one
/// that used them up has none, and only a reading in that day tells that
/// they're back.
fn free_requests(data: &Value, now: i64) -> Option<Reading> {
    let requests = &data["free_model_daily_requests"];
    let used = requests["used"].as_f64()?;
    let limit = requests["limit"].as_f64().filter(|limit| *limit > 0.0)?;
    let (starts, resets) = time::window(Period::Day, now, &TimeZone::UTC);
    Some(Reading {
        key: "free_model_daily_requests".to_owned(),
        name: "Daily".to_owned(),
        scope: Some("Free models".to_owned()),
        used: used / limit * 100.0,
        size: None,
        starts: Some(starts),
        resets: Some(resets),
    })
}

/// How often a limit resets, as OpenRouter names it: `None` for never.
/// `None` too for a name not known.
fn period(value: &Value) -> Option<Option<Period>> {
    match value {
        Value::Null => Some(None),
        Value::String(name) => Some(Some(match name.as_str() {
            "daily" => Period::Day,
            "weekly" => Period::Week,
            "monthly" => Period::Month,
            _ => return None,
        })),
        _ => None,
    }
}

/// What and its period: "Daily key limit", or "Key limit" for one that
/// never resets.
fn named(what: &str, period: Option<Period>) -> String {
    match period {
        None => what[..1].to_uppercase() + &what[1..],
        Some(Period::Day) => format!("Daily {what}"),
        Some(Period::Week) => format!("Weekly {what}"),
        Some(Period::Month) => format!("Monthly {what}"),
    }
}

/// `spent` of a limit of `limit` dollars, its window by `period`.
fn money(
    key: String,
    name: String,
    period: Option<Period>,
    spent: f64,
    limit: f64,
    now: i64,
) -> Reading {
    let window = period.map(|period| time::window(period, now, &TimeZone::UTC));
    Reading {
        key,
        name,
        scope: None,
        used: spent / limit * 100.0,
        size: Some(limit),
        starts: window.map(|(starts, _)| starts),
        resets: window.map(|(_, resets)| resets),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    #[test]
    fn a_key_limit_runs_by_utc_days_weeks_and_months() {
        // Thursday 2026-10-01 12:00 UTC.
        let now = 1_790_856_000_000;
        let read = |answer: serde_json::Value| {
            let limit = super::own_limit(&answer, "key-abc", now).unwrap().unwrap();
            (
                limit.name,
                limit.used,
                limit.size,
                limit.starts,
                limit.resets,
            )
        };
        // Monday 2026-09-28 to Monday 2026-10-05.
        assert_eq!(
            read(
                json!({"data": {"limit": 20.0, "limit_remaining": 11.0, "limit_reset": "weekly"}})
            ),
            (
                "Weekly key limit".to_owned(),
                45.0,
                Some(20.0),
                Some(1_790_553_600_000),
                Some(1_791_158_400_000)
            )
        );
        // Without what remains, the window's own usage is what was spent.
        // October 1 to 2.
        assert_eq!(
            read(json!({"data": {"limit": 10.0, "limit_reset": "daily", "usage_daily": 2.5}})),
            (
                "Daily key limit".to_owned(),
                25.0,
                Some(10.0),
                Some(1_790_812_800_000),
                Some(1_790_899_200_000)
            )
        );
        // October 1 to November 1.
        assert_eq!(
            read(
                json!({"data": {"limit": 20.0, "limit_remaining": 10.0, "limit_reset": "monthly"}})
            ),
            (
                "Monthly key limit".to_owned(),
                50.0,
                Some(20.0),
                Some(1_790_812_800_000),
                Some(1_793_491_200_000)
            )
        );
        assert_eq!(
            read(json!({"data": {"limit": 8.0, "limit_reset": null, "usage": 6.0}})),
            ("Key limit".to_owned(), 75.0, Some(8.0), None, None)
        );
        // A key with no limit has none.
        for none in [json!(null), json!(0.0)] {
            assert!(
                super::own_limit(&json!({"data": {"limit": none}}), "key-abc", now)
                    .unwrap()
                    .is_none()
            );
        }
    }

    /// A key's answer as OpenRouter gives it, ids made up.
    fn key_answer(budgets: Value, free_used: u64) -> Value {
        json!({"data": {"label": "made-up", "is_management_key": false, "limit": 50, "limit_reset": null,
            "limit_remaining": 50, "budgets": budgets, "usage": 0, "usage_daily": 0, "usage_weekly": 0,
            "usage_monthly": 0, "is_free_tier": false,
            "free_model_daily_requests": {"used": free_used, "limit": 1000, "remaining": 1000 - free_used},
            "rate_limit": {"requests": -1, "interval": "10s"}}})
    }

    fn budget(scope: &str, id: (&str, &str), limit: f64, spent: f64, every: Option<&str>) -> Value {
        let mut budget = json!({"scope": scope, "guardrail_id": null, "budget_id": null, "limit_usd": limit,
            "spend_usd": spent, "remaining_usd": limit - spent, "reset_interval": every, "resets_at": null});
        budget[id.0] = json!(id.1);
        budget
    }

    type Read = (
        String,
        String,
        Option<String>,
        f64,
        Option<f64>,
        Option<i64>,
        Option<i64>,
    );

    fn read(answer: &Value, now: i64) -> Vec<Read> {
        super::key_limits(answer, "0123456789abcdef", now)
            .unwrap()
            .into_iter()
            .map(|r| (r.key, r.name, r.scope, r.used, r.size, r.starts, r.resets))
            .collect()
    }

    #[test]
    fn a_keys_guardrails_and_workspace_budgets_are_read_beside_its_limit() {
        // Thursday 2026-10-01 12:00 UTC.
        let now = 1_790_856_000_000;
        let (day, week, month) = (
            (Some(1_790_812_800_000), Some(1_790_899_200_000)),
            (Some(1_790_553_600_000), Some(1_791_158_400_000)),
            (Some(1_790_812_800_000), Some(1_793_491_200_000)),
        );
        let guardrail = |scope, id, every| budget(scope, ("guardrail_id", id), 10.0, 2.5, every);
        let answer = key_answer(
            json!([
                guardrail("member", "gr_1", Some("daily")),
                guardrail("workspace_default_member", "gr_2", Some("weekly")),
                guardrail("api_key", "gr_3", Some("weekly")),
                guardrail("workspace_default_key", "gr_4", Some("monthly")),
                budget("workspace", ("budget_id", "wb_1"), 100.0, 40.0, None),
                // The key's own limit, read from `limit`, and what isn't known.
                budget("key_limit", ("guardrail_id", "gr_0"), 50.0, 0.0, None),
                guardrail("organization", "gr_5", Some("daily")),
                budget("workspace", ("budget_id", "wb_2"), 5.0, 1.0, Some("yearly")),
            ]),
            0,
        );
        let money = |key: &str, name: &str, used, size, (starts, resets)| {
            (
                key.to_owned(),
                name.to_owned(),
                None,
                used,
                Some(size),
                starts,
                resets,
            )
        };
        assert_eq!(
            read(&answer, now)[..6],
            [
                money("key-0123456789abcdef", "Key limit", 0.0, 50.0, (None, None)),
                money("guardrail-gr_1", "Daily guardrail", 25.0, 10.0, day),
                money("guardrail-gr_2", "Weekly guardrail", 25.0, 10.0, week),
                // On the key alone, so the key's.
                money(
                    "key-0123456789abcdef-guardrail-gr_3",
                    "Weekly guardrail",
                    25.0,
                    10.0,
                    week
                ),
                money(
                    "key-0123456789abcdef-guardrail-gr_4",
                    "Monthly guardrail",
                    25.0,
                    10.0,
                    month
                ),
                money("budget-wb_1", "Workspace budget", 40.0, 100.0, (None, None)),
            ]
        );
    }

    #[test]
    fn free_model_requests_are_a_limit_of_the_utc_day_made_or_not() {
        // Thursday 2026-10-01 12:00 UTC: the day from midnight UTC.
        let now = 1_790_856_000_000;
        for (made, used) in [(0, 0.0), (10, 1.0)] {
            assert_eq!(
                read(&key_answer(json!([]), made), now)[1],
                (
                    "free_model_daily_requests".to_owned(),
                    "Daily".to_owned(),
                    Some("Free models".to_owned()),
                    used,
                    None,
                    Some(1_790_812_800_000),
                    Some(1_790_899_200_000)
                )
            );
        }
    }

    #[test]
    fn a_key_answer_not_understood_reads_no_limit() {
        let now = 1_790_856_000_000;
        for answer in [
            json!({"data": {"limit": 10.0, "limit_remaining": 5.0, "limit_reset": "yearly"}}),
            json!({"data": {"limit": 10.0, "limit_remaining": 5.0, "limit_reset": 123}}),
            json!({"data": {"limit": "10.0"}}),
            json!({"data": null}),
            json!({"data": {"limit": 10.0, "limit_reset": "daily"}}),
            json!({"error": {"code": 401, "message": "unauthorized"}}),
        ] {
            assert!(super::own_limit(&answer, "key-abc", now).is_none());
        }
    }

    #[test]
    fn credits_are_what_was_used_of_what_was_bought() {
        let answer = json!({"data": {"total_credits": 40.0, "total_usage": 10.0}});
        let credits = super::credits(&answer).unwrap().unwrap();
        assert_eq!(
            (
                credits.name.as_str(),
                credits.used,
                credits.size,
                credits.resets
            ),
            ("Credits", 25.0, Some(40.0), None)
        );
        let none_bought = json!({"data": {"total_credits": 0.0, "total_usage": 0.0}});
        assert!(matches!(super::credits(&none_bought), Some(None)));
        assert!(super::credits(&json!({"error": "unauthorized"})).is_none());
    }
}
