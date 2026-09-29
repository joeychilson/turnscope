//! An agent picks up another's work: a session's handoff, by its id or as
//! the latest in a folder, says what was asked, what changed, what failed
//! and how to resume it; and its conversation reads a page at a time.

mod history {
    pub mod files;
    pub mod server;
    pub mod sessions;
}

use serde_json::json;
use turnscope_engine::Agent;
use turnscope_mcp::Caller;

use history::server::{answer, call, refusal};
use history::sessions::{CLAUDE, EXPLORE, server};

#[test]
fn a_handoff_says_what_was_asked_changed_and_failed_and_how_to_resume() {
    let (server, home, _data) = server();
    let id = format!("claude-code:{CLAUDE}");
    let by_id = answer(&server, "get_session", json!({"session": id}));
    // The latest session in its folder, from any agent, is the same one.
    let latest = answer(
        &server,
        "get_session",
        json!({"latest_in": home.path().join("work/ledger/src")}),
    );
    assert_eq!(latest.data, by_id.data);
    let data = &by_id.data;
    assert_eq!(data["session"]["id"], id);
    assert_eq!(
        data["first_request"],
        json!({"entry": 0, "at": "2026-09-14T07:00:00-05:00", "text": "Add a file watcher for the ledger"})
    );
    assert_eq!(data["last_request"]["text"], "Fix the failing migration");
    assert_eq!(
        data["last_reply"]["text"],
        "The migration now runs before the watcher starts."
    );
    // Claude Code's account of the edit: "-    watch();", then two added.
    assert_eq!(
        data["files"],
        json!([{"path": "src/migration.rs", "added": 2, "removed": 1, "created": false,
                "deleted": false, "moved_to": null, "changes": 1}])
    );
    // At the time its result came.
    assert_eq!(
        data["commands"],
        json!([{"command": "cargo test", "at": "2026-09-14T07:00:20-05:00", "exit": 101, "failed": true}])
    );
    assert_eq!(
        (
            data["commands_run"].clone(),
            data["commands_failed"].clone()
        ),
        (json!(1), json!(1))
    );
    assert_eq!(data["plan"], json!([]));
    assert_eq!(data["subagents"][0]["id"], format!("claude-code:{EXPLORE}"));
    assert_eq!(data["subagents"][0]["title"], "Find the ledger's tests");
    let folder = home.path().join("work/ledger");
    assert_eq!(
        data["resume"],
        format!("cd {} && claude --resume {CLAUDE}", folder.display())
    );
    let said = by_id.said();
    for line in [
        "\"Add a file watcher for the ledger\" \u{b7} Claude Code \u{b7} ~/work/ledger \u{b7} ended ",
        "First asked: \"Add a file watcher for the ledger\"",
        "Last asked: \"Fix the failing migration\"",
        "Last reply: \"The migration now runs before the watcher starts.\"",
        "Changed: src/migration.rs +2 \u{2212}1",
        "Ran 1 command, 1 failed. Last: cargo test (failed, exit 101)",
        "Subagents: \"Find the ledger's tests\" (claude-opus-5).",
        // 13,900 tokens and $0.0765, its subagent's with its own.
        "In all it used 13.9K tokens, $0.08 at list prices.",
    ] {
        assert!(said.contains(line), "{line:?} in {said}");
    }
    assert!(said.ends_with(&format!(
        "Resume it: cd {} && claude --resume {CLAUDE}",
        folder.display()
    )));
}

#[test]
fn a_handoff_is_asked_for_by_id_or_folder_but_not_both() {
    let (server, home, _data) = server();
    let refused = refusal(
        &server,
        "get_session",
        json!({"session": format!("claude-code:{CLAUDE}"), "latest_in": "~/work"}),
    );
    assert!(refused.contains("not both"), "{refused}");
    // Asked of nothing by a caller it knows nothing of, which session is
    // this one isn't known.
    let refused = refusal(&server, "get_session", json!({}));
    assert!(
        refused.starts_with("Which session is this one isn't known"),
        "{refused}"
    );
    let refused = refusal(
        &server,
        "get_session",
        json!({"latest_in": home.path().join("elsewhere")}),
    );
    assert!(
        refused.starts_with("No session other than this one ran in "),
        "{refused}"
    );
    // A subagent has a handoff too, and no one resumes it.
    let subagent = answer(
        &server,
        "get_session",
        json!({"session": format!("claude-code:{EXPLORE}")}),
    );
    assert_eq!(subagent.data["resume"], serde_json::Value::Null);
    assert!(
        subagent
            .said()
            .ends_with("It's a subagent, which no one resumes.")
    );
}

