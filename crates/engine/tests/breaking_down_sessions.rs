//! Where a session's usage went: each prompt with what it and the subagents
//! started in it used, however long they went on, each subagent, and its
//! share of the limit it drew on, every part of which adds up to the whole;
//! and a limit's window, shared among the sessions that drew on it, and what
//! nothing here explains.

mod history {
    pub mod claude_code;
    pub mod home;
    pub mod subagents;
}

use serde_json::{Value, json};
use turnscope_engine::{AccountLimits, Agent, Instant, LimitState, SessionKey, Subscription};

use history::home::{Home, write};
use history::{claude_code, subagents};

const SESSION: &str = "7c1d-breakdown";

/// Every response alike: 10 tokens in and 5 out of one model, so each costs
/// the same, whatever its price.
fn usage() -> Value {
    json!({"input_tokens": 10, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0,
           "output_tokens": 5})
}

fn at(text: &str) -> Instant {
    Instant::parse(text).unwrap()
}

/// The session: asked to audit at 10:00, it starts subagents a1 and a2 and
/// replies at 10:00:05; asked to fix at 10:30, it replies at 10:30:05.
fn session_log() -> Vec<Value> {
    let call = |id: &str| {
        json!({"type": "tool_use", "id": id, "name": "Agent",
                                  "input": {"description": "Look", "prompt": "…"}})
    };
    let mut audit = claude_code::response(
        SESSION,
        None,
        "2026-09-27T10:00:05Z",
        "1",
        "claude-opus-5",
        usage(),
    );
    audit["message"]["content"] = json!([call("toolu_1"), call("toolu_2")]);
    let fix = claude_code::response(
        SESSION,
        None,
        "2026-09-27T10:30:05Z",
        "2",
        "claude-opus-5",
        usage(),
    );
    vec![
        json!({"type": "user", "sessionId": SESSION, "timestamp": "2026-09-27T10:00:00Z",
               "cwd": "/work/ledger", "message": {"role": "user", "content": "Audit the ledger"}}),
        audit,
        json!({"type": "user", "sessionId": SESSION, "timestamp": "2026-09-27T10:30:00Z",
               "cwd": "/work/ledger", "message": {"role": "user", "content": "Now fix it"}}),
        fix,
    ]
}

/// The subagent `agent`, started by the call `call`, replying at `times`.
fn subagent(home: &Home, agent: &str, call: &str, times: &[&str]) {
    let responses: Vec<Value> = times
        .iter()
        .enumerate()
        .map(|(index, time)| {
            claude_code::response(
                SESSION,
                Some(agent),
                time,
                &format!("{agent}_{index}"),
                "claude-opus-5",
                usage(),
            )
        })
        .collect();
    write(&subagents::log(home.path(), SESSION, agent), &responses);
    subagents::describe(
        home.path(),
        SESSION,
        agent,
        json!({"description": format!("Subagent {agent}"), "toolUseId": call}),
    );
}

