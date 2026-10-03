//! `get_session`: where a session stopped, to pick up its work.
//!
//! **Which session.** One named by its id; the latest in a folder, passing
//! over the caller's own, since an agent asking for the latest session where
//! it works wants the work before its own; or, given neither, the caller's
//! own ([`crate::sessions::this`]), for what it has taken so far.
//!
//! The handoff is the engine's ([`turnscope_engine::Handoff`]): the first
//! and last requests, the last reply, the plan and how far it got, the files
//! changed and the commands run, read from the agent's own records. Beside
//! it are what the session took of each limit, its subagents, and the
//! command that resumes it. A session whose files are gone still has its
//! usage and its shares of limits; its handoff says it can't be read.
//!
//! Every part is bounded to fit what an agent takes from a tool: quotes are
//! cut short with where their rest is read ([`crate::read::clipped`]); the
//! plan, the files (the most changed first), the commands the engine keeps
//! and the subagents are each given whole up to their bounds, a line an
//! item, with how many more there are; and of the windows the session drew
//! on, the sentences say the latest of each limit, the figures every one.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};
use turnscope_engine::{
    AccountLimits, Error, FileChange, Filter, Handoff, Instant, LimitShare, SessionQuery,
    SessionRow, StepStatus,
};

use crate::accounts;
use crate::limits;
use crate::prose;
use crate::read::{self, Part};
use crate::sessions;
use crate::tools::{self, Answer, Failure, Reply, Server, object, rounded, shape};
use crate::usage;

/// The most characters of a request a handoff quotes.
const LONGEST_REQUEST: usize = 400;

/// The most characters of the last reply a handoff quotes.
const LONGEST_REPLY: usize = 800;

/// The most characters of a command a handoff gives.
const LONGEST_COMMAND: usize = 300;

/// The most files a handoff lists.
const FILES_GIVEN: usize = 100;

/// The most characters of a plan's step a handoff gives.
const LONGEST_STEP: usize = 200;

/// The most subagents a handoff lists.
const MOST_SUBAGENTS: usize = 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetSession {
    session: Option<String>,
    latest_in: Option<String>,
}

pub(crate) fn schema() -> Value {
    object(
        json!({
            "session": tools::session_schema(),
            "latest_in": {
                "type": "string",
                "description": "A folder, absolute or from ~/: the latest session there from any agent, other than this one.",
            },
        }),
        &[],
    )
}

pub(crate) fn output_schema() -> Value {
    let quote = shape(
        json!({"entry": {"type": "integer"}, "at": {"type": ["string", "null"]}, "text": {"type": "string"}}),
        &["entry", "text"],
    );
    let nullable_quote = json!({"anyOf": [quote, {"type": "null"}]});
    shape(
        json!({
            "session": sessions::figures_schema(),
            "first_request": nullable_quote,
            "last_request": nullable_quote,
            "last_reply": nullable_quote,
            "plan": {"type": "array", "items": shape(json!({
                "step": {"type": "string"},
                "status": {"type": ["string", "null"], "enum": ["pending", "in_progress", "completed", "cancelled", null]},
            }), &["step", "status"])},
            "goal": {"anyOf": [shape(json!({"objective": {"type": "string"}, "status": {"type": ["string", "null"]}}), &["objective"]), {"type": "null"}]},
            "files": {"type": "array", "items": shape(json!({
                "path": {"type": "string"},
                "added": {"type": ["integer", "null"]},
                "removed": {"type": ["integer", "null"]},
                "created": {"type": "boolean"},
                "deleted": {"type": "boolean"},
                "moved_to": {"type": ["string", "null"]},
                "changes": {"type": "integer"},
            }), &["path", "added", "removed"])},
            "files_left_out": {"type": "integer"},
            "commands": {"type": "array", "items": shape(json!({
                "command": {"type": "string"},
                "at": {"type": ["string", "null"]},
                "exit": {"type": ["integer", "null"]},
                "failed": {"type": ["boolean", "null"]},
            }), &["command", "failed"])},
            "commands_run": {"type": "integer"},
            "commands_failed": {"type": "integer"},
            "unclear": {"type": "array", "items": shape(json!({"tool": {"type": "string"}, "calls": {"type": "integer"}}), &["tool", "calls"])},
            "transcript_unavailable": {"type": "string"},
            "subagents": {"type": "array", "items": shape(json!({
                "id": {"type": "string"},
                "title": {"type": ["string", "null"]},
                "models": {"type": "array", "items": {"type": "string"}},
                "usage": usage::totals_schema(),
            }), &["id", "usage"])},
            "limits": {"type": "array", "items": shape(json!({
                "account": {"type": "string"},
                "limit": {"type": "string"},
                "resets_at": {"type": ["string", "null"]},
                "share_percent": {"type": "number"},
                "whole": {"type": "boolean"},
            }), &["account", "limit", "share_percent"])},
            "resume": {"type": ["string", "null"]},
        }),
        &["session", "files", "commands", "limits", "resume"],
    )
}

