//! Adding Turnscope's MCP server to an agent, or removing it, with the
//! agent's own command, as a person would in a terminal: Turnscope never
//! writes an agent's files.

use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use crate::agents::{Agent, Info};
use crate::platform;
use crate::process::output_within;
use crate::status::Connection;
use crate::{Error, Result};

/// Add the server to `agent`, in each of its folders, replacing any it has,
/// so it runs `engine`, the `turnscope` binary: the command line itself, or
/// the one in the app. When the agent had to remove one first and then
/// refused the new one, the one it had is put back.
pub fn connect(agent: &dyn Agent, home: &Path, engine: &Path) -> Result<()> {
    let info = agent.info();
    // Its real path, so a link to it on `PATH` isn't what agents run.
    let exe = std::fs::canonicalize(engine)?;
    if let Some(why) = platform::temporary_install(&exe) {
        return Err(Error::Failed(why));
    }
    let exe = exe.to_string_lossy();
    let path = (!platform::on_path(info.command))
        .then(platform::login_path)
        .flatten();
    for (n, folder) in agent.folders(home).iter().enumerate() {
        let at = (n > 0).then_some(folder.as_path());
        let add = |exe: &str| {
            let args = [info.mcp_add, &[exe, "mcp"]].concat();
            run(info, &args, path.as_deref(), at, home)
        };
        let earlier = agent.mcp_command(folder, home);
        if let Some(remove) = info.mcp_remove {
            // Nothing to remove is fine.
            let _ = run(info, remove, path.as_deref(), at, home);
        }
        let args = [info.mcp_add, &[&exe, "mcp"]].concat();
        let Some(failed) = failed(info, "add", add(&exe), &args, at, home) else {
            continue;
        };
        if let (Some(_), Some(earlier)) = (info.mcp_remove, earlier) {
            let _ = add(&earlier);
        }
        return Err(Error::Failed(failed));
    }
    Ok(())
}

pub fn disconnect(agent: &dyn Agent, home: &Path) -> Result<()> {
    let info = agent.info();
    let Some(remove) = info.mcp_remove else {
        return Err(Error::Failed(format!(
            "{} has no command Turnscope can run to remove a server: remove `turnscope` from its MCP settings by hand.",
            info.name
        )));
    };
    let path = (!platform::on_path(info.command))
        .then(platform::login_path)
        .flatten();
    for (n, folder) in agent.folders(home).iter().enumerate() {
        if agent.mcp_command(folder, home).is_none() {
            continue;
        }
        let at = (n > 0).then_some(folder.as_path());
        let ran = run(info, remove, path.as_deref(), at, home);
        if let Some(failed) = failed(info, "remove", ran, remove, at, home) {
            return Err(Error::Failed(failed));
        }
    }
    Ok(())
}

/// Why running `info`'s command to `act` on the server (`add` or `remove`)
/// in the folder `at` failed, as `ran` says, and how to do it by hand: the
/// command with `args`. None when it worked.
fn failed(
    info: &Info,
    act: &str,
    ran: std::io::Result<Output>,
    args: &[&str],
    at: Option<&Path>,
    home: &Path,
) -> Option<String> {
    let by_hand = by_hand(info, args, at, home);
    match ran {
        Ok(output) if output.status.success() => None,
        Ok(output) => Some(format!(
            "{} couldn't {act} the server: {}. To {act} it by hand, run: {by_hand}",
            info.name,
            error_output(&output),
        )),
        Err(error) => Some(format!(
            "couldn't run {}: {error}. Is {} installed? To {act} the server by hand, run: {by_hand}",
            info.command, info.name
        )),
    }
}

fn error_output(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        return stderr.to_owned();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = stdout.trim();
    if !stdout.is_empty() {
        return stdout.to_owned();
    }
    format!("exited with {}", output.status)
}

fn run(
    info: &Info,
    args: &[&str],
    path: Option<&str>,
    at: Option<&Path>,
    home: &Path,
) -> std::io::Result<Output> {
    let mut command = command(info, args, path, at, home);
    output_within(&mut command, Duration::from_secs(60))
}

fn command(
    info: &Info,
    args: &[&str],
    path: Option<&str>,
    at: Option<&Path>,
    home: &Path,
) -> Command {
    let mut command = Command::new(info.command);
    command.args(args);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    // The home Turnscope was given, over the process's own: with `--home` at
    // a copy, the agent added the server to the person's own settings.
    platform::set_home(&mut command, home);
    // A second account's folder, as `~/.claude-work`, keeps its own
    // settings: the agent is pointed at it as it's run for that account.
    if let Some(var) = info.folder_var {
        match at {
            Some(folder) => command.env(var, folder),
            None => command.env_remove(var),
        };
    }
    command
}

/// The command to run by hand, pointed at `home` when it isn't the person's
/// own, and at `at` when it's a second account's folder.
fn by_hand(info: &Info, args: &[&str], at: Option<&Path>, home: &Path) -> String {
    let home_text = home.to_string_lossy();
    let folder_text = at.map(|folder| folder.to_string_lossy());
    let mut env = Vec::new();
    if !platform::is_own_home(home) {
        env.push((platform::home_var(), home_text.as_ref()));
    }
    if let (Some(var), Some(folder)) = (info.folder_var, folder_text.as_deref()) {
        env.push((var, folder));
    }
    platform::shell_command(&env, info.command, args)
}

