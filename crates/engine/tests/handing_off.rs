//! Where a session stopped, from each agent's own records: its first and
//! last requests, last reply, plan, files changed with their lines, and
//! commands run with whether they failed. Each agent's fixture holds its
//! tools' calls shaped as that agent writes them, with the lines each
//! change adds and removes worked out by hand.

mod history {
    pub mod claude_code;
    pub mod home;
    pub mod opencode;
    pub mod pi;
}

use serde_json::{Value, json};
use turnscope_engine::{Agent, Command, FileChange, Handoff, SessionKey, Step, StepStatus};

use history::home::{Home, write};
use history::{claude_code, opencode, pi};

/// A file's path, lines added and removed, whether its first change
/// created it and its last deleted it.
type Changed<'a> = (&'a str, Option<u64>, Option<u64>, bool, bool);

/// Each file changed, as [`Changed`].
fn files(handoff: &Handoff) -> Vec<Changed<'_>> {
    handoff
        .files
        .iter()
        .map(|file: &FileChange| {
            (
                file.path.as_str(),
                file.added,
                file.removed,
                file.created,
                file.deleted,
            )
        })
        .collect()
}

/// Each command, its exit code and whether it failed.
fn commands(handoff: &Handoff) -> Vec<(&str, Option<i64>, Option<bool>)> {
    handoff
        .commands
        .iter()
        .map(|command: &Command| (command.command.as_str(), command.exit, command.failed))
        .collect()
}

/// Each step of the plan and its status.
fn plan(handoff: &Handoff) -> Vec<(&str, Option<StepStatus>)> {
    handoff
        .plan
        .iter()
        .map(|step: &Step| (step.text.as_str(), step.status))
        .collect()
}

const CLAUDE: &str = "5a1e-handoff";

/// A line of Claude Code's that calls `name` with `input`, as call `id`.
fn claude_call(time: &str, id: &str, name: &str, input: Value) -> Value {
    let mut line = claude_code::response(
        CLAUDE,
        None,
        time,
        id,
        "claude-opus-5",
        json!({"input_tokens": 1, "output_tokens": 1}),
    );
    line["message"]["role"] = json!("assistant");
    line["message"]["content"] =
        json!([{"type": "tool_use", "id": format!("toolu_{id}"), "name": name, "input": input}]);
    line
}

/// The line holding call `id`'s result, `content`, a failure or not, and
/// Claude Code's own account of it where it gives one.
fn claude_result(
    time: &str,
    id: &str,
    content: &str,
    failed: bool,
    account: Option<Value>,
) -> Value {
    let mut line = json!({"type": "user", "sessionId": CLAUDE, "timestamp": time, "cwd": "/work/ledger",
                          "message": {"role": "user", "content": [{"type": "tool_result",
                              "tool_use_id": format!("toolu_{id}"), "content": content, "is_error": failed}]}});
    if let Some(account) = account {
        line["toolUseResult"] = account;
    }
    line
}

