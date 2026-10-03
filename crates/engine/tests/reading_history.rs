//! Reading history gives the same ledger however the history arrives: read as
//! it grows or whole, in any order, rewritten, or deleted afterwards; and
//! what the ledger is read into answers the same, the cache built whole or
//! kept up, usage outside the conversation kept with the report it came
//! from, and sessions found to run within others or to be reviews counted
//! where they belong. The tests of these last share this file's histories
//! of every agent, which is why they are here.

mod history {
    pub mod claude_code;
    pub mod growing;
    pub mod home;
    pub mod opencode;
    pub mod pi;
    pub mod subagents;
}

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use turnscope_engine::{
    Agent, Bucket, Change, Counts, Dimension, Doctor, Engine, Filter, Instant, SessionKey,
    SessionQuery, Span, Speaker, Tokens, Totals, UsageQuery, UsageTable, Zone,
};

use history::growing::append;
use history::home::{Home, lines, write};
use history::{claude_code, opencode, pi, subagents};

// The fork's id sorts before its parent's, so a response whose copy were
// taken for the fork's own would be counted as the fork's rather than
// settled for the parent by the lower key.
const PARENT: &str = "e45f7084-d983-49d9-b587-0cf04a7b4b98";
const FORK: &str = "ac9cd968c38c96ac1";

fn usage(cache_read: u64, cache_write: u64, output: u64) -> Value {
    json!({"input_tokens": 32, "cache_read_input_tokens": cache_read,
           "cache_creation_input_tokens": cache_write, "output_tokens": output,
           "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": cache_write}})
}

fn assistant(agent: Option<&str>, time: &str, id: &str, usage: Value) -> Value {
    claude_code::response(PARENT, agent, time, id, "claude-opus-5", usage)
}

fn user(agent: Option<&str>, time: &str, text: &str) -> Value {
    let mut line = json!({"type": "user", "sessionId": PARENT, "timestamp": time, "cwd": "/work/ledger",
                          "message": {"role": "user", "content": text}});
    if let Some(agent) = agent {
        line["agentId"] = json!(agent);
    }
    line
}

/// The parent session: a prompt, one response reported over three lines,
/// Claude Code's own totals for it and its fork, and two titles it made.
///
/// The response's most complete report is not its last, so a ledger that kept
/// the latest report instead of the largest would be caught. Partway, the
/// session moves to another directory and branch, and its titles are of one
/// rank, so reading it a line at a time must settle each as reading it whole
/// does: the first directory, the latest branch, the latest of equal titles.
fn parent_lines() -> Vec<Value> {
    let mut lines = vec![
        user(None, "2026-09-14T12:00:00.000Z", "Add a file watcher"),
        assistant(
            None,
            "2026-09-14T12:00:10.000Z",
            "1",
            usage(254_907, 9_217, 16),
        ),
        assistant(
            None,
            "2026-09-14T12:00:30.000Z",
            "1",
            usage(254_907, 9_217, 13_695),
        ),
        assistant(
            None,
            "2026-09-14T12:00:20.000Z",
            "1",
            usage(254_907, 9_217, 9_000),
        ),
        json!({"type": "cost-state", "sessionId": PARENT, "modelUsage": {"claude-opus-5[1m]": {
            "inputTokens": 64, "outputTokens": 13_700, "cacheReadInputTokens": 255_907,
            "cacheCreationInputTokens": 9_267, "webSearchRequests": 0, "costUSD": 1.5}}}),
        json!({"type": "ai-title", "sessionId": PARENT, "aiTitle": "Watch the files"}),
        json!({"type": "ai-title", "sessionId": PARENT, "aiTitle": "Watch files for changes"}),
    ];
    for line in &mut lines[..2] {
        line["gitBranch"] = json!("main");
    }
    for line in &mut lines[2..4] {
        line["gitBranch"] = json!("watcher");
        line["cwd"] = json!("/work/ledger/src");
    }
    lines
}

/// The parent session's log, by where it sits under the home directory.
fn parent_log() -> (PathBuf, Vec<Value>) {
    (claude_code::session(Path::new(""), PARENT), parent_lines())
}

/// The fork's log, by where it sits under the home directory: its copy of
/// the parent's response, taken before it finished, which started the fork,
/// then its own prompt and response.
fn fork_log() -> (PathBuf, Vec<Value>) {
    let mut copied = assistant(
        Some(FORK),
        "2026-09-14T12:00:10.000Z",
        "1",
        usage(254_907, 9_217, 16),
    );
    copied["message"]["content"] =
        json!([{"type": "tool_use", "id": "toolu_fork", "name": "Agent", "input": {}}]);
    let lines = vec![
        json!({"type": "fork-context-ref", "agentId": FORK, "parentSessionId": PARENT}),
        copied,
        user(Some(FORK), "2026-09-14T12:00:40.000Z", "Write watch.rs"),
        assistant(
            Some(FORK),
            "2026-09-14T12:00:50.000Z",
            "2",
            usage(1_000, 50, 5),
        ),
    ];
    (subagents::log(Path::new(""), PARENT, FORK), lines)
}

/// A home, with an engine keeping its ledger beside it.
struct Setup {
    home: Home,
    engine: Engine,
}

impl Setup {
    /// A home holding the fork's description, which Claude Code writes as it
    /// starts the fork, and an engine.
    fn new() -> Setup {
        let home = Home::new();
        subagents::describe(
            home.path(),
            PARENT,
            FORK,
            json!({"agentType": "fork", "description": "Write watch.rs", "isFork": true,
                   "spawnDepth": 1, "toolUseId": "toolu_fork"}),
        );
        let engine = home.open();
        Setup { home, engine }
    }

    fn scan(&self) -> Doctor {
        let report = self.engine.scan().unwrap();
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        self.engine.doctor().unwrap()
    }

    /// Scan what changed since the cache was built, and check the cache
    /// caught up on it change by change rather than by building it again.
    fn scan_changes(&self) {
        let changes = self.engine.subscribe();
        self.scan();
        let rebuilt = std::iter::from_fn(|| changes.try_recv().ok()).any(|change| {
            matches!(
                change,
                Change::History {
                    everything: true,
                    ..
                }
            )
        });
        assert!(!rebuilt, "the cache caught up change by change");
    }

    /// Write `(relative, lines)` at `relative` under the home directory.
    fn put(&self, (relative, lines): (PathBuf, Vec<Value>)) {
        write(&self.home.path().join(relative), &lines);
    }
}

/// What a doctor's report says, in a form two reports can be compared in.
fn summary(doctor: &Doctor) -> Vec<String> {
    let mut lines: Vec<String> = doctor
        .agents
        .iter()
        .map(|agent| {
            format!(
                "{} sessions={} responses={} tokens={:?} diagnostics={}",
                agent.agent,
                agent.sessions,
                agent.responses,
                agent.tokens,
                agent.diagnostics.len()
            )
        })
        .collect();
    lines.extend(doctor.checks.iter().map(|check| {
        format!(
            "{} {:?} covers={} reported={:?} observed={:?}",
            check.session,
            check.model,
            check.sessions.len(),
            check.reported,
            check.observed
        )
    }));
    lines
}

