//! Adding Turnscope's MCP server to agents, as the app's Connect does and
//! `turnscope connect <agent>` does from a shell.
//!
//! **Status.** Whether an agent has the server is read from where it keeps
//! its MCP servers, every time it is asked, as those files are small and
//! agents change them: Claude Code's `~/.claude.json`, Codex's
//! `~/.codex/config.toml`, OpenCode's `~/.config/opencode/opencode.json`,
//! Pi's `~/.pi/agent/mcp.json`, and Grok's `~/.grok/config.toml`. An agent
//! is installed when its folder is there. Claude Code, Codex and Pi can be
//! pointed at another folder ([`Agent::folder_variable`]), and their own
//! commands then add servers there, so a folder the variable names is
//! looked at in place of their own, for both: Claude Code keeps
//! `.claude.json` in such a folder rather than in the home (seen
//! 2026-10-03, `claude mcp add --scope user` with `CLAUDE_CONFIG_DIR`
//! set). An agent has the server when its entry named `turnscope` runs this
//! binary; one that runs another, as a copy since moved or a build from a
//! checkout, is out of date. Pi has taken MCP servers since 1.0, with `pi
//! mcp add`; an older Pi's connecting fails, saying why.
//!
//! **Connecting.** Turnscope never writes an agent's files itself: the
//! person's click runs the agent's own command for adding an MCP server,
//! which knows its format, as they would in a terminal. An app started from
//! Finder has a bare `PATH`, so the command is found, and run, with the
//! `PATH` the person's shell has in a terminal: an interactive login shell's,
//! since agents' installers add their folders in `~/.zshrc`, which a login
//! shell alone doesn't read (on this Mac on 2026-09-30, `zsh -lc` found none
//! of Claude Code, Codex, OpenCode and Grok Build, and `zsh -ilc` all four, in
//! 0.15 s). Run with that `PATH`, an agent installed as a Node script finds
//! `node` too. Neither the shell nor the command may hang the request: each
//! is stopped, with everything it started, after [`SHELL_WAIT`] and
//! [`COMMAND_WAIT`]. An entry already there is replaced, so connecting again
//! points the agent at this copy.

use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Serialize;
use turnscope_engine::Agent;

use crate::Failure;

/// What the server is called in every agent's configuration.
const NAME: &str = "turnscope";

/// How long the person's shell may take to say its `PATH`.
const SHELL_WAIT: Duration = Duration::from_secs(10);

/// How long an agent's own command may take, which may start Node.
const COMMAND_WAIT: Duration = Duration::from_secs(60);

/// What the shell writes before its `PATH`, so that whatever its
/// configuration prints is told apart from it.
const MARK: &str = "__turnscope_path__=";

/// An agent Turnscope can serve, and whether it does.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Link {
    /// The agent's id, as the engine names it: "claude-code".
    pub(crate) id: &'static str,
    pub(crate) name: &'static str,
    /// Its logo, within the app's logos: "providers/anthropic".
    pub(crate) logo: &'static str,
    pub(crate) status: Status,
}

/// The logo `agent` is shown with, within the app's logos: its own where it
/// has one, else its maker's.
pub(crate) fn logo(agent: Agent) -> &'static str {
    match agent {
        Agent::ClaudeCode => "providers/anthropic",
        Agent::Codex => "providers/openai",
        Agent::OpenCode => "providers/opencode",
        Agent::Pi => "agents/pi",
        Agent::Grok => "providers/xai",
    }
}

/// Whether an agent has the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Status {
    /// It runs this binary.
    Connected,
    /// It runs another copy of Turnscope.
    Outdated,
    /// It has no entry for Turnscope.
    Available,
}

/// Where agents keep their MCP servers: the home directory, and the folders
/// agents' variables point them at instead of their own.
pub(crate) struct Configs {
    home: PathBuf,
    moved: Vec<(Agent, PathBuf)>,
}