#[test]
fn a_claude_code_session_hands_off_its_commands_edits_and_tasks() {
    let home = Home::new();
    let user = |time: &str, text: &str| {
        json!({"type": "user", "sessionId": CLAUDE, "timestamp": time, "cwd": "/work/ledger",
               "message": {"role": "user", "content": text}})
    };
    let mut reply = claude_code::response(
        CLAUDE,
        None,
        "2026-09-29T10:09:00Z",
        "99",
        "claude-opus-5",
        json!({"input_tokens": 1, "output_tokens": 1}),
    );
    reply["message"]["role"] = json!("assistant");
    reply["message"]["content"] = json!([{"type": "text", "text": "Two tests still fail."}]);
    let lines = vec![
        user("2026-09-29T10:00:00Z", "Shade the hills"),
        // A command that failed, opening its result with its exit code.
        claude_call(
            "2026-09-29T10:01:00Z",
            "1",
            "Bash",
            json!({"command": "cargo test", "description": "Run the tests"}),
        ),
        claude_result(
            "2026-09-29T10:01:30Z",
            "1",
            "Exit code 101\nerror: 2 tests failed",
            true,
            None,
        ),
        user("2026-09-29T10:02:00Z", "Get the tests passing"),
        // An edit whose account holds its hunk: " a", "-b", "+B", "+c", so
        // 2 added and 1 removed.
        claude_call(
            "2026-09-29T10:03:00Z",
            "2",
            "Edit",
            json!({"file_path": "/work/ledger/src/a.rs", "old_string": "a\nb", "new_string": "a\nB\nc",
                   "replace_all": false}),
        ),
        claude_result(
            "2026-09-29T10:03:01Z",
            "2",
            "The file /work/ledger/src/a.rs has been updated successfully.",
            false,
            Some(
                json!({"filePath": "/work/ledger/src/a.rs", "oldString": "a\nb", "newString": "a\nB\nc",
                        "originalFile": "a\nb\n", "replaceAll": false, "userModified": false,
                        "structuredPatch": [{"oldStart": 1, "oldLines": 2, "newStart": 1, "newLines": 3,
                                             "lines": [" a", "-b", "+B", "+c"]}]}),
            ),
        ),
        // A new file of three lines.
        claude_call(
            "2026-09-29T10:04:00Z",
            "3",
            "Write",
            json!({"file_path": "/work/ledger/src/new.rs", "content": "x\ny\nz\n"}),
        ),
        claude_result(
            "2026-09-29T10:04:01Z",
            "3",
            "File created successfully at: /work/ledger/src/new.rs",
            false,
            Some(
                json!({"type": "create", "filePath": "/work/ledger/src/new.rs", "content": "x\ny\nz\n",
                        "structuredPatch": [], "originalFile": null, "userModified": false}),
            ),
        ),
        // An edit with no account: "two" for "2" among three lines, 1 and 1.
        claude_call(
            "2026-09-29T10:05:00Z",
            "4",
            "Edit",
            json!({"file_path": "/work/ledger/src/b.rs", "old_string": "one\ntwo\nthree",
                   "new_string": "one\n2\nthree", "replace_all": false}),
        ),
        claude_result(
            "2026-09-29T10:05:01Z",
            "4",
            "The file /work/ledger/src/b.rs has been updated successfully.",
            false,
            None,
        ),
        // An edit that failed changed nothing.
        claude_call(
            "2026-09-29T10:05:30Z",
            "5",
            "Edit",
            json!({"file_path": "/work/ledger/src/c.rs", "old_string": "x", "new_string": "y"}),
        ),
        claude_result(
            "2026-09-29T10:05:31Z",
            "5",
            "<tool_use_error>String to replace not found in file.</tool_use_error>",
            true,
            None,
        ),
        // Two tasks made, and the first done.
        claude_call(
            "2026-09-29T10:06:00Z",
            "6",
            "TaskCreate",
            json!({"subject": "Fix normals", "description": "At tile edges", "activeForm": "Fixing normals"}),
        ),
        claude_result(
            "2026-09-29T10:06:01Z",
            "6",
            "Task #1 created successfully: Fix normals",
            false,
            Some(json!({"task": {"id": "1", "subject": "Fix normals"}})),
        ),
        claude_call(
            "2026-09-29T10:06:10Z",
            "7",
            "TaskCreate",
            json!({"subject": "Snapshot test", "description": "Zoom 8", "activeForm": "Testing"}),
        ),
        claude_result(
            "2026-09-29T10:06:11Z",
            "7",
            "Task #2 created successfully: Snapshot test",
            false,
            Some(json!({"task": {"id": "2", "subject": "Snapshot test"}})),
        ),
        claude_call(
            "2026-09-29T10:07:00Z",
            "8",
            "TaskUpdate",
            json!({"taskId": "1", "status": "completed"}),
        ),
        claude_result(
            "2026-09-29T10:07:01Z",
            "8",
            "Updated task #1 status",
            false,
            Some(
                json!({"success": true, "taskId": "1", "updatedFields": ["status"],
                                  "statusChange": {"from": "pending", "to": "completed"}}),
            ),
        ),
        // A command sent to the background, whose outcome isn't known yet.
        claude_call(
            "2026-09-29T10:08:00Z",
            "9",
            "Bash",
            json!({"command": "cargo run", "description": "Run it", "run_in_background": true}),
        ),
        claude_result(
            "2026-09-29T10:08:01Z",
            "9",
            "Command running in background with ID: b1",
            false,
            None,
        ),
        // A call whose input isn't what Bash takes.
        claude_call("2026-09-29T10:08:30Z", "10", "Bash", json!({"cmd": "ls"})),
        claude_result("2026-09-29T10:08:31Z", "10", "ok", false, None),
        reply,
    ];
    write(&claude_code::session(home.path(), CLAUDE), &lines);
    let engine = home.open();
    engine.scan().unwrap();
    let handoff = engine
        .handoff(&SessionKey::new(Agent::ClaudeCode, CLAUDE))
        .unwrap();
    assert_eq!(
        handoff.first_request.as_ref().unwrap().text,
        "Shade the hills"
    );
    assert_eq!(
        handoff.last_request.as_ref().unwrap().text,
        "Get the tests passing"
    );
    assert_eq!(
        handoff.last_reply.as_ref().unwrap().text,
        "Two tests still fail."
    );
    assert_eq!(
        commands(&handoff),
        [
            ("cargo test", Some(101), Some(true)),
            ("cargo run", None, None)
        ]
    );
    assert_eq!((handoff.commands_run, handoff.commands_failed), (2, 1));
    assert_eq!(
        files(&handoff),
        [
            ("/work/ledger/src/a.rs", Some(2), Some(1), false, false),
            ("/work/ledger/src/new.rs", Some(3), Some(0), true, false),
            ("/work/ledger/src/b.rs", Some(1), Some(1), false, false),
        ]
    );
    assert_eq!(
        plan(&handoff),
        [
            ("Fix normals", Some(StepStatus::Completed)),
            ("Snapshot test", Some(StepStatus::Pending))
        ]
    );
    assert_eq!(handoff.unclear, [("Bash".to_owned(), 1)]);
}

