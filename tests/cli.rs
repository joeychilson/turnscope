//! The binary as people and agents use it, over a fixed clock, the fixture
//! home's two synthetic Claude Code sessions (one a day before, one working
//! through the last two hours), and a store holding four synthetic accounts
//! as their providers were read, off the network. Every
//! answer is compared with its snapshot in `tests/snapshots`;
//! `TURNSCOPE_BLESS=1` writes them anew.

#![allow(clippy::unwrap_used)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rusqlite::{Connection, params};

/// 2026-10-01 12:00 UTC.
const NOW: i64 = 1_790_856_000_000;
const HOUR: i64 = 3_600_000;
const SESSION: &str = "claude-code:0f6e3f6a-713c-4d1e-9a6b-2b8c1d4e5f60";
/// The session working through the last two hours.
const WORKING: &str = "claude-code:7c1d2e3f-4a5b-4c6d-8e9f-0a1b2c3d4e5f";

fn home() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/home")
}

fn turnscope(data: &Path, args: &[&str], input: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_turnscope"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        // As Claude Code sets it in a server it starts: the caller, which
        // otherwise the processes this test runs under would decide.
        .env(
            "CLAUDE_CODE_SESSION_ID",
            "0f6e3f6a-713c-4d1e-9a6b-2b8c1d4e5f60",
        )
        .env("TZ", "UTC")
        .env("TURNSCOPE_NOW", NOW.to_string())
        .env("TURNSCOPE_OFFLINE", "1")
        .arg("--data")
        .arg(data)
        .arg("--home")
        .arg(home())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(input.unwrap_or_default().as_bytes())
        .unwrap();
    drop(stdin);
    child.wait_with_output().unwrap()
}

/// A store with the fixture's history read, and four accounts:
/// - Claude Max, Claude Code's: its five hours running out within the hour
///   at the pace of the last three, its week lasting;
/// - ChatGPT Plus, Codex's: room, not used since its last read;
/// - the OpenRouter API key, whose key Pi holds: credits, and the key's own
///   weekly limit;
/// - SuperGrok, Grok Build's: its login refused, never read.
fn store() -> tempfile::TempDir {
    let data = tempfile::tempdir().unwrap();
    assert!(turnscope(data.path(), &["accounts"], None).status.success());
    let db = Connection::open(data.path().join("turnscope.sqlite")).unwrap();
    let at = |hours: f64| NOW - (hours * HOUR as f64) as i64;
    let last = at(1.0 / 6.0);
    for (id, provider, kind, title, label, read_at, problem) in [
        (
            "anthropic:acct-1:org-1",
            "anthropic",
            "subscription",
            "Claude Max",
            Some("me@example.com"),
            Some(last),
            None,
        ),
        (
            "openai:acct-2",
            "openai",
            "subscription",
            "ChatGPT Plus",
            Some("me@example.com"),
            Some(last),
            None,
        ),
        (
            "openrouter:api",
            "openrouter",
            "apiKey",
            "OpenRouter API key",
            None,
            Some(last),
            None,
        ),
        (
            "xai:user-3",
            "xai",
            "subscription",
            "SuperGrok",
            Some("me@example.com"),
            None,
            Some("signIn"),
        ),
    ] {
        db.execute(
            "INSERT INTO account (id, provider, kind, title, label, read_at, problem) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, provider, kind, title, label, read_at, problem],
        )
        .unwrap();
    }
    let reading = |account: &str,
                   key: &str,
                   name: &str,
                   read: i64,
                   used: f64,
                   size: Option<f64>,
                   window: Option<(f64, f64)>| {
        db.execute(
            "INSERT INTO reading (account, limit_key, at, name, used, size, starts, resets) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![account, key, read, name, used, size, window.map(|(starts, _)| at(starts)), window.map(|(_, resets)| at(resets))],
        )
        .unwrap();
    };
    // Five hours from three hours ago, and a week from Monday.
    for (hours, five, week) in [
        (1.5, 35.0, 40.0),
        (1.0, 45.0, 42.0),
        (0.5, 60.0, 43.0),
        (1.0 / 6.0, 75.0, 44.0),
    ] {
        reading(
            "anthropic:acct-1:org-1",
            "five_hour",
            "5 hours",
            at(hours),
            five,
            None,
            Some((3.0, -2.0)),
        );
        reading(
            "anthropic:acct-1:org-1",
            "seven_day",
            "Weekly",
            at(hours),
            week,
            None,
            Some((84.0, -84.0)),
        );
    }
    for (hours, week) in [(26.0, 12.0), (1.0, 20.0), (1.0 / 6.0, 20.0)] {
        reading(
            "openai:acct-2",
            "secondary_window",
            "Weekly",
            at(hours),
            week,
            None,
            Some((60.0, -108.0)),
        );
    }
    reading(
        "openrouter:api",
        "credits",
        "Credits",
        last,
        30.0,
        Some(50.0),
        None,
    );
    reading(
        "openrouter:api",
        "key-0123456789abcdef",
        "Weekly key limit",
        last,
        45.0,
        Some(20.0),
        Some((60.0, -108.0)),
    );
    // Signed in since two days ago, so the fixture's session, a day ago,
    // drew on Claude Max.
    let folder = |name: &str| home().join(name).to_string_lossy().into_owned();
    for (agent, name, provider, account, key) in [
        (
            "claude-code",
            ".claude",
            "anthropic",
            "anthropic:acct-1:org-1",
            None,
        ),
        ("codex", ".codex", "openai", "openai:acct-2", None),
        (
            "pi",
            ".pi/agent",
            "openrouter",
            "openrouter:api",
            Some("0123456789abcdef"),
        ),
        ("grok-build", ".grok", "xai", "xai:user-3", None),
    ] {
        db.execute(
            "INSERT INTO sign_in (agent, folder, provider, since, account, key) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![agent, folder(name), provider, at(48.0), account, key],
        )
        .unwrap();
    }
    db.execute(
        "INSERT INTO price (provider, model, since, cost) VALUES ('anthropic', 'claude-opus-5', 0, '{\"input\":5,\"output\":25,\"cache_read\":0.5,\"cache_write\":6.25}')",
        [],
    )
    .unwrap();
    // Read the history again, now priced and attributed.
    db.execute("UPDATE state SET readers = 0", []).unwrap();
    data
}

