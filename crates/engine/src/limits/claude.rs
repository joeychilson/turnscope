//! Claude: the limits of a Claude plan.
//!
//! Only Claude Code holds a Claude sign-in, because Anthropic does not allow
//! its plans in other agents. Claude Code keeps it in the login Keychain as
//! `Claude Code-credentials`, with the plan beside the token, and one at a
//! time: signing in to another account replaces it. The token doesn't say
//! whose it is; Claude Code records that in `~/.claude.json`, as
//! `oauthAccount`, with the account's and its organization's ids and its
//! email address, so the accounts it is switched between stand apart.
//!
//! **Another folder.** Pointed at a folder of its own by `CLAUDE_CONFIG_DIR`,
//! Claude Code keeps a second account apart: read from Claude Code 2.1.284's
//! code on 2026-09-29, its sign-in goes to the Keychain item named
//! `Claude Code-credentials-` and the first eight hexadecimal digits of the
//! SHA-256 of the variable's value, NFC-normalized, and whose it is to
//! `.claude.json` in that folder, as `.config.json` there goes before either
//! where an older version left one. The folder's path is hashed as given,
//! which is as the person typed it; a path given with a separator at its end,
//! or a spelling that isn't NFC, names another item, and its sign-in isn't
//! found. This Mac had only `Claude Code-credentials` (2026-09-29), so the
//! naming is tested on folders made for it.
//!
//! **The answer.** The usage answer keys each window by its length,
//! `five_hour` and `seven_day`, and a window on one model as `seven_day_`
//! and the model; a window the plan
//! does not have is null. A window given is read whole or not at all: one
//! that doesn't read makes the answer one not understood, since the windows
//! left would look like all of them.
//!
//! The answer holds more than windows, and grows. On 2026-09-27 this Mac's
//! held 23 keys: `five_hour` and `seven_day` given, four `seven_day_`
//! windows null, and beside them `seven_day_breakdown`, the week's use by
//! model, which has no use of its own; `extra_usage` and `spend`; `limits`,
//! a list of the same limits told another way; and 15 keys named as
//! Anthropic names what it tries out, `tangelo` and `nimbus_quill`, all null
//! but one, which had a use and no reset. Only what has a use of its own is
//! a window, and a key of the last kind names none a person would know, so
//! it is passed over, as is anything else that is no window: taken for one,
//! the breakdown made every answer one not understood.

use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::sign_in::{Identity, Location, Source, Whose, one_account};
use super::{HOUR, LimitProblem, PlanLimits, Reader, WEEK};
use crate::agent::Agent;
use crate::folders::Folder;
use crate::time::Instant;

const URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// Five hours, in milliseconds.
const FIVE_HOURS: i64 = 5 * HOUR;

/// Where a Claude sign-in is kept, and how its limits are read.
pub(super) const READER: Reader = Reader {
    sources: &[Source {
        agent: Agent::ClaudeCode,
        provider: "anthropic",
        location: Location::ClaudeKeychain,
        token: "/claudeAiOauth/accessToken",
        expires: Some("/claudeAiOauth/expiresAt"),
        plan: Some("/claudeAiOauth/subscriptionType"),
        // A plan belongs to an organization, and one account can be in
        // several, so the two together name whose limits these are.
        whose: Some(Whose {
            file: config,
            id: &[
                "/oauthAccount/accountUuid",
                "/oauthAccount/organizationUuid",
            ],
            label: "/oauthAccount/emailAddress",
        }),
    }],
    identity: one_account,
    fetch,
};