#[test]
fn a_codex_thread_hands_off_the_commands_and_changes_it_recorded() {
    let home = Home::new();
    let thread = "01a0c0de-0000-7000-8000-00000000c0de";
    let line = |time: &str, kind: &str, payload: Value| json!({"timestamp": time, "type": kind, "payload": payload});
    let completed = |time: &str, item: Value| {
        line(
            time,
            "event_msg",
            json!({"type": "item_completed", "thread_id": thread, "item": item}),
        )
    };
    let lines = vec![
        line(
            "2026-09-29T11:00:00Z",
            "session_meta",
            json!({"id": thread, "cwd": "/work/atlas"}),
        ),
        line(
            "2026-09-29T11:00:00Z",
            "turn_context",
            json!({"model": "gpt-5.6", "cwd": "/work/atlas"}),
        ),
        line(
            "2026-09-29T11:00:01Z",
            "response_item",
            json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Shade the hills"}]}),
        ),
        // Run as a login shell's argument, and failed.
        completed(
            "2026-09-29T11:00:10Z",
            json!({"type": "CommandExecution", "id": "exec-1",
            "command": ["/bin/zsh", "-lc", "cargo test hillshade"], "cwd": "file:///work/atlas",
            "status": "failed", "exit_code": 101, "stdout": "", "stderr": "", "aggregated_output": ""}),
        ),
        completed(
            "2026-09-29T11:00:20Z",
            json!({"type": "CommandExecution", "id": "exec-2",
            "command": ["/bin/zsh", "-lc", "cargo fmt"], "cwd": "file:///work/atlas",
            "status": "completed", "exit_code": 0, "stdout": "", "stderr": "", "aggregated_output": ""}),
        ),
        // A file added with 2 lines; one updated by a hunk of 1 removed and
        // 1 added among 1 of context; one deleted, which held 1 line.
        completed(
            "2026-09-29T11:00:30Z",
            json!({"type": "FileChange", "id": "exec-3", "status": "completed",
            "stdout": "", "stderr": "",
            "changes": {
                "/work/atlas/src/cache.rs": {"type": "add", "content": "pub struct Cache;\n\n"},
                "/work/atlas/src/hillshade.rs": {"type": "update", "move_path": null,
                                                 "unified_diff": "@@ -1,2 +1,2 @@\n fn shade() {\n-    flat()\n+    shaded()\n"},
                "/work/atlas/src/old.rs": {"type": "delete", "content": "gone\n"}
            }}),
        ),
        // A change that failed changed nothing.
        completed(
            "2026-09-29T11:00:31Z",
            json!({"type": "FileChange", "id": "exec-4", "status": "failed",
            "stdout": "", "stderr": "", "changes": {"/work/atlas/x.rs": {"type": "add", "content": "x"}}}),
        ),
        line(
            "2026-09-29T11:00:40Z",
            "event_msg",
            json!({"type": "thread_goal_updated", "threadId": thread,
            "goal": {"threadId": thread, "objective": "Shade hills at low zoom", "status": "active"}}),
        ),
        line(
            "2026-09-29T11:00:50Z",
            "response_item",
            json!({"type": "function_call", "name": "update_plan",
            "call_id": "call_1",
            "arguments": "{\"plan\":[{\"step\":\"Cache per tile\",\"status\":\"completed\"},{\"step\":\"Fix normals\",\"status\":\"in_progress\"}]}"}),
        ),
        line(
            "2026-09-29T11:01:00Z",
            "response_item",
            json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Normals flip at tile edges."}]}),
        ),
    ];
    write(
        &home.path().join(format!(
            ".codex/sessions/2026/09/29/rollout-2026-09-29T11-00-00-{thread}.jsonl"
        )),
        &lines,
    );
    let engine = home.open();
    engine.scan().unwrap();
    let handoff = engine
        .handoff(&SessionKey::new(Agent::Codex, thread))
        .unwrap();
    assert_eq!(
        commands(&handoff),
        [
            ("cargo test hillshade", Some(101), Some(true)),
            ("cargo fmt", Some(0), Some(false))
        ]
    );
    let mut changed = files(&handoff);
    changed.sort();
    assert_eq!(
        changed,
        [
            ("/work/atlas/src/cache.rs", Some(2), Some(0), true, false),
            (
                "/work/atlas/src/hillshade.rs",
                Some(1),
                Some(1),
                false,
                false
            ),
            ("/work/atlas/src/old.rs", Some(0), Some(1), false, true),
        ]
    );
    assert_eq!(
        plan(&handoff),
        [
            ("Cache per tile", Some(StepStatus::Completed)),
            ("Fix normals", Some(StepStatus::InProgress))
        ]
    );
    let goal = handoff.goal.unwrap();
    assert_eq!(
        (goal.objective.as_str(), goal.status.as_deref()),
        ("Shade hills at low zoom", Some("active"))
    );
    assert_eq!(
        handoff.last_reply.unwrap().text,
        "Normals flip at tile edges."
    );
}