/// One log of every agent that writes logs, by where each sits under the
/// home directory: a Claude Code session and its fork, a Codex rollout, a Pi
/// session and a Grok Build session's updates.
fn every_log() -> [(PathBuf, Vec<Value>); 5] {
    [
        parent_log(),
        fork_log(),
        codex_rollout(),
        pi_session(),
        grok_updates(),
    ]
}

/// Ten sessions of all time, most recently active first, empty ones among
/// them and subagents counted in the sessions that ran them.
fn every_session() -> SessionQuery {
    SessionQuery {
        empty: true,
        limit: 10,
        ..SessionQuery::default()
    }
}

/// The sessions `engine` lists for `question`, by key.
fn keys(engine: &Engine, question: &SessionQuery) -> Vec<String> {
    let page = engine.sessions(question).unwrap();
    page.items.iter().map(|row| row.key.to_string()).collect()
}

/// Rollout `lines` as Codex writes them: each with its ordinal, a second
/// apart from 07:`minute` on 3 September.
fn stamped(lines: &[Value], minute: u32) -> Vec<Value> {
    lines
        .iter()
        .enumerate()
        .map(|(ordinal, line)| {
            let mut line = line.clone();
            line["ordinal"] = json!(ordinal);
            line["timestamp"] = json!(format!("2026-09-03T07:{minute}:{ordinal:02}.000Z"));
            line
        })
        .collect()
}

/// A Codex rollout: running totals, then per-response records, as a thread
/// begun on an older version and carried on in a newer one writes them. Its
/// first total comes before its model is named, and counts under it once it
/// is.
fn codex_rollout() -> (PathBuf, Vec<Value>) {
    let usage = |input: u64, cached: u64, output: u64| {
        json!({"input_tokens": input, "cached_input_tokens": cached, "output_tokens": output,
               "reasoning_output_tokens": 0, "total_tokens": input + output})
    };
    // Each running total beside the latest request's usage, as Codex writes
    // them: here one request each, what the total grew by.
    let total = |now: Value, last: Value| {
        json!({"type": "event_msg", "payload": {"type": "token_count",
               "info": {"total_token_usage": now, "last_token_usage": last}}})
    };
    let lines = [
        json!({"type": "session_meta", "payload": {"id": "01a06671-ecc2", "cwd": "/work", "model_provider": "openai"}}),
        total(usage(1_000, 600, 50), usage(1_000, 600, 50)),
        json!({"type": "turn_context", "payload": {"model": "gpt-6-astra"}}),
        total(usage(2_500, 1_800, 120), usage(1_500, 1_200, 70)),
        json!({"type": "token_usage_record", "payload": {"thread_id": "01a06671-ecc2", "response_id": "resp_1",
               "usage": usage(3_000, 2_400, 40)}}),
        total(usage(5_500, 4_200, 160), usage(3_000, 2_400, 40)),
    ];
    (
        PathBuf::from(".codex/sessions/2026/09/03/rollout-2026-09-03T03-45-37-01a06671-ecc2.jsonl"),
        stamped(&lines, 20),
    )
}

/// A review Codex ran for the rollout's thread, asking a model whether to
/// approve an action.
fn codex_review() -> (PathBuf, Vec<Value>) {
    let lines = [
        json!({"type": "session_meta", "payload": {"id": "01a06671-f00d", "cwd": "/work", "model_provider": "openai",
               "parent_thread_id": "01a06671-ecc2", "thread_source": "guardian_review",
               "source": {"subagent": {"other": "guardian"}}}}),
        json!({"type": "event_msg", "payload": {"type": "thread_settings_applied",
               "thread_settings": {"model": "codex-auto-review", "service_tier": "default"}}}),
        json!({"type": "token_usage_record", "payload": {"thread_id": "01a06671-f00d", "response_id": "resp_r1",
               "usage": {"input_tokens": 900, "cached_input_tokens": 0, "output_tokens": 12,
                         "reasoning_output_tokens": 0, "total_tokens": 912}}}),
        json!({"type": "response_item", "payload": {"type": "message", "role": "assistant",
               "content": [{"type": "output_text", "text": "Approved: the command only reads the watcher"}]}}),
    ];
    (
        PathBuf::from(".codex/sessions/2026/09/03/rollout-2026-09-03T03-46-00-01a06671-f00d.jsonl"),
        stamped(&lines, 21),
    )
}

/// A Pi session with a prompt and two responses on different branches.
fn pi_session() -> (PathBuf, Vec<Value>) {
    let assistant = |id: &str, response: &str, output: u64| {
        json!({"type": "message", "id": id, "parentId": "u1", "timestamp": "2026-09-13T08:51:00.000Z",
               "message": {"role": "assistant", "provider": "opencode-go", "model": "glm-5.3", "responseId": response,
                           "timestamp": 1_789_289_428_586u64,
                           "usage": {"input": 3_465, "output": output, "cacheRead": 50, "cacheWrite": 0, "reasoning": 10,
                                     "totalTokens": 3_515 + output, "cost": {"total": 0.005_37}}}})
    };
    let lines = vec![
        pi::header("/work"),
        pi::prompt(),
        assistant("a1", "chatcmpl-1", 115),
        assistant("a2", "chatcmpl-2", 200),
    ];
    (pi::session(Path::new("")), lines)
}

/// A Grok session's updates: a prompt, a finished turn, and a subagent.
fn grok_updates() -> (PathBuf, Vec<Value>) {
    let usage = json!({"inputTokens": 198_728, "outputTokens": 1_740, "cachedReadTokens": 70_272,
                       "cacheCreationTokens": 0, "reasoningTokens": 1_569, "modelCalls": 7,
                       "costUsdTicks": 514_229_600u64});
    let update = |timestamp: i64, update: Value| json!({"timestamp": timestamp, "method": "session/update", "params": {"sessionId": "01a0797e", "update": update}});
    let lines = vec![
        update(
            1_788_837_004,
            json!({"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": "Review it"}}),
        ),
        update(
            1_788_837_070,
            json!({"sessionUpdate": "turn_completed", "prompt_id": "p1",
                                     "usage": {"modelUsage": {"grok-4.6-build": usage}}}),
        ),
        update(
            1_788_837_080,
            json!({"sessionUpdate": "subagent_spawned", "child_session_id": "01a0798a"}),
        ),
    ];
    (
        PathBuf::from(".grok/sessions/%2Fwork/01a0797e/updates.jsonl"),
        lines,
    )
}