pub(crate) fn get(server: &Server, arguments: GetSession) -> Answer {
    let accounts = server.engine.limits()?;
    let row = match (arguments.session.as_deref(), arguments.latest_in.as_deref()) {
        (Some(id), None) => sessions::named(server, id)?,
        (None, Some(folder)) => {
            let this = accounts::asking(server, &accounts)?.0;
            sessions::latest_in(server, folder, this.as_ref().map(|row| &row.key))?.ok_or_else(
                || {
                    Failure(format!(
                        "No session other than this one ran in {folder}, or in a project inside \
                         it, by any agent."
                    ))
                },
            )?
        }
        (None, None) => accounts::asking(server, &accounts)?.0.ok_or_else(|| {
            Failure(
                "Which session is this one isn't known, as Turnscope can't tell which agent \
                 started it or where it works. Give session, an id from find_sessions, or \
                 latest_in, a folder."
                    .into(),
            )
        })?,
        (Some(_), Some(_)) => {
            return Err(Failure(
                "get_session takes session, an id, or latest_in, a folder, not both; or \
                 neither, for this session."
                    .into(),
            ));
        }
    };
    let handoff = match server.engine.handoff(&row.key) {
        Ok(handoff) => Ok(handoff),
        Err(Error::Gone(_)) => Err(
            "Its agent's files for it are gone, so what it said and did can't be read; its \
             usage is kept."
                .to_owned(),
        ),
        Err(error) => Err(format!("Its files could not be read: {error}")),
    };
    let breakdown = server.engine.session_usage_without_prompts(&row.key)?;
    let subagents = server
        .engine
        .sessions(&SessionQuery {
            filter: Filter::default(),
            subagents_of: Some(row.key.clone()),
            limit: MOST_SUBAGENTS,
            ..SessionQuery::default()
        })?
        .items;

    let mut said = vec![heading(server, &row)];
    let mut data = json!({
        "session": sessions::figures(server, &row, &accounts),
        "first_request": Value::Null,
        "last_request": Value::Null,
        "last_reply": Value::Null,
        "plan": [],
        "goal": Value::Null,
        "files": [],
        "commands": [],
        "limits": [],
        "resume": row.resume(),
    });
    match &handoff {
        Ok(handoff) => told(server, &row, handoff, &mut said, &mut data),
        Err(why) => {
            said.push(why.clone());
            data["transcript_unavailable"] = json!(why);
        }
    }
    if !subagents.is_empty() {
        let mut lines = vec![format!(
            "{}:",
            prose::capitalized(&prose::count(u64::from(row.subagents), "subagent"))
        )];
        lines.extend(subagents.iter().map(|subagent| {
            let model = subagent
                .models
                .first()
                .map_or_else(String::new, |model| format!(" \u{b7} {model}"));
            format!(
                "- {}{model} \u{b7} {} tokens",
                sessions::called(subagent),
                prose::tokens(subagent.with_subagents.tokens.total())
            )
        }));
        match (row.subagents as usize).saturating_sub(subagents.len()) {
            0 => {}
            more => lines.push(format!("- and {more} more")),
        }
        said.push(lines.join("\n"));
        data["subagents"] = subagents
            .iter()
            .map(|subagent| {
                json!({
                    "id": subagent.key.to_string(),
                    "title": subagent.title,
                    "models": subagent.models.iter().map(|model| model.as_str()).collect::<Vec<_>>(),
                    "usage": usage::totals(&subagent.with_subagents),
                })
            })
            .collect();
    }
    if let Some(breakdown) = &breakdown {
        let (sentence, figures) = took(server, &breakdown.limits, &accounts);
        if let Some(sentence) = sentence {
            said.push(sentence);
        }
        data["limits"] = figures;
    }
    let used = row.with_subagents.tokens.total();
    if used > 0 {
        said.push(format!(
            "In all it used {} tokens, {}.",
            prose::tokens(used),
            usage::cost(&row.with_subagents)
        ));
    }
    match row.resume() {
        Some(command) => said.push(format!("Resume it: {command}")),
        None => said.push("It's a subagent, which no one resumes.".to_owned()),
    }
    Ok(Reply::Answer {
        said: said.join("\n"),
        data,
    })
}

