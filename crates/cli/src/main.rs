//! Turnscope's command line, `turnscope`: the app's feed, and everything
//! for agents and for checking the data.
//!
//! - `turnscope watch` keeps history, prices and limits current and streams
//!   what the menu bar app shows, as JSON lines, taking its requests on
//!   standard input ([`watch`]); the app starts it and reads it;
//! - `turnscope mcp` answers a coding agent over the Model Context Protocol;
//! - `turnscope call` runs one of its tools from a shell;
//! - `turnscope guard` turns a limit into an exit code, for an agent's hook;
//! - `turnscope connect` adds the MCP server to an agent ([`connect`]);
//! - `turnscope doctor` reads every agent's history and reports what it
//!   found: how much each agent's files held, what could not be made sense
//!   of, and how the transcripts compare with the agents' own totals;
//! - `turnscope catalog` writes models.dev's catalog as the price catalog
//!   Turnscope ships with.

mod connect;
mod doctor;
mod feed;
mod watch;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use turnscope_engine::{Catalog, Engine, Instant};

/// The bundle identifier the app keeps its data under.
const IDENTIFIER: &str = "com.joeychilson.turnscope";

const USAGE: &str = "usage: turnscope watch [--data <dir>] [--home <dir>]
       turnscope mcp [--data <dir>] [--home <dir>]
       turnscope call <tool> ['<arguments as JSON>'] [--data <dir>] [--home <dir>]
       turnscope guard --limit <name> --below <percent> [--say <text>] [--account <name>] [--data <dir>] [--home <dir>]
       turnscope connect [<agent>]
       turnscope doctor [--strict] [--data <dir>] [--home <dir>]
       turnscope catalog <api.json> <catalog.json>

  watch          keep history, prices and limits current, and stream what the app shows as JSON
                 lines, taking its requests on standard input
  mcp            answer a coding agent over the Model Context Protocol, on standard input and output
  call           run one of the MCP server's tools and print its answer
  guard          for a hook: exit 2, saying why on standard error, when your account's limit is
                 under the percent left; exit 0 otherwise, and when that isn't known
  connect        add the MCP server to an agent, as Claude Code, Codex, OpenCode or Grok
                 Build, or with no agent, say which agents have it
  doctor         read agents' history into the ledger and report on it
  catalog        write models.dev's api.json as the price catalog the app ships with

  --data <dir>   where the ledger is kept (default: Library/Application Support/com.joeychilson.turnscope in the home read)
  --home <dir>   whose agents' history and sign-ins to read (default: $HOME)
  --strict       doctor: fail when anything could not be read or was not understood
  -h, --help     show this";

/// Why a command stopped: a command line it can't follow, which exits with
/// 2, or a failure while following it, which exits with 1.
enum Failure {
    Usage(String),
    Failed(String),
}

