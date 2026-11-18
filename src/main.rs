//! Turnscope: the limits and usage of the coding agents on this Mac. One
//! binary: the CLI, the MCP server agents ask (`mcp`), and the daemon the
//! menu bar app talks to (`serve`). See docs/architecture.md.

mod agents;
mod alerts;
mod connect;
mod db;
mod forecast;
mod ingest;
mod limits;
mod mcp;
mod prices;
mod providers;
mod serve;
mod status;
mod time;
mod usage;

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, ExitCode, Output, Stdio};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use serde_json::json;

#[derive(Debug)]
pub enum Error {
    /// Arguments that can't be followed. Exit 2.
    Usage(String),
    /// What was named doesn't exist. Exit 3.
    NotFound(String),
    /// Exit 1; the message says how to fix it.
    Failed(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let (Error::Usage(message) | Error::NotFound(message) | Error::Failed(message)) = self;
        f.write_str(message)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Error {
        Error::Failed(format!("the database: {error}"))
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Error {
        Error::Failed(error.to_string())
    }
}

/// Now, in UTC milliseconds; `TURNSCOPE_NOW` fixes it, so output can be
/// reproduced.
pub fn now() -> i64 {
    std::env::var("TURNSCOPE_NOW")
        .ok()
        .and_then(|now| now.parse().ok())
        .unwrap_or_else(|| jiff::Timestamp::now().as_millisecond())
}

pub fn sha256_hex(data: &[u8], bytes: usize) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(data)[..bytes]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

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
        /// Whether this many more hours of work fit the first account, at its pace of work
        #[arg(long)]
        hours: Option<f64>,
    },
    /// What was used and what it cost, by day, week, month, model, project, agent, account or session
    Usage {
        /// From when [default: 30 days back]: today, yesterday, week (since Monday), month (since the 1st), 24h, 7d, 4w, a date, or an RFC 3339 time
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
        /// How many rows to show; the rest are added up
        #[arg(long, default_value_t = 20)]
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
    /// The daemon the menu bar app talks to, on a local socket
    Serve {
        /// Print the socket's path and exit
        #[arg(long)]
        socket: bool,
    },
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
        Err(error) if error.use_stderr() && std::env::args().any(|arg| arg == "guard") => {
            let _ = error.print();
            return ExitCode::FAILURE;
        }
        Err(error) => error.exit(),
    };
    // Absolute, as paths under them are stored and read from other folders.
    let home = absolute(
        cli.home
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_default(),
    );
    let data = absolute(
        cli.data
            .unwrap_or_else(|| home.join("Library/Application Support/com.joeychilson.turnscope")),
    );
    let outcome = match cli.command {
        // Servers answer at once, and catch up as they're asked.
        Command::Mcp => mcp::run(&home, &data).map(|()| String::new()),
        Command::Serve { socket: true } => Ok(serve::socket_path(&data).display().to_string()),
        Command::Serve { socket: false } => serve::run(&home, &data).map(|()| String::new()),
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
                Error::Failed(_) => 1,
            })
        }
    }
}

fn absolute(path: PathBuf) -> PathBuf {
    std::path::absolute(&path).unwrap_or(path)
}