#[test]
fn an_opencode_session_hands_off_from_what_opencode_worked_out() {
    let home = Home::new();
    let database = opencode::database(&opencode::path(home.path()), "/work");
    opencode::message(
        &database,
        "msg_1",
        "user",
        1_000,
        1_000,
        &json!({"text": "Tidy the scanner", "time": {"created": 1_000}}),
    );
    let tool = |id: &str, name: &str, state: Value| json!({"type": "tool", "id": id, "name": name, "state": state});
    let reply = json!({"model": {"id": "glm-5.3", "providerID": "opencode-go"}, "cost": 0.0,
    "time": {"created": 2_000, "completed": 3_000},
    "tokens": {"input": 100, "output": 10, "reasoning": 0, "cache": {"read": 0, "write": 0}},
    "content": [
        tool("call_1", "shell", json!({"status": "completed", "input": {"command": "cargo test", "workdir": "/work"},
                                       "metadata": {"exit": 1, "status": "completed", "truncated": false},
                                       "content": [{"type": "text", "text": "1 failed"}]})),
        // OpenCode counts an edit's lines itself: 3 added, 1 removed.
        tool("call_2", "edit", json!({"status": "completed",
            "input": {"path": "src/scan.rs", "oldString": "a", "newString": "b\nc\nd"},
            "metadata": {"truncated": false, "files": [{"file": "src/scan.rs", "status": "modified",
                "additions": 3, "deletions": 1, "patch": "Index: src/scan.rs"}]},
            "content": [{"type": "text", "text": "Edited src/scan.rs (1 replacement)"}]})),
        // A new file of two lines.
        tool("call_3", "write", json!({"status": "completed", "input": {"path": "src/new.rs", "content": "a\nb"},
            "metadata": {"truncated": false},
            "content": [{"type": "text", "text": "Created file successfully: src/new.rs"}]})),
        // A file written over, whose old lines aren't recorded.
        tool("call_4", "write", json!({"status": "completed", "input": {"path": "src/old.rs", "content": "x"},
            "metadata": {"truncated": false},
            "content": [{"type": "text", "text": "Wrote file successfully: src/old.rs"}]})),
        tool("call_5", "todowrite", json!({"status": "completed",
            "input": {"todos": [{"id": "1", "content": "Split the scanner", "status": "completed", "priority": "high"},
                                {"id": "2", "content": "Test it", "status": "pending", "priority": "high"}]}})),
        {"type": "text", "text": "The scanner is split."}
    ]});
    opencode::message(&database, "msg_2", "assistant", 2_000, 3_000, &reply);
    drop(database);
    let engine = home.open();
    engine.scan().unwrap();
    let handoff = engine
        .handoff(&SessionKey::new(Agent::OpenCode, "ses_a"))
        .unwrap();
    assert_eq!(commands(&handoff), [("cargo test", Some(1), Some(true))]);
    assert_eq!(
        files(&handoff),
        [
            ("src/scan.rs", Some(3), Some(1), false, false),
            ("src/new.rs", Some(2), Some(0), true, false),
            ("src/old.rs", Some(1), None, false, false),
        ]
    );
    assert_eq!(
        plan(&handoff),
        [
            ("Split the scanner", Some(StepStatus::Completed)),
            ("Test it", Some(StepStatus::Pending))
        ]
    );
    assert_eq!(handoff.last_reply.unwrap().text, "The scanner is split.");
}

