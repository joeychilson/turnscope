//! Turnscope: the limits, usage and sessions of the coding agents on this
//! Mac. One binary: the CLI, the MCP server agents ask (`mcp`), and the
//! daemon the menu bar app talks to (`serve`). See docs/architecture.md.

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
mod redact;
mod serve;
mod sessions;
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

/// Now, in UTC milliseconds; `TURNSCOPE_NOW` fixes it, so output can be
/// reproduced.
pub fn now() -> i64 {
    std::env::var("TURNSCOPE_NOW")
        .ok()
        .and_then(|now| now.parse().ok())
        .unwrap_or_else(|| jiff::Timestamp::now().as_millisecond())
}

#[derive(Parser)]
#[command(
    name = "turnscope",
    version,
    about = "The limits, usage and sessions of the coding agents on this Mac",
    after_help = "Commands that print data take --json.\n\
        Exit codes: 0 done, 1 failed (the message says how to fix it), 2 bad usage, 3 what you named doesn't exist."
)]
struct Cli {
    /// Where Turnscope keeps its data [default: ~/Library/Application Support/com.joeychilson.turnscope]
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        help_heading = "Global options"
    )]
    data: Option<PathBuf>,
    /// Whose agents' history and logins to read [default: $HOME]
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        help_heading = "Global options"
    )]
    home: Option<PathBuf>,
    /// Print JSON instead of text
    #[arg(long, global = true, help_heading = "Global options")]
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
        /// Only this agent's: the id session ids begin with, such as claude-code
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
    /// Find sessions, most recently active first, or show or read one
    Sessions {
        #[command(subcommand)]
        action: Option<SessionAction>,
        /// All these words said in it
        #[arg(long)]
        search: Option<String>,
        /// Of this folder's project, or run in or under it
        #[arg(long)]
        folder: Option<PathBuf>,
        /// Only this agent's
        #[arg(long)]
        agent: Option<String>,
        /// Active since: today, 24h, 7d, a date, or an RFC 3339 time
        #[arg(long)]
        since: Option<String>,
        /// Only those running now
        #[arg(long)]
        running: bool,
        /// How many, up to 100
        #[arg(long, short = 'n', default_value_t = 20)]
        count: usize,
        /// The cursor the page before gave, for the next page
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Where a session stands, for another agent to pick up from
    Handoff {
        /// Its id, or a unique start of it [default: the latest other session in the folder]
        session: Option<String>,
        /// The folder to take the latest session of [default: this one]
        #[arg(long)]
        folder: Option<PathBuf>,
    },
    /// List accounts, or hide or show one
    Accounts {
        #[command(subcommand)]
        action: Option<AccountAction>,
    },
    /// Show the settings, or turn a notification on or off
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
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
        /// How long to stay after the last client leaves
        #[arg(long, value_name = "SECONDS", default_value_t = 60)]
        linger: u64,
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
enum SessionAction {
    /// One session in full, with how to resume it
    Show {
        /// The session's id, or a unique start of it, as `turnscope sessions` lists them
        session: String,
    },
    /// A session's conversation, a page at a time
    Read {
        /// The session's id, or a unique start of it, as `turnscope sessions` lists them
        session: String,
        /// The first entry; negative counts back from the end over the entries wanted, as -10 for the last ten
        #[arg(long, default_value_t = 0, allow_negative_numbers = true)]
        from: i64,
        /// How many entries, up to 100; a page also ends at about 3,000 tokens
        #[arg(long, short = 'n', default_value_t = 30)]
        count: usize,
        /// Only these kinds: user, assistant, reasoning, tool, system, summary (the agent's own, when it compacted) or task (a subagent's brief)
        #[arg(long, value_delimiter = ',')]
        kinds: Vec<agents::Kind>,
        /// Only entries holding all these words
        #[arg(long)]
        search: Option<String>,
        /// Only tool calls that failed; with --from -1, the last
        #[arg(long)]
        failed: bool,
    },
}

#[derive(Subcommand)]
enum AccountAction {
    /// Leave an account out of lists and alerts
    Hide { account: String },
    /// Show a hidden account again
    Show { account: String },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Turn a notification on or off: notify.runningOut, notify.usedUp, notify.reset or notify.signIn
    Set {
        key: String,
        #[arg(value_parser = ["on", "off"])]
        value: String,
    },
}

fn main() -> ExitCode {
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
    let absolute = |path: PathBuf| std::path::absolute(&path).unwrap_or(path);
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
        Command::Serve { socket: true, .. } => Ok(serve::socket_path(&data).display().to_string()),
        Command::Serve { linger, .. } => {
            serve::run(&home, &data, Duration::from_secs(linger)).map(|()| String::new())
        }
        // A hook reads its exit code, so a guard that can't tell blocks nothing.
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

/// A command's answer, as text or JSON. Those that report data catch up
/// first; those that change a setting don't wait for it.
fn answer(command: Command, json: bool, home: &Path, data: &Path) -> Result<String> {
    let mut db = db::open(data)?;
    let reports = match &command {
        Command::Accounts { action } => action.is_none(),
        Command::Config { .. } | Command::Connect { .. } | Command::Disconnect { .. } => false,
        _ => true,
    };
    // What's in the database still answers when what's new can't be read.
    if reports && let Err(error) = catch_up(&mut db, home) {
        eprintln!(
            "turnscope: what's new couldn't be read first, so this is as of the last read: {error}"
        );
    }
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
            let status = status::status(&db, home, now)?;
            let mut shown: Vec<&status::Account> = match &account {
                Some(name) => status::named(&status, name)?,
                None => status.accounts.iter().collect(),
            };
            shown.retain(|account| all || !account.hidden);
            let fit = match (hours, shown.first()) {
                (Some(hours), Some(first)) => Some(status::fit(first, hours, now)?),
                _ => None,
            };
            let text = status::text(&status, &shown, None);
            let text = fit
                .as_ref()
                .map_or(text.clone(), |fit| format!("{fit}\n\n{text}"));
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
            let status = status::status(&db, home, now)?;
            let (account, limit) = limit_of(&db, home, &status, account.as_deref(), &name)?;
            let by = by.unwrap_or(usage::By::Session);
            let (used, shares) =
                usage::breakdown(&db, account, limit, time_of(&from)?, by, top, now)?;
            let heading = format!(
                "{} · {}\n",
                account.full_title(),
                status::limit_text(limit, now)
            );
            let column = shares
                .iter()
                .any(Option::is_some)
                .then_some((limit.name.as_str(), shares.as_slice()));
            let text = heading + &usage::text(&used, column);
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
            let account = match account {
                Some(name) => Some(
                    one_account(&status::status(&db, home, now)?, &name)?
                        .id
                        .clone(),
                ),
                None => None,
            };
            let query = usage::Query {
                since: time_of(&from)?.unwrap_or(now - 30 * time::DAY),
                until: time_of(&until)?.unwrap_or(now),
                by: by.unwrap_or(usage::By::Day),
                agent,
                account,
                top,
            };
            let usage = usage::usage(&db, &query)?;
            print(json!(usage), usage::text(&usage, None))
        }
        Command::Sessions {
            action: Some(SessionAction::Show { session }),
            ..
        } => {
            let session = sessions::resolve(&db, &session)?;
            let detail = sessions::show(&db, home, &session, now)?;
            print(json!(detail), sessions::detail_text(&detail, now))
        }
        Command::Sessions {
            action:
                Some(SessionAction::Read {
                    session,
                    from,
                    count,
                    kinds,
                    search,
                    failed,
                }),
            ..
        } => {
            let session = sessions::resolve(&db, &session)?;
            let entries = sessions::transcript(&db, &session)?;
            let reading = sessions::Reading {
                from,
                count,
                kinds,
                search,
                failed,
            };
            let (shown, next) = sessions::page(&entries, &reading);
            let shown: Vec<_> = shown
                .into_iter()
                .map(|(at, entry)| json!({ "index": at, "entry": entry }))
                .collect();
            let value = json!({ "session": session, "entries": shown, "total": entries.len(), "next": next });
            print(
                value,
                sessions::page_text(&session, &entries, &reading, sessions::Cite::Command, now),
            )
        }
        Command::Sessions {
            action: None,
            search,
            folder,
            agent,
            since: from,
            running,
            count,
            cursor,
        } => {
            let query = sessions::Query {
                search,
                folder,
                agent,
                since: time_of(&from)?,
                running,
                count,
                cursor,
                except: None,
            };
            let page = sessions::find(&db, home, &query, now)?;
            print(
                json!(page),
                sessions::text(
                    &page,
                    sessions::Cite::Command,
                    mcp::Caller::detect().session.as_deref(),
                    now,
                ),
            )
        }
        Command::Handoff { session, folder } => {
            let (session, others) = match session {
                Some(session) => (sessions::resolve(&db, &session)?, Vec::new()),
                // Run by an agent, not the session it's run from.
                None => {
                    let folder = folder
                        .or_else(|| std::env::current_dir().ok())
                        .unwrap_or_default();
                    let own = mcp::Caller::detect().own_session(&db, home, &folder)?;
                    sessions::latest(&db, home, &folder, own.as_deref())?
                }
            };
            let handoff = sessions::handoff(&db, home, &session, sessions::Cite::Command, now)?;
            print(
                json!({ "session": session, "others": others, "handoff": handoff }),
                sessions::also_here(&others, now) + &handoff,
            )
        }
        Command::Accounts { action } => {
            if let Some(AccountAction::Hide { account } | AccountAction::Show { account }) = &action
            {
                let status = status::status(&db, home, now)?;
                let found = one_account(&status, account)?;
                let hidden = matches!(action, Some(AccountAction::Hide { .. }));
                status::hide(&db, &found.id, hidden)?;
            }
            let status = status::status(&db, home, now)?;
            let lines: Vec<String> = status
                .accounts
                .iter()
                .map(|account| {
                    let hidden = if account.hidden { " · hidden" } else { "" };
                    format!("{} · {}{hidden}", account.full_title(), account.id)
                })
                .collect();
            let listed: Vec<_> = status
                .accounts
                .iter()
                .map(|account| json!({ "id": account.id, "title": account.full_title(), "kind": account.kind, "hidden": account.hidden }))
                .collect();
            print(json!(listed), lines.join("\n"))
        }
        Command::Config { action } => {
            let mut settings = alerts::settings(&db)?;
            if let Some(ConfigAction::Set { key, value }) = action {
                let on = value == "on";
                let notify = &mut settings.notify;
                match key.as_str() {
                    "notify.runningOut" => notify.running_out = on,
                    "notify.usedUp" => notify.used_up = on,
                    "notify.reset" => notify.reset = on,
                    "notify.signIn" => notify.sign_in = on,
                    other => {
                        return Err(Error::Usage(format!(
                            "no setting {other}; there are notify.runningOut, notify.usedUp, notify.reset and notify.signIn"
                        )));
                    }
                }
                alerts::save_settings(&db, &settings)?;
            }
            let notify = settings.notify;
            let word = |on: bool| if on { "on" } else { "off" };
            let text = format!(
                "notify.runningOut {}\nnotify.usedUp {}\nnotify.reset {}\nnotify.signIn {}",
                word(notify.running_out),
                word(notify.used_up),
                word(notify.reset),
                word(notify.sign_in)
            );
            print(json!(settings), text)
        }
        Command::Connect { agent: Some(id) } => {
            let agent = agents::find(&id)?;
            connect::connect(agent, home)?;
            let text = format!(
                "{} now runs Turnscope's MCP server. Start a new session in it to use it.",
                agent.info().name
            );
            print(json!({ "agent": id, "connected": true }), text)
        }
        Command::Disconnect { agent: id } => {
            let agent = agents::find(&id)?;
            connect::disconnect(agent, home)?;
            let text = format!(
                "{} no longer runs Turnscope's MCP server. Sessions already open keep it until they end.",
                agent.info().name
            );
            print(json!({ "agent": id, "connected": false }), text)
        }
        Command::Connect { agent: None } => {
            let agents = status::status(&db, home, now)?.agents;
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

/// The one account `name` names; when it names several, which they are.
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

/// The limit `name` names, of the account `account` names, or else the
/// calling agent's account, or else the first account that has one so
/// named.
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
        None => {
            let yours = mcp::Caller::detect().account(db, home)?;
            status
                .accounts
                .iter()
                .find(|account| Some(&account.id) == yours.as_ref() && has(account))
                .or_else(|| {
                    status
                        .accounts
                        .iter()
                        .filter(|account| !account.hidden)
                        .find(has)
                })
                .ok_or_else(|| Error::NotFound(format!("no account has a limit named {name}")))?
        }
    };
    Ok((account, account.limit(name)?))
}

/// What can't be read, and how to fix it.
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
    let status = status::status(db, home, now)?;
    for account in &status.accounts {
        let agents = match account.agents.as_slice() {
            [] => "the agent that uses it".to_owned(),
            ids => ids
                .iter()
                .map(|id| agents::name(id))
                .collect::<Vec<_>>()
                .join(" or "),
        };
        let title = account.full_title();
        match (&account.state, account.state.why()) {
            (_, Some(status::Stale::SignIn)) => notes.push(format!(
                "{title}: its login was refused; sign in again in {agents}."
            )),
            (_, Some(status::Stale::Expired)) => notes.push(format!(
                "{title}: its login expired; it's read again once you use {agents}."
            )),
            (status::State::AsOf { read_at, .. }, Some(status::Stale::ReadFailed)) => {
                notes.push(format!(
                    "{title}: it couldn't be read since {}; it's tried again every 5 minutes.",
                    time::clock(*read_at, now)
                ))
            }
            (status::State::Unread { .. }, Some(status::Stale::ReadFailed)) => notes.push(format!(
                "{title}: it couldn't be read; it's tried again every 5 minutes."
            )),
            _ => {}
        }
    }
    let mut skipped = db.prepare(
        "SELECT path, skipped FROM file WHERE skipped > 0 ORDER BY skipped DESC LIMIT 10",
    )?;
    for row in skipped.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })? {
        let (path, skipped) = row?;
        notes.push(format!(
            "{path}: {skipped} lines couldn't be read; their agent may write a newer format."
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

/// Why the caller's account's `limit` blocks, when under `below` percent
/// left: `None` when there's room, or the standing isn't known.
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
    if online() {
        limits::refresh(&mut db, home)?;
    }
    let now = now();
    let status = status::status(&db, home, now)?;
    let account = match account {
        Some(name) => one_account(&status, name)?,
        None => {
            let yours = mcp::Caller::detect().account(&db, home)?;
            let Some(found) = status
                .accounts
                .iter()
                .find(|account| Some(&account.id) == yours.as_ref())
            else {
                return Ok(None);
            };
            found
        }
    };
    let found = account.limit(limit)?;
    if found.left_percent >= below {
        return Ok(None);
    }
    let called = format!("{}'s {}", account.title, limit_called(found));
    let when = |at: i64| match at >= now {
        true => format!("{} (in {})", time::clock(at, now), time::span(at - now)),
        false => format!("{} ({} ago)", time::clock(at, now), time::span(now - at)),
    };
    Ok(Some(match &found.outlook {
        status::Outlook::UsedUp { back } => format!(
            "{called} is used up{}.",
            back.map_or(String::new(), |back| format!(
                "; it's back {}",
                when(back.at)
            ))
        ),
        outlook => {
            let mut said = format!("{called} has {}% left, under {below}%.", found.left_percent);
            if let Some(window) = found.window.filter(|_| found.reset_since.is_none()) {
                said += &format!(" It resets {}", when(window.resets.at));
                if let status::Outlook::RunsOut { likely, .. } = outlook {
                    said += &format!("; at this pace it runs out {}", when(likely.at));
                }
                said += ".";
            }
            said
        }
    }))
}

/// A limit as a sentence names it: "weekly limit", "5-hour limit", "Opus
/// weekly limit".
fn limit_called(limit: &status::Limit) -> String {
    let name = limit.name.to_lowercase();
    let name = match name.split_once(' ') {
        Some((count, "hours")) if count.parse::<u32>().is_ok() => format!("{count}-hour"),
        _ => name,
    };
    let name = match &limit.scope {
        Some(scope) => format!("{scope} {name}"),
        None => name,
    };
    if name.ends_with("limit") {
        name
    } else {
        format!("{name} limit")
    }
}

/// One line for an agent's status line: the account it draws on, by
/// `account`, the session the harness names on standard input, or the
/// calling agent; else the first account in use. From the database alone,
/// so it's quick; none when nothing's known.
fn statusline(home: &Path, data: &Path, account: Option<&str>) -> Option<String> {
    let db = db::open(data).ok()?;
    let now = now();
    let status = status::status(&db, home, now).ok()?;
    let id = match account {
        // A line is better than none: of several named, the first.
        Some(name) => Some(status::named(&status, name).ok()?.first()?.id.clone()),
        None => harness(&db).or_else(|| mcp::Caller::detect().account(&db, home).ok().flatten()),
    };
    let account = match id {
        Some(id) => status.accounts.iter().find(|account| account.id == id)?,
        None => status
            .accounts
            .iter()
            .find(|account| account.in_use && !account.hidden)?,
    };
    Some(status::line(account, now))
}

/// The account of the session a harness names in the JSON it writes to a
/// status line's standard input, as Claude Code does: its `session_id`, or
/// else its folder's latest.
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
    let one = |sql: &str, value: &str| -> Option<String> {
        db.query_row(sql, [value], |row| row.get(0)).ok()
    };
    // The agent names its session by its own id.
    given["session_id"]
        .as_str()
        .and_then(|session| sessions::resolve(db, session).ok())
        .and_then(|session| {
            one(
                "SELECT account FROM response WHERE session = ?1 AND account IS NOT NULL ORDER BY at DESC LIMIT 1",
                &session,
            )
        })
        .or_else(|| {
            let folder = given["workspace"]["current_dir"]
                .as_str()
                .or(given["cwd"].as_str())?;
            one(
                "SELECT r.account FROM response r JOIN session s ON s.id = r.session
                 WHERE s.cwd = ?1 AND r.account IS NOT NULL ORDER BY r.at DESC LIMIT 1",
                folder,
            )
        })
}

/// Bring the database up to date: prices and limits when due, then what's
/// new in the agents' history, priced and attributed as it's written. A
/// provider out of reach leaves things as they were. The history is read
/// even when the limits can't be, and their error is given after.
pub fn catch_up(db: &mut rusqlite::Connection, home: &Path) -> Result<()> {
    let limits = match online() {
        true => {
            let _ = prices::refresh(db);
            limits::refresh(db, home)
        }
        false => Ok(()),
    };
    ingest::ingest(db, home)?;
    limits
}

/// Whether providers and models.dev may be asked: `TURNSCOPE_OFFLINE` works
/// from the database alone.
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
    let drain = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(child.stdout.take().map(|pipe| Box::new(pipe) as _));
    let stderr = drain(child.stderr.take().map(|pipe| Box::new(pipe) as _));
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
    let (stdout, stderr) = (
        stdout.join().unwrap_or_default(),
        stderr.join().unwrap_or_default(),
    );
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}