impl Configs {
    /// The home `home`, with the folders the environment's variables
    /// ([`Agent::folder_variable`]) point agents at, where they are set.
    pub(crate) fn of(home: &Path) -> Configs {
        let moved = Agent::ALL
            .into_iter()
            .filter_map(|agent| {
                let folder = std::env::var_os(agent.folder_variable()?)?;
                (!folder.is_empty()).then(|| (agent, PathBuf::from(folder)))
            })
            .collect();
        Configs::at(home, moved)
    }

    /// The home `home`, with agents pointed at the folders `moved` names.
    fn at(home: &Path, moved: Vec<(Agent, PathBuf)>) -> Configs {
        Configs {
            home: home.to_path_buf(),
            moved,
        }
    }

    /// The folder `agent` is pointed at, where its variable names one.
    fn moved(&self, agent: Agent) -> Option<&Path> {
        self.moved
            .iter()
            .find(|(moved, _)| *moved == agent)
            .map(|(_, folder)| folder.as_path())
    }

    /// Where `agent` keeps its files: the folder it is pointed at, or else
    /// its own in the home.
    fn folder(&self, agent: Agent) -> PathBuf {
        self.moved(agent).map_or_else(
            || {
                self.home.join(match agent {
                    Agent::ClaudeCode => ".claude",
                    Agent::Codex => ".codex",
                    Agent::OpenCode => ".config/opencode",
                    Agent::Pi => ".pi/agent",
                    Agent::Grok => ".grok",
                })
            },
            Path::to_path_buf,
        )
    }
}

/// How to connect an agent: named as the engine names it.
struct Connector {
    agent: Agent,
    /// Its command, and the arguments that add a stdio server named
    /// [`NAME`] before the command to run it.
    command: &'static str,
    add: &'static [&'static str],
    /// The arguments that remove that entry, when adding doesn't replace it.
    remove: Option<&'static [&'static str]>,
    /// The command its entry runs, as its configuration says.
    registered: fn(&Configs) -> Option<String>,
}

const CONNECTORS: [Connector; 5] = [
    Connector {
        agent: Agent::ClaudeCode,
        command: "claude",
        add: &["mcp", "add", "--scope", "user", NAME, "--"],
        remove: Some(&["mcp", "remove", "--scope", "user", NAME]),
        registered: claude,
    },
    Connector {
        agent: Agent::Codex,
        command: "codex",
        add: &["mcp", "add", NAME, "--"],
        remove: Some(&["mcp", "remove", NAME]),
        registered: codex,
    },
    Connector {
        agent: Agent::OpenCode,
        command: "opencode",
        add: &["mcp", "add", "--global", NAME, "--"],
        remove: None,
        registered: opencode,
    },
    Connector {
        agent: Agent::Grok,
        command: "grok",
        add: &["mcp", "add", "--scope", "user", NAME],
        remove: None,
        registered: grok,
    },
    Connector {
        agent: Agent::Pi,
        command: "pi",
        add: &["mcp", "add", NAME, "--"],
        remove: None,
        registered: pi,
    },
];

/// The agents installed in the home of `configs`, and whether each runs
/// `binary` as its Turnscope server.
pub(crate) fn links(configs: &Configs, binary: &Path) -> Vec<Link> {
    let binary = binary.to_string_lossy();
    CONNECTORS
        .iter()
        .filter(|connector| configs.folder(connector.agent).is_dir())
        .map(|connector| Link {
            id: connector.agent.key(),
            name: connector.agent.name(),
            logo: logo(connector.agent),
            status: match (connector.registered)(configs) {
                Some(command) if command == binary => Status::Connected,
                Some(_) => Status::Outdated,
                None => Status::Available,
            },
        })
        .collect()
}