/// Compare `text` with the snapshot `name`, or write it under
/// `TURNSCOPE_BLESS`; a snapshot that differs is added to `stale`.
fn check(name: &str, text: &str, stale: &mut Vec<String>) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name);
    if std::env::var_os("TURNSCOPE_BLESS").is_some() {
        std::fs::write(&path, text).unwrap();
    } else if std::fs::read_to_string(&path).ok().as_deref() != Some(text) {
        stale.push(name.to_owned());
    }
}

/// `text` with the directories that differ from run to run named instead.
fn steady(text: &str, data: &Path) -> String {
    text.replace(&data.display().to_string(), "<data>")
        .replace(&home().display().to_string(), "<home>")
}

#[test]
fn every_command_answers_as_its_snapshot_says() {
    let data = store();
    let mut stale = Vec::new();
    for (name, args) in [
        ("status", vec!["status"]),
        (
            "status-hours",
            vec!["status", "--account", "claude", "--hours", "3"],
        ),
        (
            "usage-model",
            vec!["usage", "--since", "2026-09-29", "--by", "model"],
        ),
        (
            "usage-account",
            vec!["usage", "--since", "2026-09-29", "--by", "account"],
        ),
        (
            "usage-limit",
            vec!["usage", "--account", "claude", "--limit", "week"],
        ),
        ("sessions", vec!["sessions"]),
        // Words said in different messages of one session find it.
        ("sessions-search", vec!["sessions", "--search", "CSV RFC"]),
        // A unique start of an agent's own id names the session.
        ("sessions-show", vec!["sessions", "show", "0f6e3f6a"]),
        (
            "sessions-read",
            vec!["sessions", "read", SESSION, "--from", "1", "--count", "3"],
        ),
        ("handoff", vec!["handoff", SESSION]),
        ("handoff-working", vec!["handoff", WORKING]),
        ("accounts", vec!["accounts"]),
        ("config", vec!["config"]),
        ("connect", vec!["connect"]),
    ] {
        for json in [false, true] {
            let mut args = args.clone();
            if json {
                args.push("--json");
            }
            let output = turnscope(data.path(), &args, None);
            assert!(
                output.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let text = steady(&String::from_utf8_lossy(&output.stdout), data.path());
            check(
                &format!("{name}.{}", if json { "json" } else { "txt" }),
                &text,
                &mut stale,
            );
        }
    }
    assert!(
        stale.is_empty(),
        "snapshots that differ: {stale:?}; run with TURNSCOPE_BLESS=1 and review the diff"
    );
}

#[test]
fn mistakes_say_how_to_fix_them_with_their_exit_codes() {
    let data = store();
    for (args, code, says) in [
        (vec!["sessions", "show", "nope"], 3, "Session ids look like"),
        (
            vec!["sessions", "show", "claude-code:nope"],
            3,
            "no session is claude-code:nope",
        ),
        (
            vec!["accounts", "hide", "me@example.com"],
            2,
            "names 3 accounts",
        ),
        (
            vec!["status", "--account", "gemini"],
            3,
            "no account is named gemini",
        ),
        (vec!["usage", "--since", "last tuesday"], 2, "24h, 7d or 4w"),
        (
            vec!["usage", "--since", "99999999999999d"],
            2,
            "24h, 7d or 4w",
        ),
        (
            vec!["config", "set", "notify.everything", "on"],
            2,
            "notify.runningOut",
        ),
    ] {
        let output = turnscope(data.path(), &args, None);
        assert_eq!(output.status.code(), Some(code), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(says),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn a_guard_blocks_under_its_percent_and_passes_otherwise() {
    let data = store();
    let guard = |below: &str| {
        turnscope(
            data.path(),
            &[
                "guard",
                "--account",
                "claude",
                "--limit",
                "5 hours",
                "--below",
                below,
            ],
            None,
        )
    };
    assert_eq!(guard("50").status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&guard("50").stderr).trim(),
        "Claude Max's 5-hour limit has 25% left, under 50%. It resets 14:00 (in 2h); at this pace it runs out 13:00 (in 1h)."
    );
    assert_eq!(guard("20").status.code(), Some(0));
    // One it can't follow blocks nothing.
    let unknown = turnscope(
        data.path(),
        &[
            "guard",
            "--account",
            "claude",
            "--limit",
            "month",
            "--below",
            "50",
        ],
        None,
    );
    assert_eq!(unknown.status.code(), Some(1));
    // Nor does one mistyped.
    assert_eq!(guard("150").status.code(), Some(1));
}

#[test]
fn a_status_line_says_the_account_of_the_session_it_is_given() {
    let data = store();
    let line = |args: &[&str], input: Option<&str>| {
        let output = turnscope(data.path(), args, input);
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    let claude = "Claude Max · 5h 25%, out in 1h · weekly 56%";
    // Claude Code's JSON names the session, whose account it draws on.
    let given = r#"{"session_id": "7c1d2e3f-4a5b-4c6d-8e9f-0a1b2c3d4e5f", "cwd": "/work/api"}"#;
    assert_eq!(line(&["statusline"], Some(given)), claude);
    assert_eq!(
        line(&["statusline", "--account", "chatgpt"], None),
        "ChatGPT Plus · weekly 80%"
    );
    assert_eq!(
        line(&["statusline", "--account", "supergrok"], None),
        "SuperGrok · sign in again"
    );
}

#[test]
fn an_account_with_no_read_for_half_an_hour_shows_as_of_its_last() {
    let data = store();
    // Claude's last read 35 minutes ago, and nothing written since.
    let db = Connection::open(data.path().join("turnscope.sqlite")).unwrap();
    let shift = HOUR * 25 / 60;
    db.execute(
        "UPDATE reading SET at = at - ?1 WHERE account = 'anthropic:acct-1:org-1'",
        [shift],
    )
    .unwrap();
    db.execute(
        "UPDATE account SET read_at = read_at - ?1 WHERE id = 'anthropic:acct-1:org-1'",
        [shift],
    )
    .unwrap();
    let output = turnscope(data.path(), &["status", "--account", "claude"], None);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("As of 11:25 (35m ago): it couldn't be read since."),
        "{text}"
    );
}

#[test]
fn an_mcp_session_answers_as_its_snapshot_says() {
    let data = store();
    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"check_limits","arguments":{"all":true,"hours":2}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"explain_usage","arguments":{"account":"claude","limit":"week","by":"model"}}}"#,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"find_sessions","arguments":{"folder":"/work/api"}}}"#,
        r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"handoff","arguments":{"folder":"/work/api"}}}"#,
        r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"read_session","arguments":{"session":"claude-code:0f6e3f6a-713c-4d1e-9a6b-2b8c1d4e5f60","kinds":["user"]}}}"#,
        r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"read_session","arguments":{"session":"nope"}}}"#,
        r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"check_limits","arguments":{"hours":"two"}}}"#,
        r#"{"jsonrpc":"2.0","id":10,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        r#"{"jsonrpc":"2.0","id":11,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2024-01-01"}}}"#,
    ];
    let output = turnscope(data.path(), &["mcp"], Some(&(requests.join("\n") + "\n")));
    assert!(output.status.success());
    let replies: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    // A notification is answered with nothing.
    assert_eq!(replies.len(), requests.len() - 1);
    let mut stale = Vec::new();
    check(
        "mcp.json",
        &(serde_json::to_string_pretty(&replies).unwrap() + "\n"),
        &mut stale,
    );
    assert!(
        stale.is_empty(),
        "mcp.json differs; run with TURNSCOPE_BLESS=1 and review the diff"
    );
}

