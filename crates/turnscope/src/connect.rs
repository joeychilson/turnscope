//! Adding Turnscope's MCP server to an agent, or removing it, with the
//! agent's own command, as a person would in a terminal: Turnscope never
//! writes an agent's files.

use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use crate::agents::{Agent, Info};
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
    if temporary(&exe) {
        return Err(Error::Failed(format!(
            "Turnscope's engine runs from {}, which goes away when its disk image is ejected or its drive is removed. Put Turnscope in Applications, quit it, and open it from there to connect",
            exe.display()
        )));
    }
    let exe = exe.to_string_lossy();
    let path = login_path();
    for (n, folder) in agent.folders(home).iter().enumerate() {
        let at = (n > 0).then_some(folder.as_path());
        let by_hand = || {
            format!(
                "{}{} {} '{exe}' mcp",
                pointed(info, at, home),
                info.command,
                info.mcp_add.join(" ")
            )
        };
        let add = |exe: &str| {
            let args = [info.mcp_add, &[exe, "mcp"]].concat();
            run(info, &args, path.as_deref(), at, home)
        };
        let earlier = agent.mcp_command(folder, home);
        if let Some(remove) = info.mcp_remove {
            // Nothing to remove is fine.
            let _ = run(info, remove, path.as_deref(), at, home);
        }
        let failed = match add(&exe) {
            Ok(output) if output.status.success() => continue,
            Ok(output) => format!(
                "{} couldn't add the server: {}. To add it by hand, run: {}",
                info.name,
                String::from_utf8_lossy(&output.stderr).trim(),
                by_hand()
            ),
            Err(error) => format!(
                "couldn't run {}: {error}. Is {} installed? To add the server by hand, run: {}",
                info.command,
                info.name,
                by_hand()
            ),
        };
        if let (Some(_), Some(earlier)) = (info.mcp_remove, earlier) {
            let _ = add(&earlier);
        }
        return Err(Error::Failed(failed));
    }
    Ok(())
}

/// Whether `exe` is on a disk image or a drive, which `/Volumes` holds, or
/// in the read-only copy macOS runs a downloaded app from (App
/// Translocation): an agent connected to it would lose it once that's gone.
fn temporary(exe: &Path) -> bool {
    exe.starts_with("/Volumes") || exe.to_string_lossy().contains("/AppTranslocation/")
}

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
        match run(info, remove, path.as_deref(), at, home) {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                return Err(Error::Failed(format!(
                    "{} couldn't remove the server: {}. To remove it by hand, run: {}{} {}",
                    info.name,
                    String::from_utf8_lossy(&output.stderr).trim(),
                    pointed(info, at, home),
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
    command.env("HOME", home);
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

/// How a command is pointed at `home`, when it isn't the person's own, and
/// at the folder `at`, to run by hand.
fn pointed(info: &Info, at: Option<&Path>, home: &Path) -> String {
    let own = std::env::var_os("HOME").is_some_and(|own| Path::new(&own) == home);
    let mut pointed = match own {
        true => String::new(),
        false => format!("HOME='{}' ", home.display()),
    };
    if let (Some(var), Some(folder)) = (info.folder_var, at) {
        pointed += &format!("{var}='{}' ", folder.display());
    }
    pointed
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

/// An app started from Finder runs with a bare `PATH`, so the agent's
/// command is found with the `PATH` of the person's interactive login shell,
/// in their own home: on 2026-09-30, `zsh -lc` found none of four agents,
/// and `zsh -ilc` all four, in 0.15 s.
fn login_path() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or("/bin/zsh".to_owned());
    let mut command = Command::new(shell);
    command.args(["-ilc", "printf '__path__=%s' \"$PATH\""]);
    let output = output_within(&mut command, Duration::from_secs(10)).ok()?;
    if !output.status.success() {
        return None;
    }
    let said = String::from_utf8_lossy(&output.stdout);
    let path = said.rsplit_once("__path__=")?.1.trim();
    (!path.is_empty()).then(|| path.to_owned())
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
                    set.get(OsStr::new("HOME")),
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
        for agent in crate::agents::ALL {
            let info = agent.info();
            // How it's pointed at a second account's folder, where it can be.
            let folder = info
                .folder_var
                .map(|var| format!("{var}='/made-up/home/second' "))
                .unwrap_or_default();
            assert_eq!(super::pointed(info, None, home), "HOME='/made-up/home' ");
            assert_eq!(
                super::pointed(info, Some(&second), home),
                format!("HOME='/made-up/home' {folder}")
            );
            // In the person's own home, nothing names it.
            if let Some(own) = std::env::var_os("HOME") {
                let own = Path::new(&own);
                assert_eq!(super::pointed(info, None, own), "");
                assert_eq!(super::pointed(info, Some(&second), own), folder);
            }
        }
    }

    #[test]
    fn an_engine_on_a_disk_image_a_drive_or_translocated_is_temporary() {
        for (exe, temporary) in [
            (
                "/Volumes/Turnscope/Turnscope.app/Contents/Helpers/turnscope",
                true,
            ),
            (
                "/private/var/folders/ab/T/AppTranslocation/0A1B/d/Turnscope.app/Contents/Helpers/turnscope",
                true,
            ),
            (
                "/Applications/Turnscope.app/Contents/Helpers/turnscope",
                false,
            ),
            ("/Users/me/Volumes/target/release/turnscope", false),
        ] {
            assert_eq!(super::temporary(Path::new(exe)), temporary, "{exe}");
        }
    }
}
