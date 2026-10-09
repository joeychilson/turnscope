//! Adding Turnscope's MCP server to an agent, or removing it, with the
//! agent's own command, as a person would in a terminal: Turnscope never
//! writes an agent's files.
//!
//! The app connects through `serve`, which an app started from Finder runs
//! with a bare `PATH`, so the agent's command is found with the `PATH` of the
//! person's interactive login shell, where agents' installers add their
//! folders: on 2026-09-30, `zsh -lc` found none of four agents, and `zsh
//! -ilc` all four, in 0.15 s.

use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use crate::agents::{Agent, Info};
use crate::status::Connection;
use crate::{Error, Result, output_within};

/// Add the server to `agent`, in each of its folders, replacing any it has,
/// so it runs this copy of Turnscope. When the agent had to remove one first
/// and then refused the new one, the one it had is put back.
pub fn connect(agent: &dyn Agent, home: &Path) -> Result<()> {
    let info = agent.info();
    // Its real path, so a link to it on `PATH` isn't what agents run.
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| Error::Failed(error.to_string()))?;
    let exe = exe.to_string_lossy();
    let path = login_path();
    for (n, folder) in agent.folders(home).iter().enumerate() {
        let at = (n > 0).then_some(folder.as_path());
        let by_hand = format!(
            "{}{} {} '{exe}' mcp",
            pointed(info, at),
            info.command,
            info.mcp_add.join(" ")
        );
        let add = |exe: &str| run(info, &[info.mcp_add, &[exe, "mcp"]].concat(), &path, at);
        let earlier = agent.mcp_command(folder, home);
        if let Some(remove) = info.mcp_remove {
            // Nothing to remove is fine.
            let _ = run(info, remove, &path, at);
        }
        let failed = match add(&exe) {
            Ok(output) if output.status.success() => continue,
            Ok(output) => format!(
                "{} couldn't add the server: {}. To add it by hand, run: {by_hand}",
                info.name,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Err(error) => format!(
                "couldn't run {}: {error}. Is {} installed? To add the server by hand, run: {by_hand}",
                info.command, info.name
            ),
        };
        if let (Some(_), Some(earlier)) = (info.mcp_remove, earlier) {
            let _ = add(&earlier);
        }
        return Err(Error::Failed(failed));
    }
    Ok(())
}

/// Remove the server from `agent`, in each of its folders, with the agent's
/// own command where it has one.
pub fn disconnect(agent: &dyn Agent, home: &Path) -> Result<()> {
    let info = agent.info();
    let Some(remove) = info.mcp_remove else {
        return Err(Error::Failed(format!(
            "{} has no command Turnscope can run to remove a server: remove `turnscope` from its MCP settings by hand.",
            info.name
        )));
    };
    let path = login_path();
    for (n, folder) in agent.folders(home).iter().enumerate() {
        if agent.mcp_command(folder, home).is_none() {
            continue;
        }
        let at = (n > 0).then_some(folder.as_path());
        match run(info, remove, &path, at) {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                return Err(Error::Failed(format!(
                    "{} couldn't remove the server: {}. To remove it by hand, run: {}{} {}",
                    info.name,
                    String::from_utf8_lossy(&output.stderr).trim(),
                    pointed(info, at),
                    info.command,
                    remove.join(" ")
                )));
            }
            Err(error) => {
                return Err(Error::Failed(format!(
                    "couldn't run {}: {error}. Is {} installed?",
                    info.command, info.name
                )));
            }
        }
    }
    Ok(())
}

/// Run the agent's command with `args`, on `path` where it's known, for the
/// folder `at`, or its own.
fn run(
    info: &Info,
    args: &[&str],
    path: &Option<String>,
    at: Option<&Path>,
) -> std::io::Result<Output> {
    let mut command = Command::new(info.command);
    command.args(args);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    // A second account's folder, as `~/.claude-work`, keeps its own
    // settings: the agent is pointed at it as it's run for that account.
    if let Some(var) = info.folder_var {
        match at {
            Some(folder) => command.env(var, folder),
            None => command.env_remove(var),
        };
    }
    output_within(&mut command, Duration::from_secs(60))
}

/// How a command for the folder `at` is pointed at it, to run by hand.
fn pointed(info: &Info, at: Option<&Path>) -> String {
    match (info.folder_var, at) {
        (Some(var), Some(folder)) => format!("{var}='{}' ", folder.display()),
        _ => String::new(),
    }
}

/// Whether `agent` runs this copy's MCP server: in every one of its
/// folders, another copy's in one, or none. Compared as files, so a link to
/// this binary, as one on the `PATH`, is it.
pub fn connection(agent: &dyn Agent, home: &Path) -> Connection {
    let real = |path: &Path| std::fs::canonicalize(path).ok();
    let me = std::env::current_exe().ok().and_then(|exe| real(&exe));
    let mine = |command: &String| me.is_some() && real(Path::new(command)) == me;
    let commands: Vec<Option<String>> = agent
        .folders(home)
        .iter()
        .map(|folder| agent.mcp_command(folder, home))
        .collect();
    if commands
        .iter()
        .all(|command| command.as_ref().is_some_and(mine))
    {
        Connection::Connected
    } else if commands.iter().flatten().any(|command| !mine(command)) {
        Connection::Outdated
    } else {
        Connection::Available
    }
}

/// The `PATH` of the person's interactive login shell.
fn login_path() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or("/bin/zsh".to_owned());
    let mut command = Command::new(shell);
    command.args(["-ilc", "printf '__path__=%s' \"$PATH\""]);
    let output = output_within(&mut command, Duration::from_secs(10)).ok()?;
    let said = String::from_utf8_lossy(&output.stdout);
    let path = said.rsplit_once("__path__=")?.1.trim();
    (output.status.success() && !path.is_empty()).then(|| path.to_owned())
}
