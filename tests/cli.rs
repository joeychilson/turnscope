//! The binary as people and agents use it, over a fixed clock, the fixture
//! home's synthetic sessions (two of Claude Code's, one a day before, one
//! working through the last two hours, and one of each other agent's, a day
//! before), and a store holding four synthetic accounts as their providers
//! were read, off the network. Every answer is compared with its snapshot in
//! `tests/snapshots`; `TURNSCOPE_BLESS=1` writes them anew.

#![allow(clippy::unwrap_used)]

use std::collections::HashSet;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

use rusqlite::{Connection, params};

/// 2026-10-01 12:00 UTC.
const NOW: i64 = 1_790_856_000_000;
const HOUR: i64 = 3_600_000;
const SESSION: &str = "claude-code:0f6e3f6a-713c-4d1e-9a6b-2b8c1d4e5f60";
/// The session working through the last two hours.
const WORKING: &str = "claude-code:7c1d2e3f-4a5b-4c6d-8e9f-0a1b2c3d4e5f";
/// Each other agent's session, a day before.
const CODEX: &str = "codex:019a2b3c-4d5e-7f60-8a1b-2c3d4e5f6a7b";
const OPENCODE: &str = "opencode:ses_2f9a8b7c6d5e4f3a2b1c0d9e8f";
const PI: &str = "pi:5e6f7a8b-9c0d-4e1f-a2b3-c4d5e6f7a8b9";
const GROK_BUILD: &str = "grok-build:8d9e0f1a-2b3c-4d5e-8f6a-7b8c9d0e1f2a";
const ANTIGRAVITY: &str = "antigravity:4c5d6e7f-8a9b-4c0d-9e1f-2a3b4c5d6e7f";

/// The fixture home, copied once a run to where tests may write: history
/// that is a database, as OpenCode's and Antigravity's, is built there from
/// the SQL beside it rather than kept as a binary file.
fn home() -> &'static Path {
    static HOME: OnceLock<(PathBuf, std::fs::File)> = OnceLock::new();
    let (home, _lock) = HOME.get_or_init(|| {
        fn copy(from: &Path, to: &Path) {
            std::fs::create_dir_all(to).unwrap();
            for entry in std::fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let (from, to) = (entry.path(), to.join(entry.file_name()));
                if entry.file_type().unwrap().is_dir() {
                    copy(&from, &to);
                } else {
                    std::fs::copy(from, &to).unwrap();
                    // An agent's database is kept as the SQL that builds it.
                    if to.extension().is_some_and(|extension| extension == "sql") {
                        let sql = std::fs::read_to_string(&to).unwrap();
                        Connection::open(to.with_extension("db"))
                            .unwrap()
                            .execute_batch(&sql)
                            .unwrap();
                    }
                }
            }
        }
        let made = Path::new(env!("CARGO_TARGET_TMPDIR"));
        // Held until the process ends, so another run of these tests waits
        // to make the home anew while this one reads it.
        let lock = std::fs::File::create(made.join("home.lock")).unwrap();
        lock.lock().unwrap();
        let home = made.join("home");
        let _ = std::fs::remove_dir_all(&home);
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/home"),
            &home,
        );
        (home, lock)
    });
    home
}

