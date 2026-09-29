//! The subagents a Claude Code session runs, each with a log of its own and
//! a description beside it.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::claude_code;
use super::home::write;

/// Where Claude Code keeps the log of `session`'s subagent `agent` under
/// `home`: in a directory named for the session, beside the session's log.
pub fn log(home: &Path, session: &str, agent: &str) -> PathBuf {
    claude_code::session(home, session)
        .with_extension("")
        .join("subagents")
        .join(format!("agent-{agent}.jsonl"))
}

/// Write `description` beside the log of `session`'s subagent `agent`, as
/// Claude Code does when it starts the subagent.
pub fn describe(home: &Path, session: &str, agent: &str, description: Value) {
    let path = log(home, session, agent).with_extension("meta.json");
    write(&path, &[description]);
}