/// A Claude account whose week started at 9:00, read at `read` with `used`
/// of it used.
fn week(engine: &turnscope_engine::Engine, read: &str, used: f64) {
    let account = AccountLimits {
        id: "claude-account".to_owned(),
        subscription: Subscription::Claude,
        label: None,
        plan: Some("max".to_owned()),
        agents: vec![Agent::ClaudeCode],
        signed_in: true,
        limits: vec![LimitState {
            key: "seven_day".to_owned(),
            name: "Weekly".to_owned(),
            scope: None,
            used: Some(used),
            starts: Some(at("2026-09-27T09:00:00Z")),
            resets: Some(at("2026-10-04T09:00:00Z")),
            read_at: at(read),
            pace: None,
            refilled: false,
        }],
        read_at: Some(at(read)),
        checked_at: Some(at(read)),
        problem: None,
        in_use: true,
        hidden: false,
        provider: None,
        folders: Vec::new(),
    };
    engine
        .record_fixture_limits(Subscription::Claude, &[account], at(read))
        .unwrap();
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn a_session_breaks_down_by_prompt_and_subagent_and_shares_its_limit() {
    let home = Home::new();
    write(&claude_code::session(home.path(), SESSION), &session_log());
    // a1 replies at 10:00:10 and goes on after the second prompt, at
    // 10:31:00; a2 replies once, at 10:00:10.
    subagent(
        &home,
        "a1",
        "toolu_1",
        &["2026-09-27T10:00:10Z", "2026-09-27T10:31:00Z"],
    );
    subagent(&home, "a2", "toolu_2", &["2026-09-27T10:00:10Z"]);
    let engine = home.scanned();
    week(&engine, "2026-09-27T10:15:00Z", 10.0);
    week(&engine, "2026-09-27T10:45:00Z", 30.0);

    let key = SessionKey::new(Agent::ClaudeCode, SESSION);
    let usage = engine.session_usage(&key).unwrap().unwrap();

    let said: Vec<&str> = usage
        .prompts
        .iter()
        .map(|prompt| prompt.said.as_str())
        .collect();
    assert_eq!(said, ["Audit the ledger", "Now fix it"]);
    // The audit started both subagents, a1 going on after the fix was
    // asked for; the fix, none.
    assert_eq!(usage.prompts[0].subagents, 2);
    assert_eq!(usage.prompts[1].subagents, 0);

    let subagents: Vec<&str> = usage
        .subagents
        .iter()
        .map(|subagent| subagent.key.native())
        .collect();
    assert_eq!(subagents, ["a1", "a2"]);

    // The week rose 10 by 10:15, while the audit's reply, a1 and a2 each
    // spent one equal part: 10/3 each. It rose 20 more by 10:45, while the
    // fix's reply and a1's late one spent: 10 each. So the audit took
    // 3 x 10/3 + 10 = 20 and the fix 10, of 30; a1 10/3 + 10 and a2 10/3.
    assert_eq!(usage.limits.len(), 1);
    let week = &usage.limits[0];
    assert_eq!(week.key, "seven_day");
    assert!(week.whole);
    assert!(close(week.share, 30.0), "{}", week.share);
    assert!(close(week.prompts[0], 20.0), "{:?}", week.prompts);
    assert!(close(week.prompts[1], 10.0), "{:?}", week.prompts);
    assert!(close(week.unprompted, 0.0));
    assert!(
        close(week.subagents[0], 10.0 / 3.0 + 10.0),
        "{:?}",
        week.subagents
    );
    assert!(close(week.subagents[1], 10.0 / 3.0), "{:?}", week.subagents);
}

#[test]
fn a_session_no_reading_covers_takes_no_share_and_one_unknown_has_no_breakdown() {
    let home = Home::new();
    write(&claude_code::session(home.path(), SESSION), &session_log());
    let engine = home.scanned();
    let usage = engine
        .session_usage(&SessionKey::new(Agent::ClaudeCode, SESSION))
        .unwrap()
        .unwrap();
    // No reading of any limit: the share is unknown, so there is none.
    assert!(usage.limits.is_empty());
    assert_eq!(usage.prompts.len(), 2);
    let missing = engine
        .session_usage(&SessionKey::new(Agent::ClaudeCode, "nowhere"))
        .unwrap();
    assert_eq!(missing, None);
}

#[test]
fn a_limits_window_is_shared_among_sessions_and_what_nothing_here_spent_on() {
    let home = Home::new();
    write(&claude_code::session(home.path(), SESSION), &session_log());
    subagent(
        &home,
        "a1",
        "toolu_1",
        &["2026-09-27T10:00:10Z", "2026-09-27T10:31:00Z"],
    );
    subagent(&home, "a2", "toolu_2", &["2026-09-27T10:00:10Z"]);
    // Another session, in another project, replying once at 10:05.
    let other = "9e2f-other";
    let mut reply = claude_code::response(
        other,
        None,
        "2026-09-27T10:05:00Z",
        "o1",
        "claude-opus-5",
        usage(),
    );
    reply["cwd"] = json!("/work/atlas");
    write(
        &claude_code::session(home.path(), other),
        &[
            json!({"type": "user", "sessionId": other, "timestamp": "2026-09-27T10:04:00Z",
                   "cwd": "/work/atlas", "message": {"role": "user", "content": "Map the tiles"}}),
            reply,
        ],
    );
    let engine = home.scanned();
    week(&engine, "2026-09-27T10:15:00Z", 10.0);
    week(&engine, "2026-09-27T10:45:00Z", 30.0);
    week(&engine, "2026-09-27T11:15:00Z", 35.0);

    let window = engine
        .limit_window("claude-account", "seven_day")
        .unwrap()
        .unwrap();
    // From none at 9:00, then each reading; a limit never read has none.
    let used: Vec<f64> = window.track.points.iter().map(|(_, used)| *used).collect();
    assert_eq!(used, [0.0, 10.0, 30.0, 35.0]);
    assert_eq!(
        engine.limit_window("claude-account", "five_hour").unwrap(),
        None
    );
    // To 10:15 it rose 10 while four equal responses spent: the session's
    // reply and its two subagents' first, three of them, and the other
    // session's one: 7.5 and 2.5. To 10:45 it rose 20 while only the
    // session spent, its fix and a1's late reply. To 11:15 it rose 5
    // while nothing here spent: elsewhere. So 27.5 and 2.5, of 35.
    let sessions: Vec<(&str, f64)> = window
        .sessions
        .iter()
        .map(|(key, share)| (key.native(), *share))
        .collect();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].0, SESSION);
    assert!(close(sessions[0].1, 27.5), "{sessions:?}");
    assert_eq!(sessions[1].0, other);
    assert!(close(sessions[1].1, 2.5), "{sessions:?}");
    assert!(close(window.elsewhere, 5.0), "{}", window.elsewhere);
    let projects: Vec<(Option<&str>, f64)> = window
        .projects
        .iter()
        .map(|project| (project.root.as_deref(), project.share))
        .collect();
    let names: Vec<Option<&str>> = window
        .projects
        .iter()
        .map(|project| project.name.as_deref())
        .collect();
    assert_eq!(names, [Some("ledger"), Some("atlas")]);
    assert_eq!(projects.len(), 2);
    assert!(
        projects[0].0.is_some_and(|root| root.ends_with("ledger")) && close(projects[0].1, 27.5),
        "{projects:?}"
    );
    assert!(
        projects[1].0.is_some_and(|root| root.ends_with("atlas")) && close(projects[1].1, 2.5),
        "{projects:?}"
    );
    assert_eq!(window.models.len(), 1);
    assert!(close(window.models[0].1, 30.0));
}