fn turnscope(data: &Path, args: &[&str], input: Option<&str>) -> Output {
    let mut child = command(data, args)
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

/// `turnscope` with `args` over the store `data`, as Claude Code runs it in
/// its session `SESSION`.
fn command(data: &Path, args: &[&str]) -> Command {
    command_as("claude", data, args)
}

/// `turnscope` with `args` over the store `data`, as the agent whose command
/// is `agent` runs it: under a shell named as that command, which waits on
/// turnscope rather than becoming it. The caller is the nearest agent above
/// turnscope, so without the shell it would be the agent these tests run in.
fn command_as(agent: &str, data: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .arg0(agent)
        .args([
            "-c",
            r#""$@"; exit $?"#,
            "sh",
            env!("CARGO_BIN_EXE_turnscope"),
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        // As Claude Code sets it in what it starts, and so in what another
        // agent run from its shell starts.
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
        .args(args);
    command
}

/// What the MCP server's `explain_usage` answers each of `arguments`, over
/// the store `data`.
fn explain_usage(data: &Path, arguments: &[&str]) -> Vec<String> {
    let requests: String = arguments
        .iter()
        .map(|arguments| {
            format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"explain_usage","arguments":{arguments}}}}}"#
            ) + "\n"
        })
        .collect();
    let output = turnscope(data, &["mcp"], Some(&requests));
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| {
            let reply: serde_json::Value = serde_json::from_str(line).unwrap();
            reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
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
    db.execute(
        "INSERT INTO account (id, provider, kind, title, label, read_at, problem) VALUES
             ('anthropic:acct-1:org-1', 'anthropic', 'subscription', 'Claude Max', 'me@example.com', ?1, NULL),
             ('openai:acct-2', 'openai', 'subscription', 'ChatGPT Plus', 'me@example.com', ?1, NULL),
             ('openrouter:api', 'openrouter', 'apiKey', 'OpenRouter API key', NULL, ?1, NULL),
             ('xai:user-3', 'xai', 'subscription', 'SuperGrok', 'me@example.com', NULL, 'signIn')",
        [NOW - HOUR / 6],
    )
    .unwrap();
    // Read, and its window started and resets, hours before now.
    db.execute(
        "WITH read (account, key, name, ago, used, size, starts, resets) AS (VALUES
             -- Five hours from three hours ago, and a week from Monday.
             ('anthropic:acct-1:org-1', 'five_hour', '5 hours', 1.5, 35, NULL, 3, -2),
             ('anthropic:acct-1:org-1', 'seven_day', 'Weekly', 1.5, 40, NULL, 84, -84),
             ('anthropic:acct-1:org-1', 'five_hour', '5 hours', 1, 45, NULL, 3, -2),
             ('anthropic:acct-1:org-1', 'seven_day', 'Weekly', 1, 42, NULL, 84, -84),
             ('anthropic:acct-1:org-1', 'five_hour', '5 hours', 0.5, 60, NULL, 3, -2),
             ('anthropic:acct-1:org-1', 'seven_day', 'Weekly', 0.5, 43, NULL, 84, -84),
             ('anthropic:acct-1:org-1', 'five_hour', '5 hours', 1 / 6.0, 75, NULL, 3, -2),
             ('anthropic:acct-1:org-1', 'seven_day', 'Weekly', 1 / 6.0, 44, NULL, 84, -84),
             ('openai:acct-2', 'secondary_window', 'Weekly', 26, 12, NULL, 60, -108),
             ('openai:acct-2', 'secondary_window', 'Weekly', 1, 20, NULL, 60, -108),
             ('openai:acct-2', 'secondary_window', 'Weekly', 1 / 6.0, 20, NULL, 60, -108),
             ('openrouter:api', 'credits', 'Credits', 1 / 6.0, 30, 50, NULL, NULL),
             ('openrouter:api', 'key-0123456789abcdef', 'Weekly key limit', 1 / 6.0, 45, 20, 60, -108))
         INSERT INTO reading (account, limit_key, at, name, used, size, starts, resets)
         SELECT account, key, ?1 - ago * ?2, name, used, size, ?1 - starts * ?2, ?1 - resets * ?2 FROM read",
        [NOW, HOUR],
    )
    .unwrap();
    // Signed in since two days ago, so the fixture's sessions a day ago drew
    // on these accounts; OpenCode's, signed in to none, on no account.
    db.execute(
        "INSERT INTO sign_in (agent, folder, provider, since, account, key) VALUES
             ('claude-code', ?1 || '/.claude', 'anthropic', ?2, 'anthropic:acct-1:org-1', NULL),
             ('codex', ?1 || '/.codex', 'openai', ?2, 'openai:acct-2', NULL),
             ('pi', ?1 || '/.pi/agent', 'openrouter', ?2, 'openrouter:api', '0123456789abcdef'),
             ('grok-build', ?1 || '/.grok', 'xai', ?2, 'xai:user-3', NULL)",
        params![home().to_string_lossy(), NOW - 48 * HOUR],
    )
    .unwrap();
    db.execute_batch(
        "INSERT INTO price (provider, model, since, cost) VALUES ('anthropic', 'claude-opus-5', 0, '{\"input\":5,\"output\":25,\"cache_read\":0.5,\"cache_write\":6.25}');
         -- Read the history again, now priced and attributed.
         UPDATE state SET readers = 0;",
    )
    .unwrap();
    data
}

