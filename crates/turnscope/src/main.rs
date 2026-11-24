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
        /// Whether this many more hours of work fit each account shown, at its work rate
        #[arg(long, value_parser = hours)]
        hours: Option<f64>,
    },
    /// What was used and what it cost, by day, week, month, model, project, agent, account or session
    Usage(UsageArgs),
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

#[derive(clap::Args)]
struct UsageArgs {
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
        Command::Status {
            account,
            all,
            hours,
        } => caught_up(&home, &data)
            .and_then(|db| cmd_status(&db, now(), cli.json, account.as_deref(), all, hours)),
        Command::Usage(mut args) => {
            caught_up(&home, &data).and_then(|db| match args.limit.take() {
                Some(name) => cmd_usage_limit(&db, &home, now(), cli.json, args, &name),
                None => cmd_usage(&db, now(), cli.json, args),
            })
        }
        // Hiding or showing one doesn't wait for what's new to be read.
        Command::Accounts { action: None } => {
            caught_up(&home, &data).and_then(|db| cmd_accounts(&db, now(), cli.json, None))
        }
        Command::Accounts { action } => {
            db::open(&data).and_then(|db| cmd_accounts(&db, now(), cli.json, action))
        }
        Command::Doctor => caught_up(&home, &data).and_then(|db| {
            let notes = turnscope::doctor(&db, &data, &home, now())?;
            Ok(print(
                cli.json,
                || json!(notes),
                || match notes.is_empty() {
                    true => "Nothing to fix.".to_owned(),
                    false => notes.join("\n"),
                },
            ))
        }),
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
                Error::Failed(_) | Error::Newer { .. } | Error::Busy(_) => 1,
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

fn print(
    json: bool,
    value: impl FnOnce() -> serde_json::Value,
    text: impl FnOnce() -> String,
) -> String {
    match json {
        true => serde_json::to_string_pretty(&value()).unwrap_or_default(),
        false => text(),
    }
}

/// `connect <agent>`, or `disconnect <agent>`.
fn change_agent(id: &str, connecting: bool, json: bool, home: &Path) -> Result<String> {
    let agent = agents::find(id)?;
    let verb = if connecting {
        connect::connect(agent, home, &std::env::current_exe()?)?;
        "now runs Turnscope's MCP server. Start a new session in it to use it."
    } else {
        connect::disconnect(agent, home)?;
        "no longer runs Turnscope's MCP server. Sessions already open keep it until they end."
    };
    Ok(print(
        json,
        || json!({ "agent": id, "connected": connecting }),
        || format!("{} {verb}", agent.info().name),
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
    Ok(print(
        json,
        || json!(agents),
        || {
            agents
                .iter()
                .map(|agent| {
                    let state = match agent.connection {
                        _ if !agent.installed => "not installed".to_owned(),
                        status::Connection::Connected => "connected".to_owned(),
                        status::Connection::Outdated => format!(
                            "runs another copy of Turnscope: `turnscope connect {}` uses this one",
                            agent.id
                        ),
                        status::Connection::Available => {
                            format!("not connected: `turnscope connect {}`", agent.id)
                        }
                    };
                    format!("{:<12} {state}", agent.name)
                })
                .collect::<Vec<_>>()
                .join("\n")
        },
    ))
}

/// The database, with what's new read first, for a command that reports
/// data.
fn caught_up(home: &Path, data: &Path) -> Result<rusqlite::Connection> {
    let mut db = db::open(data)?;
    if let Some(wait) = ingest::waits(&db) {
        eprintln!("{wait}");
    }
    if let Err(error) = catch_up(&mut db, home, None) {
        eprintln!(
            "turnscope: what's new couldn't be read first, so this is as of the last read: {error}"
        );
    }
    Ok(db)
}

fn cmd_status(
    db: &rusqlite::Connection,
    now: i64,
    json: bool,
    account: Option<&str>,
    all: bool,
    hours: Option<f64>,
) -> Result<String> {
    let status = status::status(db, now)?;
    let shown: Vec<&status::Account> = match account {
        Some(name) => status::named(&status, name)?,
        None => status
            .accounts
            .iter()
            .filter(|account| all || !account.hidden)
            .collect(),
    };
    Ok(print(
        json,
        || {
            let fit = hours.map(|hours| {
                shown
                    .iter()
                    .map(|account| {
                        let fit = status::fit(account, hours, status.at);
                        json!({ "account": account.id, "fit": fit })
                    })
                    .collect::<Vec<_>>()
            });
            json!({ "accounts": shown, "fit": fit })
        },
        || match (shown.is_empty(), hours) {
            (true, _) => status::none(&status, "`turnscope status --all` shows them"),
            (false, None) => status::text(&status, &shown, None),
            (false, Some(hours)) => format!(
                "{}\n\n{}",
                status::fits(&shown, hours, status.at),
                status::text(&status, &shown, None)
            ),
        },
    ))
}

fn cmd_usage_limit(
    db: &rusqlite::Connection,
    home: &Path,
    now: i64,
    json: bool,
    args: UsageArgs,
    name: &str,
) -> Result<String> {
    let status = status::status(db, now)?;
    let (account, limit) = limit_of(db, home, &status, args.account.as_deref(), name)?;
    let by = args.by.unwrap_or(usage::By::Session);
    let between = args
        .since
        .as_deref()
        .map(|since| time::between(since, None, now))
        .transpose()?;
    let (mut used, mut shares) = usage::breakdown(db, account, Some(limit), between, by, now)?;
    usage::fold(&mut used, shares.as_mut(), args.top);
    Ok(print(
        json,
        || {
            json!({
                "account": account.id,
                "limit": limit.key,
                "usage": used,
                "sharePercents": shares.as_ref().map(|shares| &shares.rows),
                "restSharePercent": shares.as_ref().and_then(|shares| shares.rest),
            })
        },
        || {
            let heading = format!(
                "{} · {}\n",
                account.full_title(),
                status::limit_text(limit, status.at)
            );
            let mut text =
                heading + &usage::text(&used, shares.as_ref().map(|shares| (limit, shares)));
            if let Some(why) = usage::unshared(&used, limit).filter(|_| between.is_none()) {
                text += &format!("\n{why}");
            }
            text
        },
    ))
}

fn cmd_usage(db: &rusqlite::Connection, now: i64, json: bool, args: UsageArgs) -> Result<String> {
    // Refused, as an unknown agent would show nothing used.
    if let Some(agent) = &args.agent {
        agents::find(agent)?;
    }
    let account = match args.account.as_deref() {
        Some(name) => Some(
            status::one_account(&status::status(db, now)?, name)?
                .id
                .clone(),
        ),
        None => None,
    };
    let since = args.since.as_deref().unwrap_or(usage::SINCE);
    let (since, until) = time::between(since, args.until.as_deref(), now)?;
    let query = usage::Query {
        since,
        until,
        by: args.by.unwrap_or(usage::By::Day),
        agent: args.agent,
        account,
        models: Vec::new(),
        key: None,
    };
    let mut used = usage::usage(db, &query)?;
    usage::fold(&mut used, None, args.top);
    Ok(print(json, || json!(used), || usage::text(&used, None)))
}

fn cmd_accounts(
    db: &rusqlite::Connection,
    now: i64,
    json: bool,
    action: Option<AccountAction>,
) -> Result<String> {
    let mut status = status::status(db, now)?;
    if let Some(AccountAction::Hide { account } | AccountAction::Show { account }) = &action {
        let id = status::one_account(&status, account)?.id.clone();
        let hidden = matches!(action, Some(AccountAction::Hide { .. }));
        status::hide(db, &id, hidden)?;
        if let Some(found) = status.accounts.iter_mut().find(|found| found.id == id) {
            found.hidden = hidden;
        }
    }
    Ok(print(
        json,
        || {
            let listed: Vec<_> = status
                .accounts
                .iter()
                .map(|account| {
                    json!({
                        "id": account.id,
                        "title": account.title,
                        "label": account.label,
                        "kind": account.kind,
                        "hidden": account.hidden,
                    })
                })
                .collect();
            json!(listed)
        },
        || {
            if status.accounts.is_empty() {
                status::NONE_READ.to_owned()
            } else {
                status
                    .accounts
                    .iter()
                    .map(|account| {
                        let hidden = if account.hidden { " · hidden" } else { "" };
                        format!("{} · {}{hidden}", account.full_title(), account.id)
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        },
    ))
}

/// `--hours`: of work, so zero or more.
fn hours(text: &str) -> std::result::Result<f64, String> {
    text.parse()
        .ok()
        .filter(|hours: &f64| hours.is_finite() && *hours >= 0.0)
        .ok_or_else(|| "give a number of hours, 0 or more".to_owned())
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

/// `None` when there's room, or the standing isn't known.
///
/// A hook waits for it, so it reads no history and no prices: only limits,
/// when an account's are due, and not while an app keeps the data current
/// (`engine::running`). The app reads them as they come due, within a
/// minute, so a hook that read them too would only wait on the provider,
/// up to 15 s, for what the app reads anyway.
///
/// Nor is it what makes or migrates the database, which every other process
/// waits on. Before another process has made it, or after an update until
/// the new version has opened it, the standing isn't known, as for a status
/// line.
fn guard(
    home: &Path,
    data: &Path,
    limit: &str,
    below: u8,
    account: Option<&str>,
) -> Result<Option<String>> {
    let Some(read_db) = db::read(data) else {
        return Ok(None);
    };
    // The limits read before still guard when this one can't read them
    // again, as once a newer Turnscope has written here, or does while they
    // are read: a hook that fails guards nothing. While the menu bar app
    // holds `app.lock`, it keeps limits current outside any agent sandbox.
    let db = if online() && !turnscope::engine::running(data) {
        drop(read_db);
        let mut db = db::open(data)?;
        let _ = limits::refresh(&mut db, home, limits::Ask::Scheduled);
        db
    } else {
        read_db
    };
    let status = status::status(&db, now())?;
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
    let ahead = |at: i64| time::ahead(at, status.at);
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

/// From the database alone, so it's quick. Not from the limits Claude Code
/// gives it on standard input, `rate_limits`: those are of the session's
/// last response, so a session left open gives old ones as new, over newer
/// ones another has read, and the account they'd go to, the session's
/// latest response's, may be signed out since.
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