/// Commands that report data catch up first; those that change a setting
/// don't wait for it.
fn answer(command: Command, json: bool, home: &Path, data: &Path) -> Result<String> {
    let mut db = db::open(data)?;
    let reports = match &command {
        Command::Accounts { action } => action.is_none(),
        Command::Connect { .. } | Command::Disconnect { .. } => false,
        _ => true,
    };
    if reports {
        if let Some(wait) = ingest::waits(&db) {
            eprintln!("{wait}");
        }
        if let Err(error) = catch_up(&mut db, home) {
            eprintln!(
                "turnscope: what's new couldn't be read first, so this is as of the last read: {error}"
            );
        }
    }
    let connecting = matches!(command, Command::Connect { .. });
    let now = now();
    let time_of = |text: &Option<String>| {
        text.as_deref()
            .map(|text| time::parse(text, now))
            .transpose()
    };
    let print = |value: serde_json::Value, text: String| {
        if json {
            serde_json::to_string_pretty(&value).unwrap_or_default()
        } else {
            text
        }
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
            print(json!({ "accounts": shown, "fit": fit }), text)
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
            let (used, shares) = usage::breakdown(&db, account, Some(limit), since, by, top, now)?;
            let heading = format!(
                "{} · {}\n",
                account.full_title(),
                status::limit_text(limit, status.at)
            );
            let text = heading + &usage::text(&used, shares.as_ref().map(|shares| (limit, shares)));
            let shares = shares.map(|shares| shares.rows);
            let value = json!({ "account": account.id, "limit": limit.key, "usage": used, "sharePercents": shares });
            print(value, text)
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
                Some(name) => Some(one_account(&status::status(&db, now)?, &name)?.id.clone()),
                None => None,
            };
            let since = time_of(&from)?.unwrap_or(now - 30 * time::DAY);
            let until = time_of(&until)?.unwrap_or(now);
            if since >= until {
                return Err(Error::Usage(
                    "--since is at or after --until, so nothing is between them: give an earlier --since"
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
                top,
            };
            let usage = usage::usage(&db, &query)?;
            print(json!(usage), usage::text(&usage, None))
        }
        Command::Accounts { action } => {
            let mut status = status::status(&db, now)?;
            if let Some(AccountAction::Hide { account } | AccountAction::Show { account }) = &action
            {
                let id = one_account(&status, account)?.id.clone();
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
                    let title = account.full_title();
                    let line = format!("{title} · {}{hidden}", account.id);
                    (line, json!({ "id": account.id, "title": title, "kind": account.kind, "hidden": account.hidden }))
                })
                .unzip();
            let text = match lines.is_empty() {
                true => status::none(&status, "`turnscope status --all` shows them"),
                false => lines.join("\n"),
            };
            print(json!(listed), text)
        }
        Command::Connect { agent: Some(id) } | Command::Disconnect { agent: id } => {
            let agent = agents::find(&id)?;
            let text = if connecting {
                connect::connect(agent, home)?;
                "now runs Turnscope's MCP server. Start a new session in it to use it."
            } else {
                connect::disconnect(agent, home)?;
                "no longer runs Turnscope's MCP server. Sessions already open keep it until they end."
            };
            let text = format!("{} {text}", agent.info().name);
            print(json!({ "agent": id, "connected": connecting }), text)
        }
        Command::Connect { agent: None } => {
            let agents = status::agents(home, &status::status(&db, now)?.accounts);
            let lines: Vec<String> = agents
                .iter()
                .map(|agent| {
                    let state = match agent.connection {
                        _ if !agent.installed => "not installed".to_owned(),
                        status::Connection::Connected => "connected".to_owned(),
                        status::Connection::Outdated => {
                            format!("runs another copy of Turnscope: `turnscope connect {}` uses this one", agent.id)
                        }
                        status::Connection::Available => format!("not connected: `turnscope connect {}`", agent.id),
                    };
                    format!("{:<12} {state}", agent.name)
                })
                .collect();
            print(json!(agents), lines.join("\n"))
        }
        Command::Doctor => {
            let notes = doctor(&db, home, data, now)?;
            let text = if notes.is_empty() {
                "Nothing to fix.".to_owned()
            } else {
                notes.join("\n")
            };
            print(json!(notes), text)
        }
        Command::Mcp
        | Command::Serve { .. }
        | Command::Guard { .. }
        | Command::Statusline { .. } => {
            unreachable!("served before anything is read")
        }
    })
}

