//! Anthropic: the limits of a Claude plan, which only Claude Code signs in
//! to, and the account Anthropic API keys draw on. Whose a sign-in is,
//! Claude Code's `.claude.json` says. An API key carries no limit an agent's
//! key can read.
//!
//! The usage answer keys each window by its length, `five_hour` and
//! `seven_day`, and a window on one model as `seven_day_` and the model; a
//! window the plan lacks is null. It also holds keys that aren't windows (the
//! week's use by model, extra usage, and features Anthropic tries out under
//! code names); only a key with a `utilization` of its own and a name a
//! person would know is a window. A window whose reset won't parse fails the
//! whole answer, since the rest would pass for the plan's full set.
//!
//! A window with nothing used since it last ended has no reset: it begins
//! with the next use (`resets_at` null at 0%, measured 2026-10-09). It's read
//! as one beginning now, so it says when it would reset and its work left
//! can't outrun it.

use serde_json::Value;

use super::{Account, Info, Kind, Limits, Problem, Provider, Reading};
use crate::agents::Credential;
use crate::time::{DAY, HOUR};

pub struct Anthropic;

static INFO: Info = Info {
    id: "anthropic",
    name: "Anthropic",
    subscription: Some("Claude"),
};

impl Provider for Anthropic {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn account(&self, credential: &Credential) -> Option<Account> {
        if credential.key {
            return Some(super::api_account("anthropic"));
        }
        let (id, label) = credential.identity.clone()?;
        Some(Account {
            id: format!("anthropic:{id}"),
            kind: Kind::Subscription,
            label,
        })
    }

    fn limits(&self, credential: &Credential, now: i64) -> Result<Limits, Problem> {
        if credential.key {
            return Ok(Limits {
                plan: None,
                readings: Vec::new(),
            });
        }
        let answer = super::ask(
            "https://api.anthropic.com/api/oauth/usage",
            credential,
            &["anthropic-beta: oauth-2025-04-20"],
        )?;
        let readings = parse(&answer, now)
            .filter(|readings| !readings.is_empty())
            .ok_or(Problem::Unavailable)?;
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
        let (name, scope) = match key.as_str() {
            "five_hour" => ("5 hours", None),
            "seven_day" => ("Weekly", None),
            // The plan's use through other apps signed in with it.
            "seven_day_oauth_apps" => ("Other apps weekly", None),
            other => match other.strip_prefix("seven_day_") {
                Some(model) => ("Weekly", Some(super::words(model))),
                None => continue,
            },
        };
        let length = if key == "five_hour" {
            5 * HOUR
        } else {
            7 * DAY
        };
        let used = window["utilization"].as_f64()?;
        let resets = match &window["resets_at"] {
            Value::Null if used == 0.0 => Some(now + length),
            Value::Null => None,
            at => Some(super::time(at)?),
        };
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
    }

    #[test]
    fn a_window_not_begun_begins_now() {
        let answer = json!({"five_hour": {"utilization": 0.0, "resets_at": null}});
        let readings = super::parse(&answer, 1_000).unwrap();
        assert_eq!(
            (readings[0].starts, readings[0].resets),
            (Some(1_000), Some(1_000 + 5 * HOUR))
        );
    }
}