#[test]
fn every_agents_logs_read_as_they_grow_total_the_same_as_read_whole() {
    let logs = every_log();

    let growing = Setup::new();
    let mut resumed = 0;
    for (relative, records) in &logs {
        let path = growing.home.path().join(relative);
        // Cut each log at every line and in the middle of every line, reading
        // after each piece arrives.
        let whole = lines(records);
        let bytes = whole.as_bytes();
        let mut cuts: Vec<usize> = bytes
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte == b'\n')
            .flat_map(|(end, _)| [end.saturating_sub(7), end + 1])
            .collect();
        cuts.dedup();
        let mut written = 0;
        for cut in cuts {
            append(&path, &bytes[written..cut]);
            written = cut;
            let report = growing.engine.scan().unwrap();
            assert_eq!(
                report.reread, 0,
                "an appended log is never read again from the start"
            );
            resumed += report.resumed;
        }
    }
    assert!(resumed > 0);
    let grown = growing.scan();

    let once = Setup::new();
    for (relative, records) in &logs {
        write(&once.home.path().join(relative), records);
    }
    let at_once = once.scan();
    assert_eq!(summary(&grown), summary(&at_once));
    assert_eq!(answers(&growing.engine), answers(&once.engine));
    assert_eq!(
        at_once.agents.len(),
        4,
        "Claude Code, Codex, Pi and Grok each read something"
    );
    assert!(
        at_once
            .agents
            .iter()
            .all(|agent| agent.diagnostics.is_empty())
    );
}

#[test]
fn history_read_in_any_order_totals_the_same_and_copies_stay_their_owners() {
    let fork_first = Setup::new();
    fork_first.put(fork_log());
    fork_first.scan();
    fork_first.put(parent_log());
    let later = fork_first.scan();

    let together = Setup::new();
    together.put(fork_log());
    together.put(parent_log());
    let at_once = together.scan();
    assert_eq!(summary(&later), summary(&at_once));
    assert_eq!(answers(&fork_first.engine), answers(&together.engine));

    // Each session's own response: the parent's, complete, is
    // 32 + 254,907 + 9,217 + 13,695 = 277,851 tokens, though the fork copied
    // it, and the fork's is 32 + 1,000 + 50 + 5 = 1,087.
    let own = |engine: &Engine| -> Vec<(String, u64)> {
        let question = SessionQuery {
            subagents: true,
            ..every_session()
        };
        let mut own: Vec<(String, u64)> = engine
            .sessions(&question)
            .unwrap()
            .items
            .iter()
            .map(|row| (row.key.native().to_owned(), row.totals.tokens.total()))
            .collect();
        own.sort();
        own
    };
    let expected = vec![(FORK.to_owned(), 1_087), (PARENT.to_owned(), 277_851)];
    assert_eq!(own(&fork_first.engine), expected);
    assert_eq!(own(&together.engine), expected);

    // Two responses: the parent's, counted once, and the fork's own.
    let claude = &at_once.agents[0];
    assert_eq!((claude.agent, claude.responses), (Agent::ClaudeCode, 2));
    assert_eq!(
        claude.tokens,
        Tokens {
            input: 64,
            cache_read: 255_907,
            cache_write_5m: 0,
            cache_write_1h: 9_267,
            output: 13_700,
            reasoning: 0
        }
    );
    // Claude Code's own totals cover the parent and its fork, and match.
    let check = &at_once.checks[0];
    assert_eq!((check.session.native(), check.sessions.len()), (PARENT, 2));
    let expected = Counts {
        input: 64,
        cache_read: 255_907,
        cache_write: 9_267,
        output: 13_700,
    };
    assert_eq!((check.reported, check.observed), (expected, expected));
}

#[test]
fn a_rewritten_log_is_read_again_from_the_start() {
    let setup = Setup::new();
    let path = claude_code::session(setup.home.path(), PARENT);
    let original = parent_lines()[..5].to_vec();
    write(&path, &original);
    setup.scan();

    // Rewritten in place with other contents, longer than before.
    let rewritten = [
        user(
            None,
            "2026-09-15T09:00:00.000Z",
            "Something else entirely, asked at much greater length than anything before it, \
             so that the file grows as it is rewritten",
        ),
        assistant(None, "2026-09-15T09:00:10.000Z", "9", usage(10, 0, 7)),
        assistant(None, "2026-09-15T09:00:11.000Z", "10", usage(20, 0, 8)),
        assistant(None, "2026-09-15T09:00:12.000Z", "11", usage(30, 0, 9)),
        assistant(None, "2026-09-15T09:00:13.000Z", "12", usage(40, 0, 10)),
    ];
    assert!(lines(&rewritten).len() > lines(&original).len());
    write(&path, &rewritten);
    let doctor = setup.scan();
    let claude = &doctor.agents[0];
    assert_eq!(claude.responses, 4);
    assert_eq!((claude.tokens.cache_read, claude.tokens.output), (100, 34));
    assert!(
        doctor.checks.is_empty(),
        "the old cost-state went with the old contents"
    );

    // Replaced by a new file under the same name.
    let replacement = path.with_extension("tmp");
    write(&replacement, &rewritten[..2]);
    std::fs::rename(&replacement, &path).unwrap();
    let doctor = setup.scan();
    assert_eq!(doctor.agents[0].responses, 1);
    assert_eq!(doctor.agents[0].tokens.output, 7);
}

/// A session whose first line is longer than the 256 bytes a log's opening
/// is kept to, then a response of 20 tokens out, what the person asked next,
/// a response of `output` tokens out, two digits, so a log with another is as
/// long, and a last question longer than the 256 bytes kept of where a read
/// stopped: the lines between are far from both ends.
fn long_opening(asked: &str, output: u64) -> Vec<Value> {
    let long = "Watch the agents' folders for new history, and read what they write as they \
                write it, without reading a whole file again for each line; a file that only \
                grows is read on from where the last read stopped, and one rewritten is read \
                again whole.";
    vec![
        user(None, "2026-09-15T09:00:00.000Z", long),
        assistant(None, "2026-09-15T09:00:10.000Z", "9", usage(0, 0, 20)),
        user(None, "2026-09-15T09:01:00.000Z", asked),
        assistant(None, "2026-09-15T09:01:10.000Z", "10", usage(0, 0, output)),
        user(None, "2026-09-15T09:02:00.000Z", long),
    ]
}

/// What the person asked in `setup`'s session, as its conversation shows it.
fn asked(setup: &Setup) -> Vec<String> {
    let session = SessionKey::parse(&format!("claude-code:{PARENT}")).unwrap();
    setup
        .engine
        .conversation(&session)
        .unwrap()
        .into_iter()
        .filter(|entry| entry.speaker == Speaker::User)
        .map(|entry| entry.text)
        .collect()
}

