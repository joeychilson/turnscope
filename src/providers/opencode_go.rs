//! OpenCode Go: the Go plan's rolling 5-hour, weekly and monthly limits.
//!
//! OpenCode and Pi keep the plan's API key under `opencode-go`. A key is its
//! own identity, known by its fingerprint. A plan can have several keys, and
//! nothing in an answer names the plan (measured 2026-10-05). But two keys to
//! one plan answer alike, the monthly reset to the same second (measured
//! 2026-10-09), so keys whose monthly window resets at the same instant are
//! read as one account. Two plans would have to begin in the same second to
//! be taken for one.
//!
//! Each window's `percent` is a whole number, rounded down, and nothing else
//! says how much is used (measured 2026-10-09). OpenCode's console rounds a
//! finer figure, so it can say 1% used where the answer says 0: Turnscope
//! shows up to a point more left than the console.

use serde_json::Value;

use super::{Account, Info, Kind, Limits, Problem, Provider, Reading};
use crate::agents::Credential;
use crate::time::DAY;

pub struct OpenCodeGo;

static INFO: Info = Info {
    id: "opencode-go",
    name: "OpenCode Go",
    subscription: Some("OpenCode Go"),
};

impl Provider for OpenCodeGo {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn account(&self, credential: &Credential) -> Option<Account> {
        let fingerprint = credential.secret.fingerprint();
        credential.key.then(|| Account {
            id: format!("opencode-go:key-{fingerprint}"),
            kind: Kind::Subscription,
            // Two keys to two plans are otherwise named alike.
            label: Some(format!("key {}", &fingerprint[..6])),
        })
    }

    fn limits(&self, credential: &Credential, _now: i64) -> Result<Limits, Problem> {
        let answer = super::ask("https://opencode.ai/zen/go/v1/usage", credential, &[])?;
        Ok(Limits {
            plan: None,
            readings: parse(&answer).ok_or(Problem::Unavailable)?,
        })
    }

    fn plan_mark(&self, limits: &Limits) -> Option<i64> {
        limits
            .readings
            .iter()
            .find(|reading| reading.key == "monthly")?
            .resets
    }
}

fn parse(answer: &Value) -> Option<Vec<Reading>> {
    let usage = answer.get("usage").filter(|usage| usage.is_object())?;
    let mut readings = Vec::new();
    for (key, name) in [
        ("rolling", "5 hours"),
        ("weekly", "Weekly"),
        ("monthly", "Monthly"),
    ] {
        let window = &usage[key];
        if super::absent(window) {
            continue;
        }
        let resets = super::time(&window["resetsAt"]);
        let starts = match key {
            // Always the last five hours: no start to measure from.
            "rolling" => None,
            "weekly" => resets.map(|resets| resets - 7 * DAY),
            _ => resets.and_then(|resets| {
                let at = jiff::Timestamp::from_millisecond(resets).ok()?;
                let month = at
                    .to_zoned(jiff::tz::TimeZone::UTC)
                    .checked_sub(jiff::Span::new().months(1))
                    .ok()?;
                Some(month.timestamp().as_millisecond())
            }),
        };
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
    Some(readings)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::time::DAY;

    #[test]
    fn the_plans_windows_are_read_and_the_rolling_one_has_no_start() {
        let answer = json!({"usage": {
            "rolling": {"status": "ok", "percent": 12, "resetsAt": "2026-10-01T15:00:00.000Z"},
            "weekly": {"status": "ok", "percent": 40, "resetsAt": "2026-10-05T00:00:00.000Z"},
            "monthly": {"status": "ok", "percent": 9, "resetsAt": "2026-11-01T00:00:00.000Z"}
        }});
        let readings = super::parse(&answer).unwrap();
        let read: Vec<_> = readings
            .iter()
            .map(|reading| {
                (
                    reading.name.as_str(),
                    reading.used,
                    reading.starts.is_some(),
                )
            })
            .collect();
        assert_eq!(
            read,
            [
                ("5 hours", 12.0, false),
                ("Weekly", 40.0, true),
                ("Monthly", 9.0, true)
            ]
        );
        let weekly = &readings[1];
        assert_eq!(weekly.resets.unwrap() - weekly.starts.unwrap(), 7 * DAY);
        // A month back from November 1 is October 1, 31 days.
        let monthly = &readings[2];
        assert_eq!(monthly.resets.unwrap() - monthly.starts.unwrap(), 31 * DAY);
        assert!(super::parse(&json!({"error": "unauthorized"})).is_none());
    }
}
