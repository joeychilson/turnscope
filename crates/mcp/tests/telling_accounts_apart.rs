//! An agent is told its own account among several: a second Claude account
//! kept in a folder of its own, one signed into in place of another with
//! `/login`, and one the person hid. Each session is the account's the
//! engine put its responses down to, signed in where and when they were
//! made.

mod history {
    pub mod files;
    pub mod server;
}

use std::path::Path;

use serde_json::{Value, json};
use turnscope_engine::{AccountLimits, Agent, Engine, Instant, LimitState, Subscription};
use turnscope_mcp::{Caller, Server};

use history::files::write;
use history::server::{answer, refusal};

const PERSONAL: &str = "claude:personal";
const WORK: &str = "claude:work";

/// A moment `minutes` from `now`, back when negative.
fn from_now(now: Instant, minutes: i64) -> Instant {
    Instant::from_millis(now.millis() + minutes * 60_000).unwrap()
}

/// `at` as agents write a time.
/// A Claude Code reply in `session`, in /work/atlas, at `at`.
fn reply(session: &str, id: &str, at: Instant) -> Value {
    json!({"type": "assistant", "sessionId": session, "timestamp": at.to_string(),
           "cwd": "/work/atlas", "requestId": format!("req_{id}"),
           "message": {"id": format!("msg_{id}"), "model": "claude-opus-5", "role": "assistant",
                       "usage": {"input_tokens": 1_000, "output_tokens": 100},
                       "content": [{"type": "text", "text": "Done."}]}})
}

/// The session `id`'s replies at each of `minutes` from `now`, kept in
/// Claude Code's `folder` under `home`.
fn claude_session(home: &Path, folder: &str, id: &str, now: Instant, minutes: &[i64]) {
    let replies: Vec<Value> = minutes
        .iter()
        .enumerate()
        .map(|(place, minutes)| reply(id, &format!("{id}-{place}"), from_now(now, *minutes)))
        .collect();
    write(
        &home
            .join(folder)
            .join(format!("projects/-work-atlas/{id}.jsonl")),
        &replies,
    );
}

/// A Claude Max account, signed into Claude Code in `folder` under `home`,
/// with a 5-hour limit `used` percent used.
fn claude(id: &str, home: &Path, folder: &str, used: f64, now: Instant) -> AccountLimits {
    AccountLimits {
        id: id.to_owned(),
        subscription: Subscription::Claude,
        label: Some(format!("{}@example.com", id.trim_start_matches("claude:"))),
        plan: Some("max".to_owned()),
        agents: vec![Agent::ClaudeCode],
        signed_in: true,
        limits: vec![LimitState {
            key: "five_hour".to_owned(),
            name: "5 hours".to_owned(),
            scope: None,
            used: Some(used),
            starts: Some(from_now(now, -120)),
            resets: Some(from_now(now, 180)),
            read_at: now,
            pace: None,
            refilled: false,
        }],
        read_at: None,
        checked_at: None,
        problem: None,
        in_use: false,
        hidden: false,
        provider: None,
        folders: vec![(Agent::ClaudeCode, home.join(folder))],
    }
}

/// Claude Code working in /work/atlas, pointed at `config`, a variable and
/// a folder, where given, and saying it started the server in `session`.
fn claude_code(config: Option<&Path>, session: Option<&str>) -> Caller {
    Caller {
        agent: Some(Agent::ClaudeCode),
        folder: Some("/work/atlas".to_owned()),
        config: config.map(|folder| {
            (
                "CLAUDE_CONFIG_DIR".to_owned(),
                folder.to_string_lossy().into_owned(),
            )
        }),
        session: session.map(str::to_owned),
    }
}