#[test]
fn a_log_changed_without_growing_is_read_again() {
    let setup = Setup::new();
    let path = claude_code::session(setup.home.path(), PARENT);
    write(&path, &long_opening("Add a file watcher", 30));
    let doctor = setup.scan();
    // 20 + 30 tokens out.
    assert_eq!(doctor.agents[0].tokens.output, 50);
    let first = asked(&setup);

    // Rewritten in place between its ends, as long as it was: the second
    // response 90 out, not 30, and the second question another of the same
    // length.
    let changed = long_opening("Add a file checker", 90);
    assert_eq!(
        lines(&changed).len(),
        lines(&long_opening("Add a file watcher", 30)).len()
    );
    write(&path, &changed);
    let doctor = setup.scan();
    // 20 + 90 tokens out, the first read's 30 gone with the line that said it.
    assert_eq!(doctor.agents[0].responses, 2);
    assert_eq!(doctor.agents[0].tokens.output, 110);
    assert_eq!(first[1], "Add a file watcher");
    assert_eq!(asked(&setup)[1], "Add a file checker");
}

#[test]
fn a_log_cut_short_and_written_past_its_old_end_is_read_again() {
    let setup = Setup::new();
    let path = claude_code::session(setup.home.path(), PARENT);
    let original = long_opening("Add a file watcher", 30);
    write(&path, &original);
    let doctor = setup.scan();
    assert_eq!(doctor.agents[0].tokens.output, 50);
    assert_eq!(asked(&setup)[1], "Add a file watcher");

    // Cut back to its first two lines and written on: another question, a
    // response of 40 out, and one of 5 past where the file ended. The
    // question is as long as makes the first two new lines as long as the
    // two they replace, so where the last read stopped is the start of a
    // line again, and the file opens as it did.
    let response = assistant(None, "2026-09-15T09:03:10.000Z", "11", usage(0, 0, 40));
    let question = |text: &str| user(None, "2026-09-15T09:03:00.000Z", text);
    let replaced = lines(&original[2..]).len();
    let pad = replaced - lines(&[question(""), response.clone()]).len();
    let mut regrown = original[..2].to_vec();
    regrown.push(question(&"x".repeat(pad)));
    regrown.push(response);
    assert_eq!(lines(&regrown).len(), lines(&original).len());
    regrown.push(assistant(
        None,
        "2026-09-15T09:03:20.000Z",
        "12",
        usage(0, 0, 5),
    ));
    write(&path, &regrown);
    let doctor = setup.scan();
    // 20 + 40 + 5 tokens out, in three responses; the 30 went with its line.
    assert_eq!(doctor.agents[0].responses, 3);
    assert_eq!(doctor.agents[0].tokens.output, 65);
    assert_eq!(asked(&setup)[1], "x".repeat(pad));
}

#[test]
fn history_outlives_the_file_it_was_read_from() {
    let setup = Setup::new();
    let path = claude_code::session(setup.home.path(), PARENT);
    write(&path, &parent_lines());
    let before = setup.scan();

    std::fs::remove_file(&path).unwrap();
    let after = setup.scan();
    assert_eq!(after.agents[0].absent, 1);
    assert_eq!(summary(&after), summary(&before));

    // Reopening the ledger finds the same history.
    let reopened = setup.home.open();
    assert_eq!(summary(&reopened.doctor().unwrap()), summary(&before));
}

/// Takes a folder's permissions away until dropped, so a test that fails
/// still leaves it for its temporary directory to remove.
struct Unreadable<'a>(&'a Path);

impl<'a> Unreadable<'a> {
    fn new(folder: &'a Path) -> Unreadable<'a> {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o000)).unwrap();
        Unreadable(folder)
    }
}

impl Drop for Unreadable<'_> {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn a_folder_that_cant_be_read_keeps_what_was_read_from_it() {
    let setup = Setup::new();
    let path = claude_code::session(setup.home.path(), PARENT);
    write(&path, &parent_lines());
    let before = setup.scan();
    assert!(setup.engine.health().unwrap().failed.is_empty());

    // The project's folder can't be listed now, which says nothing of
    // whether its sessions are still there: they are kept as they were,
    // and the look says what it couldn't see.
    let folder = path.parent().unwrap();
    {
        let _unreadable = Unreadable::new(folder);
        let report = setup.engine.scan().unwrap();
        assert!(
            report.failed.iter().any(|(failed, _)| failed == folder),
            "{:?}",
            report.failed
        );
        let doctor = setup.engine.doctor().unwrap();
        assert_eq!(doctor.agents[0].absent, 0);
        assert_eq!(summary(&doctor), summary(&before));
        let health = setup.engine.health().unwrap();
        assert!(health.looked.is_some());
        assert!(health.failed.iter().any(|(failed, _)| failed == folder));
    }

    // Readable again, the next look sees all of it.
    setup.scan();
    assert!(setup.engine.health().unwrap().failed.is_empty());
}

#[test]
fn a_file_that_comes_back_is_there_again() {
    let setup = Setup::new();
    setup.put(background());
    let path = claude_code::session(setup.home.path(), PARENT);
    write(&path, &parent_lines());
    setup.scan();
    let present = || {
        let page = setup.engine.sessions(&every_session()).unwrap();
        let row = page.items.iter().find(|row| row.key.native() == PARENT);
        row.unwrap().present
    };

    let away = setup.home.path().join("away.jsonl");
    std::fs::rename(&path, &away).unwrap();
    setup.scan();
    assert!(!present());
    std::fs::rename(&away, &path).unwrap();
    setup.scan();
    assert!(present());
    assert_eq!(answers(&setup.engine), built_whole(&setup.home));
}

/// Add to `ses_a` in `database` the response `id`, of 100 input and
/// `output` output tokens, at `time`.
fn opencode_message(database: &rusqlite::Connection, id: &str, time: i64, output: u64) {
    let data = json!({"model": {"id": "glm-5.3", "providerID": "opencode-go"}, "cost": 0.0,
                      "time": {"created": time, "completed": time},
                      "tokens": {"input": 100, "output": output, "reasoning": 0, "cache": {"read": 0, "write": 0}}});
    opencode::message(database, id, "assistant", time, time, &data);
}

/// Whether the session `native` of `agent` has a transcript that can be
/// read, as the session list says, and what its conversation gives: its
/// entries, or `None` when it is gone.
fn standing(setup: &Setup, agent: Agent, native: &str) -> (bool, Option<usize>) {
    let page = setup.engine.sessions(&every_session()).unwrap();
    let row = page
        .items
        .iter()
        .find(|row| row.key.native() == native)
        .unwrap();
    let entries = match setup.engine.conversation(&SessionKey::new(agent, native)) {
        Ok(entries) => Some(entries.len()),
        Err(turnscope_engine::Error::Gone(_)) => None,
        Err(error) => panic!("{error}"),
    };
    (row.present, entries)
}

/// What `engine` answers of the usage in `span` that `filter` keeps, told
/// apart `by` a dimension when given.
fn table(engine: &Engine, span: Span, filter: Filter, by: Option<Dimension>) -> UsageTable {
    let question = UsageQuery {
        span,
        filter,
        by,
        every: None,
    };
    engine.usage(&question, &Zone::system()).unwrap()
}

/// The totals of all usage in `span`.
fn total(engine: &Engine, span: Span) -> Totals {
    table(engine, span, Filter::default(), None).total
}