/// Whether `text` is the snapshot `name`; under `TURNSCOPE_BLESS`, it's
/// written as the snapshot instead.
fn fresh(name: &str, text: &str) -> bool {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(name);
    if std::env::var_os("TURNSCOPE_BLESS").is_some() {
        std::fs::write(&path, text).unwrap();
        return true;
    }
    std::fs::read_to_string(&path).ok().as_deref() == Some(text)
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
        ("usage", vec!["usage"]),
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
        // The page after the session the cursor names.
        (
            "sessions-cursor",
            vec!["sessions", "--count", "2", "--cursor", SESSION],
        ),
        // Words said in different messages of one session find it.
        ("sessions-search", vec!["sessions", "--search", "CSV RFC"]),
        // Words said together, so the message that says them is quoted.
        (
            "sessions-search-said",
            vec!["sessions", "--search", "quoted commas"],
        ),
        // A unique start of an agent's own id names the session.
        ("sessions-show", vec!["sessions", "show", "0f6e3f6a"]),
        (
            "sessions-read",
            vec!["sessions", "read", SESSION, "--from", "1", "--count", "3"],
        ),
        ("sessions-show-codex", vec!["sessions", "show", CODEX]),
        ("sessions-read-codex", vec!["sessions", "read", CODEX]),
        // The last of its two failed calls.
        (
            "sessions-read-failed",
            vec!["sessions", "read", CODEX, "--failed", "--from", "-1"],
        ),
        ("sessions-show-opencode", vec!["sessions", "show", OPENCODE]),
        ("sessions-read-opencode", vec!["sessions", "read", OPENCODE]),
        ("sessions-show-pi", vec!["sessions", "show", PI]),
        ("sessions-read-pi", vec!["sessions", "read", PI]),
        (
            "sessions-show-grok-build",
            vec!["sessions", "show", GROK_BUILD],
        ),
        (
            "sessions-read-grok-build",
            vec!["sessions", "read", GROK_BUILD],
        ),
        (
            "sessions-show-antigravity",
            vec!["sessions", "show", ANTIGRAVITY],
        ),
        (
            "sessions-read-antigravity",
            vec!["sessions", "read", ANTIGRAVITY],
        ),
        ("handoff", vec!["handoff", SESSION]),
        ("handoff-working", vec!["handoff", WORKING]),
        // Taken by default: the folder's latest session but the caller's
        // own, Codex's, with Pi's before it.
        ("handoff-folder", vec!["handoff", "--folder", "/work/app"]),
        ("accounts", vec!["accounts"]),
        ("connect", vec!["connect"]),
        ("doctor", vec!["doctor"]),
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
            // The directories that differ from run to run, named instead.
            let text = String::from_utf8_lossy(&output.stdout)
                .replace(&data.path().display().to_string(), "<data>")
                .replace(&home().display().to_string(), "<home>");
            let name = format!("{name}.{}", if json { "json" } else { "txt" });
            if !fresh(&name, &text) {
                stale.push(name);
            }
        }
    }
    assert!(
        stale.is_empty(),
        "snapshots that differ: {stale:?}; run with TURNSCOPE_BLESS=1 and review the diff"
    );
}

