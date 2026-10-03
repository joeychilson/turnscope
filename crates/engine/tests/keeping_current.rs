//! A running engine reads history as agents write it, and tells subscribers.

mod history {
    pub mod claude_code;
    pub mod growing;
    pub mod home;
    pub mod running;
    pub mod subagents;
}

use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::{Value, json};
use turnscope_engine::{Change, Engine, UsageQuery, Zone};

use history::claude_code;
use history::growing::append;
use history::home::{Home, lines, write};
use history::running::{OFFLINE, wait_until};
use history::subagents;

const SESSION: &str = "0f6e3f6a-713c-4bad-8f6d-f04fe41bbd84";

/// A response of 10 tokens in and `output` out.
fn response(id: &str, output: u64) -> Value {
    claude_code::response(
        SESSION,
        None,
        "2026-09-14T12:00:10.000Z",
        id,
        "claude-opus-5",
        json!({"input_tokens": 10, "output_tokens": output}),
    )
}

/// Wait for a change naming the session, and say how long it took.
fn wait_for_session(changes: &mpsc::Receiver<Change>) -> Duration {
    wait_until("a change names the session", || {
        matches!(changes.try_recv(), Ok(Change::History { sessions, .. })
            if sessions.iter().any(|session| session.native() == SESSION))
    })
}

fn output(engine: &Engine) -> u64 {
    let question = UsageQuery::default();
    engine
        .usage(&question, &Zone::system())
        .unwrap()
        .total
        .tokens
        .output
}

#[test]
fn history_written_while_the_engine_runs_is_read_within_moments() {
    let home = Home::new();
    // The agent's directory exists before the engine starts, as it does once
    // an agent has been used.
    std::fs::create_dir_all(home.path().join(".claude/projects")).unwrap();
    let engine = Arc::new(home.open());
    let changes = engine.subscribe();
    let running = engine.run(OFFLINE).unwrap();
    // The first look through everything finds nothing yet.
    std::thread::sleep(Duration::from_millis(300));

    let path = claude_code::session(home.path(), SESSION);
    write(&path, &[response("1", 50)]);
    let first = wait_for_session(&changes);
    assert_eq!(output(&engine), 50);

    append(&path, lines(&[response("2", 70)]).as_bytes());
    let second = wait_for_session(&changes);
    assert_eq!(output(&engine), 120);
    eprintln!("read after {first:?} and {second:?}");

    // History outlives the file.
    std::fs::remove_file(&path).unwrap();
    wait_for_session(&changes);
    assert_eq!(output(&engine), 120);
    drop(running);
}

#[test]
fn a_change_to_a_subagent_names_every_session_above_it() {
    // The session, its subagent, and a subagent that one started, as Claude
    // Code records them; each response 10 tokens in and 1 out.
    const EXPLORE: &str = "ac9cd968c38c96ac1";
    const NESTED: &str = "a5f0c2e19b7d4a3c8";
    let home = Home::new();
    let said = |agent: Option<&str>, id: &str| {
        claude_code::response(
            SESSION,
            agent,
            "2026-09-14T12:00:10.000Z",
            id,
            "claude-opus-5",
            json!({"input_tokens": 10, "output_tokens": 1}),
        )
    };
    write(
        &claude_code::session(home.path(), SESSION),
        &[said(None, "1")],
    );
    write(
        &subagents::log(home.path(), SESSION, EXPLORE),
        &[said(Some(EXPLORE), "2")],
    );
    subagents::describe(
        home.path(),
        SESSION,
        EXPLORE,
        json!({"agentType": "Explore", "description": "Find the tests"}),
    );
    write(
        &subagents::log(home.path(), SESSION, NESTED),
        &[said(Some(NESTED), "3")],
    );
    subagents::describe(
        home.path(),
        SESSION,
        NESTED,
        json!({"agentType": "Explore", "description": "Read them",
               "parentAgentId": EXPLORE, "spawnDepth": 2}),
    );
    let engine = home.scanned();

    // Only the nested subagent writes, and the totals of both sessions above
    // it change with its own, so the change names all three.
    let changes = engine.subscribe();
    append(
        &subagents::log(home.path(), SESSION, NESTED),
        lines(&[said(Some(NESTED), "4")]).as_bytes(),
    );
    engine.scan().unwrap();
    let mut named: Vec<String> = std::iter::from_fn(|| changes.try_recv().ok())
        .flat_map(|change| match change {
            Change::History {
                sessions,
                everything: false,
            } => sessions,
            other => panic!("not a change of some sessions: {other:?}"),
        })
        .map(|session| session.native().to_owned())
        .collect();
    named.sort();
    named.dedup();
    assert_eq!(named, [SESSION, NESTED, EXPLORE]);
}

#[test]
fn a_root_made_after_the_engine_starts_is_read_within_moments() {
    let home = Home::new();
    // Claude Code's folder is there, but not yet the one it keeps its
    // sessions in, as before its first session.
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    let engine = Arc::new(home.open());
    let changes = engine.subscribe();
    let running = engine.run(OFFLINE).unwrap();
    std::thread::sleep(Duration::from_millis(300));

    write(
        &claude_code::session(home.path(), SESSION),
        &[response("1", 50)],
    );
    wait_for_session(&changes);
    assert_eq!(output(&engine), 50);

    // Watched from then on.
    append(
        &claude_code::session(home.path(), SESSION),
        lines(&[response("2", 70)]).as_bytes(),
    );
    wait_for_session(&changes);
    assert_eq!(output(&engine), 120);
    drop(running);
}

#[test]
fn a_folder_moved_into_a_root_is_read_within_moments() {
    let home = Home::new();
    std::fs::create_dir_all(home.path().join(".claude/projects")).unwrap();
    let engine = Arc::new(home.open());
    let changes = engine.subscribe();
    let running = engine.run(OFFLINE).unwrap();
    std::thread::sleep(Duration::from_millis(300));

    // A project's folder, written elsewhere and moved in whole: the change
    // names the folder, not the session in it.
    let session = claude_code::session(home.path(), SESSION);
    let folder = session.parent().unwrap();
    let elsewhere = home.path().join("elsewhere");
    write(
        &elsewhere.join(session.file_name().unwrap()),
        &[response("1", 50)],
    );
    std::fs::rename(&elsewhere, folder).unwrap();
    wait_for_session(&changes);
    assert_eq!(output(&engine), 50);
    drop(running);
}
