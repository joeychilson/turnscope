//! Searching finds the sessions in which the person or a model said something,
//! and nothing the agent injected or a tool returned.

mod history {
    pub mod claude_code;
    pub mod home;
    pub mod opencode;
}

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use turnscope_engine::{Agent, Engine, Excerpt, SearchHit, SearchQuery, SessionKey, Speaker};

use history::home::{Home, write};
use history::{claude_code, opencode};

const CLAUDE: &str = "0f6e3f6a-713c-4bad-8f6d-f04fe41bbd84";
const CODEX: &str = "01a06671-ecc2";

fn codex_path(home: &Path) -> PathBuf {
    home.join(".codex/sessions/2026/09/15")
        .join(format!("rollout-2026-09-15T09-00-00-{CODEX}.jsonl"))
}

/// A Claude Code session from 14 September: a prompt with context Claude Code
/// injected beside it, a reply, and a tool call whose result is only the
/// tool's.
fn claude_lines(prompt: &str) -> Vec<Value> {
    let user = |time: &str, content: Value| json!({"type": "user", "sessionId": CLAUDE, "timestamp": time, "cwd": "/work/ledger", "message": {"role": "user", "content": content}});
    let mut reply = claude_code::response(
        CLAUDE,
        None,
        "2026-09-14T12:00:10.000Z",
        "1",
        "claude-opus-5",
        json!({"input_tokens": 10, "output_tokens": 20}),
    );
    reply["message"]["content"] = json!([{"type": "text", "text": "The migration runs before the watcher starts."},
                                         {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "ls"}}]);
    vec![
        user("2026-09-14T12:00:00.000Z", json!(prompt)),
        user(
            "2026-09-14T12:00:01.000Z",
            json!("<system-reminder>The ledger uses sqlite pragmas</system-reminder>"),
        ),
        reply,
        user(
            "2026-09-14T12:00:20.000Z",
            json!([{"type": "tool_result", "tool_use_id": "toolu_1", "content": "Cargo.toml flamegraph.svg"}]),
        ),
    ]
}

/// A Codex thread from 15 September asking about the same migration.
fn codex_lines() -> Vec<Value> {
    let item = |time: &str, payload: Value| json!({"type": "response_item", "timestamp": time, "payload": payload});
    vec![
        json!({"type": "session_meta", "timestamp": "2026-09-15T09:00:00.000Z",
               "payload": {"id": CODEX, "cwd": "/work/ledger", "model_provider": "openai"}}),
        item(
            "2026-09-15T09:00:01.000Z",
            json!({"type": "message", "role": "user",
                   "content": [{"type": "input_text", "text": "# AGENTS.md instructions for /work/ledger\n\nNo migrations without tests."}]}),
        ),
        item(
            "2026-09-15T09:00:02.000Z",
            json!({"type": "message", "role": "user",
                   "content": [{"type": "input_text", "text": "Why does the migration fail?"}]}),
        ),
        item(
            "2026-09-15T09:00:09.000Z",
            json!({"type": "message", "role": "assistant",
                   "content": [{"type": "output_text", "text": "Because the schema changed underneath it."}]}),
        ),
    ]
}

/// A home holding both sessions, with an engine that has read it.
fn setup(prompt: &str) -> (Home, Engine) {
    let home = Home::new();
    write(
        &claude_code::session(home.path(), CLAUDE),
        &claude_lines(prompt),
    );
    write(&codex_path(home.path()), &codex_lines());
    let engine = home.open();
    let report = engine.scan().unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    (home, engine)
}