/// The examples in docs/cli.md are what the binary says: each line of their
/// output is a line of some snapshot.
#[test]
fn the_docs_show_what_the_snapshots_hold() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let snapshots: Vec<String> = std::fs::read_dir(root.join("tests/snapshots"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "txt"))
        .map(|path| std::fs::read_to_string(path).unwrap())
        .collect();
    let shown: HashSet<&str> = snapshots.iter().flat_map(|text| text.lines()).collect();
    let docs = std::fs::read_to_string(root.join("docs/cli.md")).unwrap();
    let mut example = false;
    let unseen: Vec<&str> = docs
        .lines()
        .filter(|line| {
            if line.starts_with("```") {
                example = *line == "```console";
                return false;
            }
            example
                && !line.is_empty()
                && *line != "…"
                && !line.starts_with("$ ")
                && !shown.contains(line)
        })
        .collect();
    assert!(
        unseen.is_empty(),
        "lines of docs/cli.md's examples that no snapshot has: {unseen:#?}"
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
        (
            vec!["usage", "--account", "claude", "--limit", "month"],
            3,
            "has no limit named month",
        ),
        (
            vec!["usage", "--limit", "week", "--until", "today"],
            2,
            "cannot be used with",
        ),
        // As `sessions --agent claude` says.
        (
            vec!["usage", "--agent", "claude"],
            2,
            "no agent is claude; agents are claude-code, ",
        ),
        // As `explain_usage` says of a `since` ahead of now.
        (
            vec![
                "usage",
                "--account",
                "claude",
                "--limit",
                "week",
                "--since",
                "now",
            ],
            2,
            "--since is now or later",
        ),
        (vec!["usage", "--since", "last tuesday"], 2, "24h, 7d or 4w"),
        (
            vec!["usage", "--since", "99999999999999d"],
            2,
            "24h, 7d or 4w",
        ),
        (vec!["sessions", "--count", "0"], 2, "0 is not in 1..=100"),
        (vec!["sessions", "-n", "101"], 2, "101 is not in 1..=100"),
        (
            vec!["sessions", "read", "claude-code:0f6e", "--count", "0"],
            2,
            "0 is not in 1..=100",
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

/// A command says so before it reads all of history: the first time, and
/// again once a reader changed. Otherwise it says nothing.
#[test]
fn a_command_says_when_it_reads_all_of_history() {
    let data = tempfile::tempdir().unwrap();
    let said = |args: &[&str]| {
        let output = turnscope(data.path(), args, None);
        assert!(output.status.success(), "{args:?}");
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    let reading = "Reading your agents' history…\n";
    // One that reports no data reads no history.
    assert_eq!(said(&["config"]), "");
    assert_eq!(said(&["sessions"]), reading);
    assert_eq!(said(&["sessions"]), "");
    // As after an update that raised `ingest::READERS`.
    let db = Connection::open(data.path().join("turnscope.sqlite")).unwrap();
    db.execute("UPDATE state SET readers = 0", []).unwrap();
    assert_eq!(said(&["status"]), reading);
    assert_eq!(said(&["status"]), "");
}

/// A command that waits for another Turnscope reading history says so, and
/// answers once that one is done.
#[test]
fn a_command_says_when_it_waits_for_another_turnscope_reading_history() {
    let data = tempfile::tempdir().unwrap();
    assert!(turnscope(data.path(), &["sessions"], None).status.success());
    // Held as the other Turnscope holds it, all the while it reads.
    let reading = std::fs::File::create(data.path().join("ingest.lock")).unwrap();
    reading.lock().unwrap();
    let mut waiting = command(data.path(), &["sessions"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut said = BufReader::new(waiting.stderr.take().unwrap()).lines();
    let (send, first) = std::sync::mpsc::channel();
    let rest = std::thread::spawn(move || {
        let _ = send.send(said.next().and_then(Result::ok));
        said.map_while(Result::ok).collect::<Vec<String>>()
    });
    // It says so while it waits, not once the wait is over: until then, the
    // other Turnscope goes on reading.
    let first = first.recv_timeout(std::time::Duration::from_secs(20));
    drop(reading);
    let output = waiting.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        first.ok().flatten().as_deref(),
        Some("Waiting while another Turnscope reads your agents' history…")
    );
    assert_eq!(rest.join().unwrap(), Vec::<String>::new());
    assert!(String::from_utf8_lossy(&output.stdout).contains(SESSION));
}

#[test]
fn a_relative_folder_is_in_the_current_folder() {
    let data = store();
    for command_name in ["sessions", "handoff"] {
        let given = turnscope(data.path(), &[command_name, "--folder", "/work/app"], None);
        let relative = command(data.path(), &[command_name, "--folder", "work/app"])
            .current_dir("/")
            .output()
            .unwrap();
        assert!(given.status.success(), "{command_name}");
        assert!(String::from_utf8_lossy(&given.stdout).contains(CODEX));
        assert_eq!(
            String::from_utf8_lossy(&relative.stdout),
            String::from_utf8_lossy(&given.stdout),
            "{command_name}: {}",
            String::from_utf8_lossy(&relative.stderr)
        );
    }
}

/// A folder names its sessions however it's written: with `..`, `.` or a
/// slash at its end.
#[test]
fn a_folder_names_its_sessions_however_its_written() {
    let data = store();
    // Once the history is read, a session of `/work/app`'s project run
    // beside it, as in a worktree kept outside its repository.
    assert!(turnscope(data.path(), &["sessions"], None).status.success());
    Connection::open(data.path().join("turnscope.sqlite"))
        .unwrap()
        .execute(
            "INSERT INTO session (id, agent, folder, cwd, project, title, started, last)
             VALUES ('codex:beside', 'codex', 'f', '/work/app-beside', '/work/app', 'Try a faster importer', ?1, ?1)",
            [NOW - 30 * HOUR],
        )
        .unwrap();
    for command_name in ["sessions", "handoff"] {
        let answer = |folder: &str, from: &str| {
            let output = command(data.path(), &[command_name, "--folder", folder])
                .current_dir(from)
                .output()
                .unwrap();
            (
                String::from_utf8_lossy(&output.stdout).into_owned(),
                String::from_utf8_lossy(&output.stderr).into_owned(),
            )
        };
        let (plain, _) = answer("/work/app", "/");
        assert!(
            plain.contains(CODEX) && plain.contains("codex:beside"),
            "{command_name}: {plain}"
        );
        for (folder, from) in [
            ("/work/app/", "/"),
            ("/work/app/.", "/"),
            ("/work/api/../app", "/"),
            ("work/./app/", "/"),
            ("../work/app", "/usr"),
        ] {
            let (given, error) = answer(folder, from);
            assert_eq!(
                given, plain,
                "{command_name} --folder {folder} in {from}: {error}"
            );
        }
    }
}

/// Codex pointed at its agent folder by `CODEX_HOME`, written with a slash
/// at its end or with `..`: the account is still the caller's.
#[test]
fn the_callers_account_is_its_agent_folders_however_its_written() {
    let data = store();
    // Read the history first: neither command below does.
    assert!(turnscope(data.path(), &["sessions"], None).status.success());
    for written in ["/.codex", "/.codex/", "/.claude/../.codex"] {
        let folder = format!("{}{written}", home().display());
        let run = |args: &[&str]| {
            command_as("codex", data.path(), args)
                .env("CODEX_HOME", &folder)
                .output()
                .unwrap()
        };
        assert_eq!(
            String::from_utf8_lossy(&run(&["statusline"]).stdout).trim(),
            "ChatGPT Plus · weekly 80%",
            "{written}"
        );
        // Its weekly limit has 80% left, so a guard under 90% blocks.
        let guard = run(&["guard", "--limit", "week", "--below", "90"]);
        assert_eq!(guard.status.code(), Some(2), "{written}");
    }
}

/// Claude Code's folder as its variable gives it, and a tool's `folder`
/// within it, both with `..` and a slash at the end.
#[test]
fn the_tools_fold_the_callers_folder_and_the_folder_they_are_given() {
    let data = store();
    let requests = [
        // No folder: the caller's, `/work/api`.
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"handoff","arguments":{}}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"find_sessions","arguments":{"folder":"../docs/"}}}"#,
    ];
    let mut mcp = command(data.path(), &["mcp"])
        .env("CLAUDE_PROJECT_DIR", "/work/app/../api/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(mcp.stdin.take().unwrap(), "{}", requests.join("\n")).unwrap();
    let output = mcp.wait_with_output().unwrap();
    let texts: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| {
            let reply: serde_json::Value = serde_json::from_str(line).unwrap();
            reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert!(
        texts[0].starts_with("# Handoff: \"Add retries with backoff to the HTTP client.\""),
        "{}",
        texts[0]
    );
    assert!(texts[1].contains(GROK_BUILD), "{}", texts[1]);
}

#[test]
fn sessions_marks_the_session_of_an_agent_that_names_it_nowhere() {
    let data = store();
    // Run by Codex, as from Claude Code's shell, whose session is named in
    // the environment. Run from `/`, which is no folder to work in, its own
    // session is its latest anywhere. Its latest in a folder is marked in
    // `explain_usage_marks_the_callers_session_whichever_agent_asks`.
    let output = command_as("codex", data.path(), &["sessions"])
        .current_dir("/")
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    let yours: Vec<&str> = text
        .lines()
        .filter(|line| line.ends_with(" · yours"))
        .collect();
    assert!(yours.len() == 1 && yours[0].contains(CODEX), "{text}");
}

#[test]
fn a_guard_blocks_under_its_percent_and_passes_otherwise() {
    let data = store();
    let guard = |limit: &str, below: &str| {
        let args = [
            "guard",
            "--account",
            "claude",
            "--limit",
            limit,
            "--below",
            below,
        ];
        turnscope(data.path(), &args, None)
    };
    let blocked = guard("5 hours", "50");
    assert_eq!(blocked.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&blocked.stderr).trim(),
        "Claude Max's 5-hour limit has 25% left, under 50%. It resets 14:00 (in 2h); at this pace it runs out 13:00 (in 1h)."
    );
    assert_eq!(guard("5 hours", "20").status.code(), Some(0));
    // One it can't follow blocks nothing.
    assert_eq!(guard("month", "50").status.code(), Some(1));
    // Nor does one mistyped.
    assert_eq!(guard("5 hours", "150").status.code(), Some(1));
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

/// A new session isn't read yet, as a status line never catches up: its
/// agent's own account shows, not that of whichever agent ran last in its
/// folder.
#[test]
fn a_status_line_for_a_session_not_read_yet_says_its_agents_account() {
    let data = store();
    // Read the history, as the app's daemon has.
    assert!(turnscope(data.path(), &["sessions"], None).status.success());
    let new = "9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d";
    let line = |agent: &str, session: &str, folder: &str| {
        let mut line = command_as(agent, data.path(), &["statusline"])
            .env("CLAUDE_CODE_SESSION_ID", new)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let given = format!(r#"{{"session_id": "{session}", "cwd": "{folder}"}}"#);
        line.stdin
            .take()
            .unwrap()
            .write_all(given.as_bytes())
            .unwrap();
        let output = line.wait_with_output().unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    let (claude, chatgpt) = (
        "Claude Max · 5h 25%, out in 1h · weekly 56%",
        "ChatGPT Plus · weekly 80%",
    );
    // A session that's read says its own account, whoever asks.
    let (_, codex) = CODEX.split_once(':').unwrap();
    assert_eq!(line("claude", codex, "/work/app"), chatgpt);
    // Claude Code's new session, where Grok Build ran last, on SuperGrok.
    assert_eq!(line("claude", new, "/work/docs"), claude);
    // Codex's, where Claude Code ran last: not the first account in use,
    // which is Claude Code's too.
    assert_eq!(line("codex", new, "/work/api"), chatgpt);
}

#[test]
fn an_answer_tells_time_by_the_minute_its_status_is_as_of() {
    let data = store();
    // 40 seconds into the minute; Claude's 5 hours runs out at 13:00 and
    // resets at 14:00.
    let late = (NOW + 40_000).to_string();
    let mut mcp = command(data.path(), &["mcp"])
        .env("TURNSCOPE_NOW", &late)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"check_limits","arguments":{"account":"claude"}}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"explain_usage","arguments":{"account":"claude"}}}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"explain_usage","arguments":{"account":"claude","since":"1h"}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"explain_usage","arguments":{"account":"claude","since":"now"}}}"#,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"explain_usage","arguments":{"all":true}}}"#,
        r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"explain_usage","arguments":{"account":"claude","since":"2026-10-01T12:00:10Z"}}}"#,
    ];
    writeln!(mcp.stdin.take().unwrap(), "{}", requests.join("\n")).unwrap();
    let output = mcp.wait_with_output().unwrap();
    let texts: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| {
            let reply: serde_json::Value = serde_json::from_str(line).unwrap();
            reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    let (limits, usage) = (&texts[0], &texts[1]);
    // In the verdict, and in the 5 hours' own line.
    assert_eq!(
        limits.matches("resets 14:00 (in 2h)").count(),
        2,
        "{limits}"
    );
    // And in what used it, over its window from 09:00.
    assert!(
        usage.contains("since 09:00 (3h ago)") && usage.contains("resets 14:00 (in 2h)"),
        "{usage}"
    );
    // A time counted back from now is told by its minute too: an hour back
    // was 11:00:40, which its clock shows as 11:00. Since now, there's
    // nothing to tell. Earlier in this minute is gone by, not ahead.
    for (text, since) in [
        (&texts[2], "since 11:00 (1h ago)"),
        (&texts[3], "since is now or later"),
        (&texts[4], "(30d ago)"),
        (&texts[5], "since 12:00 (0m ago)"),
    ] {
        assert!(text.contains(since), "{since} in {text}");
    }
    assert!(
        texts[2].contains("its window, since 09:00 (3h ago)"),
        "{}",
        texts[2]
    );
    let line = command(data.path(), &["statusline", "--account", "claude"])
        .env("TURNSCOPE_NOW", &late)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&line.stdout).trim(),
        "Claude Max · 5h 25%, out in 1h · weekly 56%"
    );
}