#[test]
fn a_session_read_without_its_conversation_takes_the_same_shares_unprompted() {
    let home = Home::new();
    write(&claude_code::session(home.path(), SESSION), &session_log());
    subagent(
        &home,
        "a1",
        "toolu_1",
        &["2026-09-27T10:00:10Z", "2026-09-27T10:31:00Z"],
    );
    subagent(&home, "a2", "toolu_2", &["2026-09-27T10:00:10Z"]);
    let engine = home.scanned();
    week(&engine, "2026-09-27T10:15:00Z", 10.0);
    week(&engine, "2026-09-27T10:45:00Z", 30.0);

    let key = SessionKey::new(Agent::ClaudeCode, SESSION);
    let whole = engine.session_usage(&key).unwrap().unwrap();
    let without = engine.session_usage_without_prompts(&key).unwrap().unwrap();
    // No prompt is read, so the 30 the session took of the week, 20 under
    // the audit and 10 under the fix as the first test works out, is all
    // unprompted; its subagents and their shares are the same.
    assert!(without.prompts.is_empty());
    assert_eq!(without.subagents, whole.subagents);
    let (week, read) = (&whole.limits[0], &without.limits[0]);
    assert!(close(read.share, week.share), "{}", read.share);
    assert!(close(read.unprompted, 30.0), "{}", read.unprompted);
    assert_eq!(read.subagents.len(), week.subagents.len());
    for (read, whole) in read.subagents.iter().zip(&week.subagents) {
        assert!(close(*read, *whole), "{read} {whole}");
    }
}

#[test]
fn a_sessions_largest_context_is_read_without_its_conversation() {
    let home = Home::new();
    // Two responses: 1,000 in with 30,000 read from the cache and 2,000
    // written to it, a context of 33,000; then 500 in with 40,000 read, a
    // context of 40,500, the larger.
    let response = |id: &str, time: &str, usage: Value| {
        claude_code::response(SESSION, None, time, id, "claude-opus-5", usage)
    };
    write(
        &claude_code::session(home.path(), SESSION),
        &[
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
    let engine = home.scanned();
    let key = SessionKey::new(Agent::ClaudeCode, SESSION);
    let contexts = engine.largest_contexts(std::slice::from_ref(&key)).unwrap();
    assert_eq!(contexts.get(&key), Some(&40_500));
    // A session with no response has no context known.
    let none = SessionKey::new(Agent::Codex, "nothing");
    assert_eq!(engine.largest_contexts(&[none]).unwrap().len(), 0);
}