/// The login Keychain item Claude Code keeps its sign-in for `folder` in:
/// `Claude Code-credentials` for its own folder, and for one it is pointed
/// at, the same and a dash and the first eight hexadecimal digits of the
/// SHA-256 of the folder's path.
pub(super) fn service(folder: &Folder) -> String {
    const OWN: &str = "Claude Code-credentials";
    if folder.is_own() {
        return OWN.to_owned();
    }
    let digest = Sha256::digest(folder.path.as_os_str().as_encoded_bytes());
    let hex: String = digest[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{OWN}-{hex}")
}

/// The file Claude Code records whose sign-in it holds in for `folder`,
/// under `home`: `.config.json` in the folder, where an older version left
/// it, and otherwise `.claude.json` in the home directory for its own
/// folder and in the folder for one it is pointed at.
fn config(folder: &Folder, home: &Path) -> PathBuf {
    let older = folder.path.join(".config.json");
    if older.is_file() {
        older
    } else if folder.is_own() {
        home.join(".claude.json")
    } else {
        folder.path.join(".claude.json")
    }
}

fn fetch(token: &str, _identity: &Identity, _now: Instant) -> Result<PlanLimits, LimitProblem> {
    super::get(URL, token, &["anthropic-beta: oauth-2025-04-20"], parse)
}

/// The limits in a usage answer: `None` when a window it gives doesn't read,
/// since the windows left would look like all of them.
fn parse(body: &Value) -> Option<PlanLimits> {
    let mut limits = Vec::new();
    for (key, window) in body.as_object()? {
        if super::absent(window) {
            continue;
        }
        let (name, scope) = match key.as_str() {
            "five_hour" => ("5 hours", None),
            "seven_day" => ("Weekly", None),
            // With no use of its own it is no window, as the week's use by
            // model and what is said of extra usage aren't.
            _ if window.get("utilization").is_none() => continue,
            // The plan's use through other apps signed in with it, which is
            // neither a model's nor all of it. The answers read here give it
            // only as null.
            "seven_day_oauth_apps" => ("Other apps weekly", None),
            other => match other.strip_prefix("seven_day_") {
                Some(rest) => ("Weekly", Some(words(rest))),
                // Named as what Anthropic tries out is: no window a person
                // would know by it.
                None => continue,
            },
        };
        let length = if key == "five_hour" { FIVE_HOURS } else { WEEK };
        let resets = super::optional_instant(&window["resets_at"])?;
        limits.push(super::limit(
            key,
            name,
            scope,
            window["utilization"].as_f64(),
            super::ending(resets, length),
        )?);
    }
    Some(PlanLimits { plan: None, limits })
}

/// A model's key, such as `opus`, as words: `Opus`.
fn words(key: &str) -> String {
    let spaced = key.replace('_', " ");
    let mut characters = spaced.chars();
    characters
        .next()
        .map(|first| first.to_uppercase().chain(characters).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use std::path::Path;

    use super::{config, parse, service};
    use crate::agent::Agent;
    use crate::folders::{Folder, FolderOrigin};
    use crate::time::Instant;

    fn folder(path: &Path, origin: FolderOrigin) -> Folder {
        Folder {
            agent: Agent::ClaudeCode,
            path: path.to_path_buf(),
            origin,
        }
    }

    #[test]
    fn a_folder_claude_code_is_pointed_at_keeps_its_sign_in_apart() {
        // `printf '%s' /Users/joey/.claude-work | shasum -a 256` begins
        // 66b769ff.
        let work = folder(Path::new("/Users/joey/.claude-work"), FolderOrigin::Found);
        assert_eq!(service(&work), "Claude Code-credentials-66b769ff");
        let own = folder(Path::new("/Users/joey/.claude"), FolderOrigin::Own);
        assert_eq!(service(&own), "Claude Code-credentials");

        // Whose each holds is in the home directory for its own folder, in
        // the folder for another, and in `.config.json` where an older
        // version left one.
        let home = tempfile::tempdir().unwrap();
        let own = folder(&home.path().join(".claude"), FolderOrigin::Own);
        let work = folder(&home.path().join(".claude-work"), FolderOrigin::Found);
        assert_eq!(config(&own, home.path()), home.path().join(".claude.json"));
        assert_eq!(
            config(&work, home.path()),
            home.path().join(".claude-work/.claude.json")
        );
        std::fs::create_dir_all(&work.path).unwrap();
        std::fs::write(work.path.join(".config.json"), "{}").unwrap();
        assert_eq!(
            config(&work, home.path()),
            home.path().join(".claude-work/.config.json")
        );
    }

    #[test]
    fn each_window_the_plan_has_is_a_limit() {
        let answer = parse(&json!({
            "five_hour": {"utilization": 29.0, "resets_at": "2026-09-14T17:00:00Z"},
            "seven_day": {"utilization": 41.5, "resets_at": "2026-09-18T12:00:00Z"},
            "seven_day_opus": {"utilization": 12.0, "resets_at": "2026-09-18T12:00:00Z"},
            "seven_day_oauth_apps": null
        }))
        .unwrap();
        let five = answer
            .limits
            .iter()
            .find(|limit| limit.key == "five_hour")
            .unwrap();
        assert_eq!((five.name.as_str(), five.used), ("5 hours", 29.0));
        // A five-hour window resetting at 17:00 began at 12:00.
        assert_eq!(five.starts, Instant::parse("2026-09-14T12:00:00Z"));
        let opus = answer
            .limits
            .iter()
            .find(|limit| limit.key == "seven_day_opus")
            .unwrap();
        assert_eq!(opus.scope.as_deref(), Some("Opus"));
        assert_eq!(answer.limits.len(), 3);
    }

    /// A usage answer of the shape Claude's gives, its weekly window as
    /// `seven_day`.
    fn answer(seven_day: serde_json::Value) -> serde_json::Value {
        json!({
            "five_hour": {"utilization": 29.0, "resets_at": "2026-09-14T17:00:00Z"},
            "seven_day": seven_day,
            "seven_day_oauth_apps": null,
            // A window not in effect, all of it null, and what the answer
            // says of extra usage, which is no window.
            "seven_day_opus": {"utilization": null, "resets_at": null},
            "extra_usage": {"is_enabled": false, "monthly_limit": null, "utilization": null}
        })
    }

    #[test]
    fn a_window_given_that_does_not_read_is_no_answer() {
        // Read whole, and a window not yet begun, with no reset.
        let whole = parse(&answer(
            json!({"utilization": 41.5, "resets_at": "2026-09-18T12:00:00Z"}),
        ))
        .unwrap();
        assert_eq!(whole.limits.len(), 2);
        let begun = parse(&answer(json!({"utilization": 0.0, "resets_at": null}))).unwrap();
        assert_eq!((begun.limits[1].used, begun.limits[1].resets), (0.0, None));
        // The weekly window left out is left out.
        assert_eq!(parse(&answer(json!(null))).unwrap().limits.len(), 1);

        // Its use not a number, missing, below zero, or its reset not an
        // instant: the five-hour window alone would look like all of it.
        for broken in [
            json!({"utilization": "41.5", "resets_at": "2026-09-18T12:00:00Z"}),
            json!({"resets_at": "2026-09-18T12:00:00Z"}),
            json!({"utilization": -1.0, "resets_at": "2026-09-18T12:00:00Z"}),
            json!({"utilization": 41.5, "resets_at": "next Thursday"}),
            json!(41.5),
        ] {
            assert!(parse(&answer(broken.clone())).is_none(), "{broken}");
        }
    }

    #[test]
    fn only_what_has_a_use_of_its_own_is_a_window() {
        // Shaped as this Mac's answer was on 2026-09-27, its figures made up:
        // two windows given, with fields of their own left null; windows the
        // plan doesn't have; the week's use by model; a key named as what
        // Anthropic tries out, with a use and no reset; and what is said of
        // extra usage and spend, and the limits told another way.
        let window = |used: f64, resets: &str| {
            json!({"utilization": used, "resets_at": resets, "limit_dollars": null,
                   "used_dollars": null, "remaining_dollars": null, "locked_reason": null})
        };
        let mut given = json!({
            "five_hour": window(12.0, "2026-09-27T08:50:00.412337+00:00"),
            "seven_day": window(3.0, "2026-10-04T05:00:00.412360+00:00"),
            "seven_day_oauth_apps": null, "seven_day_opus": null, "seven_day_sonnet": null,
            "seven_day_cowork": null, "tangelo": null, "iguana_necktie": null,
            "nimbus_quill": {"utilization": 0.0, "resets_at": null, "limit_dollars": null,
                             "used_dollars": null, "remaining_dollars": null,
                             "locked_reason": null},
            "extra_usage": {"is_enabled": false, "monthly_limit": null, "used_credits": null,
                            "utilization": null, "user_disabled": true, "daily": null},
            "limits": [{"kind": "session", "group": "session", "percent": 12.0,
                        "resets_at": "2026-09-27T08:50:00.412337+00:00", "scope": null,
                        "is_active": true}],
            "spend": {"used": {"amount_minor": 0, "currency": "USD", "exponent": 2},
                      "limit": null, "enabled": false},
            "seven_day_breakdown": {"as_of": "2026-09-27T06:24:09.000000+00:00",
                                    "window_started_at": "2026-09-27T05:00:00.000000+00:00",
                                    "rows": [{"key": "claude_code", "display_name": "Claude Code",
                                              "percent": 2.0}]}
        });
        let keys = |given: &serde_json::Value| -> Vec<String> {
            parse(given)
                .unwrap()
                .limits
                .into_iter()
                .map(|limit| limit.key)
                .collect()
        };
        assert_eq!(keys(&given), ["five_hour", "seven_day"]);

        // A weekly window on use through other apps is neither a model's nor
        // all use. A week before its reset at noon on 18 September: noon on
        // the 11th.
        given["seven_day_oauth_apps"] = window(3.0, "2026-09-18T12:00:00Z");
        let limits = parse(&given).unwrap().limits;
        let apps = limits
            .iter()
            .find(|limit| limit.key == "seven_day_oauth_apps")
            .unwrap();
        assert_eq!(
            (
                apps.name.as_str(),
                apps.scope.as_deref(),
                apps.used,
                apps.starts
            ),
            (
                "Other apps weekly",
                None,
                3.0,
                Instant::parse("2026-09-11T12:00:00Z")
            )
        );
        // A model's weekly window that doesn't read is no answer, as any
        // window given that doesn't read is.
        given["seven_day_sonnet"] = json!({"utilization": "3%", "resets_at": null});
        assert!(parse(&given).is_none());
    }
}
