//! `turnscope`: the command line, and the MCP server agents ask (`mcp`).
//! See docs/cli.md and docs/mcp.md.
#![forbid(unsafe_code)]

mod mcp;

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{CommandFactory as _, Parser, Subcommand};
use serde_json::json;
use turnscope::{
    Error, Result, agents, catch_up, connect, db, ingest, limits, now, online, status, time, usage,
};

#[derive(Parser)]
#[command(
    name = "turnscope",
    version,
    about = "The limits and usage of the coding agents on this Mac",
    after_help = "Commands that print data take --json.\n\
        Exit codes: 0 done, 1 failed (the message says how to fix it), 2 bad usage, 3 what you named doesn't exist.",
    next_help_heading = "Global options"
)]
struct Cli {
    /// Where Turnscope keeps its data [default: ~/Library/Application Support/com.joeychilson.turnscope]
    #[arg(long, global = true, value_name = "DIR")]
    data: Option<PathBuf>,
    /// Whose agents' history and logins to read [default: $HOME]
    #[arg(long, global = true, value_name = "DIR")]
    home: Option<PathBuf>,
    /// Print JSON instead of text
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Every account's limits: what's left, when each resets, and where it's headed
    Status {
        /// Only the accounts this names: an id, or part of a title or label
        #[arg(long)]
        account: Option<String>,
        /// Hidden accounts too
        #[arg(long)]
        all: bool,
        /// Whether this many more hours of work fit the first account, at its work rate
        #[arg(long)]
        hours: Option<f64>,
    },
    /// What was used and what it cost, by day, week, month, model, project, agent, account or session
    Usage {
        /// From when [default: 30 days back]: today, yesterday, week (since Monday), month (since the 1st), 24h, 7d, 4w, a date, an RFC 3339 time, or now
        #[arg(long)]
        since: Option<String>,
        /// Until when [default: now], in the same forms
        #[arg(long)]
        until: Option<String>,
        /// How to group the rows [default: day, or session with --limit]
        #[arg(long, value_enum)]
        by: Option<usage::By>,
        /// Only this agent's: its id, as `connect` lists them, such as claude-code
        #[arg(long)]
        agent: Option<String>,
        /// Only this account's: an id, or part of a title or label
        #[arg(long)]
        account: Option<String>,
        /// What used this limit, such as week or "5 hours": since its window started, with each row's share of it
        #[arg(long, conflicts_with_all = ["until", "agent"])]
        limit: Option<String>,
        /// How many rows to show, 1 or more; the rest are added up
        #[arg(long, default_value_t = 20, value_parser = rows)]
        top: usize,
    },
    /// List accounts, or hide or show one
    Accounts {
        #[command(subcommand)]
        action: Option<AccountAction>,
    },
    /// See which agents run Turnscope's MCP server, or add it to one
    Connect {
        /// The agent's id, as `connect` lists them, such as claude-code
        agent: Option<String>,
    },
    /// Remove Turnscope's MCP server from an agent
    Disconnect {
        /// The agent's id, as `connect` lists them, such as claude-code
        agent: String,
    },
    /// What can't be read, and how to fix it
    Doctor,
    /// The MCP server on standard input and output, for a coding agent
    Mcp,
    /// For an agent's hook: exit 2 when your account's limit is under a percent left
    Guard {
        /// Part of the limit's name, such as week or "5 hours"
        #[arg(long)]
        limit: String,
        /// Block under this percent left
        #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
        below: u8,
        /// The account [default: the calling agent's]
        #[arg(long)]
        account: Option<String>,
        /// What to say when it blocks
        #[arg(long)]
        say: Option<String>,
    },
    /// One line for an agent's status line: your account's limits
    Statusline {
        /// The account [default: the calling agent's, or the first in use]
        #[arg(long)]
        account: Option<String>,
    },
}

#[derive(Subcommand)]
enum AccountAction {
    /// Leave an account out of lists and alerts
    Hide { account: String },
    /// Show a hidden account again
    Show { account: String },
}

