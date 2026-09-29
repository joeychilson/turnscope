//! An agent checks what is left of its account's limits before costly work:
//! each limit's share left, when it resets, and where the recent pace
//! leads; whether one is under a percent left; and the other accounts.

mod history {
    pub mod files;
    pub mod limits;
    pub mod server;
}

use serde_json::{Value, json};
use turnscope_engine::{Agent, Instant};
use turnscope_mcp::Caller;

use history::limits::{BETA, CHATGPT_ACCOUNT, CLAUDE_ACCOUNT, claude_code, from_now, server};
use history::server::{answer, refusal};

/// `at` as the figures give it, in UTC.
fn utc(at: Instant) -> String {
    jiff::Timestamp::from_millisecond(at.millis())
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::UTC)
        .strftime("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

/// The limit `name` of the account at `place` in `data`.
fn limit<'a>(data: &'a Value, place: usize, name: &str) -> &'a Value {
    data["accounts"][place]["limits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|limit| limit["limit"] == name)
        .unwrap()
}

#[test]
fn your_account_comes_first_with_where_each_limit_is_heading() {
    let (server, now, _dirs) = server(claude_code());
    let limits = answer(&server, "check_limits", json!({}));
    let data = &limits.data;
    assert_eq!(
        data["you"],
        json!({"agent": "claude-code", "folder": "/work/beta", "account": CLAUDE_ACCOUNT,
               "session": format!("claude-code:{BETA}")})
    );
    let yours = &data["accounts"][0];
    assert_eq!(
        (
            yours["id"].clone(),
            yours["yours"].clone(),
            yours["name"].clone()
        ),
        (
            json!(CLAUDE_ACCOUNT),
            json!(true),
            json!("Claude Max \u{b7} joey@example.com")
        )
    );
    assert_eq!(yours["tightest"], "5 hours");
    // Its 5 hours run out before they reset, so the account is running out.
    assert_eq!(yours["standing"], "running_out");
    // 80% used, rising 60 points an hour since 10 minutes ago: the 20 left
    // run out 20 minutes after that, 10 minutes from now.
    let five = limit(data, 0, "5 hours");
    assert_eq!(five["left_percent"], json!(20.0));
    assert_eq!(five["rising_per_hour"], json!(60.0));
    assert_eq!(five["runs_out_at"], json!(utc(from_now(now, 10))));
    assert_eq!(five["resets_at"], json!(utc(from_now(now, 180))));
    assert_eq!(five["left_at_reset"], Value::Null);
    // Its 20 left over the 3h 10m from its reading to its reset last at
    // 6.32 points an hour.
    assert_eq!(five["lasting_per_hour"], json!(6.32));
    assert_eq!(five["stale"], false);
    assert_eq!(five["standing"], "running_out");
    // 31% used, its week's average 0.2155 points an hour: in the 24h 10m
    // to the reset, 5.2 more, so 63.8% left then. Read 143h 50m into its
    // 168, 85.6% of it gone, it is 54.6 points in reserve.
    let week = limit(data, 0, "week");
    assert_eq!(
        (
            week["left_percent"].clone(),
            week["left_at_reset"].clone(),
            week["runs_out_at"].clone(),
            week["standing"].clone()
        ),
        (json!(69.0), json!(63.8), Value::Null, json!("lasts"))
    );
    assert_eq!(week["reserve"], json!(54.6));
    // Its 69 left over the 24h 10m to its reset: 2.86 an hour.
    assert_eq!(week["lasting_per_hour"], json!(2.86));
    let opus = limit(data, 0, "opus week");
    assert_eq!(
        (
            opus["model"].clone(),
            opus["left_percent"].clone(),
            opus["left_at_reset"].clone()
        ),
        (json!("Opus"), json!(45.0), json!(35.8))
    );
    let said = limits.said();
    for sentence in [
        "You're using Claude Max \u{b7} joey@example.com, in Claude Code.",
        "5-hour limit: 20% left. At this pace it runs out around ",
        ", 2h 50m before it resets at ",
        "Weekly limit: 69% left, resets ",
        // 63.8% is said rounded down, as what is left always is.
        ". At this pace it lasts, with about 63% to spare.",
        "Opus weekly limit: 45% left, resets ",
        ". At this pace it lasts, with about 35% to spare.",
    ] {
        assert!(said.contains(sentence), "{sentence:?} in {said}");
    }
    // The ChatGPT account isn't in use, so it waits to be asked for.
    assert!(!said.contains("ChatGPT"), "{said}");
    let all = answer(&server, "check_limits", json!({"all": true}));
    let ids: Vec<&Value> = all.data["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|account| &account["id"])
        .collect();
    assert_eq!(ids[0], CLAUDE_ACCOUNT);
    assert!(ids.contains(&&json!(CHATGPT_ACCOUNT)), "{ids:?}");
    assert!(
        all.said()
            .contains("ChatGPT Pro \u{b7} joey@example.com (Codex): 5-hour limit 90% left."),
        "{}",
        all.said()
    );
    assert!(
        all.said().contains(
            "Claude Max \u{b7} joey@example.com (yours): 5-hour limit 20% left, runs out around "
        ),
        "{}",
        all.said()
    );
}

#[test]
fn a_limit_is_asked_of_by_name_and_whether_it_is_under_a_percent_left() {
    let (server, _, _dirs) = server(claude_code());
    let five = answer(
        &server,
        "check_limits",
        json!({"limit": "5 hours", "below": 50}),
    );
    assert_eq!(
        five.data["below"],
        json!({"percent": 50.0, "status": "under"})
    );
    assert_eq!(limit(&five.data, 0, "5 hours")["under"], true);
    assert!(
        five.said()
            .contains("5-hour limit: 20% left, under 50%. At this pace"),
        "{}",
        five.said()
    );
    assert!(five.said().ends_with("Under 50% left."), "{}", five.said());
    // 69% left is not under 50, and at 2 points an hour, 50% used is 9.5
    // hours off.
    let week = answer(
        &server,
        "check_limits",
        json!({"limit": "weekly", "below": 50}),
    );
    assert_eq!(week.data["below"]["status"], "not_under");
    // Asked of one account, its limits alone; one it doesn't have is
    // refused, naming those there are.
    let chatgpt = answer(&server, "check_limits", json!({"account": "chatgpt"}));
    assert_eq!(chatgpt.data["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(chatgpt.data["accounts"][0]["yours"], false);
    let refused = refusal(&server, "check_limits", json!({"limit": "fortnight"}));
    assert!(
        refused.starts_with("No account has a limit \"fortnight\"; the limits are 5 hours, "),
        "{refused}"
    );
    let refused = refusal(&server, "check_limits", json!({"account": "gemini"}));
    assert!(refused.starts_with("No account is \"gemini\""), "{refused}");
    let refused = refusal(&server, "check_limits", json!({"below": 0}));
    assert!(refused.contains("more than 0"), "{refused}");
}

#[test]
fn only_your_account_says_whether_you_are_under_a_percent_left() {
    // Codex, whose account is the ChatGPT one: 90% of its 5 hours left.
    let codex = Caller {
        agent: Some(Agent::Codex),
        folder: Some("/work/beta".to_owned()),
        ..Caller::default()
    };
    let (server, _, _dirs) = server(codex);
    let limits = answer(&server, "check_limits", json!({"all": true, "below": 50}));
    assert_eq!(limits.data["you"]["account"], CHATGPT_ACCOUNT);
    // The Claude account's 5 hours, 20% left, is under 50 and the other
    // Claude account's stale reading is unknown, but neither is yours.
    let claude = limits.data["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|account| account["id"] == CLAUDE_ACCOUNT)
        .unwrap();
    assert_eq!(claude["limits"][0]["under"], true);
    assert_eq!(limits.data["below"]["status"], "not_under");
    assert!(
        limits.said().ends_with("Not under 50% left."),
        "{}",
        limits.said()
    );
}

#[test]
fn a_stale_reading_is_unknown_not_room_to_go_on() {
    let (server, _, _dirs) = server(claude_code());
    // Read an hour ago at 30% of its 5 hours: not under 50 then, and
    // nothing says how it stands now.
    let other = answer(
        &server,
        "check_limits",
        json!({"account": "other@example.com", "below": 50}),
    );
    let five = limit(&other.data, 0, "5 hours");
    assert_eq!(
        (five["stale"].clone(), five["under"].clone()),
        (json!(true), Value::Null)
    );
    assert!(five["why_stale"].as_str().unwrap().len() > 10, "{five}");
    assert_eq!(other.data["below"]["status"], "unknown");
    assert!(
        other.said().contains("Whether it's under 50% isn't known"),
        "{}",
        other.said()
    );
}

#[test]
fn which_account_is_yours_is_said_when_it_isnt_known() {
    let (server, _, _dirs) = server(Caller::default());
    let limits = answer(&server, "check_limits", json!({}));
    assert_eq!(limits.data["you"]["account"], Value::Null);
    assert_eq!(
        limits.data["you"]["account_unknown"],
        "Turnscope can't tell which agent started it, so which account is yours isn't known."
    );
    assert!(
        limits
            .said()
            .starts_with("Turnscope can't tell which agent started it")
    );
    // The accounts in use are given still.
    assert_eq!(limits.data["accounts"][0]["id"], CLAUDE_ACCOUNT);
    let elsewhere = Caller {
        config: Some((
            "CLAUDE_CONFIG_DIR".to_owned(),
            "/Users/joey/.claude-work".to_owned(),
        )),
        ..claude_code()
    };
    let (server, _, _dirs) = history::limits::server(elsewhere);
    let limits = answer(&server, "check_limits", json!({}));
    assert!(
        limits.data["you"]["account_unknown"]
            .as_str()
            .unwrap()
            .contains("CLAUDE_CONFIG_DIR=/Users/joey/.claude-work"),
        "{}",
        limits.data["you"]
    );
}
