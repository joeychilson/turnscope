//! The binary as people and agents use it, over a fixed clock, the fixture
//! home's synthetic sessions (two of Claude Code's, one a day before, one
//! working through the last two hours, and one of each other agent's, a day
//! before), and a store holding four synthetic accounts as their providers
//! were read, off the network. Every answer is compared with its snapshot in
//! `tests/snapshots`; `TURNSCOPE_BLESS=1` writes them anew.

#![allow(clippy::unwrap_used)]

use std::collections::{BTreeSet, HashSet};
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

use rusqlite::{Connection, params};

/// 2026-10-01 12:00 UTC.
const NOW: i64 = 1_790_856_000_000;
const HOUR: i64 = 3_600_000;
/// The session working through the last two hours.
const WORKING: &str = "claude-code:7c1d2e3f-4a5b-4c6d-8e9f-0a1b2c3d4e5f";
/// Codex's session, a day before.
const CODEX: &str = "codex:019a2b3c-4d5e-7f60-8a1b-2c3d4e5f6a7b";

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

/// `turnscope` with `args` over the store `data`, as Claude Code runs it.
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

/// What the MCP server `mcp` runs answers each of `calls`, a tool and its
/// arguments, in turn.
fn tools(mcp: &mut Command, calls: &[(&str, &str)]) -> Vec<String> {
    let requests: String = calls
        .iter()
        .enumerate()
        .map(|(id, (tool, arguments))| {
            format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"{tool}","arguments":{arguments}}}}}"#
            ) + "\n"
        })
        .collect();
    let mut mcp = mcp
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    mcp.stdin
        .take()
        .unwrap()
        .write_all(requests.as_bytes())
        .unwrap();
    let output = mcp.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
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
         UPDATE state SET readers = 0;
         UPDATE file SET readers = 0;",
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
    // Each as text, and as JSON too where its JSON has a shape no other's
    // has.
    for (name, args, as_json) in [
        ("status", vec!["status"], true),
        (
            "status-hours",
            vec!["status", "--account", "claude", "--hours", "3"],
            false,
        ),
        ("usage", vec!["usage"], true),
        (
            "usage-model",
            vec!["usage", "--since", "2026-09-29", "--by", "model"],
            false,
        ),
        (
            "usage-account",
            vec!["usage", "--since", "2026-09-29", "--by", "account"],
            false,
        ),
        // Every agent's session, each with its title.
        (
            "usage-session",
            vec!["usage", "--since", "2026-09-29", "--by", "session"],
            false,
        ),
        (
            "usage-limit",
            vec!["usage", "--account", "claude", "--limit", "week"],
            true,
        ),
        ("accounts", vec!["accounts"], true),
        ("connect", vec!["connect"], true),
        ("doctor", vec!["doctor"], false),
    ] {
        for json in [false, true].into_iter().filter(|json| !json || as_json) {
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
    assert_eq!(said(&["connect"]), "");
    assert_eq!(said(&["usage"]), reading);
    assert_eq!(said(&["usage"]), "");
    // As after an update that raised `ingest::READERS`.
    let db = Connection::open(data.path().join("turnscope.sqlite")).unwrap();
    db.execute_batch("UPDATE state SET readers = 0; UPDATE file SET readers = 0;")
        .unwrap();
    assert_eq!(said(&["status"]), reading);
    assert_eq!(said(&["status"]), "");
}

/// A command that waits for another Turnscope reading history says so, and
/// answers once that one is done.
#[test]
fn a_command_says_when_it_waits_for_another_turnscope_reading_history() {
    let data = tempfile::tempdir().unwrap();
    let by_session = ["usage", "--since", "7d", "--by", "session"];
    assert!(turnscope(data.path(), &by_session, None).status.success());
    // Held as the other Turnscope holds it, all the while it reads.
    let reading = std::fs::File::create(data.path().join("ingest.lock")).unwrap();
    reading.lock().unwrap();
    let mut waiting = command(data.path(), &by_session)
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
    // The working session, by the start of its id.
    assert!(String::from_utf8_lossy(&output.stdout).contains("7c1d2e3f "));
}

/// Codex pointed at its agent folder by `CODEX_HOME`, written with a slash
/// at its end: the account is still the caller's. The other ways to write
/// a folder are folded as this one is (`mcp::folded`).
#[test]
fn the_callers_account_is_its_agent_folders_however_its_written() {
    let data = store();
    // Read the history first: neither command below does.
    assert!(turnscope(data.path(), &["usage"], None).status.success());
    let folder = format!("{}/.codex/", home().display());
    let run = |args: &[&str]| {
        command_as("codex", data.path(), args)
            .env("CODEX_HOME", &folder)
            .output()
            .unwrap()
    };
    assert_eq!(
        String::from_utf8_lossy(&run(&["statusline"]).stdout).trim(),
        "ChatGPT Plus · weekly 80%"
    );
    // Its weekly limit has 80% left, so a guard under 90% blocks.
    let guard = run(&["guard", "--limit", "week", "--below", "90"]);
    assert_eq!(guard.status.code(), Some(2));
}

/// Claude Code names its session in the environment, so its row is marked.
/// Codex, run from Claude Code's shell, inherits that name, which isn't its
/// own: none of its rows is.
#[test]
fn explain_usage_marks_the_session_its_caller_names() {
    let data = store();
    // Over a week, which holds the session Claude Code names.
    let asked_by = |agent: &str| {
        let week = [("explain_usage", r#"{"limit":"week"}"#)];
        tools(&mut command_as(agent, data.path(), &["mcp"]), &week).remove(0)
    };
    let yours = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|line| line.contains(" (yours) |"))
            .map(str::to_owned)
            .collect()
    };
    let claude = asked_by("claude");
    assert!(claude.starts_with("Usage of Claude Max"), "{claude}");
    assert_eq!(
        yours(&claude),
        [
            "| `0f6e3f6a` \"Fix the CSV parser so quoted commas work. Don't change the public API.\" (yours) | $0.03 | 3 | 3% |"
        ],
        "{claude}"
    );
    let codex = asked_by("codex");
    assert!(codex.starts_with("Usage of ChatGPT Plus"), "{codex}");
    assert!(
        codex.contains("| `019a2b3c` ") && yours(&codex).is_empty(),
        "{codex}"
    );
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

/// A new session isn't read yet, as a status line never catches up: its
/// agent's own account shows, not that of whichever agent ran last in its
/// folder.
#[test]
fn a_status_line_for_a_session_not_read_yet_says_its_agents_account() {
    let data = store();
    // Read the history, as the app's daemon has.
    assert!(turnscope(data.path(), &["usage"], None).status.success());
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
    // An account named, whose limits can't be read.
    let named = turnscope(data.path(), &["statusline", "--account", "supergrok"], None);
    assert_eq!(
        String::from_utf8_lossy(&named.stdout).trim(),
        "SuperGrok · sign in again"
    );
}

#[test]
fn an_answer_tells_time_by_the_minute_its_status_is_as_of() {
    let data = store();
    // 40 seconds into the minute; Claude's 5 hours runs out at 13:00 and
    // resets at 14:00.
    let late = (NOW + 40_000).to_string();
    let texts = tools(
        command(data.path(), &["mcp"]).env("TURNSCOPE_NOW", &late),
        &[
            ("check_limits", r#"{"account":"claude"}"#),
            ("explain_usage", r#"{"account":"claude"}"#),
            ("explain_usage", r#"{"account":"claude","since":"1h"}"#),
            ("explain_usage", r#"{"account":"claude","since":"now"}"#),
            ("explain_usage", r#"{"all":true}"#),
            (
                "explain_usage",
                r#"{"account":"claude","since":"2026-10-01T12:00:10Z"}"#,
            ),
        ],
    );
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

    let answers = tools(
        &mut command(data.path(), &["mcp"]),
        &[
            ("explain_usage", r#"{"account":"claude","limit":"5 hours"}"#),
            (
                "explain_usage",
                r#"{"account":"claude","limit":"5 hours","since":"3h"}"#,
            ),
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
    let answers = tools(
        &mut command(data.path(), &["mcp"]),
        &[("explain_usage", r#"{"account":"chatgpt"}"#)],
    );
    assert!(
        answers[0].ends_with(
            "\nBy model: gpt-5.5 ($0.00 (+4 unpriced)).\nBy project: /work/app ($0.00 (+4 unpriced))."
        ),
        "{}",
        answers[0]
    );
}

#[test]
fn with_no_account_to_show_every_front_end_says_why() {
    let said = |data: &Path, args: &[&str]| {
        let output = turnscope(data, args, None);
        assert!(output.status.success(), "{args:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    let check_limits = |data: &Path, arguments: &str| {
        tools(&mut command(data, &["mcp"]), &[("check_limits", arguments)]).remove(0)
    };
    // A new data directory, off the network: no account is known yet.
    let data = tempfile::tempdir().unwrap();
    let none = "No account's limits have been read yet: no agent here is signed in to one Turnscope reads.";
    assert_eq!(said(data.path(), &["status"]), none);
    assert_eq!(said(data.path(), &["accounts"]), none);
    assert_eq!(check_limits(data.path(), "{}"), none);
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
    // One named shows, hidden or not.
    let named = said(data.path(), &["status", "--account", "supergrok"]);
    assert!(
        named.starts_with("SuperGrok (me@example.com) · via Grok Build · hidden"),
        "{named}"
    );
}

/// An MCP server left running while a newer Turnscope changes the schema,
/// as an agent's is through an update of the app. It reads nothing more
/// into the data, and answers from what it read, saying why.
#[test]
fn a_server_left_running_stops_catching_up_once_the_schema_is_newer() {
    let data = store();
    let mut mcp = command(data.path(), &["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = mcp.stdin.take().unwrap();
    let mut replies = BufReader::new(mcp.stdout.take().unwrap()).lines();
    // An answer's text, and whether it says the call failed.
    let mut usage = || {
        let request = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"explain_usage","arguments":{"all":true,"since":"2026-09-29"}}}"#;
        writeln!(stdin, "{request}").unwrap();
        let reply: serde_json::Value =
            serde_json::from_str(&replies.next().unwrap().unwrap()).unwrap();
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        (text.to_owned(), reply["result"]["isError"] == true)
    };
    // Its first answer, with the data open and all of history read.
    let (before, _) = usage();
    assert!(!before.contains("couldn't be read"), "{before}");

    // The schema is changed under it, and Codex's file is one not read yet.
    let db = Connection::open(data.path().join("turnscope.sqlite")).unwrap();
    db.execute_batch("PRAGMA user_version = 99; DELETE FROM file WHERE agent = 'codex';")
        .unwrap();
    let (after, failed) = usage();
    assert!(!failed);
    assert_eq!(
        after,
        format!(
            "{before}\n\nWhat's new couldn't be read first, so this is as of the last read: \
             a newer Turnscope has updated this data; update Turnscope"
        )
    );
    let read: i64 = db
        .query_row(
            "SELECT count(*) FROM file WHERE agent = 'codex'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(read, 0, "it read history into a schema it doesn't know");
    // A query that no longer fits fails, and says why first.
    db.execute("ALTER TABLE session RENAME COLUMN title TO name", [])
        .unwrap();
    let (said, failed) = usage();
    assert!(failed);
    assert!(
        said.starts_with(
            "What's new couldn't be read first: a newer Turnscope has updated this data; update Turnscope. Then this failed: "
        ),
        "{said}"
    );
    drop(stdin);
    assert!(mcp.wait().unwrap().success());
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
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"explain_usage","arguments":{"account":"nope"}}}"#,
        r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"check_limits","arguments":{"hours":"two"}}}"#,
        // A tool and a method it doesn't have.
        r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"nope","arguments":{}}}"#,
        r#"{"jsonrpc":"2.0","id":8,"method":"prompts/list"}"#,
        r#"{"jsonrpc":"2.0","id":9,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28"}}}"#,
        r#"{"jsonrpc":"2.0","id":10,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2024-01-01"}}}"#,
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
    assert!(turnscope(data.path(), &["usage"], None).status.success());
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
    let hello = ask(br#"{"jsonrpc":"2.0","id":3,"method":"hello","params":{"protocol":3}}"#);
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
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("contract");
    let example = |name: &str| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(examples.join(name)).unwrap()).unwrap()
    };
    let breakdown = ask(
        br#"{"jsonrpc":"2.0","id":4,"method":"limit.breakdown","params":{"account":"anthropic:acct-1:org-1","limit":"seven_day"}}"#,
    );
    let used = &breakdown["result"]["sessions"][0];
    assert_eq!(used["session"], WORKING);
    // Its project is its folder's name.
    assert_eq!(used["project"], "api");
    // A session that used it has the fields the contract's example has.
    let fields = |session: &serde_json::Value| -> BTreeSet<String> {
        session.as_object().unwrap().keys().cloned().collect()
    };
    let contract = example("limit.breakdown.result.json");
    assert_eq!(fields(used), fields(&contract["result"]["sessions"][0]));
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
    let mut requests: Vec<String> = std::fs::read_dir(&examples)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".request.json"))
        .filter(|name| !name.starts_with("agent.") && name != "shutdown.request.json")
        .collect();
    requests.sort();
    for name in requests {
        let answer = ask(example(&name).to_string().as_bytes());
        assert!(
            answer.get("result").is_some(),
            "{name} was refused: {answer}"
        );
    }
    assert_eq!(
        ask(example("shutdown.request.json").to_string().as_bytes())["result"],
        serde_json::json!({})
    );
    assert!(serve.wait().unwrap().success());
}