#[test]
fn an_opencode_session_deleted_is_gone_and_one_with_no_messages_is_empty() {
    let setup = Setup::new();
    let path = opencode::path(setup.home.path());
    let database = opencode::database(&path, "/work");
    opencode::message(
        &database,
        "msg_1",
        "user",
        1_000,
        1_000,
        &json!({"text": "Review the scan", "time": {"created": 1_000}}),
    );
    opencode_message(&database, "msg_2", 2_000, 10);
    // A session begun and not yet written to.
    database
        .execute(
            "INSERT INTO session_v2 (id, project_id, slug, directory, version, time_created, time_updated)
             VALUES ('ses_b', 'global', 'ses-b', '/work', '2.0.8', 3000, 3000)",
            [],
        )
        .unwrap();
    setup.scan();
    // The prompt, and a reply of no parts, which shows nothing.
    assert_eq!(standing(&setup, Agent::OpenCode, "ses_a"), (true, Some(1)));
    assert_eq!(standing(&setup, Agent::OpenCode, "ses_b"), (true, Some(0)));
    let before = total(&setup.engine, Span::default()).tokens;
    // 100 input and 10 output tokens in msg_2.
    assert_eq!((before.input, before.output), (100, 10));

    // Deleted in OpenCode, which deletes its messages with it: gone as soon
    // as it is asked for, and, once read, from the session list too.
    database
        .execute("DELETE FROM session_v2 WHERE id = 'ses_a'", [])
        .unwrap();
    assert_eq!(standing(&setup, Agent::OpenCode, "ses_a"), (true, None));
    setup.scan();
    assert_eq!(standing(&setup, Agent::OpenCode, "ses_a"), (false, None));
    assert_eq!(standing(&setup, Agent::OpenCode, "ses_b"), (true, Some(0)));
    // What it used is kept.
    assert_eq!(total(&setup.engine, Span::default()).tokens, before);

    // Put back, as a restore would, it is there again.
    database
        .execute(
            "INSERT INTO session_v2 (id, project_id, slug, directory, version, time_created, time_updated)
             VALUES ('ses_a', 'global', 'ses-a', '/work', '2.0.8', 1000, 4000)",
            [],
        )
        .unwrap();
    setup.scan();
    assert_eq!(standing(&setup, Agent::OpenCode, "ses_a"), (true, Some(0)));
}

#[test]
fn a_log_deleted_since_history_was_read_is_gone_before_it_is_read_again() {
    let setup = Setup::new();
    let (path, lines) = parent_log();
    setup.put((path.clone(), lines));
    setup.scan();
    let key = SessionKey::new(Agent::ClaudeCode, PARENT);
    assert!(setup.engine.conversation(&key).is_ok());

    // Deleted, and not yet looked for again: gone, as it is once it has
    // been, and its usage without its prompts rather than a failure.
    std::fs::remove_file(setup.home.path().join(&path)).unwrap();
    assert!(matches!(
        setup.engine.conversation(&key),
        Err(turnscope_engine::Error::Gone(_))
    ));
    let usage = setup.engine.session_usage(&key).unwrap().unwrap();
    assert!(usage.prompts.is_empty());
}

#[test]
fn a_log_that_cant_be_reached_is_an_error_not_gone() {
    use std::os::unix::fs::PermissionsExt as _;

    let setup = Setup::new();
    let (path, lines) = parent_log();
    setup.put((path.clone(), lines));
    setup.scan();
    let key = SessionKey::new(Agent::ClaudeCode, PARENT);
    // Its folder can no longer be searched: the log is there, out of reach.
    let folder = setup.home.path().join(&path).parent().unwrap().to_owned();
    let mode = |mode| std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(mode));
    mode(0o600).unwrap();
    let conversation = setup.engine.conversation(&key);
    let usage = setup.engine.session_usage(&key);
    mode(0o755).unwrap();
    assert!(
        matches!(&conversation, Err(turnscope_engine::Error::Io { source, .. })
            if source.kind() == std::io::ErrorKind::PermissionDenied),
        "{conversation:?}"
    );
    assert!(usage.is_err(), "not its usage without its prompts");
}

#[test]
fn a_grok_session_whose_conversation_is_deleted_is_gone_though_its_summary_stays() {
    let setup = Setup::new();
    let (updates, lines) = grok_session("01a0797e", 1_788_837_070, 1_000, &[]);
    let folder = setup
        .home
        .path()
        .join(&updates)
        .parent()
        .unwrap()
        .to_owned();
    setup.put((updates, lines));
    write(
        &folder.join("summary.json"),
        &[
            json!({"info": {"cwd": "/work"}, "generated_title": "Review the frontend",
                 "created_at": "2026-09-13T07:23:38Z", "updated_at": "2026-09-13T07:25:00Z"}),
        ],
    );
    let history = folder.join("chat_history.jsonl");
    write(
        &history,
        &[
            json!({"type": "user", "content": [{"type": "text", "text": "Review the frontend"}]}),
            json!({"type": "assistant", "content": "Reading the routes.", "model_id": "grok-4.6-build"}),
        ],
    );
    setup.scan();
    assert_eq!(standing(&setup, Agent::Grok, "01a0797e"), (true, Some(2)));
    let before = total(&setup.engine, Span::default()).tokens;
    // One turn of 1,000 input and 10 output tokens.
    assert_eq!((before.input, before.output), (1_000, 10));

    std::fs::remove_file(&history).unwrap();
    setup.scan();
    assert_eq!(standing(&setup, Agent::Grok, "01a0797e"), (false, None));
    assert_eq!(total(&setup.engine, Span::default()).tokens, before);
}

#[test]
fn an_opencode_session_gives_its_usage_and_its_conversation_in_order() {
    let setup = Setup::new();
    let database = opencode::database(&opencode::path(setup.home.path()), "/work");
    opencode::message(
        &database,
        "msg_1",
        "user",
        1_000,
        1_000,
        &json!({"text": "Review the scan", "time": {"created": 1_000}}),
    );
    // A reply begun before the prompt's row was last written, which only
    // the order OpenCode keeps them in, `seq`, puts after it.
    let reply = json!({"model": {"id": "glm-5.3", "providerID": "opencode-go"}, "cost": 0.0,
                       "time": {"created": 900, "completed": 2_000}, "agent": "build", "finish": "stop",
                       "tokens": {"input": 100, "output": 10, "reasoning": 4, "cache": {"read": 50, "write": 0}},
                       "content": [{"type": "reasoning", "text": "Start with ingest."},
                                   {"type": "tool", "id": "call_1", "name": "read",
                                    "state": {"status": "completed", "input": {"path": "ingest.rs"},
                                              "content": [{"type": "text", "text": "fn scan()"}]}},
                                   {"type": "text", "text": "The scan reads each file once."}]});
    opencode::message(&database, "msg_2", "assistant", 900, 2_000, &reply);
    drop(database);
    setup.scan();

    // Input 100 besides 50 read from the cache, and output 10 with its 4 of
    // reasoning on top: 14.
    let tokens = total(&setup.engine, Span::default()).tokens;
    assert_eq!(
        (tokens.input, tokens.cache_read, tokens.output),
        (100, 50, 14)
    );
    let entries = setup
        .engine
        .conversation(&SessionKey::new(Agent::OpenCode, "ses_a"))
        .unwrap();
    let shown: Vec<(Speaker, &str)> = entries
        .iter()
        .map(|entry| (entry.speaker, entry.text.as_str()))
        .collect();
    assert_eq!(
        shown,
        [
            (Speaker::User, "Review the scan"),
            (Speaker::Reasoning, "Start with ingest."),
            (Speaker::Tool, ""),
            (Speaker::Assistant, "The scan reads each file once."),
        ]
    );
}