fn main() -> ExitCode {
    db::started();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        // A hook reads a guard's exit code, and 2 blocks: a guard it can't
        // follow blocks nothing.
        Err(error) if error.use_stderr() && guarding() => {
            let _ = error.print();
            return ExitCode::FAILURE;
        }
        Err(error) => error.exit(),
    };
    let (home, data) = turnscope::places(cli.home, cli.data);
    let outcome = match cli.command {
        // The server reads what's new at each call, not before it starts.
        Command::Mcp => mcp::run(&home, &data).map(|()| String::new()),
        Command::Guard {
            limit,
            below,
            account,
            say,
        } => {
            return match guard(&home, &data, &limit, below, account.as_deref()) {
                Ok(None) => ExitCode::SUCCESS,
                Ok(Some(why)) => {
                    eprintln!("{}", say.unwrap_or(why));
                    ExitCode::from(2)
                }
                Err(error) => {
                    eprintln!("turnscope guard: {error}");
                    ExitCode::FAILURE
                }
            };
        }
        // A status line shows what's printed, so it never fails.
        Command::Statusline { account } => {
            if let Some(line) = statusline(&home, &data, account.as_deref()) {
                println!("{line}");
            }
            return ExitCode::SUCCESS;
        }
        // They change an agent's settings, not Turnscope's data, so they
        // never make the database or wait for it to be made.
        Command::Connect { agent: Some(id) } => change_agent(&id, true, cli.json, &home),
        Command::Disconnect { agent: id } => change_agent(&id, false, cli.json, &home),
        Command::Connect { agent: None } => connections(cli.json, &home, &data),
        command => answer(command, cli.json, &home, &data),
    };
    match outcome {
        Ok(out) => {
            if !out.is_empty() {
                println!("{out}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("turnscope: {error}");
            ExitCode::from(match error {
                Error::Usage(_) => 2,
                Error::NotFound(_) => 3,
                Error::Failed(_) | Error::Newer(_) => 1,
            })
        }
    }
}

/// Whether the command line asks for `guard`, by where it is among the
/// arguments: a value can be "guard" too, as in `--account guard`.
fn guarding() -> bool {
    Cli::command()
        .ignore_errors(true)
        .try_get_matches()
        .is_ok_and(|matches| matches.subcommand_name() == Some("guard"))
}

/// `value` with `--json`, or else `text`.
fn print(json: bool, value: serde_json::Value, text: String) -> String {
    match json {
        true => serde_json::to_string_pretty(&value).unwrap_or_default(),
        false => text,
    }
}

/// `connect <agent>`, or `disconnect <agent>`.
fn change_agent(id: &str, connecting: bool, json: bool, home: &Path) -> Result<String> {
    let agent = agents::find(id)?;
    let text = if connecting {
        connect::connect(agent, home, &std::env::current_exe()?)?;
        "now runs Turnscope's MCP server. Start a new session in it to use it."
    } else {
        connect::disconnect(agent, home)?;
        "no longer runs Turnscope's MCP server. Sessions already open keep it until they end."
    };
    let text = format!("{} {text}", agent.info().name);
    Ok(print(
        json,
        json!({ "agent": id, "connected": connecting }),
        text,
    ))
}

/// `connect` with no agent: which agents run Turnscope's MCP server.
fn connections(json: bool, home: &Path, data: &Path) -> Result<String> {
    let engine = std::env::current_exe()?;
    // Which agents are in use, from the database as it is: none before
    // it's made.
    let in_use = match db::read(data) {
        Some(db) => status::agents_in_use(&db, now())?,
        None => Vec::new(),
    };
    let agents = status::agents(home, &engine, |id| in_use.iter().any(|used| used == id));
    let lines: Vec<String> = agents
        .iter()
        .map(|agent| {
            let state = match agent.connection {
                _ if !agent.installed => "not installed".to_owned(),
                status::Connection::Connected => "connected".to_owned(),
                status::Connection::Outdated => {
                    format!(
                        "runs another copy of Turnscope: `turnscope connect {}` uses this one",
                        agent.id
                    )
                }
                status::Connection::Available => {
                    format!("not connected: `turnscope connect {}`", agent.id)
                }
            };
            format!("{:<12} {state}", agent.name)
        })
        .collect();
    Ok(print(json, json!(agents), lines.join("\n")))
}

/// Commands that report data catch up first; those that change a setting
/// don't wait for it.
fn answer(command: Command, json: bool, home: &Path, data: &Path) -> Result<String> {
    let mut db = db::open(data)?;
    let reports = match &command {
        Command::Accounts { action } => action.is_none(),
        _ => true,
    };
    if reports {
        if let Some(wait) = ingest::waits(&db) {
            eprintln!("{wait}");
        }
        if let Err(error) = catch_up(&mut db, home, None) {
            eprintln!(
                "turnscope: what's new couldn't be read first, so this is as of the last read: {error}"
            );
        }
    }
    let now = now();
    let time_of = |text: &Option<String>| {
        text.as_deref()
            .map(|text| time::parse(text, now))
            .transpose()
    };
    Ok(match command {
        Command::Status {
            account,
            all,
            hours,
        } => {
            let status = status::status(&db, now)?;
            let shown: Vec<&status::Account> = match &account {
                Some(name) => status::named(&status, name)?,
                None => status
                    .accounts
                    .iter()
                    .filter(|account| all || !account.hidden)
                    .collect(),
            };
            let fit = match (hours, shown.first()) {
                (Some(hours), Some(first)) => Some(status::fit(first, hours, status.at)?),
                _ => None,
            };
            let text = match shown.is_empty() {
                true => status::none(&status, "`turnscope status --all` shows them"),
                false => status::text(&status, &shown, None),
            };
            let text = match &fit {
                Some(fit) => format!("{fit}\n\n{text}"),
                None => text,
            };
            print(json, json!({ "accounts": shown, "fit": fit }), text)
        }
        Command::Usage {
            since: from,
            by,
            account,
            limit: Some(name),
            top,
            ..
        } => {
            let status = status::status(&db, now)?;
            let (account, limit) = limit_of(&db, home, &status, account.as_deref(), &name)?;
            let by = by.unwrap_or(usage::By::Session);
            let since = time_of(&from)?;
            if since.is_some_and(|since| since >= now) {
                return Err(Error::Usage(
                    "--since is now or later, so there's no usage after it: give an earlier --since"
                        .to_owned(),
                ));
            }
            let (used, shares) = usage::breakdown(
                &db,
                account,
                Some(limit),
                since.map(|since| (since, now)),
                by,
                top,
                now,
            )?;
            let heading = format!(
                "{} · {}\n",
                account.full_title(),
                status::limit_text(limit, status.at)
            );
            let mut text =
                heading + &usage::text(&used, shares.as_ref().map(|shares| (limit, shares)));
            if let Some(why) = usage::unshared(&used, limit).filter(|_| since.is_none()) {
                text += &format!("\n{why}");
            }
            let value = json!({
                "account": account.id,
                "limit": limit.key,
                "usage": used,
                "sharePercents": shares.as_ref().map(|shares| &shares.rows),
                "restSharePercent": shares.as_ref().and_then(|shares| shares.rest),
            });
            print(json, value, text)
        }
        Command::Usage {
            since: from,
            until,
            by,
            agent,
            account,
            limit: None,
            top,
        } => {
            // Refused, as an unknown agent would show nothing used.
            if let Some(agent) = &agent {
                agents::find(agent)?;
            }
            let account = match account {
                Some(name) => Some(
                    status::one_account(&status::status(&db, now)?, &name)?
                        .id
                        .clone(),
                ),
                None => None,
            };
            let since = time_of(&from)?.unwrap_or(now - 30 * time::DAY);
            let until_given = time_of(&until)?;
            let until = until_given.unwrap_or(now);
            if since >= until {
                return Err(Error::Usage(
                    match until_given {
                        Some(_) => "--since is at or after --until, so nothing is between them: give an earlier --since",
                        None => "--since is now or later, so there's no usage after it: give an earlier --since",
                    }
                    .to_owned(),
                ));
            }
            let query = usage::Query {
                since,
                until,
                by: by.unwrap_or(usage::By::Day),
                agent,
                account,
                models: Vec::new(),
                key: None,
                top,
            };
            let usage = usage::usage(&db, &query)?;
            print(json, json!(usage), usage::text(&usage, None))
        }
        Command::Accounts { action } => {
            let mut status = status::status(&db, now)?;
            if let Some(AccountAction::Hide { account } | AccountAction::Show { account }) = &action
            {
                let id = status::one_account(&status, account)?.id.clone();
                let hidden = matches!(action, Some(AccountAction::Hide { .. }));
                status::hide(&db, &id, hidden)?;
                if let Some(found) = status.accounts.iter_mut().find(|found| found.id == id) {
                    found.hidden = hidden;
                }
            }
            let (lines, listed): (Vec<String>, Vec<_>) = status
                .accounts
                .iter()
                .map(|account| {
                    let hidden = if account.hidden { " · hidden" } else { "" };
                    let line = format!("{} · {}{hidden}", account.full_title(), account.id);
                    let value = json!({
                        "id": account.id,
                        "title": account.title,
                        "label": account.label,
                        "kind": account.kind,
                        "hidden": account.hidden,
                    });
                    (line, value)
                })
                .unzip();
            let text = match lines.is_empty() {
                true => status::none(&status, "`turnscope status --all` shows them"),
                false => lines.join("\n"),
            };
            print(json, json!(listed), text)
        }
        Command::Doctor => {
            let notes = doctor(&db, data, home, now)?;
            let text = if notes.is_empty() {
                "Nothing to fix.".to_owned()
            } else {
                notes.join("\n")
            };
            print(json, json!(notes), text)
        }
        Command::Mcp
        | Command::Guard { .. }
        | Command::Statusline { .. }
        | Command::Connect { .. }
        | Command::Disconnect { .. } => {
            unreachable!("served before the database is opened")
        }
    })
}

/// `--top`'s rows: 0 would show only the row adding up the rest.
fn rows(text: &str) -> std::result::Result<usize, String> {
    match text.parse() {
        Ok(0) | Err(_) => Err("give a number of rows, 1 or more".to_owned()),
        Ok(rows) => Ok(rows),
    }
}

fn limit_of<'a>(
    db: &rusqlite::Connection,
    home: &Path,
    status: &'a status::Status,
    account: Option<&str>,
    name: &str,
) -> Result<(&'a status::Account, &'a status::Limit)> {
    let has =
        |account: &&status::Account| account.limits.iter().any(|limit| limit.answers_to(name));
    let account = match account {
        Some(account) => status::one_account(status, account)?,
        None => mcp::Caller::detect()
            .account(db, home, status)?
            .filter(has)
            .or_else(|| {
                status
                    .accounts
                    .iter()
                    .filter(|account| !account.hidden)
                    .find(has)
            })
            .ok_or_else(|| Error::NotFound(format!("no account has a limit named {name}")))?,
    };
    Ok((account, account.limit(name)?))
}