/// The first line of a handoff: the session, its agent, where and when.
fn heading(server: &Server, row: &SessionRow) -> String {
    let mut parts = vec![sessions::called(row), row.key.agent().name().to_owned()];
    if let Some(place) = sessions::place(server, row) {
        parts.push(place);
    }
    let now = Instant::now();
    if sessions::running(row) {
        parts.push("running now".to_owned());
    } else if let Some(active) = row.active {
        parts.push(format!(
            "ended {} ago",
            prose::span(now.millis() - active.millis())
        ));
    }
    format!("{}\n", parts.join(" \u{b7} "))
}

/// What the handoff tells, into the sentences `said` and the figures `data`.
fn told(
    server: &Server,
    row: &SessionRow,
    handoff: &Handoff,
    said: &mut Vec<String>,
    data: &mut Value,
) {
    for (name, key, quoted, most) in [
        (
            "First asked",
            "first_request",
            &handoff.first_request,
            LONGEST_REQUEST,
        ),
        (
            "Last asked",
            "last_request",
            &handoff.last_request,
            LONGEST_REQUEST,
        ),
        (
            "Last reply",
            "last_reply",
            &handoff.last_reply,
            LONGEST_REPLY,
        ),
    ] {
        // The first request said once when it was the last too.
        if key == "last_request"
            && handoff.last_request.as_ref().map(|quote| quote.entry)
                == handoff.first_request.as_ref().map(|quote| quote.entry)
        {
            data[key] = data["first_request"].clone();
            continue;
        }
        if let Some(quote) = quoted {
            let text = read::clipped(&quote.text, most, quote.entry, Part::Text);
            said.push(format!("{name}: \"{text}\""));
            data[key] = json!({
                "entry": quote.entry,
                "at": quote.at.map(|at| server.time(at)),
                "text": text,
            });
        }
    }
    if let Some(goal) = &handoff.goal {
        let status = goal
            .status
            .as_deref()
            .map_or_else(String::new, |status| format!(" ({status})"));
        said.push(format!(
            "Goal{status}: {}",
            prose::line(&goal.objective, 300)
        ));
        data["goal"] = json!({"objective": goal.objective, "status": goal.status});
    }
    if !handoff.plan.is_empty() {
        let done = handoff
            .plan
            .iter()
            .filter(|step| step.status == Some(StepStatus::Completed))
            .count();
        let mut lines = vec![format!("Plan, {done} of {} done:", handoff.plan.len())];
        lines.extend(handoff.plan.iter().map(|step| {
            let status = match step.status {
                Some(StepStatus::Completed) => "done",
                Some(StepStatus::InProgress) => "under way",
                Some(StepStatus::Pending) => "left",
                Some(StepStatus::Cancelled) => "given up",
                None => "status not understood",
            };
            format!("- {status}: {}", prose::line(&step.text, LONGEST_STEP))
        }));
        said.push(lines.join("\n"));
        data["plan"] = handoff
            .plan
            .iter()
            .map(|step| json!({"step": step.text, "status": step.status.map(StepStatus::key)}))
            .collect();
    }
    let folder = row.cwd.as_deref();
    if !handoff.files.is_empty() {
        // The most changed first, those whose lines aren't known after.
        let mut ordered: Vec<&FileChange> = handoff.files.iter().collect();
        ordered.sort_by_key(|file| {
            std::cmp::Reverse(
                file.added
                    .unwrap_or(0)
                    .saturating_add(file.removed.unwrap_or(0)),
            )
        });
        let more = handoff.files.len().saturating_sub(FILES_GIVEN) as u64 + handoff.files_left_out;
        let mut lines = vec![format!(
            "Changed {}:",
            prose::count(handoff.files.len() as u64 + handoff.files_left_out, "file")
        )];
        lines.extend(
            ordered
                .into_iter()
                .take(FILES_GIVEN)
                .map(|file| format!("- {}", changed(file, folder))),
        );
        if more > 0 {
            lines.push(format!("- and {more} more"));
        }
        said.push(lines.join("\n"));
    } else {
        said.push("Changed: no file edits recorded.".to_owned());
    }
    data["files"] = handoff
        .files
        .iter()
        .take(FILES_GIVEN)
        .map(|file| {
            json!({
                "path": relative(&file.path, folder),
                "added": file.added,
                "removed": file.removed,
                "created": file.created,
                "deleted": file.deleted,
                "moved_to": file.moved_to.as_deref().map(|to| relative(to, folder)),
                "changes": file.changes,
            })
        })
        .collect();
    data["files_left_out"] =
        json!(handoff.files.len().saturating_sub(FILES_GIVEN) as u64 + handoff.files_left_out);
    if handoff.commands_run > 0 {
        let kept = handoff.commands.len() as u64;
        let mut lines = vec![if kept < handoff.commands_run {
            format!(
                "Ran {}, {} failed; the last {kept}, oldest first:",
                prose::count(handoff.commands_run, "command"),
                handoff.commands_failed,
            )
        } else {
            format!(
                "Ran {}, {} failed:",
                prose::count(handoff.commands_run, "command"),
                handoff.commands_failed,
            )
        }];
        lines.extend(handoff.commands.iter().map(|command| {
            let outcome = match (command.failed, command.exit) {
                (Some(true), Some(exit)) => format!(" (failed, exit {exit})"),
                (Some(true), None) => " (failed)".to_owned(),
                (None, _) => " (outcome not recorded)".to_owned(),
                (Some(false), _) => String::new(),
            };
            format!(
                "- {}{outcome}",
                prose::line(&command.command, LONGEST_COMMAND)
            )
        }));
        said.push(lines.join("\n"));
    }
    data["commands"] = handoff
        .commands
        .iter()
        .map(|command| {
            json!({
                "command": prose::line(&command.command, LONGEST_COMMAND),
                "at": command.at.map(|at| server.time(at)),
                "exit": command.exit,
                "failed": command.failed,
            })
        })
        .collect();
    data["commands_run"] = json!(handoff.commands_run);
    data["commands_failed"] = json!(handoff.commands_failed);
    if !handoff.unclear.is_empty() {
        let calls: Vec<String> = handoff
            .unclear
            .iter()
            .map(|(tool, calls)| format!("{calls} {tool}"))
            .collect();
        said.push(format!(
            "Not understood, so missing above: {} calls.",
            prose::list(&calls)
        ));
        data["unclear"] = handoff
            .unclear
            .iter()
            .map(|(tool, calls)| json!({"tool": tool, "calls": calls}))
            .collect();
    }
}

