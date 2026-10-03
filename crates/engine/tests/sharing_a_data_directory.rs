//! Processes sharing a data directory, as the app, MCP servers and the command
//! line do, write one at a time, and one keeps it current.
//!
//! Each engine here opens its own lock files, so two engines in one test stand
//! for two processes.

mod history {
    pub mod claude_code;
    pub mod growing;
    pub mod home;
    pub mod running;
}

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use turnscope_engine::{Change, Engine, Filter, SearchQuery, SessionQuery, Span, UsageQuery, Zone};

use history::claude_code;
use history::growing::append;
use history::home::{Home, lines, write};
use history::running::{OFFLINE, wait_until};

const SESSION: &str = "0f6e3f6a-713c-4bad-8f6d-f04fe41bbd84";

/// A prompt and its reply, the `n`th of the session.
fn exchange(n: u32) -> [Value; 2] {
    let prompt = json!({"type": "user", "sessionId": SESSION, "timestamp": "2026-09-14T12:00:00.000Z",
                        "cwd": "/work/ledger", "message": {"role": "user", "content": format!("Tune the parser, pass {n}")}});
    let mut reply = claude_code::response(
        SESSION,
        None,
        "2026-09-14T12:00:10.000Z",
        &n.to_string(),
        "claude-opus-5",
        json!({"input_tokens": 10, "output_tokens": 20}),
    );
    reply["message"]["content"] = json!([{"type": "text", "text": "Done."}]);
    [prompt, reply]
}

#[test]
fn processes_reading_at_once_record_each_line_once() {
    let home = Home::new();
    let path = claude_code::session(home.path(), SESSION);
    write(&path, &exchange(0));
    let engines: Vec<Arc<Engine>> = (0..3).map(|_| Arc::new(home.open())).collect();

    // The session grows while every engine scans as fast as it can.
    let scanning: Vec<_> = engines
        .iter()
        .map(|engine| {
            let engine = Arc::clone(engine);
            std::thread::spawn(move || {
                for _ in 0..40 {
                    engine.scan().unwrap();
                }
            })
        })
        .collect();
    for n in 1..60 {
        append(&path, lines(&exchange(n)).as_bytes());
        std::thread::sleep(Duration::from_millis(2));
    }
    for thread in scanning {
        thread.join().unwrap();
    }
    engines[0].scan().unwrap();

    // Sixty prompts, each said once, however the reads interleaved.
    for engine in &engines {
        let hits = engine
            .search(&turnscope_engine::SearchQuery {
                words: "parser".to_owned(),
                limit: 10,
                ..turnscope_engine::SearchQuery::default()
            })
            .unwrap()
            .items;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].matches, 60);
        assert_eq!(engine.doctor().unwrap().agents[0].responses, 60);
    }
}

#[test]
fn one_engine_keeps_the_directory_and_another_takes_over_when_it_stops() {
    let home = Home::new();
    std::fs::create_dir_all(home.path().join(".claude/projects")).unwrap();
    let first = Arc::new(home.open());
    let second = Arc::new(home.open());
    assert!(
        !second.kept().unwrap(),
        "nothing keeps it before an engine runs"
    );

    let running = first.run(OFFLINE).unwrap();
    wait_until("the first engine keeps it", || second.kept().unwrap());

    // The second waits while the first keeps it.
    let changes = second.subscribe();
    let waiting = second.run(OFFLINE).unwrap();
    wait_until(
        "the second says it is waiting",
        || matches!(changes.try_recv(), Ok(Change::Trouble(reason)) if reason.contains("waiting")),
    );

    drop(running);
    write(&claude_code::session(home.path(), SESSION), &exchange(0));
    wait_until("the second takes over and reads the session", || {
        matches!(changes.try_recv(), Ok(Change::History { sessions, .. })
            if sessions.iter().any(|session| session.native() == SESSION))
    });
    assert!(first.kept().unwrap(), "the second keeps it now");

    drop(waiting);
    assert!(!first.kept().unwrap(), "nothing keeps it once both stop");
}

/// Delete the cache in `home`'s data directory, with its side files, as a
/// build of another schema replaces it.
fn remove_cache(home: &Home) {
    for suffix in ["", "-wal", "-shm"] {
        let file = home.data.path().join(format!("cache.sqlite{suffix}"));
        if file.exists() {
            std::fs::remove_file(file).unwrap();
        }
    }
}

/// Every token out of all history, as `engine` answers.
fn output(engine: &Engine) -> u64 {
    let question = UsageQuery {
        span: Span::default(),
        filter: Filter::default(),
        by: None,
        every: None,
    };
    engine
        .usage(&question, &Zone::system())
        .unwrap()
        .total
        .tokens
        .output
}