#[test]
fn a_database_written_only_to_its_write_ahead_log_is_read_again() {
    let setup = Setup::new();
    let path = opencode::path(setup.home.path());
    // Held open for the whole test, so SQLite keeps new writes in the -wal
    // file instead of checkpointing them into the database on close.
    let writer = opencode::database(&path, "/work");
    writer
        .execute_batch("PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;")
        .unwrap();
    opencode_message(&writer, "msg_1", 1_000, 10);
    assert_eq!(setup.scan().agents[0].responses, 1);

    let database_before = std::fs::metadata(&path).unwrap().len();
    opencode_message(&writer, "msg_2", 2_000, 10);
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        database_before,
        "the write stayed in the log"
    );
    let doctor = setup.scan();
    assert_eq!(
        (doctor.agents[0].agent, doctor.agents[0].responses),
        (Agent::OpenCode, 2)
    );
}

#[test]
fn a_database_read_again_from_the_start_keeps_what_it_said_of_rows_since_deleted() {
    let setup = Setup::new();
    let path = opencode::path(setup.home.path());
    let database = opencode::database(&path, "/work");
    opencode_message(&database, "msg_1", 1_000, 10);
    opencode_message(&database, "msg_2", 2_000, 10);
    drop(database);
    setup.scan();

    // OpenCode's database put back as a new file, as a restore or a
    // migration of its own would: the first response deleted, the second
    // with 5 output tokens where it had 10, and the session moved.
    let replacement = path.with_extension("new");
    let database = opencode::database(&replacement, "/elsewhere");
    opencode_message(&database, "msg_2", 2_000, 5);
    drop(database);
    std::fs::rename(&replacement, &path).unwrap();
    let doctor = setup.scan();

    // Both responses, the first as it was and the second as it is now:
    // 10 + 5 = 15 output tokens.
    let opencode = &doctor.agents[0];
    assert_eq!((opencode.responses, opencode.tokens.output), (2, 15));
    let sessions = setup.engine.sessions(&every_session()).unwrap();
    assert_eq!(sessions.items[0].cwd.as_deref(), Some("/elsewhere"));
}

#[test]
fn a_folder_stands_for_the_projects_inside_it() {
    let setup = Setup::new();
    setup.put(parent_log());
    // Pi's session ran in /work, above Claude Code's in /work/ledger.
    setup.put(pi_session());
    setup.scan();
    let in_folder = |project: &str| Filter {
        projects: vec![project.to_owned()],
        ..Filter::default()
    };
    let responses = |project: &str| {
        let filter = in_folder(project);
        table(&setup.engine, Span::default(), filter, None)
            .total
            .responses
    };
    assert_eq!(responses("/work/ledger"), 1);
    assert_eq!(responses("/work"), 3);
    assert_eq!(responses("/work/"), 3);
    assert_eq!(responses("/work/led"), 0, "a name's prefix is not a folder");
    assert_eq!(responses("/work/ledger/src"), 0);

    // Listing every session, those without usage included, still keeps to
    // the folder and the agents asked for.
    let listed = |filter: Filter| {
        let question = SessionQuery {
            filter,
            ..every_session()
        };
        keys(&setup.engine, &question)
    };
    let claude = format!("claude-code:{PARENT}");
    assert_eq!(listed(in_folder("/work/ledger")), [claude.as_str()]);
    assert_eq!(listed(in_folder("/work")).len(), 2);
    let pi_only = Filter {
        agents: vec![Agent::Pi],
        ..Filter::default()
    };
    assert_eq!(listed(pi_only), ["pi:01a099f5"]);
}

/// Everything the cache answers, in a form two answers can be compared in:
/// usage by session and model by the hour, and every session with its totals.
fn answers(engine: &Engine) -> Vec<String> {
    let zone = Zone::named("America/New_York").unwrap();
    let mut lines = Vec::new();
    for by in [Dimension::Session, Dimension::Model] {
        let question = UsageQuery {
            span: Span::default(),
            filter: Filter::default(),
            by: Some(by),
            every: Some(Bucket::Hour),
        };
        let table = engine.usage(&question, &zone).unwrap();
        lines.extend(table.rows.iter().map(|row| format!("{row:?}")));
        lines.push(format!("{:?}", table.total));
    }
    let sessions = engine
        .sessions(&SessionQuery {
            subagents: true,
            limit: 1_000,
            ..every_session()
        })
        .unwrap();
    lines.extend(sessions.items.iter().map(|row| format!("{row:?}")));
    lines
}

/// What a cache built from nothing answers for the ledger kept beside `home`.
fn built_whole(home: &Home) -> Vec<String> {
    std::fs::remove_file(home.data.path().join("cache.sqlite")).unwrap();
    answers(&home.open())
}

/// A Claude Code session in a project of its own, of forty responses, so
/// that a change to a few responses is worked out change by change, not by
/// building the cache again as for a change to most of what it holds.
fn background() -> (PathBuf, Vec<Value>) {
    let session = "0b5e1e55-2a4c-4d8e-9f10-6c7d8e9fa0b1";
    let lines: Vec<Value> = (0..40)
        .map(|n| {
            json!({"type": "assistant", "sessionId": session,
                   "timestamp": format!("2026-09-10T10:{n:02}:00.000Z"),
                   "requestId": format!("req_bg{n}"), "cwd": "/work/other",
                   "message": {"id": format!("msg_bg{n}"), "model": "claude-opus-5",
                               "usage": usage(100, 0, 10)}})
        })
        .collect();
    (
        PathBuf::from(format!(".claude/projects/-work-other/{session}.jsonl")),
        lines,
    )
}

/// A copy of the ledger at `from`, whole, at `to`.
fn copy_ledger(from: &Path, to: &Path) {
    rusqlite::Connection::open(from)
        .unwrap()
        .execute("VACUUM INTO ?1", [to.to_str().unwrap()])
        .unwrap();
}

