//! Accounts as the person keeps them: two accounts of one subscription in
//! two folders, each with the usage made where it was signed in; usage told
//! apart and kept to the account it drew on; use of an API key kept as an
//! account of its provider's, with the limit a key carries of its own; and
//! whether it is shown, as the person sets it.

mod history {
    pub mod claude_code;
    pub mod home;
    pub mod pi;
}

use std::path::PathBuf;

use serde_json::{Value, json};
use turnscope_engine::{
    AccountLimits, Agent, Dimension, Engine, Error, Filter, Instant, LimitState, SessionQuery,
    Span, Subscription, UsageQuery, Zone,
};

use history::claude_code::{self, response};
use history::home::{Home, write};
use history::pi;

/// A synthetic account of `subscription` used by `agent`, signed in at
/// `folders`, with a five-hour limit `used` percent used, as read at `now`.
fn account(
    id: &str,
    subscription: Subscription,
    agent: Agent,
    folders: Vec<(Agent, PathBuf)>,
    used: f64,
    now: Instant,
) -> AccountLimits {
    AccountLimits {
        id: id.to_owned(),
        subscription,
        label: None,
        plan: None,
        agents: vec![agent],
        signed_in: true,
        limits: vec![LimitState {
            key: "five_hour".to_owned(),
            name: "5 hours".to_owned(),
            scope: None,
            used: Some(used),
            starts: None,
            resets: None,
            read_at: now,
            pace: None,
            runs_out: None,
            refilled: false,
        }],
        read_at: Some(now),
        checked_at: Some(now),
        problem: None,
        in_use: false,
        hidden: false,
        provider: None,
        folders,
    }
}

/// Every account as the engine gives it, by id.
fn accounts(engine: &Engine) -> Vec<AccountLimits> {
    let mut accounts = engine.limits().unwrap();
    accounts.sort_by(|a, b| a.id.cmp(&b.id));
    accounts
}

#[test]
fn two_accounts_of_one_subscription_keep_the_use_made_where_each_is_signed_in() {
    let home = Home::new();
    let own = home.path().join(".claude");
    let work = home.path().join(".claude-work");
    let usage = || json!({"input_tokens": 10, "output_tokens": 5});
    write(
        &claude_code::session(home.path(), "own"),
        &[response(
            "own",
            None,
            "2026-09-27T10:00:00Z",
            "1",
            "claude-opus-5",
            usage(),
        )],
    );
    write(
        &work.join("projects/-work-ledger/work.jsonl"),
        &[response(
            "work",
            None,
            "2026-09-27T10:05:00Z",
            "2",
            "claude-opus-5",
            usage(),
        )],
    );
    std::fs::write(work.join(".claude.json"), "{}").unwrap();
    let engine = home.scanned();
    let now = Instant::parse("2026-09-27T11:00:00Z").unwrap();
    engine
        .record_fixture_limits(
            Subscription::Claude,
            &[
                account(
                    "claude:personal",
                    Subscription::Claude,
                    Agent::ClaudeCode,
                    vec![(Agent::ClaudeCode, own.clone())],
                    10.0,
                    now,
                ),
                account(
                    "claude:work",
                    Subscription::Claude,
                    Agent::ClaudeCode,
                    vec![(Agent::ClaudeCode, work.clone())],
                    20.0,
                    now,
                ),
            ],
            now,
        )
        .unwrap();

    let sessions: Vec<(String, Option<String>)> = {
        let mut sessions: Vec<(String, Option<String>)> = engine
            .sessions(&SessionQuery {
                empty: true,
                ..SessionQuery::default()
            })
            .unwrap()
            .items
            .into_iter()
            .map(|row| (row.key.native().to_owned(), row.account))
            .collect();
        sessions.sort();
        sessions
    };
    assert_eq!(
        sessions,
        [
            ("own".to_owned(), Some("claude:personal".to_owned())),
            ("work".to_owned(), Some("claude:work".to_owned())),
        ]
    );
    let folders: Vec<(String, Vec<(Agent, PathBuf)>)> = accounts(&engine)
        .into_iter()
        .map(|account| (account.id, account.folders))
        .collect();
    assert_eq!(
        folders,
        [
            ("claude:personal".to_owned(), vec![(Agent::ClaudeCode, own)]),
            ("claude:work".to_owned(), vec![(Agent::ClaudeCode, work)]),
        ]
    );

    // The person hides one.
    engine.set_account_hidden("claude:personal", true).unwrap();
    let hidden: Vec<(String, bool)> = accounts(&engine)
        .into_iter()
        .map(|account| (account.id, account.hidden))
        .collect();
    assert_eq!(
        hidden,
        [
            ("claude:personal".to_owned(), true),
            ("claude:work".to_owned(), false),
        ]
    );
}

