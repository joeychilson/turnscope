//! An agent totals usage over a period, narrowed and split as it asks: by
//! local day, week or month, by project, model, agent, account or session.
//! A cost nobody knows the price of stays unknown.

mod history {
    pub mod files;
    pub mod server;
    pub mod sessions;
}

use serde_json::{Value, json};
use turnscope_engine::{AccountLimits, Agent, Engine, Instant, Subscription};
use turnscope_mcp::Server;

use history::server::{answer, refusal};
use history::sessions::{CLAUDE, CODEX, EXPLORE, server};

/// A server over the history, with a Claude account signed into Claude
/// Code, as limits record one.
fn with_an_account() -> (Server, tempfile::TempDir, tempfile::TempDir) {
    let (server, home, data) = server();
    let account = AccountLimits {
        id: "claude:acc-1:org-9".to_owned(),
        subscription: Subscription::Claude,
        label: Some("joey@example.com".to_owned()),
        plan: Some("max".to_owned()),
        agents: vec![Agent::ClaudeCode],
        signed_in: true,
        limits: Vec::new(),
        read_at: None,
        checked_at: None,
        problem: None,
        in_use: false,
        hidden: false,
        provider: None,
        folders: Vec::new(),
    };
    Engine::open(data.path(), home.path())
        .unwrap()
        .record_fixture_limits(Subscription::Claude, &[account], Instant::now())
        .unwrap();
    (server, home, data)
}