/// Whether `agent` runs `engine`, the `turnscope` binary, plus the differing
/// command path when `Connection::Outdated`. Compared as files, so a link to
/// it, as one on the `PATH`, is the same copy.
pub fn connection(agent: &dyn Agent, home: &Path, engine: &Path) -> (Connection, Option<String>) {
    let real = |path: &Path| std::fs::canonicalize(path).ok();
    let me = real(engine);
    let commands: Vec<Option<String>> = agent
        .folders(home)
        .iter()
        .map(|folder| agent.mcp_command(folder, home))
        .collect();
    let other = commands
        .iter()
        .flatten()
        .find(|command| me.is_none() || real(Path::new(command)) != me);
    match other {
        Some(other) => (Connection::Outdated, Some(other.clone())),
        None if commands.iter().all(Option::is_some) => (Connection::Connected, None),
        None => (Connection::Available, None),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::path::Path;

    use serde_json::json;

    use crate::status::Connection;

    /// Connected once every folder of the agent runs this binary, a link to
    /// it counting; one that runs another says which.
    #[cfg(unix)]
    #[test]
    fn an_agent_is_connected_once_each_of_its_folders_runs_this_turnscope() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let (engine, other, link) = (home.join("ts"), home.join("old-ts"), home.join("link"));
        std::fs::write(&engine, "").unwrap();
        std::fs::write(&other, "").unwrap();
        std::os::unix::fs::symlink(&engine, &link).unwrap();
        let work = home.join(".claude-work");
        std::fs::create_dir(&work).unwrap();
        let runs = |settings: &Path, command: Option<&Path>| {
            let servers = command.map(|command| json!({"turnscope": {"command": command}}));
            let settings = settings.join(".claude.json");
            std::fs::write(settings, json!({ "mcpServers": servers }).to_string()).unwrap();
        };
        let agent = crate::agents::by_id("claude-code").unwrap();
        let connection = || super::connection(agent, home, &engine);
        runs(home, Some(&link));
        runs(&work, None);
        assert_eq!(connection(), (Connection::Available, None));
        runs(&work, Some(&other));
        let configured = other.to_string_lossy().into_owned();
        assert_eq!(connection(), (Connection::Outdated, Some(configured)));
        runs(&work, Some(&engine));
        assert_eq!(connection(), (Connection::Connected, None));
    }

    #[test]
    fn an_agents_command_is_run_in_the_home_turnscope_was_given() {
        let home = Path::new("/made-up/home");
        let second = home.join("second");
        let path = Some("/made-up/bin");
        for agent in crate::agents::ALL {
            let info = agent.info();
            let args = [info.mcp_add, &["/made-up/turnscope", "mcp"]].concat();
            // For its own folder, then for a second account's.
            for at in [None, Some(second.as_path())] {
                let command = super::command(info, &args, path, at, home);
                assert_eq!(command.get_program(), info.command);
                assert_eq!(command.get_args().collect::<Vec<_>>(), args);
                let set: HashMap<&OsStr, Option<&OsStr>> = command.get_envs().collect();
                assert_eq!(
                    set.get(OsStr::new(crate::platform::home_var())),
                    Some(&Some(home.as_os_str())),
                    "{}",
                    info.id
                );
                assert_eq!(
                    set.get(OsStr::new("PATH")),
                    Some(&Some(OsStr::new("/made-up/bin")))
                );
                if let Some(var) = info.folder_var {
                    assert_eq!(
                        set.get(OsStr::new(var)),
                        Some(&at.map(Path::as_os_str)),
                        "{}",
                        info.id
                    );
                }
            }
        }
    }

    #[test]
    fn a_command_to_run_by_hand_names_a_home_that_isnt_the_persons_own() {
        let home = Path::new("/made-up/home");
        let second = home.join("second");
        let home_var = crate::platform::home_var();
        for agent in crate::agents::ALL {
            let info = agent.info();
            let args = ["mcp", "remove", "turnscope"];
            let tail = format!("{} mcp remove turnscope", info.command);
            // How it's pointed at a second account's folder, where it can be.
            let folder = info
                .folder_var
                .map(|var| format!("{var}='/made-up/home/second' "))
                .unwrap_or_default();
            assert_eq!(
                super::by_hand(info, &args, None, home),
                format!("{home_var}='/made-up/home' {tail}")
            );
            assert_eq!(
                super::by_hand(info, &args, Some(&second), home),
                format!("{home_var}='/made-up/home' {folder}{tail}")
            );
            // In the person's own home, nothing names it.
            let own = crate::platform::home();
            if !own.as_os_str().is_empty() {
                assert_eq!(super::by_hand(info, &args, None, &own), tail);
                assert_eq!(
                    super::by_hand(info, &args, Some(&second), &own),
                    format!("{folder}{tail}")
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn failed_falls_back_to_stdout_when_stderr_is_empty() {
        let info = crate::agents::by_id("claude-code").unwrap().info();
        let status = std::process::Command::new("/usr/bin/false")
            .status()
            .unwrap();
        let home = Path::new("/Users/O'Connor");
        let args = [info.mcp_add, &["/Users/O'Connor/turnscope", "mcp"]].concat();
        let stdout_only = std::process::Output {
            status,
            stdout: b"already configured\n".to_vec(),
            stderr: Vec::new(),
        };
        let message = super::failed(info, "add", Ok(stdout_only), &args, None, home).unwrap();
        assert!(
            message.contains("couldn't add the server: already configured."),
            "{message}"
        );
        assert!(
            message.contains(r"HOME='/Users/O'\''Connor'")
                && message.contains(r"'/Users/O'\''Connor/turnscope'"),
            "{message}"
        );
        let neither = std::process::Output {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        let message = super::failed(info, "add", Ok(neither), &args, None, home).unwrap();
        assert!(
            !message.contains(": .") && message.contains("exited with"),
            "{message}"
        );
    }
}
