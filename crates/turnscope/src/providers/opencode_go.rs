//! OpenCode Go: the Go plan's limits, on a 5-hour, a weekly and a monthly
//! window, as <https://opencode.ai/docs/go/> gives them (read 2026-11-26).
//!
//! The 5-hour window starts at the first use and lasts five hours, so it
//! starts five hours before it resets. Before that first use, its
//! `resetsAt` is five hours from now: it reads as a window beginning now.
//! Only a key that isn't valid is answered 401. One to a workspace with no
//! Go subscription is answered 403.
//!
//! A plan can have several keys, and nothing in an answer names the plan
//! (measured 2026-10-05). But two keys to one plan answer alike, the monthly
//! reset to the same second (measured 2026-10-09), so keys whose monthly
//! window resets at the same instant are read as one account. Two plans
//! would have to begin in the same second to be taken for one.
//!
//! Each window's `percent` is a whole number, rounded down, and nothing else
//! says how much is used (measured 2026-10-09). OpenCode's console rounds a
//! finer figure, so it can say 1% used where the answer says 0: Turnscope
//! shows up to a point more left than the console.

use std::time::Instant;

use jiff::ToSpan;
use jiff::tz::TimeZone;
use serde_json::Value;

use super::{Account, Answered, Info, Kind, Limits, Problem, Provider, Reading};
use crate::agents::Credential;

pub struct OpenCodeGo;

static INFO: Info = Info {
    id: "opencode-go",
    name: "OpenCode Go",
    subscription: Some("OpenCode Go"),
    key_limits: true,
};

const USAGE: &str = "https://opencode.ai/inference/go/v1/usage";

impl Provider for OpenCodeGo {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn account(&self, credential: &Credential) -> Option<Account> {
        credential.key.then(|| {
            let fingerprint = credential.secret.fingerprint();
            Account {
                id: format!("{}:key-{fingerprint}", INFO.id),
                kind: Kind::Subscription,
                // Two keys to two plans are otherwise named alike.
                label: Some(format!("key {}", &fingerprint[..6])),
            }
        })
    }

    fn limits(&self, credential: &Credential, _now: i64, by: Instant) -> Result<Limits, Problem> {
        read(&super::requested(USAGE, &credential.secret, &[], None, by)?)
    }

    fn plan_mark(&self, limits: &Limits) -> Option<i64> {
        limits
            .readings
            .iter()
            .find(|reading| reading.key == "monthly")?
            .resets
    }
}

/// The limits in OpenCode's answer, or why there are none.
fn read(answered: &Answered) -> Result<Limits, Problem> {
    if answered.status == 403 && !answered.page() {
        return Err(Problem::NoPlan);
    }
    let answer = super::answer(USAGE, answered, &[401])?;
    Ok(parse(&answer)
        .ok_or_else(|| Problem::not_understood(USAGE))?
        .into())
}

fn parse(answer: &Value) -> Option<Vec<Reading>> {
    let usage = answer.get("usage").filter(|usage| usage.is_object())?;
    let mut readings = Vec::new();
    for (key, name, length) in [
        ("rolling", "5 hours", 5.hours()),
        ("weekly", "Weekly", 1.week()),
        ("monthly", "Monthly", 1.month()),
    ] {
        let window = &usage[key];
        if super::absent(window) {
            continue;
        }
        let resets = super::reset(&window["resetsAt"])?;
        let starts = resets.and_then(|resets| {
            let start = crate::time::zoned(resets, &TimeZone::UTC)
                .checked_sub(length)
                .ok()?;
            Some(start.timestamp().as_millisecond())
        });
        readings.push(Reading {
            key: key.to_owned(),
            name: name.to_owned(),
            scope: None,
            used: window["percent"].as_f64()?,
            size: None,
            starts,
            resets,
        });
    }
    (!readings.is_empty()).then_some(readings)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Answered, Problem};
    use crate::time::{DAY, HOUR};

    /// Each window is its length back from its reset, the 5 hours too, as
    /// it starts at the first use.
    #[test]
    fn the_plans_windows_are_read_from_their_starts() {
        // OpenCode's docs' example.
        let answer = json!({"usage": {
            "rolling": {"status": "ok", "percent": 30, "resetsAt": "2026-11-21T15:00:00.000Z"},
            "weekly": {"status": "rate-limited", "percent": 100, "resetsAt": "2026-11-23T00:00:00.000Z"},
            "monthly": {"status": "ok", "percent": 50, "resetsAt": "2026-12-01T00:00:00.000Z"}
        }});
        let read: Vec<_> = super::parse(&answer)
            .unwrap()
            .iter()
            .map(|reading| {
                let length = reading.resets.unwrap() - reading.starts.unwrap();
                (reading.name.clone(), reading.used, length)
            })
            .collect();
        // A month back from December 1 is November 1, 30 days.
        assert_eq!(
            read,
            [
                ("5 hours".to_owned(), 30.0, 5 * HOUR),
                ("Weekly".to_owned(), 100.0, 7 * DAY),
                ("Monthly".to_owned(), 50.0, 30 * DAY)
            ]
        );
        assert!(super::parse(&json!({"error": "unauthorized"})).is_none());
        assert!(super::parse(&json!({"usage": {}})).is_none());
    }

    /// A key Go doesn't cover has no plan to read, which signing in again
    /// wouldn't change.
    #[test]
    fn only_a_401_is_a_refused_login() {
        let answered = |status, body: &[u8]| {
            super::read(&Answered {
                status,
                retry_after: None,
                body: body.to_vec(),
            })
            .err()
        };
        // OpenCode's docs' example.
        let no_plan = br#"{"type":"error","error":{"type":"EntitlementError","message":"OpenCode Go subscription required."}}"#;
        assert_eq!(answered(403, no_plan), Some(Problem::NoPlan));
        assert_eq!(answered(401, b""), Some(Problem::SignIn));
        assert_eq!(
            answered(403, b"<!DOCTYPE html><title>Just a moment...</title>"),
            Some(Problem::unavailable(format!(
                "{}: answered 403",
                super::USAGE
            )))
        );
    }

    #[test]
    fn a_reset_that_wont_parse_fails_the_answer() {
        let answer = json!({"usage": {
            "rolling": {"percent": 12, "resetsAt": "soon"},
            "monthly": {"percent": 9, "resetsAt": "2026-11-01T00:00:00.000Z"}
        }});
        assert!(super::parse(&answer).is_none());
        let answer = json!({"usage": {"monthly": {"percent": 9, "resetsAt": null}}});
        assert_eq!(super::parse(&answer).unwrap()[0].resets, None);
    }
}