/// Each found session's id and account.
fn found(server: &Server, arguments: Value) -> Vec<(Value, Value)> {
    answer(server, "find_sessions", arguments).data["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| (session["id"].clone(), session["account"].clone()))
        .collect()
}

#[test]
fn a_caller_is_told_the_account_of_the_folder_it_signs_in_from() {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let now = Instant::now();
    // In one project, personal's session in ~/.claude, and work's, later,
    // in ~/.claude-work, where Claude Code wrote its sign-in's file.
    claude_session(home.path(), ".claude", "mine", now, &[-30]);
    claude_session(home.path(), ".claude-work", "theirs", now, &[-20]);
    std::fs::write(home.path().join(".claude-work/.claude.json"), "{}").unwrap();
    let engine = Engine::open(data.path(), home.path()).unwrap();
    engine.scan().unwrap();
    engine
        .record_fixture_limits(
            Subscription::Claude,
            &[
                claude(PERSONAL, home.path(), ".claude", 10.0, from_now(now, -40)),
                claude(WORK, home.path(), ".claude-work", 60.0, from_now(now, -40)),
            ],
            from_now(now, -40),
        )
        .unwrap();
    let server = |caller: Caller| {
        let engine = Engine::open(data.path(), home.path()).unwrap();
        Server::as_it_stands(engine, "UTC")
            .unwrap()
            .with_caller(caller)
    };
    let you = |server: &Server| {
        let you = &answer(server, "check_limits", json!({})).data["you"];
        (you["account"].clone(), you["session"].clone())
    };

    // Its own folder's account and session, though work's is later.
    let personal = server(claude_code(None, None));
    assert_eq!(you(&personal), (json!(PERSONAL), json!("claude-code:mine")));
    // Pointed at ~/.claude-work, with a separator after it: work's.
    let work_folder = home.path().join(".claude-work/");
    let at_work = server(claude_code(Some(&work_folder), None));
    assert_eq!(you(&at_work), (json!(WORK), json!("claude-code:theirs")));
    let limits = answer(&at_work, "check_limits", json!({}));
    assert_eq!(
        limits.data["accounts"][0]["name"],
        "Claude Max \u{b7} work@example.com"
    );
    assert!(
        limits
            .said()
            .starts_with("You're using Claude Max \u{b7} work@example.com, in Claude Code."),
        "{}",
        limits.said()
    );
    // Pointed at a folder Turnscope doesn't read: not known.
    let elsewhere = server(claude_code(Some(&home.path().join(".claude-other")), None));
    let unknown = answer(&elsewhere, "check_limits", json!({}));
    assert_eq!(unknown.data["you"]["account"], Value::Null);
    assert!(
        unknown.data["you"]["account_unknown"]
            .as_str()
            .unwrap()
            .contains("a folder Turnscope doesn't read"),
        "{}",
        unknown.data["you"]
    );
    // Each session is the account's it was made under.
    assert_eq!(
        found(&personal, json!({"account": "work@example.com"})),
        [(json!("claude-code:theirs"), json!(WORK))]
    );
    assert_eq!(
        found(&personal, json!({"account": "personal@example.com"})),
        [(json!("claude-code:mine"), json!(PERSONAL))]
    );
    // A subscription names both its accounts, whose usage is asked of alike.
    assert_eq!(
        found(&personal, json!({"account": "claude"})),
        [
            (json!("claude-code:theirs"), json!(WORK)),
            (json!("claude-code:mine"), json!(PERSONAL))
        ]
    );

    // Hidden, personal isn't listed, named, or taken for the caller's.
    engine.set_account_hidden(PERSONAL, true).unwrap();
    let every = answer(&personal, "check_limits", json!({"all": true}));
    let ids: Vec<Value> = every.data["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|account| account["id"].clone())
        .collect();
    assert_eq!(ids, [json!(WORK)]);
    assert_eq!(every.data["you"]["account"], Value::Null);
    assert!(
        every.data["you"]["account_unknown"]
            .as_str()
            .unwrap()
            .contains("hidden in Turnscope (claude:personal)")
    );
    let refused = refusal(
        &personal,
        "check_limits",
        json!({"account": "personal@example.com"}),
    );
    assert!(!refused.contains(PERSONAL), "{refused}");
    // By its id, it is answered.
    let named = answer(&personal, "check_limits", json!({"account": PERSONAL}));
    assert_eq!(named.data["accounts"][0]["id"], PERSONAL);
    let mine =
        &answer(&personal, "find_sessions", json!({"agent": "claude-code"})).data["sessions"];
    let hidden: Vec<(Value, Value, Value)> = mine
        .as_array()
        .unwrap()
        .iter()
        .map(|session| {
            (
                session["id"].clone(),
                session["account"].clone(),
                session["account_hidden"].clone(),
            )
        })
        .collect();
    assert_eq!(
        hidden,
        [
            (json!("claude-code:theirs"), json!(WORK), Value::Null),
            (json!("claude-code:mine"), json!(PERSONAL), json!(true)),
        ]
    );
}

#[test]
fn signed_into_another_account_since_your_session_began_it_is_yours() {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let now = Instant::now();
    // In ~/.claude, looks found personal 60 and 40 minutes ago, then, after
    // a /login, work 10 and 2 minutes ago. The caller's session replied 50
    // and 45 minutes ago, under personal, and 3 minutes ago, under work, so
    // most of it drew on personal, and it is running. Another session
    // replied a minute ago, under work.
    claude_session(home.path(), ".claude", "long", now, &[-50, -45, -3]);
    claude_session(home.path(), ".claude", "fresh", now, &[-1]);
    let engine = Engine::open(data.path(), home.path()).unwrap();
    engine.scan().unwrap();
    for (id, used, minutes) in [
        (PERSONAL, 10.0, -60),
        (PERSONAL, 12.0, -40),
        (WORK, 30.0, -10),
        (WORK, 31.0, -2),
    ] {
        let at = from_now(now, minutes);
        engine
            .record_fixture_limits(
                Subscription::Claude,
                &[claude(id, home.path(), ".claude", used, at)],
                at,
            )
            .unwrap();
    }
    let server = Server::as_it_stands(engine, "UTC")
        .unwrap()
        .with_caller(claude_code(None, Some("long")));
    let limits = answer(&server, "check_limits", json!({}));
    assert_eq!(
        (
            limits.data["you"]["account"].clone(),
            limits.data["you"]["session"].clone()
        ),
        (json!(WORK), json!("claude-code:long"))
    );
    // Its session drew on both, and is given as personal's, as most of it
    // drew on personal.
    assert_eq!(
        found(&server, json!({"account": PERSONAL})),
        [(json!("claude-code:long"), json!(PERSONAL))]
    );
    assert_eq!(
        found(&server, json!({"account": WORK})),
        [
            (json!("claude-code:fresh"), json!(WORK)),
            (json!("claude-code:long"), json!(PERSONAL))
        ]
    );
    // Of what each drew on: 2 of the session's 3 replies personal's, its
    // last and the other session's work's, at 1,100 tokens a reply.
    let usage = answer(&server, "get_usage", json!({"by": "account"}));
    let rows: Vec<(Value, Value)> = usage.data["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["group"].clone(),
                row["usage"]["tokens"]["total"].clone(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [(json!(PERSONAL), json!(2_200)), (json!(WORK), json!(2_200))]
    );
}
