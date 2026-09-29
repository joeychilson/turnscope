//! Who is asking: the agent that started the server, where it works, and
//! which of its sign-ins it runs with.
//!
//! An agent starts `turnscope mcp` itself, so the server inherits the
//! agent's working folder, some of its environment, and the agent as its
//! parent process. From them it works out the three things that make "my
//! limit" and "this session" mean something, with no ids passed and nothing
//! set up. Which account those make yours is [`crate::accounts`]'s.
//!
//! **Which agent.** A marker in the environment where the agent sets one,
//! and otherwise the nearest ancestor process named for an agent. Measured
//! 2026-09-29 on the agents installed here:
//!
//! - Claude Code 2.1.284 starts a stdio server with its own environment and
//!   `CLAUDECODE=1`, `CLAUDE_PROJECT_DIR` (the folder it works in) and
//!   `CLAUDE_CODE_SESSION_ID` (the session it was started in) added, and
//!   sets `AI_AGENT` to `claude-code_<version>_…` for what it runs.
//! - Codex passes a server only a fixed list of variables (`HOME`, `PATH`,
//!   `SHELL`, `USER`, `LOGNAME`, `TMPDIR`, `TZ`, `LANG`, `LC_ALL`, `TERM` and
//!   `__CF_USER_TEXT_ENCODING`), so it is known by the process that started
//!   the server, `codex`, and its `CODEX_HOME`, when not in the server's
//!   environment, is read from that process's.
//! - OpenCode starts servers through the MCP SDK's stdio transport and sets
//!   no marker of its own that a server sees; it is known by its process,
//!   `opencode`.
//! - Grok Build keeps its home in `GROK_HOME`, and is known by its process,
//!   `grok`.
//! - Pi has no MCP client of its own; started by one of its extensions, it
//!   is known by its process, `pi`.
//!
//! A process is known by its command's name, or for one a runtime such as
//! `node` runs, by its script's. The server's own ancestors are looked at,
//! at most eight of them, through `/bin/ps`, which reads no more than the
//! system's process table.
//!
//! **Which folder.** Where the agent says it works (Claude Code's
//! `CLAUDE_PROJECT_DIR`), and otherwise the server's working folder, which
//! is the agent's: its latest session there is "this session".
//!
//! **Which sign-ins.** The folder the agent keeps its sign-in and history
//! in, where its environment names one other than its own: the variable
//! the engine knows it by ([`Agent::folder_variable`]), `CLAUDE_CONFIG_DIR`,
//! `CODEX_HOME` or `PI_CODING_AGENT_DIR`, as a second account of one
//! subscription is kept; or Grok Build's `GROK_HOME`, which Turnscope
//! doesn't follow, so that an account there isn't taken for the one in
//! `~/.grok`. Which account that folder holds is [`crate::accounts`]'s.

use std::path::{Path, PathBuf};
use std::process::Command;

use turnscope_engine::Agent;

/// The most ancestors looked at for an agent's process.
const MOST_ANCESTORS: usize = 8;

/// Who is asking, as far as the server can tell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Caller {
    /// The agent that started the server.
    pub agent: Option<Agent>,
    /// The folder it works in: absolute.
    pub folder: Option<String>,
    /// The folder its sign-ins and history are kept in, where its
    /// environment names one other than its own: the variable that names
    /// it, and the folder as the variable gives it.
    pub config: Option<(String, String)>,
    /// The agent's own id for the session it started the server in, where
    /// it says: a session that has since been cleared or left may not be
    /// the one it works in now, so this only settles which of the folder's
    /// sessions is "this session" when several are running.
    pub session: Option<String>,
}

impl Caller {
    /// Who started this process: from its environment, its working folder
    /// and its ancestors. What can't be told is left unknown.
    pub fn detect() -> Caller {
        let ancestors = ancestors();
        let commands: Vec<&str> = ancestors
            .iter()
            .map(|(_, command)| command.as_str())
            .collect();
        let agent_process = commands
            .iter()
            .position(|command| agent_named(command).is_some())
            .map(|index| ancestors[index].0);
        // The agent's own environment, read only if the server's lacks a
        // variable looked for, and then once.
        let mut theirs: Option<Option<String>> = None;
        let variable = |name: &str| -> Option<String> {
            if let Some(value) = std::env::var(name).ok().filter(|value| !value.is_empty()) {
                return Some(value);
            }
            let line = theirs
                .get_or_insert_with(|| agent_process.and_then(environment_of))
                .as_deref()?;
            variable_in(line, name)
        };
        let working = std::env::current_dir().ok();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Caller::identify(variable, working.as_deref(), home.as_deref(), &commands)
    }

    /// Who is asking, from `variable`, which gives the value of an
    /// environment variable, the server's `working` folder, the `home`
    /// folder defaults are under, and the command lines of the server's
    /// ancestors, nearest first.
    pub fn identify(
        mut variable: impl FnMut(&str) -> Option<String>,
        working: Option<&Path>,
        home: Option<&Path>,
        ancestors: &[&str],
    ) -> Caller {
        let marked = if variable("CLAUDECODE").as_deref() == Some("1")
            || variable("AI_AGENT").is_some_and(|agent| agent.starts_with("claude-code"))
        {
            Some(Agent::ClaudeCode)
        } else {
            None
        };
        let agent = marked.or_else(|| {
            ancestors
                .iter()
                .take(MOST_ANCESTORS)
                .find_map(|command| agent_named(command))
        });
        let said_folder = match agent {
            Some(Agent::ClaudeCode) => variable("CLAUDE_PROJECT_DIR"),
            _ => None,
        };
        let folder = said_folder
            .map(PathBuf::from)
            .or_else(|| working.map(Path::to_path_buf))
            .filter(|folder| folder.is_absolute())
            .map(|folder| folder.to_string_lossy().into_owned());
        let config = agent.and_then(|agent| {
            let (name, own) = folder_variable(agent)?;
            let value = variable(name)?;
            let given = PathBuf::from(value.trim_end_matches('/'));
            let usual = home.map(|home| home.join(own));
            (usual.as_deref() != Some(given.as_path())).then(|| (name.to_owned(), value))
        });
        let session = match agent {
            Some(Agent::ClaudeCode) => variable("CLAUDE_CODE_SESSION_ID"),
            _ => None,
        };
        Caller {
            agent,
            folder,
            config,
            session,
        }
    }
}