pub fn one_account<'a>(status: &'a status::Status, name: &str) -> Result<&'a status::Account> {
    match status::named(status, name)?.as_slice() {
        [one] => Ok(one),
        many => Err(Error::Usage(format!(
            "{name} names {} accounts: {}. Name one by more of its title or label, or by its id.",
            many.len(),
            many.iter()
                .map(|account| format!("{} ({})", account.full_title(), account.id))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
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
        Some(account) => one_account(status, account)?,
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

fn doctor(db: &rusqlite::Connection, home: &Path, data: &Path, now: i64) -> Result<Vec<String>> {
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
    let unpriced: Vec<(String, i64)> = db
        .prepare("SELECT model, count(*) FROM response WHERE cost IS NULL GROUP BY model ORDER BY count(*) DESC LIMIT 5")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (model, count) in unpriced {
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
    let priced: bool = db.query_row("SELECT EXISTS (SELECT 1 FROM price)", [], |row| row.get(0))?;
    if !priced {
        notes.push(
            "Prices haven't been read from models.dev yet: costs are unknown until they are."
                .to_owned(),
        );
    }
    if std::os::unix::net::UnixStream::connect(serve::socket_path(data)).is_err() {
        notes.push("serve isn't running: the menu bar app starts it.".to_owned());
    }
    Ok(notes)
}

/// `None` when there's room, or the standing isn't known.
///
/// A hook waits for it, so it reads no history and no prices: only limits,
/// when five minutes have passed since any process read them. While the
/// app runs, its daemon has always read them more recently.
fn guard(
    home: &Path,
    data: &Path,
    limit: &str,
    below: u8,
    account: Option<&str>,
) -> Result<Option<String>> {
    let mut db = db::open(data)?;
    // The limits read before still guard when this one can't read them
    // again, as once a newer Turnscope has written here: a hook that fails
    // guards nothing.
    if online() && db::writing(&db).is_ok() {
        limits::refresh(&mut db, home)?;
    }
    let now = now();
    let status = status::status(&db, now)?;
    let account = match account {
        Some(name) => one_account(&status, name)?,
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
    let db = db::open(data).ok()?;
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

/// Bring the database up to date: prices and limits when due, then what's
/// new in the agents' history, priced and attributed as it's written. A
/// provider out of reach leaves things as they were. The history is read
/// even when the limits can't be, and their error is given after. Whether
/// there was anything new: prices, limits or history.
///
/// Nothing is read into a schema a newer Turnscope has changed since `db`
/// was opened, as under an agent's MCP server left running through an
/// update: the caller answers from what was read before, and says why.
/// That's over going on until a statement fails. 0.1.8 did, and after
/// 0.2.0 changed the schema its server's answers ended with SQLite's own
/// words, `no such column: indexed in SELECT path, agent, …`; a statement
/// that still fits a changed schema would write the old way and say
/// nothing. It's checked here, over beside the readers in `ingest`, since
/// prices and limits are written before history is read. A query that no
/// longer fits still fails: 0.1.8's usage by session did. Reading the
/// revision takes 2.8 µs (November 2026).
pub fn catch_up(db: &mut rusqlite::Connection, home: &Path) -> Result<bool> {
    db::writing(db)?;
    let (priced, limits) = match online() {
        true => (
            prices::refresh(db).unwrap_or(false),
            limits::refresh(db, home),
        ),
        false => (false, Ok(false)),
    };
    let read = ingest::ingest(db, home)?;
    Ok(limits? | priced | read)
}

fn online() -> bool {
    std::env::var_os("TURNSCOPE_OFFLINE").is_none_or(|offline| offline == "0")
}

/// Run `command` to its end, with nothing on its standard input, waiting no
/// longer than `wait`: a locked Keychain waits for a password no one may
/// type, and an agent's command may wait for a prompt.
pub fn output_within(command: &mut Process, wait: Duration) -> std::io::Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Read as written, so a child that writes more than a pipe holds
    // doesn't wait on us.
    fn drain(
        pipe: Option<impl std::io::Read + Send + 'static>,
    ) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (sent, read) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Some(mut pipe) = pipe else { return };
            let mut bytes = [0; 8192];
            while let Ok(count @ 1..) = pipe.read(&mut bytes) {
                if sent.send(bytes[..count].to_vec()).is_err() {
                    return;
                }
            }
        });
        read
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + wait;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            let message = format!("no answer within {} s", wait.as_secs());
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, message));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // A child can leave a process holding its pipes open, as a login
    // shell's background job does, so each pipe is read until it closes or
    // is quiet for `QUIET`: until the deadline, or `QUIET` past it for a
    // child that ended just before. Chosen over reading to the deadline,
    // which waited all of it: 10 s for a login shell's `PATH`. The cost:
    // output is lost if its reader waits longer than `QUIET` for a
    // processor. Measured 2026-11-14 at a load of 7 on 8 cores: `sleep 4 &
    // echo said` came back in 0.22 s, not 4 s, and 300 children writing
    // 1 MB per pipe lost nothing.
    const QUIET: Duration = Duration::from_millis(100);
    let until = deadline.max(Instant::now() + QUIET);
    let collect = |read: std::sync::mpsc::Receiver<Vec<u8>>| {
        let mut bytes = Vec::new();
        while let Ok(more) =
            read.recv_timeout(QUIET.min(until.saturating_duration_since(Instant::now())))
        {
            bytes.extend(more);
        }
        bytes
    };
    Ok(Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
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
            ingest::ingest(&mut db, home.path()).unwrap();
            doctor(&db, home.path(), data.path(), now())
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

    #[test]
    fn a_command_is_waited_on_to_its_end_not_for_what_it_leaves_running() {
        let began = Instant::now();
        let mut command = Process::new("/bin/sh");
        command.args(["-c", "sleep 4 & echo said"]);
        let output = output_within(&mut command, Duration::from_secs(4)).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"said\n");
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "{:?}",
            began.elapsed()
        );
    }

    #[test]
    fn a_command_is_waited_on_no_longer_than_asked_whatever_it_leaves_running() {
        let began = Instant::now();
        let mut command = Process::new("/bin/sh");
        // What it leaves running goes on writing, for 3 s or more.
        let writing = "n=0; while [ $n -lt 150 ]; do echo more; sleep 0.02; n=$((n + 1)); done";
        command.args(["-c", &format!("({writing}) & echo said")]);
        let output = output_within(&mut command, Duration::from_secs(1)).unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("said\n"));
        assert!(
            began.elapsed() < Duration::from_millis(2500),
            "{:?}",
            began.elapsed()
        );
    }

    #[test]
    fn a_command_that_writes_more_than_a_pipe_holds_gives_all_of_it() {
        let mut command = Process::new("/bin/sh");
        let megabyte = "/usr/bin/head -c 1048576 /dev/zero";
        command.args(["-c", &format!("{megabyte}; {megabyte} >&2")]);
        let output = output_within(&mut command, Duration::from_secs(30)).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 1 << 20);
        assert_eq!(output.stderr.len(), 1 << 20);
    }
}
