//! Claude Code's sessions, begun in /work/ledger.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// Where Claude Code keeps the log of the session `id` under `home`.
pub fn session(home: &Path, id: &str) -> PathBuf {
    home.join(".claude/projects/-work-ledger")
        .join(format!("{id}.jsonl"))
}

/// A response in `session`, as Claude Code writes it: from the subagent
/// `agent` when one made it, at `time`, the request and message named by
/// `id`, of `model` and reporting `usage`.
pub fn response(
    session: &str,
    agent: Option<&str>,
    time: &str,
    id: &str,
    model: &str,
    usage: Value,
) -> Value {
    let mut line = json!({"type": "assistant", "sessionId": session, "timestamp": time,
                          "requestId": format!("req_{id}"), "cwd": "/work/ledger",
                          "message": {"id": format!("msg_{id}"), "model": model, "usage": usage}});
    if let Some(agent) = agent {
        line["agentId"] = json!(agent);
    }
    line
}