impl<E: std::fmt::Display> From<E> for Failure {
    fn from(error: E) -> Failure {
        Failure::Failed(error.to_string())
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let mut args = args.into_iter().peekable();
    let command = match args.peek() {
        None => None,
        Some(first) if first.starts_with('-') => None,
        Some(_) => args.next(),
    };
    let done = match command.as_deref() {
        None => Err(Failure::Usage("a command is needed".to_owned())),
        Some("watch") => options(args).and_then(|(data, home)| watch::run(&data, &home)),
        Some("connect") => connect::command(args),
        Some("mcp") => {
            let given: Vec<String> = args.collect();
            options(given.iter().cloned()).and_then(|(data, home)| mcp(&data, &home, &given))
        }
        Some("call") => call(args),
        // A hook reads its exit code, so it exits as a hook needs.
        Some("guard") => return guard(args),
        Some("doctor") => {
            let (strict, rest): (Vec<String>, Vec<String>) =
                args.partition(|arg| arg == "--strict");
            options(rest.into_iter())
                .and_then(|(data, home)| doctor::run(&data, &home, !strict.is_empty()))
        }
        Some("catalog") => catalog(args),
        Some(other) => Err(Failure::Usage(format!("there is no command {other}"))),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(message)) => {
            eprintln!("turnscope: {message}\n\n{USAGE}");
            ExitCode::from(2)
        }
        Err(Failure::Failed(message)) => {
            eprintln!("turnscope: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Whose history is read unless `--home` says otherwise: the home the
/// environment names.
fn usual_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Where the ledger is kept, for the person whose home is `home`, unless
/// `--data` says otherwise.
fn usual_data(home: &Path) -> PathBuf {
    home.join("Library/Application Support").join(IDENTIFIER)
}

/// The data and home directories the rest of the command line names: the
/// home the environment names unless `--home` says otherwise, and the ledger
/// kept in whichever home is read unless `--data` says otherwise, so a home
/// chosen to read a fixture never writes into the person's own ledger.
fn options(mut args: impl Iterator<Item = String>) -> Result<(PathBuf, PathBuf), Failure> {
    let (mut data, mut home) = (None, None);
    while let Some(option) = args.next() {
        let chosen = match option.as_str() {
            "--data" => &mut data,
            "--home" => &mut home,
            _ => return Err(Failure::Usage(format!("there is no option {option}"))),
        };
        let value = args
            .next()
            .ok_or_else(|| Failure::Usage(format!("{option} needs a directory")))?;
        *chosen = Some(PathBuf::from(value));
    }
    let home = match home {
        Some(home) => home,
        None => usual_home().ok_or_else(|| {
            Failure::Usage("HOME is not set, so --home must name a home".to_owned())
        })?,
    };
    let data = data.unwrap_or_else(|| usual_data(&home));
    Ok((data, home))
}

/// Answer an agent over the Model Context Protocol until it closes its input.
/// `given` are the options it was started with, which the server tells
/// agents to give a tool run from a shell, so that it reads the same ledger
/// and home, and gives the newer server an update leaves, which it hands the
/// session over to.
fn mcp(data: &Path, home: &Path, given: &[String]) -> Result<(), Failure> {
    let server = turnscope_mcp::Server::new(Engine::open(data, home)?);
    turnscope_mcp::serve(
        &server,
        std::env::current_exe().ok().as_deref(),
        given,
        std::io::stdin().lock(),
        std::io::stdout(),
    )?;
    Ok(())
}

/// Run one of the MCP server's tools, as `turnscope call <tool> ['<arguments
/// as JSON>']` asks, and print its answer.
fn call(mut args: impl Iterator<Item = String>) -> Result<(), Failure> {
    let tools = || turnscope_mcp::tool_names().collect::<Vec<_>>().join(", ");
    let tool = args
        .next()
        .ok_or_else(|| Failure::Usage(format!("call needs a tool: {}", tools())))?;
    let unknown = || {
        Failure::Usage(format!(
            "there is no tool named {tool}; the tools are {}",
            tools()
        ))
    };
    // Refused before the engine opens, which would make a ledger for nothing.
    if !turnscope_mcp::tool_names().any(|name| name == tool) {
        return Err(unknown());
    }
    let mut rest: Vec<String> = args.collect();
    let arguments = if rest.first().is_some_and(|first| !first.starts_with("--")) {
        rest.remove(0)
    } else {
        "{}".to_owned()
    };
    let arguments = match serde_json::from_str::<serde_json::Value>(&arguments) {
        Ok(arguments @ serde_json::Value::Object(_)) => arguments,
        _ => {
            return Err(Failure::Usage(
                "a tool's arguments are a JSON object, such as '{\"since\": \"today\"}'".to_owned(),
            ));
        }
    };
    let (data, home) = options(rest.into_iter())?;
    let server = turnscope_mcp::Server::new(Engine::open(&data, &home)?);
    match server.call(&tool, arguments) {
        None => Err(unknown()),
        Some(Ok(reply)) => show(&format!("{}\n", reply.laid_out())),
        Some(Err(failure)) => Err(Failure::Failed(failure.0)),
    }
}

/// Whether the caller's account's limit is under a percent left, as
/// `turnscope guard --limit <name> --below <percent> [--say <text>]
/// [--account <name>]` asks, as an exit code for an agent's hook.
///
/// Under it, the reason, or `--say`'s text before it, goes to standard
/// error, and it exits 2: a Claude Code `PreToolUse` hook that exits 2
/// blocks the call it guards, and shows the model what it wrote to standard
/// error, as Claude Code 2.1.284 describes its hooks (checked 2026-09-29:
/// "Exit code 2 - show stderr to model and block tool call. Other exit
/// codes - show stderr to user only but continue with tool call"). Otherwise
/// it exits 0, and so it does whenever the limit's
/// standing isn't known, as for a stale reading, a limit no account has or
/// an account that can't be told, with a note on standard error: a guard
/// never blocks for want of data. A command line it can't follow, or data
/// it can't read, exits 1, which Claude Code shows the person without
/// blocking anything, never 2.
fn guard(args: impl Iterator<Item = String>) -> ExitCode {
    let failed = |message: &str| {
        eprintln!("turnscope guard: {message}");
        ExitCode::FAILURE
    };
    let (mut limit, mut below, mut say, mut account) = (None, None, None, None);
    let mut rest = Vec::new();
    let mut args = args.peekable();
    while let Some(option) = args.next() {
        let chosen = match option.as_str() {
            "--limit" => &mut limit,
            "--below" => &mut below,
            "--say" => &mut say,
            "--account" => &mut account,
            _ => {
                rest.push(option);
                if let Some(value) = args.next() {
                    rest.push(value);
                }
                continue;
            }
        };
        match args.next() {
            Some(value) => *chosen = Some(value),
            None => return failed(&format!("{option} needs a value")),
        }
    }
    let Some(limit) = limit else {
        return failed("--limit names the limit to guard, such as week or \"5 hours\"");
    };
    let Some(below) = below
        .as_deref()
        .and_then(|below| below.trim_end_matches('%').parse::<f64>().ok())
    else {
        return failed("--below takes a percent left, such as 50");
    };
    let (data, home) = match options(rest.into_iter()) {
        Ok(chosen) => chosen,
        Err(Failure::Usage(message) | Failure::Failed(message)) => return failed(&message),
    };
    let engine = match Engine::open(&data, &home) {
        Ok(engine) => engine,
        Err(error) => return failed(&error.to_string()),
    };
    let server = turnscope_mcp::Server::new(engine);
    match turnscope_mcp::guard(&server, &limit, below, account.as_deref()) {
        Ok(verdict) => {
            match (&verdict, say) {
                (turnscope_mcp::Guard::Pass(Some(note)), _) => {
                    eprintln!("turnscope guard: {note}");
                }
                (turnscope_mcp::Guard::Pass(None), _) => {}
                (turnscope_mcp::Guard::Block(reason), Some(say)) => {
                    eprintln!("{say} ({reason})");
                }
                (turnscope_mcp::Guard::Block(reason), None) => eprintln!("{reason}"),
            }
            ExitCode::from(verdict.exit_code())
        }
        Err(failure) => failed(&failure.0),
    }
}

/// Write `text` to standard output. One that has closed, as when the report
/// is piped into `head`, has read all it wanted: that is no failure.
fn show(text: &str) -> Result<(), Failure> {
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => Err(error.into()),
        _ => Ok(()),
    }
}

/// Write models.dev's `api.json`, as `turnscope catalog <from> <to>` names
/// it, as the catalog the app ships with.
fn catalog(mut args: impl Iterator<Item = String>) -> Result<(), Failure> {
    let (Some(from), Some(to), None) = (args.next(), args.next(), args.next()) else {
        return Err(Failure::Usage(
            "catalog needs models.dev's api.json and where to write the catalog".to_owned(),
        ));
    };
    let json = std::fs::read(&from).map_err(|error| Failure::Failed(format!("{from}: {error}")))?;
    let catalog = Catalog::from_models_dev(&json, Instant::now())?;
    std::fs::write(&to, catalog.to_json()?)
        .map_err(|error| Failure::Failed(format!("{to}: {error}")))?;
    show(&format!(
        "Wrote {} priced models to {to}.\n",
        catalog.priced()
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Failure, options};

    fn parsed(args: &[&str]) -> Result<(PathBuf, PathBuf), String> {
        options(args.iter().map(|arg| (*arg).to_owned())).map_err(|failure| match failure {
            Failure::Usage(message) | Failure::Failed(message) => message,
        })
    }

    #[test]
    fn a_home_chosen_alone_keeps_its_own_ledger() {
        // Reading a fixture's home never writes into the person's ledger.
        assert_eq!(
            parsed(&["--home", "/tmp/fixture"]),
            Ok((
                PathBuf::from("/tmp/fixture/Library/Application Support/com.joeychilson.turnscope"),
                PathBuf::from("/tmp/fixture")
            ))
        );
        assert_eq!(
            parsed(&["--data", "/tmp/data", "--home", "/tmp/fixture"]),
            Ok((PathBuf::from("/tmp/data"), PathBuf::from("/tmp/fixture")))
        );
    }

    #[test]
    fn an_option_is_known_by_its_name_before_its_value_is_taken() {
        assert_eq!(
            parsed(&["--verbose"]),
            Err("there is no option --verbose".to_owned())
        );
        assert_eq!(
            parsed(&["--home", "/tmp/fixture", "--data"]),
            Err("--data needs a directory".to_owned())
        );
    }
}