fn doctor(db: &rusqlite::Connection, data: &Path, home: &Path, now: i64) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    for agent in agents::ALL {
        for folder in agent.folders(home).iter().filter(|folder| folder.exists()) {
            if let Err(error) = agent.logins(folder, home) {
                notes.push(format!(
                    "{}'s logins in {} can't be read: {error}",
                    agent.info().name,
                    folder.display()
                ));
            }
        }
    }
    let status = status::status(db, now)?;
    // An account no agent here is signed in to has nothing to fix.
    for account in &status.accounts {
        if account.state.why() != Some(status::Stale::SignedOut)
            && let Some(stale) = status::stale(account, false, status.at)
        {
            notes.push(format!("{}: {stale}.", account.full_title()));
        }
    }
    let mut failed =
        db.prepare("SELECT path, failed FROM file WHERE failed IS NOT NULL ORDER BY path")?;
    let rows = failed.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    // One deleted isn't tried again; one in a folder that can't be looked
    // into may still be there. Before the first ten, so deleted ones never
    // hide one that's still failing.
    let failing = rows.filter(|row| {
        row.as_ref().map_or(true, |(path, _)| {
            Path::new(path).try_exists().unwrap_or(true)
        })
    });
    for row in failing.take(10) {
        let (path, why) = row?;
        // A reader's error often names its file already.
        let why = if why.contains(&path) {
            why
        } else {
            format!("{path}: {why}")
        };
        notes.push(format!(
            "{why}. It's tried again each time history is read."
        ));
    }
    let mut skipped = db.prepare(
        "SELECT path, skipped FROM file WHERE skipped > 0 ORDER BY skipped DESC LIMIT 10",
    )?;
    for row in skipped.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })? {
        let (path, skipped) = row?;
        let lines = if skipped == 1 { "line" } else { "lines" };
        notes.push(format!(
            "{path}: {skipped} {lines} couldn't be read; their agent may write a newer format."
        ));
    }
    // Before any price is read, every model is unpriced: one note says so.
    let priced: bool = db.query_row("SELECT EXISTS (SELECT 1 FROM price)", [], |row| row.get(0))?;
    if !priced {
        notes.push(
            "Prices haven't been read from models.dev yet: costs are unknown until they are."
                .to_owned(),
        );
    }
    let unpriced: Vec<(String, i64)> = db
        .prepare("SELECT model, count(*) FROM response WHERE cost IS NULL GROUP BY model ORDER BY count(*) DESC LIMIT 5")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (model, count) in unpriced.into_iter().filter(|_| priced) {
        let model = if model.is_empty() {
            "no model named".to_owned()
        } else {
            model
        };
        let responses = if count == 1 { "response" } else { "responses" };
        notes.push(format!(
            "{model}: {} {responses} with no known price; models.dev doesn't list it.",
            usage::count(count as u64)
        ));
    }
    // What went wrong in the last day, the latest five: the app is started
    // from its icon, and limits are read in agents' hooks, where no one sees
    // their errors.
    let log = std::fs::read_to_string(data.join("app.log")).unwrap_or_default();
    let logged: Vec<&str> = log
        .lines()
        .filter(|line| {
            line.split_once(' ')
                .and_then(|(at, _)| at.parse::<jiff::Timestamp>().ok())
                .is_some_and(|at| at.as_millisecond() > now - time::DAY)
        })
        .collect();
    for line in &logged[logged.len().saturating_sub(5)..] {
        notes.push(format!("Logged: {line}"));
    }
    Ok(notes)
}