#[test]
fn asked_of_nothing_a_handoff_is_of_this_session_and_latest_in_passes_over_it() {
    let (server, home, _data) = server();
    let ledger = home.path().join("work/ledger");
    // Claude Code, working in the ledger, whose latest session there is
    // the Claude session: this one.
    let server = server.with_caller(Caller {
        agent: Some(Agent::ClaudeCode),
        folder: Some(ledger.to_string_lossy().into_owned()),
        ..Caller::default()
    });
    let this = answer(&server, "get_session", json!({}));
    assert_eq!(this.data["session"]["id"], format!("claude-code:{CLAUDE}"));
    // 13,900 tokens and $0.0765, its subagent's with its own.
    assert!(
        this.said()
            .contains("In all it used 13.9K tokens, $0.08 at list prices."),
        "{}",
        this.said()
    );
    // The ledger holds no other session, so the latest there is none.
    let refused = refusal(&server, "get_session", json!({"latest_in": ledger}));
    assert!(
        refused.starts_with("No session other than this one ran in "),
        "{refused}"
    );
}

#[test]
fn a_conversation_is_read_a_page_at_a_time() {
    let (server, _home, _data) = server();
    let session = format!("claude-code:{CLAUDE}");
    let page = call(&server, "read_session", json!({"session": session}));
    assert!(!page.failed, "{}", page.text);
    // A page is text alone, with no figures beside it.
    assert_eq!(page.data, serde_json::Value::Null);
    // The edit's arguments are JSON, whose keys come in the order the build
    // keeps them, so its line is read apart.
    let (before, rest) = page.text.split_once("[4] tool Edit").unwrap();
    let (edit, after) = rest.split_once("\n\n").unwrap();
    assert_eq!(
        format!("{before}{after}"),
        format!(
            "{session} \u{b7} Add a file watcher for the ledger \u{b7} 6 entries\n\
             Entries 0 to 5.\n\n\
             [0] user \u{b7} 2026-09-14 07:00\nAdd a file watcher for the ledger\n\n\
             [1] assistant (claude-opus-5) \u{b7} 2026-09-14 07:00\nI'll run the tests first.\n\n\
             [2] tool Bash \u{b7} 2026-09-14 07:00: {{\"command\":\"cargo test\"}} \u{2192} failed\n\n\
             [3] user \u{b7} 2026-09-14 07:01\nFix the failing migration\n\n\
             [5] assistant (claude-opus-5) \u{b7} 2026-09-14 07:01\nThe migration now runs before the watcher starts."
        )
    );
    let (when, arguments) = edit.split_once(": ").unwrap();
    assert_eq!(when, " \u{b7} 2026-09-14 07:01");
    let arguments: serde_json::Value = serde_json::from_str(arguments).unwrap();
    assert_eq!(arguments["old_string"], "    watch();");
    assert_eq!(arguments["new_string"], "    migrate();\n    watch();");
    // Back over the four entries said, not the six there are.
    let said = call(
        &server,
        "read_session",
        json!({"session": session, "detail": "conversation", "offset": -3}),
    );
    assert!(
        said.text.contains("Entries 1 to 5.") && !said.text.contains("[2]"),
        "{}",
        said.text
    );
    let full = call(
        &server,
        "read_session",
        json!({"session": session, "detail": "full", "find": "FAILED"}),
    );
    assert!(
        full.text.contains("1 mention \"failed\": 2"),
        "{}",
        full.text
    );
    assert!(full.text.contains("error: 2 tests failed"), "{}", full.text);
    let first = call(
        &server,
        "read_session",
        json!({"session": session, "limit": 2}),
    );
    assert!(
        first
            .text
            .ends_with("4 more to show; continue with offset 2."),
        "{}",
        first.text
    );
    // Entry 3 is "Fix the failing migration", and from its fifth character
    // on, "the failing migration".
    let slice = call(
        &server,
        "read_session",
        json!({"session": session, "entry": 3.0, "from": 4.0}),
    );
    let slice: serde_json::Value = serde_json::from_str(&slice.text).unwrap();
    assert_eq!(slice["text"], "the failing migration");
    let refused = refusal(
        &server,
        "read_session",
        json!({"session": session, "limit": 1.5}),
    );
    assert_eq!(
        refused,
        "The arguments are not what this tool takes: invalid type: floating point `1.5`, \
         expected u32."
    );
}