/// Add the server that runs `binary` to the agent `id`, with its own
/// command, replacing any entry already there.
///
/// # Errors
///
/// Says why when the agent isn't one Turnscope connects, the person's shell
/// can't say its `PATH`, the agent's command isn't on it, or it fails.
pub(crate) fn connect(id: &str, binary: &Path) -> Result<(), String> {
    let shell = std::env::var_os("SHELL").map_or_else(|| PathBuf::from("/bin/zsh"), PathBuf::from);
    connect_with(id, binary, &shell)
}

/// [`connect`], with the `PATH` the shell `shell` has.
fn connect_with(id: &str, binary: &Path, shell: &Path) -> Result<(), String> {
    let agent = CONNECTORS
        .iter()
        .find(|connector| connector.agent.key() == id)
        .ok_or_else(|| format!("Turnscope can't connect {id}"))?;
    let path = shell_path(shell)?;
    let program = locate(agent.command, &path)
        .ok_or_else(|| format!("{} isn't on your shell's PATH", agent.command))?;
    let command = |args: &[&str]| {
        let mut command = Command::new(&program);
        command.args(args).env("PATH", &path);
        command
    };
    if let Some(remove) = agent.remove {
        // Nothing to remove is no failure: adding says what went wrong.
        let _removed = run(&mut command(remove), COMMAND_WAIT);
    }
    let mut add = command(agent.add);
    add.arg(binary).arg("mcp");
    let output = run(&mut add, COMMAND_WAIT)
        .map_err(|error| format!("{} mcp add {error}", agent.command))?;
    if output.status.success() {
        return Ok(());
    }
    // Its last words, on standard error or, as some write them, its output.
    let last = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_owned)
    };
    Err(last(&output.stderr)
        .or_else(|| last(&output.stdout))
        .unwrap_or_else(|| format!("{} mcp add failed", agent.command)))
}

/// `turnscope connect [<agent>]`: connect one agent, or say which have it.
pub(crate) fn command(mut args: impl Iterator<Item = String>) -> Result<(), Failure> {
    let binary = std::env::current_exe()?;
    let home = crate::usual_home().ok_or_else(|| Failure::Usage("HOME is not set".to_owned()))?;
    match (args.next(), args.next()) {
        (None, _) => {
            let mut said = String::new();
            for link in links(&Configs::of(&home), &binary) {
                let status = match link.status {
                    Status::Connected => "connected",
                    Status::Outdated => "runs another copy of Turnscope",
                    Status::Available => "not connected",
                };
                said.push_str(&format!("{} ({}): {status}\n", link.name, link.id));
            }
            crate::show(&said)
        }
        (Some(agent), None) => {
            connect(&agent, &binary).map_err(Failure::Failed)?;
            crate::show(&format!("Connected {agent}.\n"))
        }
        (Some(_), Some(extra)) => Err(Failure::Usage(format!(
            "connect takes one agent, not {extra}"
        ))),
    }
}

/// The `PATH` the person's shell `shell` has in a terminal: an interactive
/// login shell's.
///
/// # Errors
///
/// Says why when the shell can't be run, takes longer than [`SHELL_WAIT`],
/// or says no `PATH`.
fn shell_path(shell: &Path) -> Result<String, String> {
    let said = format!("printf '{MARK}%s\\n' \"$PATH\"");
    let output = run(Command::new(shell).args(["-ilc", &said]), SHELL_WAIT)
        .map_err(|error| format!("Your shell, {}, {error}", shell.display()))?;
    marked_path(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| format!("Your shell, {}, said no PATH", shell.display()))
}

/// The `PATH` in what the shell wrote: the rest of the last line that
/// begins with [`MARK`].
fn marked_path(written: &str) -> Option<String> {
    written
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(MARK))
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
}