#[test]
fn an_engine_answers_from_a_cache_another_built_in_place_of_its_own() {
    let home = Home::new();
    let path = claude_code::session(home.path(), SESSION);
    write(&path, &exchange(0));
    let first = home.scanned();
    // Asked once, so its connections to the cache are kept for the next.
    assert_eq!(output(&first), 20);

    // The cache is replaced, as a build of another schema replaces it: the
    // file goes, and another engine opening the directory builds a new one.
    remove_cache(&home);
    let second = home.open();
    append(&path, lines(&exchange(1)).as_bytes());
    second.scan().unwrap();
    // 20 + 20 tokens out, as each engine answers.
    assert_eq!(output(&second), 40);
    assert_eq!(output(&first), 40);
}

/// What a newer build records of an agent this one has no reader for, as it
/// would record it: an artifact, which is still there, a session run as a
/// subagent of this build's one, its facts, a response, its agent's own
/// totals and what was said, and the changes all that made.
const NEWER_AGENT: &str = "
    INSERT INTO artifact (id, agent, path, kind, device, inode, size, modified, fingerprint,
                          read_to, state, reader_version, present, read_at)
    VALUES (1000, 'hologram', '/elsewhere/hologram.jsonl', 'log', 1, 1, 10, 0, x'00', 10, x'',
            1, 1, 0);
    INSERT INTO session (id, agent, native) VALUES (1000, 'hologram', 'h1');
    INSERT INTO session_fact (artifact_id, session_id, started, last, title, title_rank)
    VALUES (1000, 1000, 1789387220000, 1789387220000, 'Tune the parser', 1);
    INSERT INTO session_link (artifact_id, child_id, parent_id, kind)
    SELECT 1000, 1000, id, 'subagent' FROM session WHERE agent = 'claude-code';
    INSERT INTO observation (artifact_id, agent, response, session_id, copy, at, provider, model,
                             input, cache_read, cache_write_5m, cache_write_1h, output,
                             reasoning, prompt, web_searches, cost, priority)
    VALUES (1000, 'hologram', 'r1', 1000, 0, 1789387220000, 'openai', 'gpt-6', 5, 0, 0, 0, 7, 0,
            5, 0, NULL, 0);
    INSERT INTO report (artifact_id, session_id, scope, at, model, input, cache_read,
                        cache_write, output, web_searches, cost)
    VALUES (1000, 1000, 'session', 1789387220000, '', 5, 0, 0, 7, 0, NULL);
    INSERT INTO said (id, artifact_id, session_id, at) VALUES (1000, 1000, 1000, 1789387220000);
    INSERT INTO said_text (rowid, text) VALUES (1000, 'Tune the parser');
    UPDATE revision SET value = value + 1;
    INSERT INTO touched (revision, kind, agent, key)
    SELECT value, 'session', 'hologram', 'h1' FROM revision
    UNION ALL SELECT value, 'response', 'hologram', 'r1' FROM revision;
";

/// Every session `engine` lists, subagents and empty ones among them.
fn listed(engine: &Engine) -> Vec<String> {
    let question = SessionQuery {
        subagents: true,
        empty: true,
        ..SessionQuery::default()
    };
    let page = engine.sessions(&question).unwrap();
    page.items.iter().map(|row| row.key.to_string()).collect()
}

#[test]
fn an_agent_a_build_does_not_know_is_left_alone_and_passed_over() {
    let home = Home::new();
    let path = claude_code::session(home.path(), SESSION);
    write(&path, &exchange(0));
    let engine = home.scanned();
    let ledger = rusqlite::Connection::open(home.data.path().join("ledger.sqlite")).unwrap();
    ledger.execute_batch(NEWER_AGENT).unwrap();

    // Looking through history finds no hologram, and leaves its artifact as
    // the build that reads it left it.
    engine.scan().unwrap();
    let present: bool = ledger
        .query_row("SELECT present FROM artifact WHERE id = 1000", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(present, "not this build's to mark gone");

    // It answers of the agent it knows, as it catches up on what changed and
    // as it builds its cache from nothing.
    let answers_of_its_own = |engine: &Engine| {
        engine.scan().unwrap();
        let only = vec![format!("claude-code:{SESSION}")];
        assert_eq!(listed(engine), only);
        // Claude Code's 20 tokens out, and none of the hologram's 7.
        assert_eq!(output(engine), 20);
        let search = SearchQuery {
            words: "parser".to_owned(),
            limit: 10,
            ..SearchQuery::default()
        };
        let hits: Vec<String> = engine
            .search(&search)
            .unwrap()
            .items
            .iter()
            .map(|hit| hit.session.to_string())
            .collect();
        assert_eq!(hits, only);
        let doctor = engine.doctor().unwrap();
        assert!(!format!("{doctor:?}").contains("hologram"));
    };
    answers_of_its_own(&engine);
    drop(engine);
    remove_cache(&home);
    answers_of_its_own(&home.open());
}