#[test]
fn a_pi_session_hands_off_its_commands_and_edits() {
    let home = Home::new();
    let assistant = |id: &str, content: Value| {
        json!({"type": "message", "id": id, "parentId": "u1", "timestamp": "2026-09-13T08:51:00.000Z",
               "message": {"role": "assistant", "provider": "openai-codex", "model": "gpt-5.6", "responseId": id,
                           "timestamp": 1_789_289_460_000u64, "content": content,
                           "usage": {"input": 10, "output": 10, "cacheRead": 0, "cacheWrite": 0,
                                     "totalTokens": 20, "cost": {"total": 0.001}}}})
    };
    let result = |id: &str, call: &str, name: &str, text: &str, failed: bool| {
        json!({"type": "message", "id": id, "parentId": "a1", "timestamp": "2026-09-13T08:51:10.000Z",
               "message": {"role": "toolResult", "toolCallId": call, "toolName": name,
                           "content": [{"type": "text", "text": text}], "isError": failed,
                           "timestamp": 1_789_289_470_000u64}})
    };
    write(
        &pi::session(home.path()),
        &[
            pi::header("/work"),
            pi::prompt(),
            assistant(
                "a1",
                json!([{"type": "toolCall", "id": "c1", "name": "bash", "arguments": {"command": "cargo build"}},
                       {"type": "toolCall", "id": "c2", "name": "edit",
                        "arguments": {"path": "src/lib.rs", "oldText": "one\ntwo", "newText": "one\n2"}}]),
            ),
            // As Pi ends a failed command's output with its exit code.
            result(
                "r1",
                "c1",
                "bash",
                "error[E0425]\n\n\nCommand exited with code 101",
                true,
            ),
            result(
                "r2",
                "c2",
                "edit",
                "Successfully replaced text in src/lib.rs.",
                false,
            ),
        ],
    );
    let engine = home.open();
    engine.scan().unwrap();
    let handoff = engine
        .handoff(&SessionKey::new(Agent::Pi, "01a099f5"))
        .unwrap();
    assert_eq!(commands(&handoff), [("cargo build", Some(101), Some(true))]);
    // "two" for "2": 1 added and 1 removed.
    assert_eq!(
        files(&handoff),
        [("src/lib.rs", Some(1), Some(1), false, false)]
    );
    assert_eq!(handoff.first_request.unwrap().text, "Index the sessions");
}

