//! An agent is given a tool's sentences alone, so they are the whole
//! answer: they name every session the figures behind them do, by its id,
//! so that one can be asked about again.

mod history {
    pub mod files;
    pub mod limits;
    pub mod server;
    pub mod sessions;
}

use serde_json::{Value, json};

use history::limits::{BETA, claude_code};
use history::server::{answer, refusal};
use history::sessions::{CLAUDE, EXPLORE};

/// The sessions `figures` name by their ids, as an `id`, a `key` or a
/// `group`: those of them that are a session's, its agent's key and a colon
/// before its own id.
fn sessions_named(figures: &Value) -> Vec<String> {
    let mut named = Vec::new();
    match figures {
        Value::Object(fields) => {
            for (name, field) in fields {
                if let ("id" | "key" | "group", Value::String(id)) = (name.as_str(), field)
                    && turnscope_engine::Agent::ALL
                        .iter()
                        .any(|agent| id.starts_with(&format!("{}:", agent.key())))
                {
                    named.push(id.clone());
                }
                named.extend(sessions_named(field));
            }
        }
        Value::Array(items) => named.extend(items.iter().flat_map(sessions_named)),
        _ => {}
    }
    named
}

#[test]
fn every_answer_names_each_session_its_figures_do() {
    // Sessions from several agents, and limits with the sessions that used
    // them.
    let (sessions, _home, _data) = history::sessions::server();
    let (limits, _, _dirs) = history::limits::server(claude_code());
    let calls = [
        (&limits, "check_limits", json!({})),
        (&limits, "check_limits", json!({"all": true, "below": 50})),
        (
            &limits,
            "check_limits",
            json!({"account": "other@example.com", "below": 50}),
        ),
        (&limits, "explain_limit", json!({})),
        (&limits, "explain_limit", json!({"by": "projects"})),
        (&limits, "explain_limit", json!({"by": "models"})),
        (
            &limits,
            "explain_limit",
            json!({"by": "agents", "limit": "week"}),
        ),
        (
            &limits,
            "explain_limit",
            json!({"session": format!("claude-code:{BETA}")}),
        ),
        (&sessions, "find_sessions", json!({})),
        (&limits, "find_sessions", json!({})),
        (&sessions, "find_sessions", json!({"words": "migration"})),
        (
            &sessions,
            "get_session",
            json!({"session": format!("claude-code:{CLAUDE}")}),
        ),
        (
            &sessions,
            "get_session",
            json!({"session": format!("claude-code:{EXPLORE}")}),
        ),
        (&limits, "get_session", json!({})),
        (&sessions, "get_usage", json!({})),
        (&sessions, "get_usage", json!({"by": "day"})),
        (&limits, "get_usage", json!({"by": "account"})),
        (&sessions, "get_usage", json!({"by": "session"})),
    ];
    for (server, tool, arguments) in calls {
        let answered = answer(server, tool, arguments.clone());
        assert!(!answered.said().is_empty(), "{tool} {arguments}");
        for session in sessions_named(&answered.data) {
            assert!(
                answered.text.contains(&session),
                "{tool} {arguments}: {session} isn't in {}",
                answered.text
            );
        }
    }
    // A refusal is sentences alone.
    let refused = refusal(&sessions, "get_usage", json!({"by": "colour"}));
    assert!(!refused.contains('{'), "{refused}");
}
