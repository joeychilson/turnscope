//! A home with a Claude Code session in a repository, with a subagent it
//! ran, another in which nothing happened, and a Codex thread in a scratch
//! folder, all in September 2026.

use std::path::Path;

use serde_json::{Value, json};
use tempfile::TempDir;
use turnscope_engine::Engine;
use turnscope_mcp::Server;

use super::files::write;

pub const CLAUDE: &str = "0f6e3f6a-713c-4bad-8f6d-f04fe41bbd84";
pub const EXPLORE: &str = "a4b8c2d6e0f135790";
pub const CODEX: &str = "01a06671-ecc2";
pub const EMPTY: &str = "5d1c7e2a-0b8e-4f3c-9a41-2c6f0e9b7d10";

/// A server over the history, telling times in Chicago, and the home and
/// data directory it reads.
pub fn server() -> (Server, TempDir, TempDir) {
    let (home, data) = history();
    let engine = Engine::open(data.path(), home.path()).unwrap();
    engine.scan().unwrap();
    let server = Server::as_it_stands(engine, "America/Chicago").unwrap();
    (server, home, data)
}

/// A home with the history, and a data directory in which none of it is
/// read yet.
fn history() -> (TempDir, TempDir) {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let repository = home.path().join("work/ledger");
    std::fs::create_dir_all(repository.join(".git")).unwrap();
    write(
        &home
            .path()
            .join(format!(".claude/projects/-work-ledger/{CLAUDE}.jsonl")),
        &claude(&repository),
    );
    write(
        &home.path().join(format!(
            ".claude/projects/-work-ledger/{CLAUDE}/subagents/agent-{EXPLORE}.jsonl"
        )),
        &explore(&repository),
    );
    // Claude Code describes each subagent beside its log.
    write(
        &home.path().join(format!(
            ".claude/projects/-work-ledger/{CLAUDE}/subagents/agent-{EXPLORE}.meta.json"
        )),
        &[json!({"agentType": "Explore", "description": "Find the ledger's tests"})],
    );
    write(
        &home
            .path()
            .join(format!(".claude/projects/-work-ledger/{EMPTY}.jsonl")),
        &empty(&repository),
    );
    write(
        &home.path().join(format!(
            ".codex/sessions/2026/09/15/rollout-2026-09-15T09-00-00-{CODEX}.jsonl"
        )),
        &codex(&home.path().join("scratch")),
    );
    (home, data)
}

/// A Claude Code session on 14 September: two prompts, two replies, a
/// command that failed and an edit of the migration.
///
/// At Claude Opus 5's prices of $5 input, $25 output and $0.50 cache reads a
/// million tokens, the first reply costs (1,000 × 5 + 2,000 × 25 + 10,000 ×
/// 0.5) / 1,000,000 = $0.06, and the second (200 × 5 + 400 × 25) / 1,000,000
/// = $0.011.
fn claude(cwd: &Path) -> Vec<Value> {
    let cwd = cwd.to_string_lossy();
    let user = |time: &str, content: Value| {
        json!({"type": "user", "sessionId": CLAUDE, "timestamp": time, "cwd": cwd,
               "message": {"role": "user", "content": content}})
    };
    let assistant = |time: &str, id: &str, usage: Value, content: Value| {
        json!({"type": "assistant", "sessionId": CLAUDE, "timestamp": time, "cwd": cwd,
               "requestId": format!("req_{id}"),
               "message": {"id": format!("msg_{id}"), "model": "claude-opus-5", "role": "assistant",
                           "usage": usage, "content": content}})
    };
    let migration = format!("{cwd}/src/migration.rs");
    let mut edited = user(
        "2026-09-14T12:01:20.000Z",
        json!([{"type": "tool_result", "tool_use_id": "toolu_2",
                "content": format!("The file {migration} has been updated successfully.")}]),
    );
    // Claude Code's account of the edit: one line for two.
    edited["toolUseResult"] = json!({"filePath": migration, "structuredPatch": [
        {"oldStart": 3, "oldLines": 1, "newStart": 3, "newLines": 2,
         "lines": ["-    watch();", "+    migrate();", "+    watch();"]}]});
    vec![
        user(
            "2026-09-14T12:00:00.000Z",
            json!("Add a file watcher for the ledger"),
        ),
        assistant(
            "2026-09-14T12:00:10.000Z",
            "1",
            json!({"input_tokens": 1_000, "output_tokens": 2_000, "cache_read_input_tokens": 10_000}),
            json!([{"type": "text", "text": "I'll run the tests first."},
                   {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "cargo test"}}]),
        ),
        user(
            "2026-09-14T12:00:20.000Z",
            json!([{"type": "tool_result", "tool_use_id": "toolu_1",
                    "content": "Exit code 101\nerror: 2 tests failed", "is_error": true}]),
        ),
        user(
            "2026-09-14T12:01:00.000Z",
            json!("Fix the failing migration"),
        ),
        assistant(
            "2026-09-14T12:01:10.000Z",
            "2",
            json!({"input_tokens": 200, "output_tokens": 400}),
            json!([{"type": "tool_use", "id": "toolu_2", "name": "Edit",
                    "input": {"file_path": migration, "old_string": "    watch();",
                              "new_string": "    migrate();\n    watch();"}}]),
        ),
        edited,
        assistant(
            "2026-09-14T12:01:30.000Z",
            "2",
            json!({"input_tokens": 200, "output_tokens": 400}),
            json!([{"type": "text", "text": "The migration now runs before the watcher starts."}]),
        ),
    ]
}

