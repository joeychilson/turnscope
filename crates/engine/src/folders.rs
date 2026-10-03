//! Where each agent keeps its history and its sign-ins: its folders.
//!
//! An agent keeps both in one folder of its own: Claude Code in `~/.claude`,
//! noting which account is signed in in `~/.claude.json` and keeping the
//! sign-in itself in the Keychain; Codex in `~/.codex`; OpenCode in `~/.local/share/opencode`;
//! Pi in `~/.pi/agent`; Grok Build in `~/.grok`. Claude Code, Codex and Pi
//! can each be pointed at another folder, by the variable
//! [`Agent::folder_variable`] names, and that is how a second account of one
//! subscription is kept on one Mac: each folder holds a sign-in and a history
//! of its own. So both are read from every folder, and what was used in a
//! folder drew on the account signed in there ([`crate::limits`]).
//!
//! **Found.** Beside an agent's own folder, one named as a second account's
//! usually is, `~/.claude-<name>` or `~/.codex-<name>`, is read without being
//! asked for once the agent has written its sign-in there: Claude Code's
//! `.claude.json`, which it keeps in the folder it is pointed at, and
//! Codex's `auth.json`. The file tells a folder the agent uses from one only
//! named alike, as a copy of `~/.claude` kept aside has no `.claude.json` in
//! it, and history read from a copy would be put down to the copy. Pi's
//! folders follow no naming, so only its own is read. A folder found that is
//! another folder already read, through a symbolic link, is read once.
//!
//! Finding folders is a look at the home directory's entries, made with each
//! look for sign-ins.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::agent::Agent;

/// A folder an agent keeps its history and sign-ins in.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Folder {
    /// The agent whose folder it is.
    pub agent: Agent,
    /// Where it is: an absolute path.
    pub path: PathBuf,
    /// How it came to be read.
    pub origin: FolderOrigin,
}

/// How a folder came to be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FolderOrigin {
    /// The agent's own, where it keeps everything unless pointed elsewhere.
    Own,
    /// Found beside the agent's own, named as a second account's folder is
    /// and holding its sign-in.
    Found,
}

impl Folder {
    /// Whether it is the agent's own folder, where the agent keeps
    /// everything unless pointed elsewhere.
    pub(crate) fn is_own(&self) -> bool {
        self.origin == FolderOrigin::Own
    }
}

/// `agent`'s own folder under `home`.
pub(crate) fn own(agent: Agent, home: &Path) -> PathBuf {
    match agent {
        Agent::ClaudeCode => home.join(".claude"),
        Agent::Codex => home.join(".codex"),
        Agent::OpenCode => home.join(".local/share/opencode"),
        Agent::Pi => home.join(".pi/agent"),
        Agent::Grok => home.join(".grok"),
    }
}

/// How a second folder of `agent` is named in the home directory, and the
/// file the agent writes there once signed in: `None` for an agent whose
/// folders follow no naming.
fn named(agent: Agent) -> Option<(&'static str, &'static str)> {
    match agent {
        Agent::ClaudeCode => Some((".claude-", ".claude.json")),
        Agent::Codex => Some((".codex-", "auth.json")),
        Agent::OpenCode | Agent::Pi | Agent::Grok => None,
    }
}

/// Every folder to read now: each agent's own and those found in `home`, by
/// agent and then path, each once.
pub(crate) fn read(home: &Path) -> Vec<Folder> {
    let mut folders: Vec<Folder> = Agent::ALL
        .into_iter()
        .map(|agent| Folder {
            agent,
            path: own(agent, home),
            origin: FolderOrigin::Own,
        })
        .chain(found(home))
        .collect();
    // One folder reached by two paths, as through a symbolic link, is read
    // once, as the first of them: its own, then found.
    let mut seen = HashSet::new();
    folders.retain(|folder| {
        let resolved = std::fs::canonicalize(&folder.path).unwrap_or_else(|_| folder.path.clone());
        seen.insert((folder.agent, resolved))
    });
    folders.sort();
    folders
}

/// The folders in `home` named as a second folder of an agent is, holding
/// the file the agent writes there once signed in.
fn found(home: &Path) -> Vec<Folder> {
    // A home directory that can't be listed has none to find.
    let Ok(entries) = std::fs::read_dir(home) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        for agent in Agent::ALL {
            let Some((prefix, signed)) = named(agent) else {
                continue;
            };
            let path = entry.path();
            if name.len() > prefix.len()
                && name.starts_with(prefix)
                && path.is_dir()
                && path.join(signed).is_file()
            {
                found.push(Folder {
                    agent,
                    path,
                    origin: FolderOrigin::Found,
                });
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Folder, FolderOrigin, read};
    use crate::agent::Agent;

    fn folders(home: &Path) -> Vec<(Agent, PathBuf, FolderOrigin)> {
        read(home)
            .into_iter()
            .map(
                |Folder {
                     agent,
                     path,
                     origin,
                 }| {
                    (
                        agent,
                        path.strip_prefix(home).unwrap().to_path_buf(),
                        origin,
                    )
                },
            )
            .collect()
    }

    #[test]
    fn a_second_folder_is_found_once_its_agent_has_signed_in_there() {
        let home = tempfile::tempdir().unwrap();
        let make = |path: &str| std::fs::create_dir_all(home.path().join(path)).unwrap();
        let touch = |path: &str| std::fs::write(home.path().join(path), "{}").unwrap();
        // Claude Code pointed at ~/.claude-work has signed in there; a copy
        // of ~/.claude kept aside holds history and no sign-in; Codex pointed
        // at ~/.codex-personal has signed in; a folder only named alike, and
        // one named with nothing after the dash, are neither.
        make(".claude-work/projects");
        touch(".claude-work/.claude.json");
        make(".claude-backup/projects");
        make(".codex-personal/sessions");
        touch(".codex-personal/auth.json");
        make(".claude-");
        touch(".claude-/.claude.json");
        std::fs::write(home.path().join(".claude-notes"), "not a folder").unwrap();

        let own = |agent: Agent, path: &str| (agent, PathBuf::from(path), FolderOrigin::Own);
        let found = |agent: Agent, path: &str| (agent, PathBuf::from(path), FolderOrigin::Found);
        assert_eq!(
            folders(home.path()),
            [
                own(Agent::ClaudeCode, ".claude"),
                found(Agent::ClaudeCode, ".claude-work"),
                own(Agent::Codex, ".codex"),
                found(Agent::Codex, ".codex-personal"),
                own(Agent::OpenCode, ".local/share/opencode"),
                own(Agent::Pi, ".pi/agent"),
                own(Agent::Grok, ".grok"),
            ]
        );
    }

    #[test]
    fn a_folder_reached_by_a_link_to_another_is_read_once() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".claude")).unwrap();
        std::fs::write(home.path().join(".claude/.claude.json"), "{}").unwrap();
        std::os::unix::fs::symlink(
            home.path().join(".claude"),
            home.path().join(".claude-main"),
        )
        .unwrap();
        let claude: Vec<(Agent, PathBuf, FolderOrigin)> = folders(home.path())
            .into_iter()
            .filter(|(agent, _, _)| *agent == Agent::ClaudeCode)
            .collect();
        assert_eq!(
            claude,
            [(
                Agent::ClaudeCode,
                PathBuf::from(".claude"),
                FolderOrigin::Own
            )]
        );
    }
}