#[test]
fn signing_into_another_account_in_one_folder_moves_the_use_made_after() {
    // One folder, ~/.claude: looks find the personal account at 09:00 and
    // 10:00, then, after a `/login`, the work account at 12:00 and 13:00.
    let home = Home::new();
    let own = home.path().join(".claude");
    let usage = || json!({"input_tokens": 10, "output_tokens": 5});
    let said = |session: &str, time: &str, id: &str| {
        response(session, None, time, id, "claude-opus-5", usage())
    };
    // 08:00 is before the first look, so the first account found's. 10:50
    // is 50 minutes after the last look to find personal and 70 before the
    // first to find work, so personal's; 11:40 is 100 after and 20 before,
    // so work's. `across` made one response under personal and two under
    // work, so is work's, as most of its responses are.
    for (session, responses) in [
        ("before", vec![said("before", "2026-09-27T08:00:00Z", "1")]),
        (
            "personal",
            vec![said("personal", "2026-09-27T09:30:00Z", "2")],
        ),
        (
            "nearer-personal",
            vec![said("nearer-personal", "2026-09-27T10:50:00Z", "3")],
        ),
        (
            "nearer-work",
            vec![said("nearer-work", "2026-09-27T11:40:00Z", "4")],
        ),
        (
            "across",
            vec![
                said("across", "2026-09-27T10:20:00Z", "5"),
                said("across", "2026-09-27T12:20:00Z", "6"),
                said("across", "2026-09-27T12:40:00Z", "7"),
            ],
        ),
    ] {
        write(&claude_code::session(home.path(), session), &responses);
    }
    let engine = home.scanned();
    let signed_in = |id: &str, used: f64, at: &str| {
        let at = Instant::parse(at).unwrap();
        let found = account(
            id,
            Subscription::Claude,
            Agent::ClaudeCode,
            vec![(Agent::ClaudeCode, own.clone())],
            used,
            at,
        );
        engine
            .record_fixture_limits(Subscription::Claude, &[found], at)
            .unwrap();
    };
    signed_in("claude:personal", 10.0, "2026-09-27T09:00:00Z");
    signed_in("claude:personal", 12.0, "2026-09-27T10:00:00Z");
    signed_in("claude:work", 30.0, "2026-09-27T12:00:00Z");
    signed_in("claude:work", 31.0, "2026-09-27T13:00:00Z");
    // Made after the last look, under work.
    write(
        &claude_code::session(home.path(), "after"),
        &[said("after", "2026-09-27T13:30:00Z", "8")],
    );
    engine.scan().unwrap();

    let mut sessions: Vec<(String, Option<String>)> = engine
        .sessions(&SessionQuery {
            empty: true,
            ..SessionQuery::default()
        })
        .unwrap()
        .items
        .into_iter()
        .map(|row| (row.key.native().to_owned(), row.account))
        .collect();
    sessions.sort();
    let personal = || Some("claude:personal".to_owned());
    let work = || Some("claude:work".to_owned());
    assert_eq!(
        sessions,
        [
            ("across".to_owned(), work()),
            ("after".to_owned(), work()),
            ("before".to_owned(), personal()),
            ("nearer-personal".to_owned(), personal()),
            ("nearer-work".to_owned(), work()),
            ("personal".to_owned(), personal()),
        ]
    );
    // Signed out of personal, it is kept with its last reading.
    let kept: Vec<(String, bool, Option<f64>)> = accounts(&engine)
        .into_iter()
        .map(|account| (account.id, account.signed_in, account.limits[0].used))
        .collect();
    assert_eq!(
        kept,
        [
            ("claude:personal".to_owned(), false, Some(12.0)),
            ("claude:work".to_owned(), true, Some(31.0)),
        ]
    );
}