#[test]
fn serve_says_hello_with_the_status_and_answers_in_turn() {
    let data = store();
    // Read the history first, or serve reads it while the test asks.
    assert!(turnscope(data.path(), &["sessions"], None).status.success());
    let socket =
        String::from_utf8(turnscope(data.path(), &["serve", "--socket"], None).stdout).unwrap();
    let mut serve = Command::new(env!("CARGO_BIN_EXE_turnscope"))
        .env("TZ", "UTC")
        .env("TURNSCOPE_NOW", NOW.to_string())
        .env("TURNSCOPE_OFFLINE", "1")
        .arg("--data")
        .arg(data.path())
        .arg("--home")
        .arg(home())
        .args(["serve", "--linger", "5"])
        .spawn()
        .unwrap();
    let stream = (0..100)
        .find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            std::os::unix::net::UnixStream::connect(socket.trim()).ok()
        })
        .unwrap();
    let mut lines = BufReader::new(stream.try_clone().unwrap()).lines();
    let mut ask = |request: &str| {
        writeln!(&stream, "{request}").unwrap();
        loop {
            let reply: serde_json::Value =
                serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
            // Pushed statuses come between answers.
            if reply.get("id").is_some() {
                return reply;
            }
        }
    };
    let early = ask(r#"{"jsonrpc":"2.0","id":1,"method":"alerts.ack","params":{"ids":[]}}"#);
    assert_eq!(early["error"]["code"], -32002);
    let other = ask(r#"{"jsonrpc":"2.0","id":2,"method":"hello","params":{"protocol":2}}"#);
    assert_eq!(other["error"]["code"], -32001);
    let hello = ask(r#"{"jsonrpc":"2.0","id":3,"method":"hello","params":{"protocol":1}}"#);
    assert_eq!(
        hello["result"]["server"],
        format!("turnscope {}", env!("CARGO_PKG_VERSION"))
    );
    let accounts: Vec<&str> = hello["result"]["status"]["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|account| account["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        accounts,
        [
            "anthropic:acct-1:org-1",
            "openai:acct-2",
            "openrouter:api",
            "xai:user-3"
        ]
    );
    let breakdown = ask(
        r#"{"jsonrpc":"2.0","id":4,"method":"limit.breakdown","params":{"account":"anthropic:acct-1:org-1","limit":"seven_day"}}"#,
    );
    assert_eq!(breakdown["result"]["sessions"][0]["session"], WORKING);
    let hidden = ask(
        r#"{"jsonrpc":"2.0","id":5,"method":"account.hide","params":{"account":"xai:user-3","hidden":true}}"#,
    );
    assert_eq!(hidden["result"], serde_json::json!({}));
    // Each request in the contract's examples, which the app's tests check
    // it writes, is one serve reads: answered, not refused. Not `agent.*`,
    // which would run an agent's own command on this Mac, and `shutdown`
    // last.
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("contract");
    let example = |name: &str| std::fs::read_to_string(examples.join(name)).unwrap();
    let mut requests: Vec<String> = std::fs::read_dir(&examples)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".request.json"))
        .filter(|name| !name.starts_with("agent.") && name != "shutdown.request.json")
        .collect();
    requests.sort();
    for name in requests {
        let request: serde_json::Value = serde_json::from_str(&example(&name)).unwrap();
        let answer = ask(&request.to_string());
        assert!(
            answer.get("result").is_some(),
            "{name} was refused: {answer}"
        );
    }
    let shutdown: serde_json::Value =
        serde_json::from_str(&example("shutdown.request.json")).unwrap();
    assert_eq!(ask(&shutdown.to_string())["result"], serde_json::json!({}));
    assert!(serve.wait().unwrap().success());
}
