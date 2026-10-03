//! OpenCode Go: the Go plan's rolling, weekly and monthly limits.
//!
//! OpenCode and Pi both keep the plan's API key under `opencode-go`. A key is
//! its own identity, so the same key in both apps is one account; only a hash
//! of it identifies the account, so the key itself is never stored.

use jiff::ToSpan;
use serde_json::Value;

use super::{Answer, Identity, LimitProblem, Location, Reader, Source, WEEK};
use crate::agent::Agent;
use crate::time::Instant;

const URL: &str = "https://opencode.ai/zen/go/v1/usage";

/// Where an OpenCode Go key can be kept, and how its limits are read.
pub(super) const READER: Reader = Reader {
    sources: &[
        Source {
            agent: Agent::OpenCode,
            provider: "opencode-go",
            location: Location::OpenCode("opencode-go"),
            token: "/key",
            expires: None,
            plan: None,
            whose: None,
        },
        // Where OpenCode kept sign-ins before its database did.
        Source {
            agent: Agent::OpenCode,
            provider: "opencode-go",
            location: Location::File("auth.json"),
            token: "/opencode-go/key",
            expires: None,
            plan: None,
            whose: None,
        },
        Source {
            agent: Agent::Pi,
            provider: "opencode-go",
            location: Location::File("auth.json"),
            token: "/opencode-go/key",
            expires: None,
            plan: None,
            whose: None,
        },
    ],
    identity,
    fetch,
};

/// The account a key belongs to, as an FNV-1a hash of the key.
fn identity(key: &str) -> Identity {
    Identity {
        key: format!("{:016x}", super::fnv(super::FNV, key.as_bytes())),
        label: None,
    }
}

fn fetch(key: &str, _identity: &Identity, _now: Instant) -> Result<Answer, LimitProblem> {
    super::get(URL, key, &[], parse)
}

/// The limits in a usage answer: `None` when a window it gives doesn't read.
/// A rolling window is always the last five hours, so it has no start that
/// how much of it has passed could be told from.
fn parse(body: &Value) -> Option<Answer> {
    let usage = &body["usage"];
    let mut limits = Vec::new();
    for (key, name) in [
        ("rolling", "5 hours"),
        ("weekly", "Weekly"),
        ("monthly", "Monthly"),
    ] {
        let window = &usage[key];
        if super::absent(window) {
            continue;
        }
        let resets = super::optional_instant(&window["resetsAt"])?;
        let span = match key {
            "rolling" => (None, resets),
            "weekly" => super::ending(resets, WEEK),
            _ => (resets.and_then(month_before), resets),
        };
        limits.push(super::limit(
            key,
            name,
            None,
            window["percent"].as_f64(),
            span,
        )?);
    }
    Some(Answer {
        plan: Some("Go".to_owned()),
        limits,
    })
}

/// A calendar month before `at`, in UTC, however long that month was.
fn month_before(at: Instant) -> Option<Instant> {
    let zoned = jiff::Timestamp::from_millisecond(at.millis())
        .ok()?
        .to_zoned(jiff::tz::TimeZone::UTC);
    Instant::from_millis(
        zoned
            .checked_sub(1.month())
            .ok()?
            .timestamp()
            .as_millisecond(),
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::parse;
    use crate::time::Instant;

    #[test]
    fn a_monthly_window_began_a_calendar_month_before_it_resets() {
        let answer = parse(&json!({"usage": {
            "rolling": {"percent": 3, "resetsAt": "2026-09-14T17:00:00Z"},
            "weekly": {"percent": 20, "resetsAt": "2026-09-21T00:00:00Z"},
            "monthly": {"percent": 45, "resetsAt": "2026-10-01T00:00:00Z"}}}))
        .unwrap();
        let rolling = &answer.limits[0];
        assert_eq!((rolling.key.as_str(), rolling.starts), ("rolling", None));
        assert_eq!(
            answer.limits[1].starts,
            Instant::parse("2026-09-14T00:00:00Z")
        );
        // September has thirty days.
        assert_eq!(
            answer.limits[2].starts,
            Instant::parse("2026-09-01T00:00:00Z")
        );
    }

    #[test]
    fn a_window_given_that_does_not_read_is_no_answer() {
        let answer = |monthly: serde_json::Value| {
            parse(&json!({"usage": {
                "rolling": {"percent": 3, "resetsAt": "2026-09-14T17:00:00Z"},
                "weekly": {"percent": 20, "resetsAt": "2026-09-21T00:00:00Z"},
                "monthly": monthly}}))
        };
        // A plan without a monthly limit.
        assert_eq!(answer(json!(null)).unwrap().limits.len(), 2);
        for broken in [
            json!({"percent": "45", "resetsAt": "2026-10-01T00:00:00Z"}),
            json!({"resetsAt": "2026-10-01T00:00:00Z"}),
            json!({"percent": 45, "resetsAt": "October"}),
        ] {
            assert!(answer(broken.clone()).is_none(), "{broken}");
        }
    }
}