#[test]
fn usage_is_told_apart_and_kept_to_the_account_it_drew_on() {
    // ~/.claude is signed into personal at 09:00 and, after a `/login`,
    // into work at 12:00; ~/.claude-other holds a sign-in never looked at.
    let home = Home::new();
    let own = home.path().join(".claude");
    let other = home.path().join(".claude-other");
    let said = |session: &str, time: &str, id: &str, input: u64, output: u64| {
        let usage = json!({"input_tokens": input, "output_tokens": output});
        response(session, None, time, id, "claude-opus-5", usage)
    };
    // 10:20 is 80 minutes after personal was found and 100 before work
    // was, so personal's.
    write(
        &claude_code::session(home.path(), "morning"),
        &[said("morning", "2026-09-27T09:30:00Z", "1", 100, 10)],
    );
    write(
        &claude_code::session(home.path(), "across"),
        &[
            said("across", "2026-09-27T10:20:00Z", "2", 200, 20),
            said("across", "2026-09-27T12:20:00Z", "3", 300, 30),
        ],
    );
    write(
        &claude_code::session(home.path(), "afternoon"),
        &[said("afternoon", "2026-09-27T12:40:00Z", "4", 400, 40)],
    );
    write(
        &other.join("projects/-work-ledger/other.jsonl"),
        &[said("other", "2026-09-27T11:00:00Z", "5", 1_000, 100)],
    );
    std::fs::write(other.join(".claude.json"), "{}").unwrap();
    // Read before any look, so all of it is put down to an account only
    // when the looks are recorded.
    let engine = home.scanned();
    for (id, at) in [
        ("claude:personal", "2026-09-27T09:00:00Z"),
        ("claude:work", "2026-09-27T12:00:00Z"),
    ] {
        let at = Instant::parse(at).unwrap();
        let found = account(
            id,
            Subscription::Claude,
            Agent::ClaudeCode,
            vec![(Agent::ClaudeCode, own.clone())],
            10.0,
            at,
        );
        engine
            .record_fixture_limits(Subscription::Claude, &[found], at)
            .unwrap();
    }
    // Read after the looks, as the cache catches up on it.
    write(
        &claude_code::session(home.path(), "late"),
        &[said("late", "2026-09-27T13:00:00Z", "6", 50, 5)],
    );
    engine.scan().unwrap();

    // Personal: 100 + 10 at 09:30 and 200 + 20 at 10:20, 330. Work: 300 +
    // 30 at 12:20, 400 + 40 at 12:40 and 50 + 5 at 13:00, 825. No account
    // known: 1,000 + 100 at 11:00, 1,100. `across` is in both.
    let everything = Filter::default();
    let by_account = |span: Span, filter: &Filter| -> Vec<(Option<String>, u64)> {
        engine
            .usage(
                &UsageQuery {
                    span,
                    filter: filter.clone(),
                    by: Some(Dimension::Account),
                    every: None,
                },
                &Zone::named("UTC").unwrap(),
            )
            .unwrap()
            .rows
            .into_iter()
            .map(|row| (row.group, row.totals.tokens.total()))
            .collect()
    };
    assert_eq!(
        by_account(Span::default(), &everything),
        [
            (Some(String::new()), 1_100),
            (Some("claude:personal".to_owned()), 330),
            (Some("claude:work".to_owned()), 825),
        ]
    );
    // From 10:10 to 12:25, both ends within a quarter hour: 220 of
    // personal's at 10:20, 1,100 at 11:00 and 330 of work's at 12:20.
    let cut = Span {
        from: Instant::parse("2026-09-27T10:10:00Z"),
        until: Instant::parse("2026-09-27T12:25:00Z"),
    };
    assert_eq!(
        by_account(cut, &everything),
        [
            (Some(String::new()), 1_100),
            (Some("claude:personal".to_owned()), 220),
            (Some("claude:work".to_owned()), 330),
        ]
    );

    // Kept to work: 825 of all time, and the three sessions made under it,
    // each with only its usage under work: 330 of across's 550.
    let work = Filter {
        accounts: vec!["claude:work".to_owned()],
        ..Filter::default()
    };
    assert_eq!(
        by_account(Span::default(), &work),
        [(Some("claude:work".to_owned()), 825)]
    );
    assert_eq!(
        by_account(cut, &work),
        [(Some("claude:work".to_owned()), 330)]
    );
    let mut sessions: Vec<(String, u64)> = engine
        .sessions(&SessionQuery {
            filter: work.clone(),
            ..SessionQuery::default()
        })
        .unwrap()
        .items
        .into_iter()
        .map(|row| (row.key.native().to_owned(), row.totals.tokens.total()))
        .collect();
    sessions.sort();
    assert_eq!(
        sessions,
        [
            ("across".to_owned(), 330),
            ("afternoon".to_owned(), 440),
            ("late".to_owned(), 55)
        ]
    );
}