/// Put the ledger `copy` in place of the one in `data`, as a person moving
/// files would: the database alone, without the old one's write-ahead log.
fn replace_ledger(data: &Path, copy: &Path) {
    for name in ["ledger.sqlite", "ledger.sqlite-wal", "ledger.sqlite-shm"] {
        match std::fs::remove_file(data.join(name)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => panic!("{error}"),
            _ => {}
        }
    }
    std::fs::rename(copy, data.join("ledger.sqlite")).unwrap();
}

#[test]
fn a_cache_is_built_again_for_a_ledger_it_was_not_built_from() {
    let setup = Setup::new();
    let (home, data) = (setup.home.path(), setup.home.data.path());
    drop(setup.engine);
    let put = |(relative, lines): (PathBuf, Vec<Value>)| write(&home.join(relative), &lines);

    // A copy of the ledger taken before Pi's session was read, restored once
    // the session's file is gone: the cache knew the session, the ledger it
    // is kept beside never did.
    put(background());
    drop(setup.home.scanned());
    let copy = data.join("copy.sqlite");
    copy_ledger(&data.join("ledger.sqlite"), &copy);
    put(pi_session());
    drop(setup.home.scanned());
    std::fs::remove_file(home.join(pi_session().0)).unwrap();
    replace_ledger(data, &copy);
    let restored = setup.home.scanned();
    assert_eq!(answers(&restored), built_whole(&setup.home));
    drop(restored);

    // Another ledger altogether, of other history, and written to more often
    // than the one the cache was built from.
    let other = Setup::new();
    for (relative, records) in every_log() {
        let path = other.home.path().join(relative);
        for line in lines(&records).split_inclusive('\n') {
            append(&path, line.as_bytes());
            other.engine.scan().unwrap();
        }
    }
    copy_ledger(&other.home.data.path().join("ledger.sqlite"), &copy);
    replace_ledger(data, &copy);
    let replaced = setup.home.open();
    assert_eq!(answers(&replaced), built_whole(&setup.home));
}

#[test]
fn a_cache_kept_up_as_history_grows_answers_as_one_built_whole() {
    let growing = Setup::new();
    // History enough beside what grows that each line is worked out change
    // by change, rather than by building the cache again.
    growing.put(background());
    growing.scan();
    for (relative, records) in every_log() {
        let path = growing.home.path().join(relative);
        for line in lines(&records).split_inclusive('\n') {
            append(&path, line.as_bytes());
            growing.scan_changes();
        }
    }
    // A log rewritten, and another deleted, after the cache caught up on them.
    write(
        &pi::session(growing.home.path()),
        &[pi::header("/elsewhere")],
    );
    std::fs::remove_file(growing.home.path().join(grok_updates().0)).unwrap();
    growing.scan_changes();

    let kept_up = answers(&growing.engine);
    assert_eq!(kept_up, built_whole(&growing.home));
    assert!(
        kept_up.len() > 10,
        "the answers cover every agent's sessions"
    );
}

/// The parent session, whose cost-state counts more than its transcript
/// shows: 1,000 more input, 50,000 more cache reads and 500 more output, all
/// of one model, which it names two ways. Together, 1,000 + 32 = 1,032 input,
/// 300,000 + 4,907 = 304,907 cache reads, 9,217 cache writes and
/// 14,000 + 195 = 14,195 output.
fn counted_beyond_the_transcript() -> Vec<Value> {
    let mut lines = parent_lines();
    lines[4] = json!({"type": "cost-state", "sessionId": PARENT, "modelUsage": {
        "claude-opus-5[1m]": {"inputTokens": 1_000, "outputTokens": 14_000, "cacheReadInputTokens": 300_000,
                              "cacheCreationInputTokens": 9_217, "webSearchRequests": 0, "costUSD": 1.9},
        "claude-opus-5": {"inputTokens": 32, "outputTokens": 195, "cacheReadInputTokens": 4_907,
                          "cacheCreationInputTokens": 0, "webSearchRequests": 0, "costUSD": 0.1}}});
    lines
}

#[test]
fn usage_outside_the_conversation_makes_up_the_agents_own_totals() {
    let setup = Setup::new();
    write(
        &claude_code::session(setup.home.path(), PARENT),
        &counted_beyond_the_transcript(),
    );
    setup.scan();

    let total = total(&setup.engine, Span::default());
    // The transcript's one response: input 32, cache read 254,907, an hour's
    // cache writes 9,217, output 13,695.
    assert_eq!(
        (
            total.tokens.input,
            total.tokens.cache_read,
            total.tokens.cache_write(),
            total.tokens.output
        ),
        (1_032, 304_907, 9_217, 14_195)
    );
    assert_eq!(total.outside, 1_000 + 50_000 + 500);
    assert_eq!(
        total.responses, 1,
        "usage outside the conversation is not a response"
    );
    assert!(total.approximate);
}

#[test]
fn usage_outside_a_conversation_without_responses_is_put_when_the_session_was_active() {
    let setup = Setup::new();
    // Claude Code's own totals, before any line says when, and then a
    // prompt at noon that went unanswered.
    let lines = [
        counted_beyond_the_transcript()[4].clone(),
        user(None, "2026-09-14T12:00:00.000Z", "Add a file watcher"),
    ];
    write(&claude_code::session(setup.home.path(), PARENT), &lines);
    setup.scan();
    let day = Span {
        from: Instant::parse("2026-09-14T00:00:00Z"),
        until: Instant::parse("2026-09-15T00:00:00Z"),
    };
    // All of the totals are outside the conversation: 1,032 input,
    // 304,907 cache reads, 9,217 cache writes and 14,195 output make
    // 329,351.
    assert_eq!(total(&setup.engine, day).outside, 329_351);
}

#[test]
fn usage_outside_a_conversation_at_no_time_known_counts_in_all_time_and_in_no_span() {
    let setup = Setup::new();
    // Claude Code's own totals alone, their transcript gone: nothing says
    // when.
    let lines = [counted_beyond_the_transcript()[4].clone()];
    write(&claude_code::session(setup.home.path(), PARENT), &lines);
    setup.scan();
    // 1,032 input, 304,907 cache reads, 9,217 cache writes and 14,195
    // output make 329,351, all of them outside the conversation.
    assert_eq!(total(&setup.engine, Span::default()).outside, 329_351);
    // No stretch of time holds it, however long.
    let ages = Span {
        from: Instant::parse("1900-01-01T00:00:00Z"),
        until: Instant::parse("2100-01-01T00:00:00Z"),
    };
    assert_eq!(total(&setup.engine, ages).outside, 0);
    // By month over all time, it is in the total and in no month.
    let question = UsageQuery {
        span: Span::default(),
        filter: Filter::default(),
        by: None,
        every: Some(Bucket::Month),
    };
    let table = setup.engine.usage(&question, &Zone::system()).unwrap();
    assert_eq!(table.total.outside, 329_351);
    assert_eq!(
        table
            .rows
            .iter()
            .map(|row| (row.start, row.totals.outside))
            .collect::<Vec<_>>(),
        [(None, 329_351)]
    );
}