#[test]
fn a_grok_session_hands_off_from_what_its_results_say() {
    let home = Home::new();
    let folder = home.path().join(".grok/sessions/%2Fwork/01a0beef");
    write(
        &folder.join("updates.jsonl"),
        &[
            json!({"timestamp": 1_788_837_070, "method": "session/update",
                 "params": {"sessionId": "01a0beef", "update": {"sessionUpdate": "turn_completed", "prompt_id": "p1",
                     "usage": {"modelUsage": {"grok-4.6-build": {"inputTokens": 100, "outputTokens": 10,
                         "cachedReadTokens": 0, "cacheCreationTokens": 0, "reasoningTokens": 0,
                         "modelCalls": 1, "costUsdTicks": 1_000u64}}}}}}),
        ],
    );
    write(
        &folder.join("summary.json"),
        &[
            json!({"info": {"cwd": "/work"}, "generated_title": "Tidy the routes",
                 "created_at": "2026-09-13T07:23:38Z", "updated_at": "2026-09-13T07:25:00Z"}),
        ],
    );
    let call = |id: &str, name: &str, arguments: Value| json!({"id": id, "name": name, "arguments": arguments.to_string()});
    let result =
        |id: &str, text: &str| json!({"type": "tool_result", "tool_call_id": id, "content": text});
    write(
        &folder.join("chat_history.jsonl"),
        &[
            json!({"type": "user", "content": [{"type": "text", "text": "Tidy the routes"}]}),
            json!({"type": "assistant", "content": "On it.", "model_id": "grok-4.6-build", "tool_calls": [
                call("t1", "run_terminal_command", json!({"command": "npm test", "description": "Test"})),
                call("t2", "search_replace", json!({"file_path": "/work/routes.ts", "old_string": "a\nb\nc",
                                                     "new_string": "a\nc"})),
                call("t3", "write", json!({"file_path": "/work/new.ts", "content": "x\ny\n"})),
                call("t4", "todo_write", json!({"merge": false, "todos": [
                    {"id": "1", "content": "Split routes", "status": "in_progress"},
                    {"id": "2", "content": "Test them", "status": "pending"}]})),
                call("t5", "todo_write", json!({"merge": true, "todos": [{"id": "1", "status": "completed"}]})),
                call("t6", "search_replace", json!({"file_path": "/work/fresh.ts", "old_string": "",
                                                     "new_string": "a\nb\nc"})),
                call("t7", "search_replace", json!({"file_path": "/work/x.ts", "old_string": "q",
                                                     "new_string": "r"})),
            ]}),
            result("t1", "exit: 1\n1 failing"),
            result(
                "t2",
                "The file /work/routes.ts has been updated successfully.",
            ),
            result("t3", "The file /work/new.ts has been created successfully."),
            result(
                "t4",
                "- [in_progress] 1: Split routes\n- [pending] 2: Test them\n",
            ),
            result(
                "t5",
                "- [completed] 1: Split routes\n- [pending] 2: Test them\n",
            ),
            // An edit given no old text creates the file.
            result(
                "t6",
                "The file /work/fresh.ts has been created successfully.",
            ),
            // A result in other words may be a failure.
            result("t7", "Error: old_string not found in /work/x.ts"),
        ],
    );
    let engine = home.open();
    engine.scan().unwrap();
    let handoff = engine
        .handoff(&SessionKey::new(Agent::Grok, "01a0beef"))
        .unwrap();
    assert_eq!(commands(&handoff), [("npm test", Some(1), Some(true))]);
    // "b" removed from three lines, a new file of two, and one of three.
    assert_eq!(
        files(&handoff),
        [
            ("/work/routes.ts", Some(0), Some(1), false, false),
            ("/work/new.ts", Some(2), Some(0), true, false),
            ("/work/fresh.ts", Some(3), Some(0), true, false),
        ]
    );
    assert_eq!(handoff.unclear, [("search_replace".to_owned(), 1)]);
    assert_eq!(
        plan(&handoff),
        [
            ("Split routes", Some(StepStatus::Completed)),
            ("Test them", Some(StepStatus::Pending))
        ]
    );
}

#[test]
fn a_sessions_largest_context_and_its_share_are_read_without_its_conversation() {
    let home = Home::new();
    // Two responses: 1,000 in with 30,000 read from the cache and 2,000
    // written to it, a context of 33,000; then 500 in with 40,000 read, a
    // context of 40,500, the larger.
    let response = |id: &str, time: &str, usage: Value| {
        claude_code::response(CLAUDE, None, time, id, "claude-opus-5", usage)
    };
    write(
        &claude_code::session(home.path(), CLAUDE),
        &[
            json!({"type": "user", "sessionId": CLAUDE, "timestamp": "2026-09-29T10:00:00Z",
                   "cwd": "/work/ledger", "message": {"role": "user", "content": "Go"}}),
            response(
                "1",
                "2026-09-29T10:00:10Z",
                json!({"input_tokens": 1_000, "cache_read_input_tokens": 30_000,
                "cache_creation_input_tokens": 2_000, "output_tokens": 50}),
            ),
            response(
                "2",
                "2026-09-29T10:00:20Z",
                json!({"input_tokens": 500, "cache_read_input_tokens": 40_000,
                "cache_creation_input_tokens": 0, "output_tokens": 50}),
            ),
        ],
    );
    let engine = home.open();
    engine.scan().unwrap();
    let key = SessionKey::new(Agent::ClaudeCode, CLAUDE);
    let contexts = engine.largest_contexts(std::slice::from_ref(&key)).unwrap();
    assert_eq!(contexts.get(&key), Some(&40_500));
    // A session with no response has no context known.
    let none = SessionKey::new(Agent::Codex, "nothing");
    assert_eq!(engine.largest_contexts(&[none]).unwrap().len(), 0);
    // Its usage without its prompts: no prompt.
    let usage = engine.session_usage_without_prompts(&key).unwrap().unwrap();
    assert!(usage.prompts.is_empty());
}