/// `None` when there's room, or the standing isn't known.
///
/// A hook waits for it, so it reads no history and no prices: only limits,
/// when five minutes have passed since any process read them. While the
/// app runs, it has always read them more recently.
///
/// Nor is it what makes the database, or takes in 0.2's: that took 6.9 s
/// for a made-up one of 240 MB, while every other process waited. Before
/// another process has made it, or after an update until the new version
/// has opened it, the standing isn't known, as for a status line.
fn guard(
    home: &Path,
    data: &Path,
    limit: &str,
    below: u8,
    account: Option<&str>,
) -> Result<Option<String>> {
    if db::read(data).is_none() {
        return Ok(None);
    }
    let mut db = db::open(data)?;
    // The limits read before still guard when this one can't read them
    // again, as once a newer Turnscope has written here, or does while they
    // are read: a hook that fails guards nothing.
    if online() && db::writing(&db).is_ok() {
        let _ = limits::refresh(&mut db, home);
    }
    let now = now();
    let status = status::status(&db, now)?;
    let account = match account {
        Some(name) => status::one_account(&status, name)?,
        None => match mcp::Caller::detect().account(&db, home, &status)? {
            Some(yours) => yours,
            None => return Ok(None),
        },
    };
    let found = account.limit(limit)?;
    if found.left_percent >= below {
        return Ok(None);
    }
    // As a sentence names it: "weekly limit", "5-hour limit", "Opus weekly
    // limit".
    let called = found.lowercase("-hour");
    let called = match called.ends_with("limit") {
        true => format!("{}'s {called}", account.title),
        false => format!("{}'s {called} limit", account.title),
    };
    let ahead = |at: i64| time::ahead(at, now);
    Ok(Some(match &found.outlook {
        status::Outlook::UsedUp { back } => format!(
            "{called} is used up{}.",
            back.map_or(String::new(), |back| format!(
                "; it's back {}",
                ahead(back.at)
            ))
        ),
        outlook => {
            let mut said = format!("{called} has {}% left, under {below}%.", found.left_percent);
            if let Some(resets) = found.resets() {
                said += &format!(" It resets {}", ahead(resets));
                if let status::Outlook::RunsOut { likely, .. } = outlook {
                    said += &format!("; at this pace it runs out {}", ahead(likely.at));
                }
                said += ".";
            }
            said
        }
    }))
}

