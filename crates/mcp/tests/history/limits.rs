//! Limits read over the last hour, and the Claude Code sessions that used
//! them, all placed from the moment a test starts, as limits are only ever
//! read of now.
//!
//! **The Claude account**, `claude:acc-1:org-9`, is read twice: 40 and 10
//! minutes ago. Its 5-hour window, from two hours ago to three hours ahead,
//! went from 50% to 80% used: 30 points in half an hour, 60 an hour, so the
//! 20 left last 20 minutes, to 10 minutes from now. Its week, from six days
//! ago to a day ahead, went from 30% to 31% over the half hour, too little
//! of a day to pace a week by, so it is paced by its average: 31 points in
//! the 143 hours and 50 minutes since it began, 0.2155 an hour. From 10
//! minutes ago to the reset, 24 hours and 10 minutes, it rises 5.2 more, to
//! 36.2%, and 63.8% is left then. Its Opus week stayed at 55%, paced by
//! its average too, 0.3824 an hour: 9.2 more by the reset, 35.8% left. **A ChatGPT account**, signed into Codex in `~/.codex`, was read once,
//! 10 minutes ago, at 10% of its 5 hours, so it isn't rising and not in
//! use. **A second Claude account**, `claude:acc-2:org-9`, signed in to
//! Claude Code in a folder of its own, `~/.claude-other`, was read an hour
//! ago, at 30% of its 5 hours: stale, so whether it is under 50% left isn't
//! known. The sessions, kept in `~/.claude`, are the first account's.
//!
//! **The sessions.** `alpha`, in /work/alpha, replied once 60 minutes ago,
//! in the 5-hour window's first stretch, before the first reading: the only
//! spending then, so it took all of the rise to 50%, 50 points. `beta`, in
//! /work/beta, replied 30 and 20 minutes ago, and its subagent 25 minutes
//! ago, in the stretch to 80%: the only spending then, so they took its 30
//! points by cost. At Claude Opus 5's $5 input, $25 output and $0.50 cache
//! reads a million tokens, beta's replies cost 1,000 × 5 + 9,000 × 0.5 +
//! 100 × 25 = 12,000 and 1,000 × 5 + 19,000 × 0.5 + 100 × 25 = 17,000
//! millionths of a dollar, and its subagent's 1,000 × 5 + 100 × 25 = 7,500:
//! 36,500 in all, of which the subagent's is 30 × 7,500 / 36,500 = 6.16
//! points. Beta's largest context is 1,000 + 19,000 = 20,000 tokens, and of
//! its own 10,100 + 20,100 = 30,200 tokens, 28,000 were cache reads: 92.7%.

use std::path::Path;

use serde_json::{Value, json};
use tempfile::TempDir;
use turnscope_engine::{AccountLimits, Agent, Engine, Instant, LimitState, Subscription};
use turnscope_mcp::{Caller, Server};

use super::files::write;

pub const ALPHA: &str = "a1a1a1a1-0000-4000-8000-00000000a1fa";
pub const BETA: &str = "b2b2b2b2-0000-4000-8000-00000000be7a";
const HELPER: &str = "c3c3c3c3c3c3c3c3c";
pub const CLAUDE_ACCOUNT: &str = "claude:acc-1:org-9";
const STALE_ACCOUNT: &str = "claude:acc-2:org-9";
pub const CHATGPT_ACCOUNT: &str = "chatgpt:ws-1";

/// A server over this history, read as of now, telling times in UTC,
/// answering `caller`; the moment it was read as of; and the home and data
/// directories it reads, which it needs while it answers.
pub fn server(caller: Caller) -> (Server, Instant, [TempDir; 2]) {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let now = Instant::now();
    sessions(home.path(), now);
    let engine = Engine::open(data.path(), home.path()).unwrap();
    engine.scan().unwrap();
    record(&engine, now);
    let server = Server::as_it_stands(engine, "UTC")
        .unwrap()
        .with_caller(caller);
    (server, now, [home, data])
}

/// Claude Code, working in /work/beta.
pub fn claude_code() -> Caller {
    Caller {
        agent: Some(Agent::ClaudeCode),
        folder: Some("/work/beta".to_owned()),
        ..Caller::default()
    }
}

/// A moment `minutes` from `now`, back when negative.
pub fn from_now(now: Instant, minutes: i64) -> Instant {
    Instant::from_millis(now.millis() + minutes * 60_000).unwrap()
}

/// The time `at`, as agents write one.
pub fn stamp(at: Instant) -> String {
    jiff::Timestamp::from_millisecond(at.millis())
        .unwrap()
        .to_string()
}

/// A limit `name`d, with `key`, `used` percent, from `starts` to `resets`.
fn limit(
    key: &str,
    name: &str,
    scope: Option<&str>,
    used: f64,
    window: (Instant, Instant),
) -> LimitState {
    LimitState {
        key: key.to_owned(),
        name: name.to_owned(),
        scope: scope.map(str::to_owned),
        used: Some(used),
        starts: Some(window.0),
        resets: Some(window.1),
        read_at: window.0,
        pace: None,
        refilled: false,
    }
}