/// The first ten sessions in which something said matches `words`.
fn search(engine: &Engine, words: &str) -> Vec<SearchHit> {
    engine
        .search(&SearchQuery {
            words: words.to_owned(),
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap()
        .items
}

fn sessions(hits: &[SearchHit]) -> Vec<&str> {
    hits.iter().map(|hit| hit.session.native()).collect()
}

/// The excerpt of `session` for `words`, as a list of hits asks for it.
fn excerpt(engine: &Engine, session: &SessionKey, words: &str) -> Option<Excerpt> {
    engine
        .excerpts(std::slice::from_ref(session), words)
        .remove(0)
        .unwrap()
}

#[test]
fn search_finds_what_was_said_most_recent_first() {
    let (_home, engine) = setup("Add a file watcher for the café migration");

    let hits = search(&engine, "migration");
    assert_eq!(sessions(&hits), [CODEX, CLAUDE]);
    // Claude's prompt and reply both say it.
    assert_eq!(hits[1].matches, 2);

    // A word matches the start of longer ones, and every word must be in one
    // message: the café is in Claude's prompt, the schema in Codex's reply.
    assert_eq!(sessions(&search(&engine, "migr watch")), [CLAUDE]);
    assert_eq!(sessions(&search(&engine, "cafe watcher")), [CLAUDE]);
    assert!(search(&engine, "café schema").is_empty());

    // Case, accents and punctuation are not the person's problem, and words
    // FTS5 reads as operators are only words.
    assert_eq!(sessions(&search(&engine, "\"MIGRATION\" (fail")), [CODEX]);
    assert!(search(&engine, "NOT OR NEAR").is_empty());
    assert!(search(&engine, "  -*  ").is_empty());
    let first = engine
        .search(&SearchQuery {
            words: "migration".to_owned(),
            limit: 1,
            ..SearchQuery::default()
        })
        .unwrap();
    assert_eq!(first.items.len(), 1);
}

#[test]
fn a_word_finds_its_other_forms_in_all_the_history_kept() {
    let (home, engine) = setup("Add a file watcher for the café migration");
    // Neither session says "migrations" or "migrating", which aren't the
    // start of "migration"; Snowball stems all three to "migrat".
    assert_eq!(sessions(&search(&engine, "migrations")), [CODEX, CLAUDE]);
    let hits = search(&engine, "migrating fail");
    assert_eq!(sessions(&hits), [CODEX]);
    assert_eq!(
        excerpt(&engine, &hits[0].session, "migrating fail").map(|found| found.text),
        Some("Why does the migration fail?".to_owned())
    );
    // What Codex injected says "migrations", and is still not found.
    assert!(search(&engine, "migrations without").is_empty());

    // An agent deletes its transcript, as Claude Code does after a time:
    // what it said is kept, and found by its words' other forms as by them.
    std::fs::remove_file(claude_code::session(home.path(), CLAUDE)).unwrap();
    engine.scan().unwrap();
    let hits = search(&engine, "migrations");
    assert_eq!(sessions(&hits), [CODEX, CLAUDE]);
    assert_eq!(hits[1].matches, 2);
}

#[test]
fn what_an_agent_injected_or_a_tool_returned_is_not_found() {
    let (_home, engine) = setup("Add a file watcher");
    assert!(search(&engine, "pragmas").is_empty());
    assert!(search(&engine, "flamegraph").is_empty());
    assert!(search(&engine, "tests").is_empty());
}

#[test]
fn an_excerpt_shows_where_a_session_matched() {
    let (_home, engine) = setup("Add a file watcher");
    let hits = search(&engine, "watcher starts");
    assert_eq!(sessions(&hits), [CLAUDE]);
    let found = excerpt(&engine, &hits[0].session, "watcher starts").unwrap();
    assert_eq!(
        (found.index, found.text.as_str()),
        (2, "The migration runs before the watcher starts.")
    );
    assert_eq!(excerpt(&engine, &hits[0].session, "schema"), None);
}

#[test]
fn a_rewritten_session_is_found_only_by_what_it_says_now() {
    let (home, engine) = setup("Add a file watcher");
    assert_eq!(sessions(&search(&engine, "watcher")), [CLAUDE]);

    write(
        &claude_code::session(home.path(), CLAUDE),
        &claude_lines("Profile the renderer"),
    );
    engine.scan().unwrap();
    assert_eq!(sessions(&search(&engine, "renderer")), [CLAUDE]);
    // The reply still mentions the watcher; the old prompt is gone.
    assert_eq!(search(&engine, "watcher")[0].matches, 1);
    assert!(search(&engine, "file watcher add").is_empty());
}

#[test]
fn a_message_read_again_as_it_changes_is_found_by_what_it_says_now() {
    let home = Home::new();
    let writer = opencode::database(&opencode::path(home.path()), "/work");
    // OpenCode writes a reply as it streams, updating its row in place.
    let reply = |updated: i64, parts: &[&str]| {
        let content: Vec<Value> = parts
            .iter()
            .map(|text| json!({"type": "text", "text": text}))
            .collect();
        let data = json!({"model": {"id": "glm-5.3", "providerID": "opencode-go"}, "cost": 0.0,
                          "time": {"created": 1_000}, "content": content,
                          "tokens": {"input": 100, "output": 10, "reasoning": 0, "cache": {"read": 0, "write": 0}}});
        opencode::message(&writer, "msg_1", "assistant", 1_000, updated, &data);
    };
    let engine = home.open();
    let matches = |words: &str| {
        engine.scan().unwrap();
        search(&engine, words)
            .iter()
            .map(|hit| hit.matches)
            .sum::<u64>()
    };

    reply(1_000, &["Drafting the parser"]);
    assert_eq!(matches("parser"), 1);
    reply(
        2_000,
        &["Drafting the parser", "The parser handles escapes"],
    );
    assert_eq!(matches("parser"), 2);
    reply(3_000, &["Rewrote the tokenizer"]);
    assert_eq!(matches("parser"), 0);
    assert_eq!(matches("tokenizer"), 1);
    // Its words taken back, as when only its calls are left.
    reply(4_000, &[]);
    assert_eq!(matches("tokenizer"), 0);
}

/// A review Codex ran for the thread on 16 September, asking a model whether
/// to approve a change to the migration: kept as it was said, and counted
/// nowhere.
fn review_lines() -> Vec<Value> {
    vec![
        json!({"type": "session_meta", "timestamp": "2026-09-16T09:00:00.000Z",
               "payload": {"id": "01a06671-f00d", "cwd": "/work/ledger", "model_provider": "openai",
                           "parent_thread_id": CODEX, "thread_source": "guardian_review",
                           "source": {"subagent": {"other": "guardian"}}}}),
        json!({"type": "response_item", "timestamp": "2026-09-16T09:00:05.000Z",
               "payload": {"type": "message", "role": "assistant",
                           "content": [{"type": "output_text", "text": "Approved: the migration only adds a table"}]}}),
    ]
}

#[test]
fn a_review_takes_no_place_among_the_sessions_found() {
    let (home, engine) = setup("Add a file watcher for the café migration");
    write(
        &home
            .path()
            .join(".codex/sessions/2026/09/16/rollout-2026-09-16T09-00-00-01a06671-f00d.jsonl"),
        &review_lines(),
    );
    engine.scan().unwrap();
    // The review said it last, and is not one of the sessions found.
    let first = engine
        .search(&SearchQuery {
            words: "migration".to_owned(),
            limit: 1,
            ..SearchQuery::default()
        })
        .unwrap()
        .items;
    assert_eq!(sessions(&first), [CODEX]);
    assert_eq!(sessions(&search(&engine, "migration")), [CODEX, CLAUDE]);
}

/// A Claude Code session `id` begun in `cwd` at `time`, in which the person
/// asked about the parser.
fn asking_about_the_parser(home: &Path, id: &str, cwd: &str, time: &str) {
    let folder = cwd.replace('/', "-");
    let path = home
        .join(".claude/projects")
        .join(folder)
        .join(format!("{id}.jsonl"));
    write(
        &path,
        &[
            json!({"type": "user", "sessionId": id, "timestamp": time, "cwd": cwd,
                 "message": {"role": "user", "content": "Tune the parser"}}),
        ],
    );
}

#[test]
fn a_filtered_search_finds_an_older_match_behind_hundreds_of_others() {
    let home = Home::new();
    // 300 sessions in /work/other mention the parser on 15 September, and
    // one in /work/ledger, a day before.
    for n in 0..300 {
        asking_about_the_parser(
            home.path(),
            &format!("10000000-0000-4000-8000-{n:012}"),
            "/work/other",
            &format!("2026-09-15T{:02}:{:02}:00.000Z", n / 60, n % 60),
        );
    }
    let older = "20000000-0000-4000-8000-000000000001";
    asking_about_the_parser(
        home.path(),
        older,
        "/work/ledger",
        "2026-09-14T09:00:00.000Z",
    );
    let engine = home.open();
    engine.scan().unwrap();

    let ledger = turnscope_engine::Filter {
        projects: vec![engine.project_root("/work/ledger")],
        ..turnscope_engine::Filter::default()
    };
    let found = engine
        .search(&SearchQuery {
            words: "parser".to_owned(),
            filter: ledger,
            limit: 10,
            ..SearchQuery::default()
        })
        .unwrap();
    assert_eq!(sessions(&found.items), [older]);
    assert_eq!(found.next, None);

    // All 301, fifty at a time: seven pages, none repeated or missed, and
    // the one from the day before last.
    let mut seen = Vec::new();
    let mut after = None;
    let mut pages = 0;
    loop {
        let page = engine
            .search(&SearchQuery {
                words: "parser".to_owned(),
                limit: 50,
                after,
                ..SearchQuery::default()
            })
            .unwrap();
        pages += 1;
        seen.extend(page.items.into_iter().map(|hit| hit.session.to_string()));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!((seen.len(), unique.len(), pages), (301, 301, 7));
    assert_eq!(
        seen.last().map(String::as_str),
        Some(&*format!("claude-code:{older}"))
    );
}

#[test]
fn a_rollout_moved_to_the_archive_is_counted_once() {
    let (home, engine) = setup("Add a file watcher");
    let matches = |engine: &Engine| {
        search(engine, "migration")
            .into_iter()
            .find(|hit| hit.session.native() == CODEX)
            .map(|hit| hit.matches)
    };
    // The person asked about the migration, and the model answered: 1.
    let before = matches(&engine);
    assert_eq!(before, Some(1));
    // Codex moves a thread's rollout to its archive, as archiving it does.
    let archived = home
        .path()
        .join(".codex/archived_sessions")
        .join(codex_path(home.path()).file_name().unwrap());
    std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
    std::fs::rename(codex_path(home.path()), &archived).unwrap();
    engine.scan().unwrap();
    assert_eq!(matches(&engine), before);
}

const RESUMED: &str = "01a0934d-19cf-78e2-9fd2-8b796a3ef310";

/// A rollout of the Codex thread [`RESUMED`] begun at `at`, a minute given
/// as `HH:MM` on 20 September, from the ordinal `base` of what came before
/// when there is one, holding each of `said`: its ordinal, who said it and
/// what, written that many seconds after it began.
fn resumed_rollout(home: &Path, at: &str, base: Option<i64>, said: &[(i64, &str, &str)]) {
    let mut meta = json!({"id": RESUMED, "cwd": "/work/lexer", "model_provider": "openai"});
    if let Some(base) = base {
        meta["history_base"] =
            json!({"thread_id": RESUMED, "end_ordinal_exclusive": base, "end_byte_offset": 0});
    }
    let mut lines = vec![json!({"type": "session_meta", "ordinal": base.unwrap_or(0),
                                "timestamp": format!("2026-09-20T{at}:00.000Z"), "payload": meta})];
    for (ordinal, role, text) in said {
        let part = if *role == "user" {
            "input_text"
        } else {
            "output_text"
        };
        lines.push(json!({"type": "response_item", "ordinal": ordinal,
                          "timestamp": format!("2026-09-20T{at}:{ordinal:02}.000Z"),
                          "payload": {"type": "message", "role": role,
                                      "content": [{"type": part, "text": text}]}}));
    }
    let name = at.replace(':', "-");
    write(
        &home
            .join(".codex/sessions/2026/09/20")
            .join(format!("rollout-2026-09-20T{name}-00-{RESUMED}.jsonl")),
        &lines,
    );
}

/// The thread's first rollout, at 08:00, soon stopped: the person asked
/// about the grammar, and the model answered at ordinal 3. The next begins
/// the thread again, numbering it afresh.
fn first_try(home: &Path) {
    resumed_rollout(
        home,
        "08:00",
        None,
        &[
            (1, "user", "Try the grammar"),
            (3, "assistant", "The grammar is ambiguous"),
        ],
    );
}

/// The thread begun again at 09:00: the person asked for a lexer and then a
/// parser, and the model answered each.
fn begun(home: &Path) {
    resumed_rollout(
        home,
        "09:00",
        None,
        &[
            (1, "user", "Sketch the lexer"),
            (2, "assistant", "The lexer splits on whitespace"),
            (3, "user", "Now the parser"),
            (4, "assistant", "The parser recurses into brackets"),
        ],
    );
}

/// The thread taken up again at 10:00 from before the parser was asked for,
/// which took back ordinals 3 and 4, asking about escapes instead.
fn taken_up_again(home: &Path) {
    resumed_rollout(
        home,
        "10:00",
        Some(3),
        &[
            (3, "user", "Tokenize the escapes"),
            (4, "assistant", "Escapes become one token"),
        ],
    );
}

/// Whether the conversation of `session` shows the person or a model saying
/// `word`, having checked that search finds the session by it exactly as
/// often as the conversation shows it said, and that the excerpt falls on
/// the first entry that says it.
fn found_as_shown(engine: &Engine, session: &SessionKey, word: &str) -> bool {
    let shown: Vec<u32> = engine
        .conversation(session)
        .unwrap()
        .iter()
        .filter(|entry| matches!(entry.speaker, Speaker::User | Speaker::Assistant))
        .filter(|entry| entry.text.to_lowercase().contains(word))
        .map(|entry| entry.index)
        .collect();
    let found = search(engine, word)
        .iter()
        .find(|hit| hit.session == *session)
        .map(|hit| hit.matches);
    let excerpt = excerpt(engine, session, word).map(|found| found.index);
    let times = u64::try_from(shown.len()).unwrap();
    assert_eq!(found, (times > 0).then_some(times), "{word}");
    assert_eq!(excerpt, shown.first().copied(), "{word}");
    times > 0
}

#[test]
fn what_a_resumed_thread_took_back_is_not_found_whichever_rollout_is_read_first() {
    let thread = SessionKey::new(Agent::Codex, RESUMED);
    let words = ["grammar", "lexer", "parser", "brackets", "escapes", "token"];
    // What the thread shows: the grammar, from before it began again; the
    // lexer asked for and answered, then the escapes. The parser, asked for
    // and answered at ordinals 3 and 4 before the thread was taken up from
    // 3, was taken back; the grammar's answer, at 3 before the fresh start,
    // was not.
    let shown = [true, true, false, false, true, true];
    let found = |engine: &Engine| -> Vec<bool> {
        words
            .iter()
            .map(|word| found_as_shown(engine, &thread, word))
            .collect()
    };

    // Each rollout read on its own, in order, backwards, and in neither.
    let orders: [[fn(&Path); 3]; 3] = [
        [first_try, begun, taken_up_again],
        [taken_up_again, begun, first_try],
        [begun, taken_up_again, first_try],
    ];
    for order in orders {
        let home = Home::new();
        let engine = home.open();
        for rollout in order {
            rollout(home.path());
            engine.scan().unwrap();
        }
        assert_eq!(found(&engine), shown);
    }
    // All at once.
    let home = Home::new();
    for rollout in [first_try, begun, taken_up_again] {
        rollout(home.path());
    }
    let engine = home.open();
    engine.scan().unwrap();
    assert_eq!(found(&engine), shown);
}

#[test]
fn what_a_revert_took_back_is_not_found() {
    let home = Home::new();
    let writer = opencode::database(&opencode::path(home.path()), "/work");
    let asked = |id: &str, at: i64, text: &str| {
        opencode::message(&writer, id, "user", at, at, &json!({"text": text}));
    };
    let replied = |id: &str, at: i64, text: &str| {
        let data = json!({"model": {"id": "glm-5.3", "providerID": "opencode-go"}, "cost": 0.0,
                          "time": {"created": at}, "content": [{"type": "text", "text": text}],
                          "tokens": {"input": 100, "output": 10, "reasoning": 0, "cache": {"read": 0, "write": 0}}});
        opencode::message(&writer, id, "assistant", at, at, &data);
    };
    asked("msg_1", 1_000, "Draft the parser");
    replied("msg_2", 2_000, "The parser handles escapes");
    asked("msg_3", 3_000, "Rename the tokenizer");
    replied("msg_4", 4_000, "Renamed the tokenizer");
    let engine = home.open();
    engine.scan().unwrap();
    let session = SessionKey::new(Agent::OpenCode, "ses_a");
    let found = |engine: &Engine| -> Vec<bool> {
        ["parser", "tokenizer"]
            .iter()
            .map(|word| found_as_shown(engine, &session, word))
            .collect()
    };
    assert_eq!(found(&engine), [true, true]);

    // The person reverts to before the rename, and OpenCode, committing the
    // revert, deletes the messages it took back and updates the session.
    writer
        .execute_batch(
            "DELETE FROM session_message WHERE id IN ('msg_3', 'msg_4');
             UPDATE session_v2 SET time_updated = 5000 WHERE id = 'ses_a';",
        )
        .unwrap();
    engine.scan().unwrap();
    assert_eq!(found(&engine), [true, false]);
}