/// From the database alone, so it's quick.
fn statusline(home: &Path, data: &Path, account: Option<&str>) -> Option<String> {
    let db = db::read(data)?;
    let status = status::status(&db, now()).ok()?;
    let account = match account {
        // A line is better than none: of several named, the first.
        Some(name) => *status::named(&status, name).ok()?.first()?,
        None => harness(&db)
            .and_then(|id| status.accounts.iter().find(|account| account.id == id))
            .or_else(|| mcp::Caller::detect().account(&db, home, &status).ok()?)
            .or_else(|| {
                status
                    .accounts
                    .iter()
                    .find(|account| account.in_use && !account.hidden)
            })?,
    };
    Some(status::line(account, status.at))
}

/// The account of the session a harness names by `session_id` in the JSON
/// it writes to a status line's standard input, as Claude Code does. None
/// for a session not read yet, as a status line never catches up.
///
/// The calling agent's account answers then, over the account of the latest
/// response in the session's folder, which can be another agent's: after
/// Codex ran last in a folder, a new Claude Code session showed the Codex
/// account's limits.
fn harness(db: &rusqlite::Connection) -> Option<String> {
    use std::io::IsTerminal as _;
    if std::io::stdin().is_terminal() {
        return None;
    }
    // A harness writes it at once; one that writes nothing isn't waited on.
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::stdin().take(1 << 20).read_to_string(&mut text);
        let _ = send.send(text);
    });
    let text = receive.recv_timeout(Duration::from_millis(300)).ok()?;
    let given: serde_json::Value = serde_json::from_str(&text).ok()?;
    // The agent names its session by its own id, whichever agent's it is.
    let native = given["session_id"].as_str()?;
    agents::ALL.iter().find_map(|agent| {
        let session = agent.info().session(native);
        usage::session_account(db, &session).ok().flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Eleven files that can't be read, one more than doctor names.
    #[test]
    fn doctor_names_a_file_that_cant_be_read_until_its_deleted() {
        use std::os::unix::fs::PermissionsExt as _;
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let folder = home.path().join(".claude/projects/-work-a");
        std::fs::create_dir_all(&folder).unwrap();
        let locked: Vec<PathBuf> = (0..11)
            .map(|n| folder.join(format!("s{n:02}.jsonl")))
            .collect();
        for file in &locked {
            std::fs::write(file, "{}\n").unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        let mut db = db::open(data.path()).unwrap();
        let mut notes = || {
            ingest::ingest(&mut db, home.path(), None).unwrap();
            doctor(&db, data.path(), home.path(), now())
                .unwrap()
                .join("\n")
        };
        assert!(notes().contains("s00.jsonl"));
        // The ten it named are deleted, and don't hide the last.
        for file in &locked[..10] {
            std::fs::remove_file(file).unwrap();
        }
        let named = notes();
        assert!(
            named.contains("s10.jsonl") && !named.contains("s00.jsonl"),
            "{named}"
        );
        std::fs::remove_file(&locked[10]).unwrap();
        assert!(!notes().contains("s10.jsonl"));
    }
}