#[test]
fn usage_outside_the_conversation_goes_with_the_report_it_came_from() {
    let setup = Setup::new();
    setup.put(background());
    let path = claude_code::session(setup.home.path(), PARENT);
    let lines = counted_beyond_the_transcript();
    write(&path, &lines);
    setup.scan();
    assert_eq!(total(&setup.engine, Span::default()).outside, 51_500);

    // Written again without its cost-state.
    write(&path, &lines[..4]);
    setup.scan_changes();
    assert_eq!(total(&setup.engine, Span::default()).outside, 0);
    assert_eq!(answers(&setup.engine), built_whole(&setup.home));
}

/// A Grok Build session's updates: one turn of `input` tokens at
/// `timestamp`, and the subagents it spawned.
fn grok_session(
    session: &str,
    timestamp: i64,
    input: u64,
    spawned: &[&str],
) -> (PathBuf, Vec<Value>) {
    let update = |timestamp: i64, update: Value| {
        json!({"timestamp": timestamp, "method": "session/update",
               "params": {"sessionId": session, "update": update}})
    };
    let usage = json!({"inputTokens": input, "outputTokens": 10, "cachedReadTokens": 0,
                       "cacheCreationTokens": 0, "reasoningTokens": 0, "modelCalls": 1,
                       "costUsdTicks": 1_000u64});
    let mut lines = vec![update(
        timestamp,
        json!({"sessionUpdate": "turn_completed", "prompt_id": "p1",
               "usage": {"modelUsage": {"grok-4.6-build": usage}}}),
    )];
    lines.extend(spawned.iter().map(|child| {
        update(
            timestamp + 10,
            json!({"sessionUpdate": "subagent_spawned", "child_session_id": child}),
        )
    }));
    (
        PathBuf::from(format!(".grok/sessions/%2Fwork/{session}/updates.jsonl")),
        lines,
    )
}

#[test]
fn subagents_move_with_a_session_found_to_run_within_another() {
    let setup = Setup::new();
    setup.put(background());
    // A session and the subagent it spawned, and later the session that
    // spawned the first.
    setup.put(grok_session("01a0797e", 1_788_837_000, 100, &["01a0798a"]));
    setup.put(grok_session("01a0798a", 1_788_837_020, 200, &[]));
    setup.scan();
    setup.put(grok_session("01a0796f", 1_788_836_000, 300, &["01a0797e"]));
    setup.scan_changes();

    let rows = setup
        .engine
        .sessions(&SessionQuery {
            filter: Filter {
                agents: vec![Agent::Grok],
                ..Filter::default()
            },
            subagents: true,
            ..every_session()
        })
        .unwrap()
        .items;
    let own: u64 = rows.iter().map(|row| row.totals.tokens.total()).sum();
    let top = rows.iter().find(|row| row.key.native() == "01a0796f");
    assert_eq!(top.unwrap().with_subagents.tokens.total(), own);
    assert_eq!(answers(&setup.engine), built_whole(&setup.home));
}

#[test]
fn a_session_found_to_be_a_review_is_counted_nowhere() {
    let setup = Setup::new();
    setup.put(background());
    setup.put(codex_rollout());
    // A thread first seen as the rollout's subagent, and then in a rollout
    // that says it reviews the rollout's requests for approval.
    let (_, review) = codex_review();
    let unmarked: Vec<Value> = review
        .into_iter()
        .take(3)
        .map(|mut line| {
            if let Some(meta) = line["payload"].as_object_mut() {
                meta.remove("thread_source");
                meta.remove("source");
            }
            if line["type"] == "token_usage_record" {
                line["payload"]["response_id"] = json!("resp_r0");
            }
            line
        })
        .collect();
    setup.put((
        PathBuf::from(".codex/sessions/2026/09/03/rollout-2026-09-03T03-45-59-01a06671-f00d.jsonl"),
        unmarked,
    ));
    setup.scan();
    let listed = || {
        let question = SessionQuery {
            subagents: true,
            ..every_session()
        };
        keys(&setup.engine, &question)
    };
    assert!(listed().contains(&"codex:01a06671-f00d".to_owned()));

    setup.put(codex_review());
    setup.scan_changes();
    assert!(!listed().contains(&"codex:01a06671-f00d".to_owned()));
    assert_eq!(answers(&setup.engine), built_whole(&setup.home));
}

#[test]
fn sessions_in_which_nothing_happened_are_left_out_unless_asked_for() {
    let setup = Setup::new();
    setup.put(parent_log());
    // A Pi session opened and closed: its header, and nothing else.
    write(&pi::session(setup.home.path()), &[pi::header("/work")]);
    setup.scan();
    let listed = |empty: bool| {
        let question = SessionQuery {
            empty,
            ..every_session()
        };
        keys(&setup.engine, &question)
    };
    let claude = format!("claude-code:{PARENT}");
    assert_eq!(listed(false), [claude.as_str()]);
    assert_eq!(listed(true), [claude.as_str(), "pi:01a099f5"]);
}

#[test]
fn a_sessions_subagents_are_counted_on_it_and_its_reviews_nowhere() {
    let setup = Setup::new();
    setup.put(parent_log());
    setup.put(fork_log());
    setup.put(codex_rollout());
    setup.put(codex_review());
    setup.scan();
    let listed = |subagents_of: Option<SessionKey>| -> Vec<(String, u32, u64, u64)> {
        let question = SessionQuery {
            subagents_of,
            ..every_session()
        };
        let page = setup.engine.sessions(&question).unwrap();
        page.items
            .iter()
            .map(|row| {
                (
                    row.key.to_string(),
                    row.subagents,
                    row.totals.tokens.total(),
                    row.with_subagents.tokens.total(),
                )
            })
            .collect()
    };
    // The fork ran within the parent's process, as its subagent: it is
    // counted on the parent and left out of the list, and asked for, it is
    // the parent's one subagent, with none of its own.
    let parent = format!("claude-code:{PARENT}");
    let fork = format!("claude-code:{FORK}");
    let sessions = listed(None);
    assert_eq!(sessions[0].0, parent);
    assert_eq!(sessions[0].1, 1);
    let under = listed(Some(SessionKey::new(Agent::ClaudeCode, PARENT)));
    assert_eq!(
        under
            .iter()
            .map(|row| (row.0.as_str(), row.1))
            .collect::<Vec<_>>(),
        [(fork.as_str(), 0)]
    );
    assert!(listed(Some(SessionKey::new(Agent::ClaudeCode, FORK))).is_empty());
    // The Codex thread's review, Codex asking a model whether to approve an
    // action, is neither listed under it nor counted: the thread's usage with
    // its subagents is its own, without the review's 912 tokens.
    let (thread, subagents, own, with) = sessions[1].clone();
    assert_eq!((thread.as_str(), subagents), ("codex:01a06671-ecc2", 0));
    assert_eq!(with, own);
    assert!(listed(Some(SessionKey::new(Agent::Codex, "01a06671-ecc2"))).is_empty());
}