/// The Explore subagent the Claude Code session ran, asked where the ledger
/// is watched: one reply, which at Claude Opus 5's prices costs (100 × 5 +
/// 200 × 25) / 1,000,000 = $0.0055.
fn explore(cwd: &Path) -> Vec<Value> {
    let cwd = cwd.to_string_lossy();
    vec![
        json!({"type": "user", "sessionId": CLAUDE, "agentId": EXPLORE, "isSidechain": true,
               "timestamp": "2026-09-14T12:00:30.000Z", "cwd": cwd,
               "message": {"role": "user", "content": "Find where the ledger is watched"}}),
        json!({"type": "assistant", "sessionId": CLAUDE, "agentId": EXPLORE, "isSidechain": true,
               "timestamp": "2026-09-14T12:00:40.000Z", "cwd": cwd, "requestId": "req_3",
               "message": {"id": "msg_3", "model": "claude-opus-5", "role": "assistant",
                           "usage": {"input_tokens": 100, "output_tokens": 200},
                           "content": [{"type": "text", "text": "Nothing watches it yet."}]}}),
    ]
}

/// A Claude Code session opened on 16 September and closed with nothing
/// said: only a note of Claude Code's own.
fn empty(cwd: &Path) -> Vec<Value> {
    vec![json!({"type": "user", "isMeta": true, "sessionId": EMPTY,
               "timestamp": "2026-09-16T12:00:00.000Z", "cwd": cwd.to_string_lossy(),
               "message": {"role": "user", "content": "<local-command-caveat>Caveat: the messages below were generated by the user while running local commands.</local-command-caveat>"}})]
}

/// A Codex thread on 15 September, with a model no catalog prices.
fn codex(cwd: &Path) -> Vec<Value> {
    let item = |time: &str, payload: Value| json!({"type": "response_item", "timestamp": time, "payload": payload});
    vec![
        json!({"type": "session_meta", "timestamp": "2026-09-15T09:00:00.000Z",
               "payload": {"id": CODEX, "cwd": cwd.to_string_lossy(), "model_provider": "openai"}}),
        json!({"type": "turn_context", "timestamp": "2026-09-15T09:00:00.500Z",
               "payload": {"model": "gpt-unlisted-test", "cwd": cwd.to_string_lossy()}}),
        item(
            "2026-09-15T09:00:01.000Z",
            json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Why does the migration fail?"}]}),
        ),
        item(
            "2026-09-15T09:00:09.000Z",
            json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Because the schema changed."}]}),
        ),
        json!({"type": "token_usage_record", "timestamp": "2026-09-15T09:00:09.500Z",
               "payload": {"thread_id": CODEX, "response_id": "resp_1",
                           "usage": {"input_tokens": 3_000, "cached_input_tokens": 2_400, "output_tokens": 40,
                                     "reasoning_output_tokens": 0, "total_tokens": 3_040}}}),
    ]
}