fn groups(data: &Value, key: &str) -> Vec<Value> {
    data["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row[key].clone())
        .collect()
}

#[test]
fn usage_is_totalled_by_local_day_and_split_as_asked() {
    let (server, _home, _data) = server();
    let days = answer(&server, "get_usage", json!({"by": "day"}));
    assert_eq!(days.data["total"]["responses"], 4);
    // Claude Code's $0.071 and its subagent's $0.0055; Codex's model has no
    // price, so the cost leaves it out, and says so.
    assert_eq!(days.data["total"]["cost_usd"], json!(0.0765));
    assert_eq!(days.data["total"]["unpriced_usage"], true);
    assert_eq!(
        groups(&days.data, "start"),
        [
            json!("2026-09-14T00:00:00-05:00"),
            json!("2026-09-15T00:00:00-05:00")
        ]
    );
    // 13,900 and 3,040 tokens, 16,940: 1,200 + 100 + 600 = 1,900 input,
    // 2,400 + 200 + 40 = 2,640 output, and 10,000 + 2,400 = 12,400 cache
    // reads.
    assert!(
        days.said().starts_with(
            "All of history: 16.9K tokens (1.9K input, 2.6K output, 12.4K cache reads), 4 \
             responses, $0.08 at list prices, leaving out usage with no known price."
        ),
        "{}",
        days.said()
    );

    let codex = answer(
        &server,
        "get_usage",
        json!({"since": "2026-09-15", "until": "2026-09-15"}),
    );
    assert_eq!(codex.data["since"], "2026-09-15T00:00:00-05:00");
    assert_eq!(codex.data["until"], "2026-09-16T00:00:00-05:00");
    // Codex's input counts its cached input: 3,000 with 2,400 cached.
    assert_eq!(
        codex.data["total"]["tokens"],
        json!({"input": 600, "cache_read": 2_400, "cache_write": 0, "output": 40,
               "reasoning": 0, "total": 3_040})
    );
    assert!(codex.data.get("rows").is_none());
    assert!(codex.said().ends_with("cost unknown."), "{}", codex.said());
    // A month as since starts it and as until ends it: September holds all
    // of the history, 13,900 + 3,040 = 16,940 tokens.
    let september = answer(
        &server,
        "get_usage",
        json!({"since": "2026-09", "until": "2026-09"}),
    );
    assert_eq!(september.data["since"], "2026-09-01T00:00:00-05:00");
    assert_eq!(september.data["until"], "2026-10-01T00:00:00-05:00");
    assert_eq!(september.data["total"]["tokens"]["total"], 16_940);
    // As agents say it, a week back from now.
    answer(&server, "get_usage", json!({"since": "past week"}));

    // Each agent by its name, and its key, as the agent argument takes it.
    let agents = answer(&server, "get_usage", json!({"by": "agent"}));
    assert_eq!(
        groups(&agents.data, "group"),
        [json!("claude-code"), json!("codex")]
    );
    assert_eq!(
        groups(&agents.data, "label"),
        [json!("Claude Code"), json!("Codex")]
    );
    for line in ["\n- Claude Code (claude-code): ", "\n- Codex (codex): "] {
        assert!(
            agents.said().contains(line),
            "{line:?} in {}",
            agents.said()
        );
    }
    let sessions = answer(
        &server,
        "get_usage",
        json!({"by": "session", "agent": "claude-code"}),
    );
    // A subagent's usage is its own session's row.
    let mut ids = groups(&sessions.data, "group");
    ids.sort_by_key(ToString::to_string);
    assert_eq!(
        ids,
        [
            json!(format!("claude-code:{CLAUDE}")),
            json!(format!("claude-code:{EXPLORE}"))
        ]
    );
    // Each model by its key, as the model argument takes it, and the name
    // the catalog gives it; one no catalog knows by its key alone.
    let models = answer(&server, "get_usage", json!({"by": "model"}));
    assert_eq!(
        groups(&models.data, "group"),
        [json!("claude-opus-5"), json!("gpt-unlisted-test")]
    );
    assert_eq!(
        groups(&models.data, "label"),
        [json!("Claude Opus 5"), Value::Null]
    );
    // Said by its name, with the key the model argument takes beside it.
    assert!(
        models
            .said()
            .contains("\n- Claude Opus 5 (claude-opus-5): 13.9K tokens"),
        "{}",
        models.said()
    );
    let opus = answer(&server, "get_usage", json!({"model": "claude-opus-5"}));
    assert_eq!(opus.data["total"]["tokens"]["total"], 13_900);
    // A model with usage in history, but none on Codex's day, has none then.
    let none = answer(
        &server,
        "get_usage",
        json!({"model": "claude-opus-5", "since": "2026-09-15", "until": "2026-09-15"}),
    );
    assert_eq!(none.data["total"]["tokens"]["total"], 0);
    let project = answer(&server, "get_usage", json!({"folder": "~/work/ledger"}));
    assert_eq!(project.data["total"]["tokens"]["total"], 13_900);

    for (arguments, expected) in [
        (
            json!({"by": "colour"}),
            "unknown variant `colour`, expected one of `day`, `week`",
        ),
        (
            json!({"model": "claude-opus-6"}),
            "No usage in history is of the model \"claude-opus-6\"",
        ),
        (json!({"agent": "cursor"}), "There is no agent"),
        (json!({"account": "gemini"}), "No account is \"gemini\""),
        (
            json!({"since": "today", "until": "yesterday"}),
            "since must come before until",
        ),
    ] {
        let refused = refusal(&server, "get_usage", arguments.clone());
        assert!(refused.contains(expected), "{arguments}: {refused}");
    }
}

#[test]
fn usage_is_told_apart_by_the_account_it_draws_on() {
    let (server, _home, _data) = with_an_account();
    let accounts = answer(&server, "get_usage", json!({"by": "account"}));
    // Claude Code's usage, made in ~/.claude where the Claude account is
    // signed in, drew on it; Codex's, made in ~/.codex, where no look for
    // a sign-in was made, drew on no account known, a row of its own after.
    assert_eq!(
        groups(&accounts.data, "group"),
        [json!("claude:acc-1:org-9"), Value::Null]
    );
    assert_eq!(
        groups(&accounts.data, "label"),
        [
            json!("Claude Max \u{b7} joey@example.com"),
            json!("No account known")
        ]
    );
    let usage: Vec<Value> = groups(&accounts.data, "usage")
        .iter()
        .map(|usage| usage["tokens"]["total"].clone())
        .collect();
    assert_eq!(usage, [json!(13_900), json!(3_040)]);
    let claude = answer(&server, "get_usage", json!({"account": "claude"}));
    assert_eq!(claude.data["total"]["tokens"]["total"], 13_900);
    // None of Codex's usage drew on it.
    let codex_on_claude = answer(
        &server,
        "get_usage",
        json!({"account": "claude", "agent": "codex"}),
    );
    assert_eq!(codex_on_claude.data["total"]["tokens"]["total"], 0);
    // The Codex thread is no account's.
    assert_eq!(
        answer(&server, "find_sessions", json!({"account": "claude"})).data["sessions"][0]["id"],
        format!("claude-code:{CLAUDE}")
    );
    let codex = answer(&server, "find_sessions", json!({"agent": "codex"}));
    assert_eq!(codex.data["sessions"][0]["id"], format!("codex:{CODEX}"));
    assert_eq!(codex.data["sessions"][0]["account"], Value::Null);
}
