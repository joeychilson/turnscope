//! An agent finds sessions by where they ran, by agent, period and words
//! said in them, a page at a time, each with what it took, in local time.

mod history {
    pub mod files;
    pub mod server;
    pub mod sessions;
}

use serde_json::{Value, json};

use history::server::{answer, refusal};
use history::sessions::{CLAUDE, CODEX, EMPTY, server};

fn ids(data: &Value) -> Vec<&str> {
    data["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| session["id"].as_str().unwrap())
        .collect()
}

#[test]
fn sessions_are_found_by_folder_agent_and_period() {
    let (server, home, _data) = server();
    let claude = format!("claude-code:{CLAUDE}");
    let codex = format!("codex:{CODEX}");

    // The session in which nothing happened isn't listed.
    let all = answer(&server, "find_sessions", json!({}));
    assert_eq!(ids(&all.data), [codex.as_str(), claude.as_str()]);
    let session = &all.data["sessions"][1];
    assert_eq!(session["agent"], "claude-code");
    assert_eq!(session["title"], "Add a file watcher for the ledger");
    assert_eq!(session["project"], "ledger");
    assert_eq!(session["started"], "2026-09-14T07:00:00-05:00");
    assert_eq!(session["last_active"], "2026-09-14T07:01:30-05:00");
    assert_eq!(session["running"], false);
    assert_eq!(session["subagents"], 1);
    // What it took, its subagent's with its own: 1,200 in, 10,000 read from
    // the cache and 2,400 out, and the subagent's 100 and 200: 13,900 tokens
    // and $0.071 + $0.0055 = $0.0765.
    assert_eq!(session["usage"]["tokens"]["total"], 13_900);
    assert_eq!(session["usage"]["cost_usd"], json!(0.0765));
    // The sentences name it with its id, which says its agent, where and
    // when it ran, and what it took. Its day is said as seen from now, which
    // moves. Codex's thread began and ended within a minute.
    for part in [
        format!("2. \"Add a file watcher for the ledger\" ({claude}) \u{b7} ~/work/ledger \u{b7} "),
        " 7:00 AM to 7:01 AM, ended \u{b7} 13.9K tokens, $0.08 at list prices".to_owned(),
        " 4:00 AM, ended \u{b7} 3K tokens, cost unknown".to_owned(),
    ] {
        assert!(all.said().contains(&part), "{}", all.said());
    }

    // A subfolder of the repository stands for the repository, and so does
    // one from ~/.
    let here = answer(
        &server,
        "find_sessions",
        json!({"folder": home.path().join("work/ledger/src")}),
    );
    assert_eq!(ids(&here.data), [claude.as_str()]);
    let from_home = answer(
        &server,
        "find_sessions",
        json!({"folder": "~/work/ledger/src"}),
    );
    assert_eq!(ids(&from_home.data), [claude.as_str()]);
    let codex_only = answer(&server, "find_sessions", json!({"agent": "codex"}));
    assert_eq!(ids(&codex_only.data), [codex.as_str()]);
    // Codex's model has no price, which is unknown rather than free.
    assert_eq!(
        codex_only.data["sessions"][0]["usage"]["cost_usd"],
        Value::Null
    );
    let on_the_14th = answer(
        &server,
        "find_sessions",
        json!({"since": "2026-09-14", "until": "2026-09-14"}),
    );
    assert_eq!(ids(&on_the_14th.data), [claude.as_str()]);
    // Nothing ran in the last five minutes of this history.
    let running = answer(&server, "find_sessions", json!({"running": true}));
    assert_eq!(ids(&running.data), Vec::<&str>::new());
    assert_eq!(running.said(), "No session matches.");
    // By what they used: Claude Code's 13,900 tokens before Codex's 3,040.
    let by_usage = answer(&server, "find_sessions", json!({"order": "usage"}));
    assert_eq!(ids(&by_usage.data), [claude.as_str(), codex.as_str()]);

    for (arguments, expected) in [
        (json!({"folder": "work/ledger"}), "absolute path"),
        (json!({"agents": ["codex"]}), "unknown field `agents`"),
        (json!({"agent": "cursor"}), "There is no agent \"cursor\""),
        (json!({"since": "soon"}), "today, yesterday"),
        (
            json!({"order": "cost"}),
            "unknown variant `cost`, expected `recent` or `usage`",
        ),
        (json!({"limit": 0}), "limit takes 1 to 50"),
    ] {
        let refused = refusal(&server, "find_sessions", arguments.clone());
        assert!(refused.contains(expected), "{arguments}: {refused}");
    }
    // The empty session is still there to be named; a session no history
    // holds isn't.
    let refused = refusal(
        &server,
        "get_session",
        json!({"session": "claude-code:nope"}),
    );
    assert!(refused.contains("find_sessions gives ids"), "{refused}");
    let empty = answer(
        &server,
        "get_session",
        json!({"session": format!("claude-code:{EMPTY}")}),
    );
    assert_eq!(empty.data["session"]["id"], format!("claude-code:{EMPTY}"));
}

#[test]
fn a_page_ends_where_the_next_begins() {
    let (server, _home, _data) = server();
    let first = answer(&server, "find_sessions", json!({"limit": 1}));
    assert_eq!(ids(&first.data), [format!("codex:{CODEX}")]);
    let cursor = first.data["next_cursor"].as_str().unwrap().to_owned();
    assert!(first.said().contains(&format!("pass cursor {cursor:?}")));
    let rest = answer(&server, "find_sessions", json!({"cursor": cursor}));
    assert_eq!(ids(&rest.data), [format!("claude-code:{CLAUDE}")]);
    assert_eq!(rest.data["next_cursor"], Value::Null);
}

#[test]
fn what_was_said_is_searched_and_its_passage_quoted() {
    let (server, _home, _data) = server();
    let found = answer(&server, "find_sessions", json!({"words": "migration"}));
    assert_eq!(
        ids(&found.data),
        [format!("codex:{CODEX}"), format!("claude-code:{CLAUDE}")]
    );
    let claude = &found.data["sessions"][1];
    // "Fix the failing migration" and the reply about it.
    assert_eq!(claude["mentions"], 2);
    assert_eq!(
        claude["passage"],
        json!({"entry": 3, "text": "Fix the failing migration"})
    );
    assert!(
        found
            .said()
            .contains("   Said: \"Fix the failing migration\" (entry 3)"),
        "{}",
        found.said()
    );
    // Narrowed as a list is, and a page at a time.
    let codex = answer(
        &server,
        "find_sessions",
        json!({"words": "migration", "agent": "codex"}),
    );
    assert_eq!(ids(&codex.data), [format!("codex:{CODEX}")]);
    let first = answer(
        &server,
        "find_sessions",
        json!({"words": "migration", "limit": 1}),
    );
    let rest = answer(
        &server,
        "find_sessions",
        json!({"words": "migration", "cursor": first.data["next_cursor"]}),
    );
    assert_eq!(ids(&rest.data), [format!("claude-code:{CLAUDE}")]);
    let refused = refusal(&server, "find_sessions", json!({"words": "--"}));
    assert!(refused.contains("no word"), "{refused}");
    let refused = refusal(
        &server,
        "find_sessions",
        json!({"words": "migration", "order": "usage"}),
    );
    assert!(refused.contains("latest mention"), "{refused}");
}