/// The executable file `command` names in the first folder of `path` that
/// has one.
fn locate(command: &str, path: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|folder| folder.is_absolute())
        .map(|folder| folder.join(command))
        .find(|file| {
            std::fs::metadata(file)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

/// Run `command`, with nothing on its input and what it writes kept, for
/// `wait` at the most: then it is stopped, with every process it started,
/// as they share its process group.
///
/// # Errors
///
/// Says why, in words that follow the command's name, when it can't be
/// started or is stopped.
fn run(command: &mut Command, wait: Duration) -> Result<Output, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| format!("couldn't be run: {error}"))?;
    // Each pipe is read on a thread of its own, so a full one never holds
    // the command up; a process it left running may keep one open, so what
    // was read is waited for only briefly once it ends.
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + wait;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                // The group's id is the command's own.
                let _stopped = Command::new("/bin/kill")
                    .args(["-KILL", &format!("-{}", child.id())])
                    .stderr(Stdio::null())
                    .status();
                let _killed = child.kill();
                let _reaped = child.wait();
                return Err(format!("took over {}s, and was stopped", wait.as_secs()));
            }
            Err(error) => return Err(format!("couldn't be waited for: {error}")),
        }
    };
    let taken = |read: mpsc::Receiver<Vec<u8>>| {
        read.recv_timeout(Duration::from_secs(1))
            .unwrap_or_default()
    };
    Ok(Output {
        status,
        stdout: taken(stdout),
        stderr: taken(stderr),
    })
}

/// Everything `pipe` holds, read on a thread of its own, once it closes.
fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _read = pipe.read_to_end(&mut bytes);
            let _sent = sender.send(bytes);
        });
    }
    receiver
}

/// The command Claude Code's user-wide entry runs.
fn claude(configs: &Configs) -> Option<String> {
    let folder = configs.moved(Agent::ClaudeCode).unwrap_or(&configs.home);
    servers_command(&folder.join(".claude.json"))
}

/// The command Codex's entry runs.
fn codex(configs: &Configs) -> Option<String> {
    toml_command(&configs.folder(Agent::Codex).join("config.toml"))
}

/// The command OpenCode's global entry runs, where its configuration keeps
/// servers under `mcp`, or `mcp.servers`.
fn opencode(configs: &Configs) -> Option<String> {
    let json = read_json(&configs.folder(Agent::OpenCode).join("opencode.json"))?;
    let mcp = json.get("mcp")?;
    let entry = mcp
        .get(NAME)
        .or_else(|| mcp.get("servers").and_then(|servers| servers.get(NAME)))?;
    entry.get("command")?.get(0)?.as_str().map(str::to_owned)
}

/// The command Grok's user-wide entry runs.
fn grok(configs: &Configs) -> Option<String> {
    toml_command(&configs.folder(Agent::Grok).join("config.toml"))
}

/// The command Pi's user-wide entry runs.
fn pi(configs: &Configs) -> Option<String> {
    servers_command(&configs.folder(Agent::Pi).join("mcp.json"))
}

