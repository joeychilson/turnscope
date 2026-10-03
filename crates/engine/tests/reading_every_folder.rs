//! History is read from every folder an agent keeps it in: its own and a
//! second account's folder found beside it; and a session kept in another
//! folder is taken up again there.

mod history {
    pub mod claude_code;
    pub mod home;
}

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use turnscope_engine::{Agent, Folder, FolderOrigin, SessionKey, SessionQuery};

use history::claude_code::{self, response};
use history::home::{Home, write};

/// A response's usage: 10 tokens in and 5 out.
fn usage() -> Value {
    json!({"input_tokens": 10, "output_tokens": 5})
}

/// Where Claude Code keeps the log of the session `id` in `folder`.
fn claude_log(folder: &Path, id: &str) -> PathBuf {
    folder
        .join("projects/-work-ledger")
        .join(format!("{id}.jsonl"))
}

/// Every session of all time.
fn every_session() -> SessionQuery {
    SessionQuery {
        empty: true,
        limit: 10,
        ..SessionQuery::default()
    }
}

/// Each session listed, with the folder it is kept in, when not its agent's
/// own, and whether its transcript can still be read.
fn listed(engine: &turnscope_engine::Engine) -> Vec<(String, Option<PathBuf>, bool)> {
    let mut sessions: Vec<(String, Option<PathBuf>, bool)> = engine
        .sessions(&every_session())
        .unwrap()
        .items
        .into_iter()
        .map(|row| (row.key.native().to_owned(), row.folder, row.present))
        .collect();
    sessions.sort();
    sessions
}

#[test]
fn a_second_accounts_folder_is_read_and_its_sessions_taken_up_there() {
    let home = Home::new();
    let own = home.path().join(".claude");
    let work = home.path().join(".claude-work");
    let copy = home.path().join(".claude-backup");
    write(
        &claude_code::session(home.path(), "own"),
        &[response(
            "own",
            None,
            "2026-09-27T10:00:00Z",
            "1",
            "claude-opus-5",
            usage(),
        )],
    );
    // Claude Code pointed at ~/.claude-work by CLAUDE_CONFIG_DIR keeps its
    // history there, and notes which account is signed in in .claude.json
    // inside it.
    write(
        &claude_log(&work, "work"),
        &[response(
            "work",
            None,
            "2026-09-27T11:00:00Z",
            "2",
            "claude-opus-5",
            usage(),
        )],
    );
    std::fs::write(work.join(".claude.json"), "{}").unwrap();
    // A copy of ~/.claude kept aside holds history and no sign-in: not a
    // folder Claude Code uses.
    write(
        &claude_log(&copy, "copy"),
        &[response(
            "copy",
            None,
            "2026-09-27T12:00:00Z",
            "3",
            "claude-opus-5",
            usage(),
        )],
    );

    let engine = home.scanned();
    let claude: Vec<Folder> = engine
        .folders()
        .into_iter()
        .filter(|folder| folder.agent == Agent::ClaudeCode)
        .collect();
    assert_eq!(
        claude,
        [
            Folder {
                agent: Agent::ClaudeCode,
                path: own.clone(),
                origin: FolderOrigin::Own,
            },
            Folder {
                agent: Agent::ClaudeCode,
                path: work.clone(),
                origin: FolderOrigin::Found,
            },
        ]
    );
    assert_eq!(
        listed(&engine),
        [
            ("own".to_owned(), None, true),
            ("work".to_owned(), Some(work.clone()), true),
        ]
    );
    // Taken up again with Claude Code pointed at its folder.
    let row = engine
        .sessions(&every_session())
        .unwrap()
        .items
        .into_iter()
        .find(|row| row.key == SessionKey::new(Agent::ClaudeCode, "work"))
        .unwrap();
    assert_eq!(
        row.resume().unwrap(),
        format!(
            "cd /work/ledger && CLAUDE_CONFIG_DIR={} claude --resume work",
            work.display()
        )
    );
}