#[test]
fn an_account_with_no_read_for_half_an_hour_shows_as_of_its_last() {
    let data = store();
    // Claude's last read 35 minutes ago, and nothing written since.
    let db = Connection::open(data.path().join("turnscope.sqlite")).unwrap();
    db.execute_batch(&format!(
        "UPDATE reading SET at = at - {shift} WHERE account = 'anthropic:acct-1:org-1';
         UPDATE account SET read_at = read_at - {shift} WHERE id = 'anthropic:acct-1:org-1';",
        shift = HOUR * 25 / 60
    ))
    .unwrap();
    let output = turnscope(data.path(), &["status", "--account", "claude"], None);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(
            "As of 11:25 (35m ago): it couldn't be read since; it's tried again every 5 minutes."
        ),
        "{text}"
    );
    let output = turnscope(data.path(), &["doctor"], None);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("Claude Max (me@example.com): it couldn't be read since 11:25; it's tried again every 5 minutes."),
        "{text}"
    );
}

#[test]
fn what_used_a_limit_whose_window_reset_since_its_read_counts_from_the_reset() {
    let data = store();
    // Claude's last read at 10:20, of a 5 hours from 06:00 that reset at
    // 11:00. The working session ran from 10:00 until now, noon.
    let db = Connection::open(data.path().join("turnscope.sqlite")).unwrap();
    db.execute_batch(&format!(
        "UPDATE reading SET at = at - {shift} WHERE account = 'anthropic:acct-1:org-1';
         UPDATE account SET read_at = read_at - {shift} WHERE id = 'anthropic:acct-1:org-1';
         UPDATE reading SET starts = {starts}, resets = {resets} WHERE limit_key = 'five_hour';",
        shift = HOUR * 3 / 2,
        starts = NOW - 6 * HOUR,
        resets = NOW - HOUR,
    ))
    .unwrap();
    let limit = ["usage", "--account", "claude", "--limit", "5 hours"];
    let output = turnscope(data.path(), &limit, None);
    let text = String::from_utf8_lossy(&output.stdout);
    // What it used before the reset is no part of the window it's in, and
    // how much of that one is used isn't known: no shares.
    assert!(
        text.contains("5 hours: 100% left, reset 11:00 (1h ago).")
            && text.contains("Thu Oct 1 11:00 to Thu Oct 1 12:00: 12 responses, $0.23\n")
            && text.contains(" Responses   Cost\n"),
        "{text}"
    );
    let output = turnscope(data.path(), &[&limit[..], &["--json"]].concat(), None);
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["usage"]["since"], NOW - HOUR);
    assert_eq!(json["sharePercents"], serde_json::Value::Null);

    let answers = explain_usage(
        data.path(),
        &[
            r#"{"account":"claude","limit":"5 hours"}"#,
            r#"{"account":"claude","limit":"5 hours","since":"3h"}"#,
        ],
    );
    assert!(
        answers[0].contains("since 11:00 (1h ago): $0.23 at list prices over 12 responses.")
            && answers[0].contains("| Session | Cost | Responses |\n"),
        "{}",
        answers[0]
    );
    // Asked since a time, there are no shares to say how to get.
    assert!(
        answers[1].contains("since 09:00 (3h ago)") && !answers[1].contains("Shares of"),
        "{}",
        answers[1]
    );
}