/// A file changed, as a sentence says it: `src/lib.rs +12 −3`, marked new,
/// deleted or moved, its lines `?` where unknown.
fn changed(file: &FileChange, folder: Option<&str>) -> String {
    let count =
        |count: Option<u64>| count.map_or_else(|| "?".to_owned(), |count| count.to_string());
    let mut said = relative(&file.path, folder);
    if file.deleted {
        said.push_str(" (deleted)");
    } else {
        said.push_str(&format!(
            " +{} \u{2212}{}",
            count(file.added),
            count(file.removed)
        ));
        if file.created {
            said.push_str(" (new)");
        }
    }
    if let Some(to) = &file.moved_to {
        said.push_str(&format!(" \u{2192} {}", relative(to, folder)));
    }
    said
}

/// `path` from `folder` where it lies inside it, and as it is otherwise.
fn relative(path: &str, folder: Option<&str>) -> String {
    folder
        .and_then(|folder| path.strip_prefix(folder.trim_end_matches('/')))
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|rest| !rest.is_empty())
        .map_or_else(|| path.to_owned(), str::to_owned)
}

/// What a session took of each limit, as a sentence says it, and as
/// figures: of each limit, the latest window it drew on in the sentence,
/// and every window in the figures.
fn took(
    server: &Server,
    shares: &[LimitShare],
    accounts: &[AccountLimits],
) -> (Option<String>, Value) {
    let figures: Value = shares
        .iter()
        .map(|share| {
            json!({
                "account": share.account,
                "limit": limits::named_as(&share.name, share.scope.as_deref()),
                "resets_at": share.resets.map(|at| server.time(at)),
                "share_percent": rounded(share.share, 2),
                "whole": share.whole,
            })
        })
        .collect();
    // The latest window of each limit, in the order of their accounts and
    // keys, so that limits of an equal share are always said alike.
    let mut latest: BTreeMap<(&str, &str), &LimitShare> = BTreeMap::new();
    for share in shares {
        let place = latest.entry((&share.account, &share.key)).or_insert(share);
        if share.through > place.through {
            *place = share;
        }
    }
    let mut latest: Vec<&LimitShare> = latest.into_values().collect();
    latest.sort_by(|a, b| b.share.total_cmp(&a.share));
    let parts: Vec<String> = latest
        .iter()
        .filter(|share| share.share >= 0.05)
        .map(|share| {
            let account =
                accounts::called(&share.account, accounts).unwrap_or_else(|| share.account.clone());
            let when = share.resets.map_or_else(String::new, |at| {
                format!(" (the window resetting {})", server.clock(at))
            });
            let least = if share.whole { "" } else { "at least " };
            format!(
                "{least}{} of {account}'s {}{when}",
                prose::share(share.share),
                limits::spoken_as(&share.name, share.scope.as_deref())
            )
        })
        .collect();
    let sentence = (!parts.is_empty()).then(|| format!("It took {}.", prose::list(&parts)));
    (sentence, figures)
}

#[cfg(test)]
mod tests {
    use turnscope_engine::FileChange;

    use super::{changed, relative};

    #[test]
    fn a_file_is_said_from_the_session_folder_with_its_lines() {
        let file = FileChange {
            path: "/work/atlas/src/hillshade.rs".into(),
            added: Some(212),
            removed: Some(40),
            created: false,
            deleted: false,
            moved_to: None,
            changes: 3,
        };
        assert_eq!(
            changed(&file, Some("/work/atlas")),
            "src/hillshade.rs +212 \u{2212}40"
        );
        let unknown = FileChange {
            removed: None,
            created: true,
            ..file.clone()
        };
        assert_eq!(
            changed(&unknown, Some("/work/atlas/")),
            "src/hillshade.rs +212 \u{2212}? (new)"
        );
        assert_eq!(
            relative("/elsewhere/a.rs", Some("/work/atlas")),
            "/elsewhere/a.rs"
        );
        assert_eq!(
            relative("/work/atlasx/a.rs", Some("/work/atlas")),
            "/work/atlasx/a.rs"
        );
    }
}
