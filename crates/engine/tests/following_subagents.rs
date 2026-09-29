//! A call that started a subagent names it: exactly, as its agent records
//! the start on both sides, whichever of their files is read first, and
//! never by a guess.

mod history {
    pub mod claude_code;
    pub mod home;
    pub mod subagents;
}

use serde_json::{Value, json};
use turnscope_engine::{Agent, Engine, SessionKey};

use history::home::{Home, write};
use history::{claude_code, subagents};

const SESSION: &str = "5f0c2b1e-parent";

fn usage() -> Value {
    json!({"input_tokens": 10, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0,
           "output_tokens": 5})
}

/// The parent's log: it is asked for an audit, starts two subagents with
/// Agent calls, and runs a command.
fn parent_log() -> Vec<Value> {
    let call = |id: &str, name: &str, input: Value| json!({"type": "tool_use", "id": id, "name": name, "input": input});
    let mut started = claude_code::response(
        SESSION,
        None,
        "2026-09-27T10:00:05Z",
        "1",
        "claude-opus-5",
        usage(),
    );
    started["message"]["content"] = json!([
        call(
            "toolu_1",
            "Agent",
            json!({"description": "Read the schema", "prompt": "…"})
        ),
        call(
            "toolu_2",
            "Agent",
            json!({"description": "Read the tests", "prompt": "…"})
        ),
        call("toolu_3", "Bash", json!({"command": "ls"})),
    ]);
    vec![
        json!({"type": "user", "sessionId": SESSION, "timestamp": "2026-09-27T10:00:00Z",
               "cwd": "/work/ledger", "message": {"role": "user", "content": "Audit the ledger"}}),
        started,
    ]
}

/// The subagent `agent`'s log and the description beside it, which names
/// the call that started it as `call`.
fn subagent(home: &Home, agent: &str, call: &str) {
    let response = claude_code::response(
        SESSION,
        Some(agent),
        "2026-09-27T10:00:10Z",
        &format!("{agent}_1"),
        "claude-haiku-4-5",
        usage(),
    );
    write(&subagents::log(home.path(), SESSION, agent), &[response]);
    subagents::describe(
        home.path(),
        SESSION,
        agent,
        json!({"description": format!("Subagent {agent}"), "toolUseId": call}),
    );
}

/// Each tool call in `session`'s conversation, by name, with the subagent it
/// names.
fn calls(engine: &Engine, session: &SessionKey) -> Vec<(String, Option<SessionKey>)> {
    engine
        .conversation(session)
        .unwrap()
        .into_iter()
        .filter_map(|entry| entry.tool)
        .map(|tool| (tool.name, tool.subagent))
        .collect()
}

fn claude(native: &str) -> SessionKey {
    SessionKey::new(Agent::ClaudeCode, native)
}

#[test]
fn each_agent_call_names_the_subagent_it_started_whichever_is_read_first() {
    let expected = vec![
        ("Agent".to_owned(), Some(claude("a1"))),
        ("Agent".to_owned(), Some(claude("a2"))),
        ("Bash".to_owned(), None),
    ];
    // The parent read first, then its subagents.
    let home = Home::new();
    write(&claude_code::session(home.path(), SESSION), &parent_log());
    let engine = home.open();
    engine.scan().unwrap();
    subagent(&home, "a1", "toolu_1");
    subagent(&home, "a2", "toolu_2");
    engine.scan().unwrap();
    assert_eq!(calls(&engine, &claude(SESSION)), expected);

    // The subagents read first, then the parent.
    let home = Home::new();
    subagent(&home, "a2", "toolu_2");
    subagent(&home, "a1", "toolu_1");
    let engine = home.open();
    engine.scan().unwrap();
    write(&claude_code::session(home.path(), SESSION), &parent_log());
    engine.scan().unwrap();
    assert_eq!(calls(&engine, &claude(SESSION)), expected);
}

#[test]
fn a_start_two_subagents_both_claim_names_neither() {
    let home = Home::new();
    write(&claude_code::session(home.path(), SESSION), &parent_log());
    subagent(&home, "a1", "toolu_1");
    // Two descriptions naming the same call: which subagent it started is
    // unknown, so the call names neither rather than one at random.
    subagent(&home, "a2", "toolu_2");
    subagent(&home, "a3", "toolu_2");
    let engine = home.open();
    engine.scan().unwrap();
    assert_eq!(
        calls(&engine, &claude(SESSION)),
        vec![
            ("Agent".to_owned(), Some(claude("a1"))),
            ("Agent".to_owned(), None),
            ("Bash".to_owned(), None),
        ]
    );
}

/// A Codex rollout of the thread `id`, begun at `minute` past ten, holding
/// `lines` after its `session_meta`, which says `meta` besides.
fn rollout(home: &Home, id: &str, minute: u32, meta: Value, lines: &[Value]) {
    let time = format!("2026-09-27T10:{minute:02}:00Z");
    let mut payload = json!({"id": id, "cwd": "/work", "model_provider": "openai"});
    payload
        .as_object_mut()
        .unwrap()
        .extend(meta.as_object().unwrap().clone());
    let mut records = vec![json!({"timestamp": time, "type": "session_meta", "payload": payload})];
    records.extend(lines.iter().map(|line| {
        let mut line = line.clone();
        line["timestamp"] = json!(time);
        line
    }));
    let path = home.path().join(format!(
        ".codex/sessions/2026/09/27/rollout-2026-09-27T10-{minute:02}-00-{id}.jsonl"
    ));
    write(&path, &records);
}

#[test]
fn each_spawn_names_the_codex_subagent_given_its_task() {
    let spawn = |call: &str, task: &str| {
        json!({"type": "response_item", "payload": {"type": "function_call", "name": "spawn_agent",
               "call_id": call,
               "arguments": json!({"task_name": task, "message": "gAAAAB…"}).to_string()}})
    };
    let home = Home::new();
    // The children first, as their rollouts can be.
    rollout(
        &home,
        "01a0-tests",
        2,
        json!({"parent_thread_id": "01a0-parent", "agent_path": "/root/test_audit"}),
        &[],
    );
    rollout(
        &home,
        "01a0-schema",
        3,
        json!({"parent_thread_id": "01a0-parent", "agent_path": "/root/schema_audit"}),
        &[],
    );
    rollout(
        &home,
        "01a0-parent",
        1,
        json!({}),
        &[
            spawn("call_1", "schema_audit"),
            spawn("call_2", "test_audit"),
        ],
    );
    let engine = home.open();
    engine.scan().unwrap();
    let codex = |native: &str| SessionKey::new(Agent::Codex, native);
    assert_eq!(
        calls(&engine, &codex("01a0-parent")),
        vec![
            ("spawn_agent".to_owned(), Some(codex("01a0-schema"))),
            ("spawn_agent".to_owned(), Some(codex("01a0-tests"))),
        ]
    );
}