/// The variable that points `agent` at a folder other than its own, and
/// that folder's place under the home folder; `None` for an agent that
/// can't be pointed elsewhere.
fn folder_variable(agent: Agent) -> Option<(&'static str, &'static str)> {
    match agent {
        Agent::ClaudeCode => Some(("CLAUDE_CONFIG_DIR", ".claude")),
        Agent::Codex => Some(("CODEX_HOME", ".codex")),
        Agent::Pi => Some(("PI_CODING_AGENT_DIR", ".pi/agent")),
        Agent::Grok => Some(("GROK_HOME", ".grok")),
        Agent::OpenCode => None,
    }
}

/// The agent a process whose command line is `command` is: by its
/// command's name, or for a runtime, its script's.
fn agent_named(command: &str) -> Option<Agent> {
    let mut words = command.split_whitespace();
    let first = name_of(words.next()?);
    let name = match first.as_str() {
        "node" | "bun" | "deno" | "python" | "python3" => name_of(words.next()?),
        _ => first,
    };
    // A login shell's name opens with a dash, and a versioned build names
    // itself after its version, as `grok-1.0.41-macos-aarch64`.
    let name = name.trim_start_matches('-');
    let is = |agent: &str| name == agent || name.starts_with(&format!("{agent}-"));
    if is("claude") {
        Some(Agent::ClaudeCode)
    } else if is("codex") {
        Some(Agent::Codex)
    } else if is("opencode") {
        Some(Agent::OpenCode)
    } else if is("grok") {
        Some(Agent::Grok)
    } else if name == "pi" {
        Some(Agent::Pi)
    } else {
        None
    }
}

/// The last part of a path, as a command's name.
fn name_of(path: &str) -> String {
    Path::new(path).file_name().map_or_else(
        || path.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// This process's ancestors, nearest first, each its id and command line:
/// none where `/bin/ps` can't be run or says nothing.
fn ancestors() -> Vec<(u32, String)> {
    let Ok(output) = Command::new("/bin/ps")
        .args(["-A", "-ww", "-o", "pid=,ppid=,args="])
        .output()
    else {
        return Vec::new();
    };
    let table: Vec<(u32, u32, String)> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let parent = words.next()?.parse().ok()?;
            Some((pid, parent, words.collect::<Vec<_>>().join(" ")))
        })
        .collect();
    let mut ancestors = Vec::new();
    let mut pid = std::os::unix::process::parent_id();
    while pid > 1 && ancestors.len() < MOST_ANCESTORS {
        let Some((_, parent, command)) = table.iter().find(|(id, _, _)| *id == pid) else {
            break;
        };
        ancestors.push((pid, command.clone()));
        pid = *parent;
    }
    ancestors
}

/// The command line and environment of the process `pid`, as `/bin/ps -E`
/// gives them for a process of the same user: its arguments and then each
/// variable as `NAME=value`, all on one line.
fn environment_of(pid: u32) -> Option<String> {
    let output = Command::new("/bin/ps")
        .args(["-E", "-ww", "-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The value of the variable `name` in a line `/bin/ps -E` wrote, where
/// variables stand one after another as `NAME=value`: what follows ` NAME=`
/// up to the next word that opens another variable, so a value with a
/// space in it is read whole unless what follows the space looks like one.
fn variable_in(line: &str, name: &str) -> Option<String> {
    let marker = format!(" {name}=");
    let start = line.rfind(&marker)? + marker.len();
    let rest = &line[start..];
    let end = rest
        .match_indices(' ')
        .map(|(at, _)| at)
        .find(|at| opens_variable(&rest[at + 1..]))
        .unwrap_or(rest.len());
    let value = rest[..end].trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Whether `text` opens with `NAME=`, a variable's name being letters,
/// digits and underscores, not starting with a digit.
fn opens_variable(text: &str) -> bool {
    let Some((name, _)) = text.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !name.starts_with(|character: char| character.is_ascii_digit())
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[cfg(test)]
mod tests {
    use turnscope_engine::Agent;

    use super::{folder_variable, variable_in};

    #[test]
    fn an_agents_folder_is_read_from_the_variable_the_engine_follows() {
        // Grok Build's is looked for though the engine follows none, so an
        // account elsewhere isn't taken for its own folder's.
        for agent in Agent::ALL {
            let looked_for = folder_variable(agent).map(|(name, _)| name);
            match agent.folder_variable() {
                Some(followed) => assert_eq!(looked_for, Some(followed), "{agent}"),
                None if agent == Agent::Grok => assert_eq!(looked_for, Some("GROK_HOME")),
                None => assert_eq!(looked_for, None, "{agent}"),
            }
        }
    }

    #[test]
    fn a_variable_is_read_from_a_line_of_ps() {
        let line = "codex --yolo TERM=xterm CODEX_HOME=/Users/joey/My Codex HOME=/Users/joey";
        assert_eq!(
            variable_in(line, "CODEX_HOME").as_deref(),
            Some("/Users/joey/My Codex")
        );
        assert_eq!(variable_in(line, "HOME").as_deref(), Some("/Users/joey"));
        assert_eq!(variable_in(line, "GROK_HOME"), None);
    }
}