/// An account of `subscription` with `limits`, signed into `agents`.
fn account(
    id: &str,
    subscription: Subscription,
    plan: &str,
    agents: &[Agent],
    limits: Vec<LimitState>,
) -> AccountLimits {
    AccountLimits {
        id: id.to_owned(),
        subscription,
        label: Some("joey@example.com".to_owned()),
        plan: Some(plan.to_owned()),
        agents: agents.to_vec(),
        signed_in: true,
        limits,
        read_at: None,
        checked_at: None,
        problem: None,
        in_use: false,
        hidden: false,
        provider: None,
        folders: Vec::new(),
    }
}

/// Record the accounts' readings in `engine`, as of `now`.
fn record(engine: &Engine, now: Instant) {
    let five = (from_now(now, -120), from_now(now, 180));
    let week = (from_now(now, -6 * 24 * 60), from_now(now, 24 * 60));
    let claude = |five_used: f64, week_used: f64| {
        account(
            CLAUDE_ACCOUNT,
            Subscription::Claude,
            "max",
            &[Agent::ClaudeCode],
            vec![
                limit("five_hour", "5 hours", None, five_used, five),
                limit("seven_day", "Weekly", None, week_used, week),
                limit("seven_day_opus", "Weekly", Some("Opus"), 55.0, week),
            ],
        )
    };
    let mut stale = account(
        STALE_ACCOUNT,
        Subscription::Claude,
        "pro",
        &[Agent::ClaudeCode],
        vec![limit("five_hour", "5 hours", None, 30.0, five)],
    );
    stale.label = Some("other@example.com".to_owned());
    stale.folders = vec![(Agent::ClaudeCode, engine.home().join(".claude-other"))];
    engine
        .record_fixture_limits(Subscription::Claude, &[stale], from_now(now, -60))
        .unwrap();
    engine
        .record_fixture_limits(
            Subscription::Claude,
            &[claude(50.0, 30.0)],
            from_now(now, -40),
        )
        .unwrap();
    engine
        .record_fixture_limits(
            Subscription::Claude,
            &[claude(80.0, 31.0)],
            from_now(now, -10),
        )
        .unwrap();
    let mut chatgpt = account(
        CHATGPT_ACCOUNT,
        Subscription::ChatGpt,
        "pro",
        &[Agent::Codex],
        vec![limit("primary_window", "5 hours", None, 10.0, five)],
    );
    chatgpt.folders = vec![(Agent::Codex, engine.home().join(".codex"))];
    engine
        .record_fixture_limits(Subscription::ChatGpt, &[chatgpt], from_now(now, -10))
        .unwrap();
}

/// Write the sessions `alpha` and `beta` under `home`, as of `now`.
fn sessions(home: &Path, now: Instant) {
    let reply = |session: &str, agent: Option<&str>, minutes: i64, id: &str, usage: Value| {
        let mut line = json!({"type": "assistant", "sessionId": session,
            "timestamp": stamp(from_now(now, minutes)), "cwd": format!("/work/{}", if session == ALPHA { "alpha" } else { "beta" }),
            "requestId": format!("req_{id}"),
            "message": {"id": format!("msg_{id}"), "model": "claude-opus-5", "role": "assistant",
                        "usage": usage, "content": [{"type": "text", "text": "Done."}]}});
        if let Some(agent) = agent {
            line["agentId"] = json!(agent);
            line["isSidechain"] = json!(true);
        }
        line
    };
    let prompt = |session: &str, minutes: i64, text: &str| {
        json!({"type": "user", "sessionId": session, "timestamp": stamp(from_now(now, minutes)),
               "cwd": format!("/work/{}", if session == ALPHA { "alpha" } else { "beta" }),
               "message": {"role": "user", "content": text}})
    };
    write(
        &home.join(format!(".claude/projects/-work-alpha/{ALPHA}.jsonl")),
        &[
            prompt(ALPHA, -61, "Map the tiles"),
            reply(
                ALPHA,
                None,
                -60,
                "a1",
                json!({"input_tokens": 1_000, "output_tokens": 100}),
            ),
        ],
    );
    write(
        &home.join(format!(".claude/projects/-work-beta/{BETA}.jsonl")),
        &[
            prompt(BETA, -31, "Shade the hills"),
            reply(
                BETA,
                None,
                -30,
                "b1",
                json!({"input_tokens": 1_000, "cache_read_input_tokens": 9_000, "output_tokens": 100}),
            ),
            reply(
                BETA,
                None,
                -20,
                "b2",
                json!({"input_tokens": 1_000, "cache_read_input_tokens": 19_000, "output_tokens": 100}),
            ),
        ],
    );
    write(
        &home.join(format!(
            ".claude/projects/-work-beta/{BETA}/subagents/agent-{HELPER}.jsonl"
        )),
        &[
            json!({"type": "user", "sessionId": BETA, "agentId": HELPER, "isSidechain": true,
                   "timestamp": stamp(from_now(now, -26)), "cwd": "/work/beta",
                   "message": {"role": "user", "content": "Find the normals"}}),
            reply(
                BETA,
                Some(HELPER),
                -25,
                "h1",
                json!({"input_tokens": 1_000, "output_tokens": 100}),
            ),
        ],
    );
    write(
        &home.join(format!(
            ".claude/projects/-work-beta/{BETA}/subagents/agent-{HELPER}.meta.json"
        )),
        &[json!({"agentType": "Explore", "description": "Find the normals"})],
    );
}
