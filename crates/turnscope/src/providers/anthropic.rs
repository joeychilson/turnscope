//! Anthropic: the limits of a Claude plan, which only Claude Code signs in
//! to, and the account Anthropic API keys draw on. An API key carries no
//! limit an agent's key can read.
//!
//! The usage answer keys each window by its length, `five_hour` and
//! `seven_day`, and a window on one model as `seven_day_` and the model; a
//! window the plan lacks is null. It also holds keys that aren't windows (the
//! week's use by model, extra usage). Every `seven_day_` key with a
//! `utilization` of its own is read as a window, named by its words, code
//! names of features Anthropic tries out included: `seven_day_opus` is
//! Weekly (Opus). One of the plan's own two whose use or reset doesn't read
//! fails the whole answer, since the rest would pass for the plan's full
//! set. Any other is passed over: one feature tried out wrote in a way of
//! its own shouldn't leave the plan unread.
//!
//! A window with nothing used since it last ended has no reset: it begins
//! with the next use (`resets_at` null at 0%, measured 2026-10-09). It's read
//! as one beginning now, so it says when it would reset and its work left
//! can't outrun it.

use std::time::Instant;

use serde_json::Value;

use super::{Account, Info, Limits, Problem, Provider, Reading};
use crate::agents::Credential;
use crate::time::{DAY, HOUR};

pub struct Anthropic;

static INFO: Info = Info {
    id: "anthropic",
    name: "Anthropic",
    subscription: Some("Claude"),
    key_limits: false,
};

const USAGE: &str = "https://api.anthropic.com/api/oauth/usage";

impl Provider for Anthropic {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn account(&self, credential: &Credential) -> Option<Account> {
        super::account(INFO.id, credential, || credential.identity.clone())
    }

    fn owns_model(&self, model: &str) -> bool {
        model.starts_with("claude-")
    }

    fn limits(&self, credential: &Credential, now: i64, by: Instant) -> Result<Limits, Problem> {
        let answer = super::ask(USAGE, credential, &["anthropic-beta: oauth-2025-04-20"], by)?;
        let readings = parse(&answer, now)
            .filter(|readings| !readings.is_empty())
            .ok_or_else(|| Problem::not_understood(USAGE))?;
        // The plan is the credential's, which the account's title takes.
        Ok(Limits {
            plan: None,
            readings,
        })
    }
}

fn parse(answer: &Value, now: i64) -> Option<Vec<Reading>> {
    let mut readings = Vec::new();
    for (key, window) in answer.as_object()? {
        if super::absent(window) || window.get("utilization").is_none() {
            continue;
        }
        let (name, scope, length) = match key.as_str() {
            "five_hour" => ("5 hours", None, 5 * HOUR),
            "seven_day" => ("Weekly", None, 7 * DAY),
            // The plan's use through other apps signed in with it, not
            // Claude Code's. So it's scoped, as a limit on one model is:
            // used up, it doesn't decide the account or hold back the
            // return of 5 hours or Weekly, and as `Other apps` names no
            // model, none of Claude Code's responses count toward it. With
            // no scope, it did all three. Not measured: no answer read here
            // has carried it.
            "seven_day_oauth_apps" => ("Weekly", Some("Other apps".to_owned()), 7 * DAY),
            other => match other.strip_prefix("seven_day_") {
                Some(model) => ("Weekly", Some(super::words(model)), 7 * DAY),
                None => continue,
            },
        };
        let read = window["utilization"]
            .as_f64()
            .zip(super::reset(&window["resets_at"]));
        let Some((used, resets)) = read else {
            match key.as_str() {
                "five_hour" | "seven_day" => return None,
                _ => continue,
            }
        };
        let resets = resets.or((used == 0.0).then_some(now + length));
        readings.push(Reading {
            key: key.clone(),
            name: name.to_owned(),
            scope,
            used,
            size: None,
            starts: resets.map(|resets| resets - length),
            resets,
        });
    }
    Some(readings)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::time::HOUR;

    #[test]
    fn windows_are_read_and_what_isnt_one_is_passed_over() {
        let answer = json!({
            "five_hour": {"utilization": 52.0, "resets_at": "2026-10-01T15:00:00Z"},
            "seven_day": {"utilization": 44.0, "resets_at": "2026-10-05T00:00:00Z"},
            "seven_day_opus": {"utilization": 10.0, "resets_at": null},
            "seven_day_sonnet": null,
            "seven_day_breakdown": {"opus": 3},
            "tangelo": {"utilization": 1.0, "resets_at": null},
        });
        let readings = super::parse(&answer, 0).unwrap();
        let read: Vec<_> = readings
            .iter()
            .map(|reading| {
                (
                    reading.key.as_str(),
                    reading.name.as_str(),
                    reading.scope.as_deref(),
                    reading.used,
                )
            })
            .collect();
        assert_eq!(
            read,
            [
                ("five_hour", "5 hours", None, 52.0),
                ("seven_day", "Weekly", None, 44.0),
                ("seven_day_opus", "Weekly", Some("Opus"), 10.0)
            ]
        );
        assert_eq!(
            readings[0].resets.unwrap() - readings[0].starts.unwrap(),
            5 * HOUR
        );
        // A window not begun begins now.
        let answer = json!({"five_hour": {"utilization": 0.0, "resets_at": null}});
        let readings = super::parse(&answer, 1_000).unwrap();
        assert_eq!(
            (readings[0].starts, readings[0].resets),
            (Some(1_000), Some(1_000 + 5 * HOUR))
        );
    }

    /// Scoped, so used up it doesn't decide the account, hold back another
    /// limit's return, or list Claude Code's sessions as what used it.
    #[test]
    fn other_apps_weekly_covers_other_apps_alone() {
        let answer = json!({
            "seven_day": {"utilization": 44.0, "resets_at": "2026-10-05T00:00:00Z"},
            "seven_day_oauth_apps": {"utilization": 100.0, "resets_at": "2026-10-05T00:00:00Z"},
        });
        let readings = super::parse(&answer, 0).unwrap();
        let other = &readings[1];
        assert_eq!(
            (other.name.as_str(), other.scope.as_deref()),
            ("Weekly", Some("Other apps"))
        );
    }

    #[test]
    fn a_window_beside_the_plans_own_that_doesnt_read_is_passed_over() {
        let answer = json!({
            "five_hour": {"utilization": 52.0, "resets_at": "2026-10-01T15:00:00Z"},
            "seven_day_tangelo": {"utilization": "n/a", "resets_at": null},
            "seven_day_quince": {"utilization": 5.0, "resets_at": "soon"},
        });
        let read: Vec<_> = super::parse(&answer, 0)
            .unwrap()
            .iter()
            .map(|reading| reading.key.clone())
            .collect();
        assert_eq!(read, ["five_hour"]);
        // One of the plan's own fails the answer, which would otherwise pass
        // for a plan without it.
        let answer = json!({
            "five_hour": {"utilization": "n/a", "resets_at": null},
            "seven_day": {"utilization": 44.0, "resets_at": "2026-10-05T00:00:00Z"},
        });
        assert!(super::parse(&answer, 0).is_none());
    }
}