/// The `command` of Turnscope's entry under `mcpServers` in the JSON file at
/// `path`, as Claude Code and Pi keep their servers.
fn servers_command(path: &Path) -> Option<String> {
    let json = read_json(path)?;
    json.get("mcpServers")?
        .get(NAME)?
        .get("command")?
        .as_str()
        .map(str::to_owned)
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// The `command` string of the TOML table of Turnscope's server, as Codex
/// and Grok Build name it, in the file at `path`, read line by line: these
/// tables are written by the agents' own commands, one key to a line.
fn toml_command(path: &Path) -> Option<String> {
    let header = format!("[mcp_servers.{NAME}]");
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text
        .lines()
        .skip_while(|line| line.trim() != header)
        .skip(1);
    lines
        .by_ref()
        .take_while(|line| !line.trim_start().starts_with('['))
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "command").then(|| value.trim().trim_matches('"').to_owned())
        })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use turnscope_engine::Agent;

    use super::{Configs, Status, connect_with, links, locate, marked_path, run};

    /// An executable script at `path` in `folder`.
    fn script(folder: &Path, name: &str, text: &str) -> std::path::PathBuf {
        let path = folder.join(name);
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn an_agent_is_connected_with_its_own_command_on_the_shells_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let said = dir.path().join("said");
        // Claude Code, as a script that says what it was asked, and which
        // finds another command only on the shell's PATH, as a Node script
        // finds node.
        script(&bin, "helper", "#!/bin/sh\necho helped\n");
        script(
            &bin,
            "claude",
            &format!(
                "#!/bin/sh\nhelper >/dev/null || exit 3\necho \"$*\" >> {}\n",
                said.display()
            ),
        );
        // A shell whose configuration prints before the PATH, as some do.
        let shell = script(
            dir.path(),
            "shell",
            &format!(
                "#!/bin/sh\necho 'Welcome back'\nprintf '__turnscope_path__=%s\\n' '/nowhere:{}'\n",
                bin.display()
            ),
        );
        connect_with(
            "claude-code",
            Path::new("/Applications/Turnscope.app/Contents/Helpers/turnscope"),
            &shell,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&said).unwrap(),
            "mcp remove --scope user turnscope\n\
             mcp add --scope user turnscope -- /Applications/Turnscope.app/Contents/Helpers/turnscope mcp\n"
        );
        // An agent not on the shell's PATH is said to be missing.
        assert_eq!(
            connect_with("codex", Path::new("/t"), &shell),
            Err("codex isn't on your shell's PATH".to_owned())
        );
    }

    #[test]
    fn a_failing_command_says_its_last_words() {
        let dir = tempfile::tempdir().unwrap();
        script(
            dir.path(),
            "codex",
            "#!/bin/sh\necho 'usage: codex mcp add' >&2\necho 'Error: no such option --' >&2\nexit 2\n",
        );
        let shell = script(
            dir.path(),
            "shell",
            &format!(
                "#!/bin/sh\nprintf '__turnscope_path__=%s\\n' '{}'\n",
                dir.path().display()
            ),
        );
        assert_eq!(
            connect_with("codex", Path::new("/t"), &shell),
            Err("Error: no such option --".to_owned())
        );
    }

    #[test]
    fn a_command_that_hangs_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("pid");
        let started = Instant::now();
        let ran = run(
            Command::new("/bin/sh").args([
                "-c",
                &format!("sleep 30 & echo $! > {}; wait", pid.display()),
            ]),
            Duration::from_secs(1),
        );
        assert_eq!(ran.err().as_deref(), Some("took over 1s, and was stopped"));
        assert!(started.elapsed() < Duration::from_secs(5));
        // What it started was stopped with it.
        let pid = std::fs::read_to_string(pid).unwrap();
        let alive = Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(!alive.success(), "sleep {} outlived it", pid.trim());
    }

    #[test]
    fn the_path_is_what_follows_the_last_mark() {
        assert_eq!(
            marked_path("motd\n__turnscope_path__=/a:/b\n"),
            Some("/a:/b".to_owned())
        );
        assert_eq!(marked_path("__turnscope_path__=\n"), None);
        assert_eq!(marked_path("no path here\n"), None);
        // Only an executable file is a command; relative folders are passed.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plain"), "").unwrap();
        let tool = script(dir.path(), "tool", "#!/bin/sh\n");
        let path = format!("relative:{}", dir.path().display());
        assert_eq!(locate("tool", &path), Some(tool));
        assert_eq!(locate("plain", &path), None);
    }

    fn write(home: &Path, path: &str, text: &str) {
        let path = home.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn an_agent_has_turnscope_when_its_entry_runs_this_copy() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let this = Path::new("/Applications/Turnscope.app/Contents/Helpers/turnscope");
        // Claude Code runs this copy.
        write(
            home,
            ".claude.json",
            r#"{"mcpServers": {"turnscope": {"type": "stdio", "command": "/Applications/Turnscope.app/Contents/Helpers/turnscope", "args": ["mcp"]}}}"#,
        );
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        // OpenCode, keeping servers under mcp.servers, runs an older copy.
        write(
            home,
            ".config/opencode/opencode.json",
            r#"{"mcp": {"servers": {"turnscope": {"type": "local", "command": ["/Applications/Turnscope.app/Contents/MacOS/turnscope", "mcp"]}}}}"#,
        );
        // Grok Build, as `grok mcp add` writes it, runs this one.
        write(
            home,
            ".grok/config.toml",
            "[mcp_servers.other]\ncommand = \"x\"\n\n[mcp_servers.turnscope]\ncommand = \"/Applications/Turnscope.app/Contents/Helpers/turnscope\"\nargs = [\"mcp\"]\n",
        );
        // Codex has other servers, and not this one.
        write(
            home,
            ".codex/config.toml",
            "[mcp_servers.node_repl]\ncommand = \"node\"\n",
        );
        // Pi, as `pi mcp add` 1.0.1 writes it, runs this one.
        write(
            home,
            ".pi/agent/mcp.json",
            "{\n  \"mcpServers\": {\n    \"turnscope\": {\n      \"command\": \"/Applications/Turnscope.app/Contents/Helpers/turnscope\",\n      \"args\": [\n        \"mcp\"\n      ]\n    }\n  }\n}\n",
        );
        let status: Vec<(&str, Status)> = links(&Configs::at(home, Vec::new()), this)
            .into_iter()
            .map(|link| (link.id, link.status))
            .collect();
        assert_eq!(
            status,
            [
                ("claude-code", Status::Connected),
                ("codex", Status::Available),
                ("opencode", Status::Outdated),
                ("grok", Status::Connected),
                ("pi", Status::Connected),
            ]
        );
    }

    #[test]
    fn an_agent_pointed_at_another_folder_is_read_there() {
        // Claude Code, Codex and Pi pointed at folders of their own, each
        // with an entry running this copy as its own command writes it, and
        // none of their folders in the home.
        let home = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let this = Path::new("/t");
        let folder = |name: &str| elsewhere.path().join(name);
        write(
            &folder("claude"),
            ".claude.json",
            r#"{"mcpServers": {"turnscope": {"type": "stdio", "command": "/t", "args": ["mcp"]}}}"#,
        );
        write(
            &folder("codex"),
            "config.toml",
            "[mcp_servers.turnscope]\ncommand = \"/t\"\n",
        );
        write(
            &folder("pi"),
            "mcp.json",
            r#"{"mcpServers": {"turnscope": {"command": "/t", "args": ["mcp"]}}}"#,
        );
        let moved = vec![
            (Agent::ClaudeCode, folder("claude")),
            (Agent::Codex, folder("codex")),
            (Agent::Pi, folder("pi")),
        ];
        let status = |moved: Vec<(Agent, std::path::PathBuf)>| {
            links(&Configs::at(home.path(), moved), this)
                .into_iter()
                .map(|link| (link.id, link.status))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            status(moved.clone()),
            [
                ("claude-code", Status::Connected),
                ("codex", Status::Connected),
                ("pi", Status::Connected),
            ]
        );
        // Not pointed anywhere, none is installed.
        assert_eq!(status(Vec::new()), []);
        // Their own folders there, with no entry, are passed over for the
        // folders they are pointed at.
        for own in [".claude", ".codex", ".pi/agent"] {
            std::fs::create_dir_all(home.path().join(own)).unwrap();
        }
        write(home.path(), ".claude.json", r#"{"mcpServers": {}}"#);
        assert_eq!(
            status(moved),
            [
                ("claude-code", Status::Connected),
                ("codex", Status::Connected),
                ("pi", Status::Connected),
            ]
        );
        assert_eq!(
            status(Vec::new()),
            [
                ("claude-code", Status::Available),
                ("codex", Status::Available),
                ("pi", Status::Available),
            ]
        );
    }

    #[test]
    fn an_agent_not_installed_is_not_listed() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            links(&Configs::at(home.path(), Vec::new()), Path::new("/t")),
            []
        );
    }
}
