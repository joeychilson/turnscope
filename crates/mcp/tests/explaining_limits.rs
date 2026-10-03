//! An agent explains what used a limit, and why: each session's share of
//! the window, with its model, responses, how large its context grew, its
//! cache reads and its subagents' share; or by project, model or agent; or
//! one session's share by prompt and subagent; and a window that has reset
//! since it was read, as the window that ended.

mod history {
    pub mod files;
    pub mod limits;
    pub mod server;
}

use serde_json::{Value, json};
use turnscope_engine::{AccountLimits, Agent, Engine, LimitState, Subscription};

use history::limits::{ALPHA, BETA, CLAUDE_ACCOUNT, claude_code, from_now, server};
use history::server::{answer, refusal};

/// Each part's key and share.
fn parts(data: &Value) -> Vec<(Value, Value)> {
    data["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| (part["key"].clone(), part["share_percent"].clone()))
        .collect()
}

#[test]
fn a_window_is_explained_by_the_sessions_that_took_it_and_why() {
    let (server, _, _dirs) = server(claude_code());
    // Your account's limit that runs out first: its 5 hours.
    let explained = answer(&server, "explain_limit", json!({}));
    let data = &explained.data;
    assert_eq!(data["account"]["id"], CLAUDE_ACCOUNT);
    assert_eq!(data["limit"]["limit"], "5 hours");
    assert_eq!(
        (
            data["limit"]["used_percent"].clone(),
            data["limit"]["left_percent"].clone()
        ),
        (json!(80.0), json!(20.0))
    );
    // alpha alone spent while it rose to 50%, and beta while it rose to 80%.
    assert_eq!(
        parts(data),
        [
            (json!(format!("claude-code:{ALPHA}")), json!(50.0)),
            (json!(format!("claude-code:{BETA}")), json!(30.0))
        ]
    );
    let beta = &data["parts"][1];
    assert_eq!(beta["largest_context_tokens"], 20_000);
    // 28,000 of 30,200 tokens: 0.927.
    assert_eq!(beta["cache_read_share"], json!(0.927));
    assert_eq!(beta["responses"], 2);
    assert_eq!(beta["subagents"], 1);
    // 30 × 7,500 / 36,500.
    assert_eq!(beta["subagents_share_percent"], json!(6.16));
    // From its prompt 31 minutes ago to its last reply 20 minutes ago.
    assert_eq!(beta["duration_minutes"], 11);
    assert_eq!(data["elsewhere_percent"], json!(0.0));
    assert_eq!(data["unpriced_percent"], json!(0.0));
    assert_eq!(data["approximate"], true);
    let said = explained.said();
    // Each session with its id, which says its agent, its project and the
    // day it ran, which is said as seen from now and so not checked here.
    for sentence in [
        "Claude Max \u{b7} joey@example.com, 5-hour limit since ".to_owned(),
        format!(
            ": 80% used, 20% left.\n\n1. \"Map the tiles\" (claude-code:{ALPHA}) \u{b7} alpha \
             \u{b7} "
        ),
        ": 50.0%.\n   Claude Opus 5 for 1m, 1 response. Its context grew to 1K tokens.\n"
            .to_owned(),
        format!("2. \"Shade the hills\" (claude-code:{BETA}) \u{b7} beta \u{b7} "),
        ": 30.0%.\n   Claude Opus 5 for 11m, 2 responses. Its context grew to 20K tokens and \
         was read again on every response: 93% of its tokens were cache reads. 1 subagent, on \
         Claude Opus 5, took 6.2% of it."
            .to_owned(),
    ] {
        assert!(said.contains(&sentence), "{sentence:?} in {said}");
    }
    assert!(!said.contains("Not on this Mac"), "{said}");
}

#[test]
fn a_window_is_split_by_project_model_or_agent() {
    let (server, _, _dirs) = server(claude_code());
    let projects = answer(&server, "explain_limit", json!({"by": "projects"}));
    assert_eq!(
        parts(&projects.data),
        [
            (json!("/work/alpha"), json!(50.0)),
            (json!("/work/beta"), json!(30.0))
        ]
    );
    // Each named, and beside its name what another tool's argument takes:
    // a project's folder, a model's key, an agent's.
    assert!(
        projects
            .said()
            .contains("1. alpha (/work/alpha): 50.0%.\n2. beta (/work/beta): 30.0%."),
        "{}",
        projects.said()
    );
    let models = answer(&server, "explain_limit", json!({"by": "models"}));
    assert_eq!(parts(&models.data), [(json!("claude-opus-5"), json!(80.0))]);
    assert!(
        models
            .said()
            .contains("1. Claude Opus 5 (claude-opus-5): 80.0%."),
        "{}",
        models.said()
    );
    let agents = answer(
        &server,
        "explain_limit",
        json!({"by": "agents", "limit": "week"}),
    );
    // The week rose 30 points to the first reading, all alpha's, and 1 more
    // to the second, beta's.
    assert_eq!(parts(&agents.data), [(json!("claude-code"), json!(31.0))]);
    assert!(
        agents
            .said()
            .contains("1. Claude Code (claude-code): 31.0%."),
        "{}",
        agents.said()
    );
    assert_eq!(agents.data["limit"]["limit"], "week");
}

#[test]
fn a_session_is_explained_by_prompt_and_subagent() {
    let (server, _, _dirs) = server(claude_code());
    let beta = answer(
        &server,
        "explain_limit",
        json!({"session": format!("claude-code:{BETA}")}),
    );
    let session = &beta.data["session"];
    assert_eq!(session["share_percent"], json!(30.0));
    // Its one prompt holds all of it, its subagent's too, which was started
    // in it.
    assert_eq!(session["prompts"][0]["said"], "Shade the hills");
    assert_eq!(session["prompts"][0]["share_percent"], json!(30.0));
    assert_eq!(session["prompts"][0]["subagents"], 1);
    assert_eq!(session["subagents"][0]["title"], "Find the normals");
    assert_eq!(session["subagents"][0]["share_percent"], json!(6.16));
    let said = beta.said();
    assert!(
        said.contains(&format!(
            "\"Shade the hills\" (claude-code:{BETA}) took 30.0%."
        )),
        "{said}"
    );
    assert!(
        said.contains(
            "Its subagents took 6.2% of it:\n- \"Find the normals\" \
             (claude-code:c3c3c3c3c3c3c3c3c) \u{b7} claude-opus-5: 6.2%."
        ),
        "{said}"
    );
    let refused = refusal(
        &server,
        "explain_limit",
        json!({"session": format!("claude-code:{BETA}"), "by": "models"}),
    );
    assert!(refused.contains("by doesn't come with it"), "{refused}");
    let refused = refusal(&server, "explain_limit", json!({"limit": "month"}));
    assert!(
        refused.starts_with("No account has a limit \"month\""),
        "{refused}"
    );
}

#[test]
fn a_window_that_reset_since_it_was_read_is_said_to_have_ended() {
    let (server, now, dirs) = server(claude_code());
    // A SuperGrok account's 5 hours, from 6 hours ago to an hour ago, read
    // 90 minutes ago at 70% used: its window has reset since, and nothing
    // has read the new one.
    let ended = LimitState {
        key: "five_hour".to_owned(),
        name: "5 hours".to_owned(),
        scope: None,
        used: Some(70.0),
        starts: Some(from_now(now, -360)),
        resets: Some(from_now(now, -60)),
        read_at: from_now(now, -90),
        pace: None,
        refilled: false,
    };
    let account = AccountLimits {
        id: "supergrok:g-1".to_owned(),
        subscription: Subscription::SuperGrok,
        label: None,
        plan: None,
        agents: vec![Agent::Grok],
        signed_in: true,
        limits: vec![ended],
        read_at: None,
        checked_at: None,
        problem: None,
        in_use: false,
        hidden: false,
        provider: None,
        folders: Vec::new(),
    };
    Engine::open(dirs[1].path(), dirs[0].path())
        .unwrap()
        .record_fixture_limits(Subscription::SuperGrok, &[account], from_now(now, -90))
        .unwrap();
    let explained = answer(
        &server,
        "explain_limit",
        json!({"account": "supergrok:g-1"}),
    );
    assert_eq!(explained.data["limit"]["ended"], true);
    assert_eq!(explained.data["limit"]["used_percent"], json!(70.0));
    let said = explained.said();
    assert!(
        said.starts_with("SuperGrok, 5-hour limit: the window from ")
            && said.contains(", 70% used by then. Nothing has read the new one yet"),
        "{said}"
    );
    assert!(!said.contains("left"), "{said}");
}