#[test]
fn explain_usage_says_a_cost_it_doesnt_know_is_unpriced() {
    let data = store();
    // Codex's four responses are of a model with no known price.
    let answers = explain_usage(data.path(), &[r#"{"account":"chatgpt"}"#]);
    assert!(
        answers[0].ends_with(
            "\nBy model: gpt-5.5 ($0.00 (+4 unpriced)).\nBy project: /work/app ($0.00 (+4 unpriced))."
        ),
        "{}",
        answers[0]
    );
}

#[test]
fn what_used_a_limit_gives_the_rest_row_its_share_and_no_shares_as_null() {
    let data = store();
    let limit = ["usage", "--account", "claude", "--limit", "week"];
    let asked = |more: &[&str]| {
        let output = turnscope(data.path(), &[&limit[..], more].concat(), None);
        assert!(output.status.success(), "{more:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    // Of the 44 points used, the row shown has 41 and the rest 3, as the
    // tool says them.
    let text = asked(&["--top", "1"]);
    let rest = text.lines().find(|line| line.starts_with("  1 more "));
    assert!(
        rest.is_some_and(|rest| rest.ends_with(" 3  $0.03           3%")),
        "{text}"
    );
    let answers = explain_usage(
        data.path(),
        &[r#"{"account":"claude","limit":"week","top":1}"#],
    );
    assert!(
        answers[0].contains("| 1 more session | $0.03 | 3 | 3% |\n"),
        "{}",
        answers[0]
    );
    // As JSON, a share for each row, or null when no row has one, as since
    // a time.
    let shares = |more: &[&str]| -> serde_json::Value {
        let json: serde_json::Value = serde_json::from_str(&asked(more)).unwrap();
        json["sharePercents"].clone()
    };
    assert_eq!(
        shares(&["--top", "1", "--json"]).as_array().unwrap().len(),
        1
    );
    assert_eq!(
        shares(&["--since", "2h", "--json"]),
        serde_json::Value::Null
    );
}

#[test]
fn status_shows_a_hidden_account_it_names() {
    let data = store();
    let hide = turnscope(data.path(), &["accounts", "hide", "supergrok"], None);
    assert!(hide.status.success());
    let status = |args: &[&str]| {
        let output = turnscope(data.path(), args, None);
        assert!(output.status.success(), "{args:?}");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let grok = "SuperGrok (me@example.com) · via Grok Build · hidden";
    let named = status(&["status", "--account", "supergrok"]);
    assert!(named.starts_with(grok), "{named:?}");
    assert!(!status(&["status"]).contains(grok));
    assert!(status(&["status", "--all"]).contains(grok));
}

#[test]
fn with_no_account_to_show_every_front_end_says_why() {
    let said = |data: &Path, args: &[&str]| {
        let output = turnscope(data, args, None);
        assert!(output.status.success(), "{args:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    let check_limits = |data: &Path, arguments: &str| {
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"check_limits","arguments":{arguments}}}}}"#
        );
        let output = turnscope(data, &["mcp"], Some(&(request + "\n")));
        let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    // A new data directory, off the network: no account is known yet.
    let data = tempfile::tempdir().unwrap();
    let none = "No account's limits have been read yet: no agent here is signed in to one Turnscope reads.";
    assert_eq!(said(data.path(), &["status"]), none);
    assert_eq!(said(data.path(), &["accounts"]), none);
    assert_eq!(check_limits(data.path(), "{}"), none);
    // As JSON, none is an empty list.
    assert_eq!(said(data.path(), &["accounts", "--json"]), "[]");
    assert_eq!(
        said(data.path(), &["status", "--json"]),
        "{\n  \"accounts\": [],\n  \"fit\": null\n}"
    );
    // Every account hidden: there are accounts, and each front end says how
    // it shows one.
    let data = store();
    for account in ["claude", "chatgpt", "openrouter", "supergrok"] {
        said(data.path(), &["accounts", "hide", account]);
    }
    assert_eq!(
        said(data.path(), &["status"]),
        "Every account is hidden: `turnscope status --all` shows them."
    );
    assert_eq!(
        said(data.path(), &["status", "--all"])
            .matches(" · hidden")
            .count(),
        4
    );
    assert_eq!(
        check_limits(data.path(), r#"{"all":true}"#),
        "Every account is hidden: name one with account to see its limits."
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
    assert!(
        fresh(
            "mcp.json",
            &(serde_json::to_string_pretty(&replies).unwrap() + "\n")
        ),
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
    let mut serve = command(data.path(), &["serve"]).spawn().unwrap();
    let stream = (0..100)
        .find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            std::os::unix::net::UnixStream::connect(socket.trim()).ok()
        })
        .unwrap();
    let mut lines = BufReader::new(stream.try_clone().unwrap()).lines();
    let mut ask = |request: &[u8]| {
        (&stream).write_all(&[request, b"\n"].concat()).unwrap();
        loop {
            let reply: serde_json::Value =
                serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
            // Pushed statuses come between answers.
            if reply.get("id").is_some() {
                return reply;
            }
        }
    };
    let early = ask(br#"{"jsonrpc":"2.0","id":1,"method":"alerts.ack","params":{"ids":[]}}"#);
    assert_eq!(early["error"]["code"], -32002);
    let other = ask(br#"{"jsonrpc":"2.0","id":2,"method":"hello","params":{"protocol":1}}"#);
    assert_eq!(other["error"]["code"], -32001);
    let hello = ask(br#"{"jsonrpc":"2.0","id":3,"method":"hello","params":{"protocol":2}}"#);
    assert_eq!(
        hello["result"]["server"],
        format!("turnscope {}", env!("CARGO_PKG_VERSION"))
    );
    let ids = |list: &str| -> Vec<String> {
        let list = hello["result"]["status"][list].as_array().unwrap();
        list.iter()
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        ids("accounts"),
        [
            "anthropic:acct-1:org-1",
            "openai:acct-2",
            "openrouter:api",
            "xai:user-3"
        ]
    );
    assert_eq!(
        ids("agents"),
        [
            "claude-code",
            "codex",
            "opencode",
            "pi",
            "grok-build",
            "antigravity"
        ]
    );
    let breakdown = ask(
        br#"{"jsonrpc":"2.0","id":4,"method":"limit.breakdown","params":{"account":"anthropic:acct-1:org-1","limit":"seven_day"}}"#,
    );
    assert_eq!(breakdown["result"]["sessions"][0]["session"], WORKING);
    let hidden = ask(
        br#"{"jsonrpc":"2.0","id":5,"method":"account.hide","params":{"account":"xai:user-3","hidden":true}}"#,
    );
    assert_eq!(hidden["result"], serde_json::json!({}));
    // A line that isn't UTF-8 isn't JSON, and the client stays: what follows
    // is answered.
    let garbled = ask(b"\xff\xfe{\"x\":1}");
    assert_eq!(garbled["error"]["code"], -32700);
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
        let answer = ask(request.to_string().as_bytes());
        assert!(
            answer.get("result").is_some(),
            "{name} was refused: {answer}"
        );
    }
    let shutdown: serde_json::Value =
        serde_json::from_str(&example("shutdown.request.json")).unwrap();
    assert_eq!(
        ask(shutdown.to_string().as_bytes())["result"],
        serde_json::json!({})
    );
    assert!(serve.wait().unwrap().success());
}