/// A Pi response at `now`, of `provider`'s `model`, costing
/// `cost` dollars by Pi's own reckoning.
fn pi_response(id: &str, provider: &str, model: &str, cost: f64, now: Instant) -> Value {
    json!({"type": "message", "id": id, "parentId": "u1", "timestamp": now.to_string(),
           "message": {"role": "assistant", "provider": provider, "model": model,
                       "responseId": id, "timestamp": now.millis(),
                       "usage": {"input": 100, "output": 10, "cacheRead": 0, "cacheWrite": 0,
                                 "totalTokens": 110, "cost": {"total": cost}}}})
}

#[test]
fn use_of_an_api_key_is_an_account_of_its_providers() {
    let home = Home::new();
    let now = Instant::now();
    // Pi, signed into OpenCode Go, used it, and Anthropic with a key from
    // the environment: a model the catalog doesn't price, at Pi's own $0.25.
    write(
        &pi::session(home.path()),
        &[
            pi::header("/work"),
            pi::prompt(),
            pi_response("a1", "opencode-go", "glm-5.3", 0.01, now),
            pi_response("a2", "anthropic", "claude-unlisted-1", 0.25, now),
        ],
    );
    let engine = home.scanned();
    let go = account(
        "opencode-go:go",
        Subscription::OpenCodeGo,
        Agent::Pi,
        Vec::new(),
        5.0,
        now,
    );
    engine
        .record_fixture_limits(Subscription::OpenCodeGo, &[go], now)
        .unwrap();

    let api = || {
        accounts(&engine)
            .into_iter()
            .find(|account| account.id == "api:anthropic")
            .unwrap()
    };
    let anthropic = api();
    assert_eq!(anthropic.subscription, Subscription::ApiKey);
    assert_eq!(anthropic.provider.as_deref(), Some("anthropic"));
    assert_eq!(anthropic.agents, [Agent::Pi]);
    assert!(
        anthropic.in_use,
        "a response of the last half hour drew on it"
    );
    assert!(anthropic.limits.is_empty(), "no key of its carries a limit");
}

#[test]
fn a_keys_own_limit_is_drawn_from_its_readings() {
    let home = Home::new();
    let engine = home.open();
    // An OpenRouter key whose limit, in a window that started two hours
    // ago, was read 30% used an hour ago, and then 45% now.
    let now = Instant::now();
    let hours_ago = |hours: i64| Instant::from_millis(now.millis() - hours * 3_600_000).unwrap();
    let read = |used: f64, at: Instant| AccountLimits {
        id: "api:openrouter".to_owned(),
        subscription: Subscription::ApiKey,
        provider: Some("openrouter".to_owned()),
        limits: vec![LimitState {
            key: "key:abc".to_owned(),
            name: "Monthly key limit".to_owned(),
            starts: Some(hours_ago(2)),
            read_at: at,
            ..account("", Subscription::ApiKey, Agent::Pi, Vec::new(), used, at).limits[0].clone()
        }],
        agents: Vec::new(),
        ..account("", Subscription::ApiKey, Agent::Pi, Vec::new(), used, at)
    };
    engine
        .record_fixture_limits(
            Subscription::ApiKey,
            &[read(30.0, hours_ago(1))],
            hours_ago(1),
        )
        .unwrap();
    engine
        .record_fixture_limits(Subscription::ApiKey, &[read(45.0, now)], now)
        .unwrap();
    let used: Vec<f64> = engine
        .limit_window("api:openrouter", "key:abc")
        .unwrap()
        .unwrap()
        .track
        .points
        .iter()
        .map(|(_, used)| *used)
        .collect();
    assert_eq!(used, [0.0, 30.0, 45.0]);
}

#[test]
fn only_an_accounts_id_can_be_hidden() {
    let home = Home::new();
    let engine = home.open();
    let invalid =
        |result: turnscope_engine::Result<()>| matches!(result, Err(Error::Invalid { .. }));
    assert!(invalid(engine.set_account_hidden("work", true)));
    assert!(invalid(engine.set_account_hidden("gemini:work", true)));
    assert!(engine.set_account_hidden("claude:work", true).is_ok());
}
