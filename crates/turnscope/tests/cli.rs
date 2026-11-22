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
use serde_json::json;

/// 2026-10-01 12:00 UTC.
const NOW: i64 = 1_790_856_000_000;
const HOUR: i64 = 3_600_000;
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
    command_of(
        Path::new(env!("CARGO_BIN_EXE_turnscope")),
        agent,
        data,
        args,
    )
}

/// `command_as`, running the binary at `binary`.
fn command_of(binary: &Path, agent: &str, data: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .arg0(agent)
        .args(["-c", r#""$@"; exit $?"#, "sh"])
        .arg(binary)
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

struct Mcp {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    replies: std::io::Lines<BufReader<std::process::ChildStdout>>,
}

impl Mcp {
    fn spawn(cmd: &mut Command) -> Mcp {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let replies = BufReader::new(child.stdout.take().unwrap()).lines();
        Mcp {
            child,
            stdin,
            replies,
        }
    }

    fn usage(&mut self) -> (String, bool) {
        let request = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"explain_usage","arguments":{"all":true,"since":"2026-09-29"}}}"#;
        writeln!(self.stdin, "{request}").unwrap();
        let reply: serde_json::Value =
            serde_json::from_str(&self.replies.next().unwrap().unwrap()).unwrap();
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        (text.to_owned(), reply["result"]["isError"] == true)
    }

    fn close(mut self) {
        drop(self.stdin);
        assert!(self.child.wait().unwrap().success());
    }
}

/// A store with the fixture's history read, and four accounts:
/// - Claude Max, Claude Code's: its five hours running out within the hour
///   at the pace of the last three, its week lasting;
/// - ChatGPT Plus, Codex's: room, not used since its last read;
/// - the OpenRouter API key, whose key Pi holds: credits, and the key's own
///   weekly limit;
/// - SuperGrok, Grok Build's: its login refused at two reads in a row, never
///   read.
fn store() -> tempfile::TempDir {
    let data = tempfile::tempdir().unwrap();
    let db = turnscope::db::open(data.path()).unwrap();
    db.execute(
        "INSERT INTO account (id, provider, kind, title, label, read_at, problem) VALUES
             ('anthropic:acct-1:org-1', 'anthropic', 'subscription', 'Claude Max', 'me@example.com', ?1, NULL),
             ('openai:acct-2', 'openai', 'subscription', 'ChatGPT Plus', 'me@example.com', ?1, NULL),
             ('openrouter:api', 'openrouter', 'apiKey', 'OpenRouter API key', NULL, ?1, NULL),
             ('xai:user-3', 'xai', 'subscription', 'SuperGrok', 'me@example.com', NULL, 'signIn')",
        [NOW - HOUR / 6],
    )
    .unwrap();
    db.execute(
        "INSERT INTO failing (account, reads) VALUES ('xai:user-3', 2)",
        [],
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
    db.execute(
        "INSERT INTO price (provider, model, since, cost)
         VALUES ('anthropic', 'claude-opus-5', 0, '{\"input\":5,\"output\":25,\"cache_read\":0.5,\"cache_write\":6.25}')",
        [],
    )
    .unwrap();
    drop(db);
    assert!(turnscope(data.path(), &["accounts"], None).status.success());
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

/// The lines of `doc`'s examples in blocks fenced as one of `fences`, each
/// without its fence's indent.
fn examples<'a>(doc: &'a str, fences: &[&str]) -> Vec<&'a str> {
    // Outside a block, or inside one, with its indent if it's an example.
    let mut inside: Option<Option<usize>> = None;
    let mut lines = Vec::new();
    for line in doc.lines() {
        let fence = line.trim_start();
        if fence.starts_with("```") {
            inside = match inside {
                Some(_) => None,
                None => Some(fences.contains(&fence).then_some(line.len() - fence.len())),
            };
        } else if let Some(Some(indent)) = inside {
            lines.push(line.get(indent..).unwrap_or(line));
        }
    }
    lines
}

/// The examples in docs/cli.md and docs/mcp.md are what the binary says:
/// each line of their output is a line of some snapshot, and an MCP
/// message shown is one the server sent.
#[test]
fn the_docs_show_what_the_snapshots_hold() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let read = |path: &str| std::fs::read_to_string(root.join(path)).unwrap();
    let said = |line: &&str| !line.is_empty() && *line != "…" && !line.starts_with("$ ");

    let snapshots: Vec<String> = std::fs::read_dir(root.join("tests/snapshots"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "txt"))
        .map(|path| std::fs::read_to_string(path).unwrap())
        .collect();
    let shown: HashSet<&str> = snapshots.iter().flat_map(|text| text.lines()).collect();
    let cli = read("../../docs/cli.md");
    let unseen: Vec<&str> = examples(&cli, &["```console"])
        .into_iter()
        .filter(|line| said(line) && !shown.contains(line))
        .collect();
    assert!(
        unseen.is_empty(),
        "lines of docs/cli.md's examples that no snapshot has: {unseen:#?}"
    );

    let replies: Vec<serde_json::Value> =
        serde_json::from_str(&read("tests/snapshots/mcp.json")).unwrap();
    let answers: Vec<&str> = replies
        .iter()
        .filter_map(|reply| reply["result"]["content"][0]["text"].as_str())
        .collect();
    let answered: HashSet<&str> = answers.iter().flat_map(|text| text.lines()).collect();
    let mcp = read("../../docs/mcp.md");
    // An answer's text, or the text of one that failed.
    let mut unseen: Vec<&str> = examples(&mcp, &["```markdown", "```"])
        .into_iter()
        .filter(|line| said(line) && !answered.contains(line))
        .collect();
    // A protocol error, as the server sent it.
    unseen.extend(examples(&mcp, &["```json"]).into_iter().filter(|line| {
        let message: serde_json::Value = serde_json::from_str(line).unwrap();
        !replies.contains(&message)
    }));
    assert!(
        unseen.is_empty(),
        "lines of docs/mcp.md's examples that tests/snapshots/mcp.json doesn't have: {unseen:#?}"
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
        // With no --until, until now.
        (
            vec!["usage", "--since", "now"],
            2,
            "--since is now or later",
        ),
        (vec!["usage", "--since", "last tuesday"], 2, "24h, 7d or 4w"),
        (
            vec!["usage", "--since", "99999999999999d"],
            2,
            "24h, 7d or 4w",
        ),
        // A value of "guard" isn't `guard`, whose mistakes exit 1.
        (
            vec!["status", "--account", "guard", "--hours", "x"],
            2,
            "invalid value 'x'",
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
    // Every account's usage has no limit, whose window would run to now.
    let answers = tools(
        &mut command(data.path(), &["mcp"]),
        &[("explain_usage", r#"{"all":true,"until":"yesterday"}"#)],
    );
    assert_eq!(
        answers,
        ["until is with since: without since, the usage runs to now"]
    );
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
    let db = Connection::open(data.path().join("turnscope.db")).unwrap();
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

/// A status line and a guard only read: with no database, as before the
/// app first runs, the status line prints nothing, the guard lets anything
/// through, and neither makes one.
#[test]
fn a_status_line_or_a_guard_never_makes_the_database() {
    let data = tempfile::tempdir().unwrap();
    let output = turnscope(data.path(), &["statusline"], None);
    assert!(output.status.success() && output.stdout.is_empty());
    let guard = ["guard", "--limit", "week", "--below", "100"];
    assert_eq!(turnscope(data.path(), &guard, None).status.code(), Some(0));
    assert!(!data.path().join("turnscope.db").exists());
}

/// `connect` and `disconnect` change an agent's settings, not Turnscope's
/// data: they never make the database, and one a newer Turnscope changed
/// doesn't stop them.
#[test]
fn connect_and_disconnect_leave_the_database_alone() {
    let data = tempfile::tempdir().unwrap();
    assert!(turnscope(data.path(), &["connect"], None).status.success());
    assert!(!data.path().join("turnscope.db").exists());
    Connection::open(data.path().join("turnscope.db"))
        .unwrap()
        .execute_batch("PRAGMA user_version = 99")
        .unwrap();
    assert!(turnscope(data.path(), &["connect"], None).status.success());
    // OpenCode has no command to remove a server with, so none is run.
    let output = turnscope(data.path(), &["disconnect", "opencode"], None);
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim_end(),
        "turnscope: OpenCode has no command Turnscope can run to remove a server: remove `turnscope` from its MCP settings by hand."
    );
}

/// Claude Code names its session in the environment, so its row is marked.
/// Codex, run from Claude Code's shell, inherits that name, which isn't its
/// own: none of its rows is.
#[test]
fn explain_usage_marks_the_session_its_caller_names() {
    let data = store();
    // Over a week, which holds the session Claude Code names.
    let asked = |agent: &str, arguments: &str| {
        let week = [("explain_usage", arguments)];
        tools(&mut command_as(agent, data.path(), &["mcp"]), &week).remove(0)
    };
    let asked_by = |agent: &str| asked(agent, r#"{"limit":"week"}"#);
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
    // Not among the top rows, it has a line of its own.
    let top = asked("claude", r#"{"limit":"week","top":1}"#);
    assert!(
        yours(&top).is_empty()
            && top.contains("\nYour session: $0.03, over 3 responses, 3% of Weekly.\n"),
        "{top}"
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
        (&texts[3], "since is at or after until, or now"),
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
    let db = Connection::open(data.path().join("turnscope.db")).unwrap();
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
            "As of 11:25 (35m ago): it couldn't be read since; it's tried again every 5 to 30 minutes."
        ),
        "{text}"
    );
    let output = turnscope(data.path(), &["doctor"], None);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("Claude Max (me@example.com): it couldn't be read since 11:25; it's tried again every 5 to 30 minutes."),
        "{text}"
    );
}

#[test]
fn what_used_a_limit_whose_window_reset_since_its_read_counts_from_the_reset() {
    let data = store();
    // Claude's last read at 10:20, of a 5 hours from 06:00 that reset at
    // 11:00. The working session ran from 10:00 until now, noon.
    let db = Connection::open(data.path().join("turnscope.db")).unwrap();
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
            "\nBy model: gpt-5.5: $0.00 (+4 unpriced).\nBy project: /work/app: $0.00 (+4 unpriced)."
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
    // Nor can one be named.
    let hide = turnscope(data.path(), &["accounts", "hide", "claude"], None);
    assert_eq!(hide.status.code(), Some(3));
    assert_eq!(
        String::from_utf8_lossy(&hide.stderr).trim_end(),
        format!("turnscope: {none}")
    );
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

/// A tool call that finds all of history being read, as after an update,
/// says its usage is only part of it, over "as of the last read". Only when
/// it waited for that read: any other reason is said as before.
#[test]
fn a_tool_call_says_its_usage_is_part_of_it_while_all_of_history_is_read() {
    let data = store();
    // As when `ingest::READERS` was raised, with another Turnscope reading.
    let db = Connection::open(data.path().join("turnscope.db")).unwrap();
    db.execute("UPDATE state SET readers = 0", []).unwrap();
    let reading = std::fs::File::create(data.path().join("ingest.lock")).unwrap();
    reading.lock().unwrap();
    let mut mcp = Mcp::spawn(&mut command(data.path(), &["mcp"]));
    let (text, _) = mcp.usage();
    assert!(
        text.ends_with(
            "\n\nYour agents' history is still being read, so the usage here is only part of \
             it: ask again in a few minutes."
        ),
        "{text}"
    );
    db.execute("UPDATE state SET version = 99000000", [])
        .unwrap();
    let (text, _) = mcp.usage();
    assert!(
        text.ends_with(
            "\n\nWhat's new couldn't be read first, so this is as of the last read: a newer \
             Turnscope has updated this data; update Turnscope"
        ),
        "{text}"
    );
    mcp.close();
}

/// An MCP server left running while a newer Turnscope changes the schema,
/// as an agent's is through an update of the app. It reads nothing more
/// into the data, and answers from what it read, saying why.
#[test]
fn a_server_left_running_stops_catching_up_once_the_schema_is_newer() {
    let data = store();
    let mut mcp = Mcp::spawn(&mut command(data.path(), &["mcp"]));
    // Its first answer, with the data open and all of history read.
    let (before, _) = mcp.usage();
    assert!(!before.contains("couldn't be read"), "{before}");

    // The schema is changed under it, and Codex's file is one not read yet.
    let db = Connection::open(data.path().join("turnscope.db")).unwrap();
    db.execute_batch("PRAGMA user_version = 99; DELETE FROM file WHERE agent = 'codex';")
        .unwrap();
    let (after, failed) = mcp.usage();
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
    let (said, failed) = mcp.usage();
    assert!(failed);
    assert!(
        said.starts_with(
            "What's new couldn't be read first: a newer Turnscope has updated this data; update Turnscope. Then this failed: "
        ),
        "{said}"
    );
    mcp.close();
}

#[test]
fn a_server_left_running_stops_writing_once_a_newer_turnscope_has() {
    let data = store();
    // Its own copy of the binary, which an update can replace.
    let folder = tempfile::tempdir().unwrap();
    let binary = folder.path().join("turnscope");
    std::fs::copy(env!("CARGO_BIN_EXE_turnscope"), &binary).unwrap();
    let mut mcp = Mcp::spawn(&mut command_of(&binary, "claude", data.path(), &["mcp"]));
    let (before, _) = mcp.usage();
    // A newer version writes, and Codex's file is one not read yet.
    let db = Connection::open(data.path().join("turnscope.db")).unwrap();
    db.execute_batch(
        "UPDATE state SET version = 99000000; DELETE FROM file WHERE agent = 'codex';",
    )
    .unwrap();
    assert_eq!(
        mcp.usage().0,
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
    assert_eq!(read, 0, "it wrote after a newer Turnscope had");
    // Once an update has replaced its binary, it says to start it again:
    // the one who updated has nothing more to update.
    std::fs::remove_file(&binary).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_turnscope"), &binary).unwrap();
    assert_eq!(
        mcp.usage().0,
        format!(
            "{before}\n\nWhat's new couldn't be read first, so this is as of the last read: \
             Turnscope was updated after this one started; start it again"
        )
    );
    mcp.close();
}

#[test]
fn an_account_isnt_hidden_by_an_older_turnscope() {
    let data = store();
    let db = Connection::open(data.path().join("turnscope.db")).unwrap();
    db.execute("UPDATE state SET version = 99000000", [])
        .unwrap();
    let hide = turnscope(data.path(), &["accounts", "hide", "claude"], None);
    assert!(!hide.status.success());
    assert_eq!(
        String::from_utf8_lossy(&hide.stderr).trim_end(),
        "turnscope: a newer Turnscope has updated this data; update Turnscope"
    );
    let hidden: i64 = db
        .query_row("SELECT count(*) FROM account WHERE hidden", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(hidden, 0);
}

/// A message the server can't take gets an error, and the server goes on:
/// a line that isn't UTF-8 once ended it.
#[test]
fn an_mcp_server_answers_a_malformed_message_and_goes_on() {
    let data = store();
    let mut child = command(data.path(), &["mcp"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"\xff\xfe\n").unwrap();
    for line in [
        r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
        r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"check_limits","arguments":null}}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
    ] {
        stdin.write_all(format!("{line}\n").as_bytes()).unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    let replies: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let codes: Vec<_> = replies
        .iter()
        .map(|reply| (reply["id"].clone(), reply["error"]["code"].clone()))
        .collect();
    assert_eq!(
        codes,
        [
            (json!(null), json!(-32700)),
            (json!(null), json!(-32600)),
            (json!(null), json!(-32600)),
            (json!(2), json!(null)),
            (json!(3), json!(null)),
        ]
    );
    // Arguments of null are none.
    assert_eq!(replies[3]["result"]["isError"], json!(null));
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

/// While an app keeps the data current, a hook's guard and an MCP server's
/// call leave limits and prices to it.
#[test]
fn guard_and_mcp_leave_limits_to_the_app() {
    let data = store();
    let app = std::fs::File::create(data.path().join("app.lock")).unwrap();
    app.lock().unwrap();
    let db = Connection::open(data.path().join("turnscope.db")).unwrap();
    db.execute("UPDATE state SET limits_at = NULL, prices_at = NULL", [])
        .unwrap();
    // Online, with every request sent to a port that takes none, so a read
    // that should have been left reaches no provider.
    let online = |mut command: Command| {
        command
            .env("TURNSCOPE_OFFLINE", "0")
            .env("ALL_PROXY", "http://127.0.0.1:9")
            .env("HTTPS_PROXY", "http://127.0.0.1:9");
        command
    };
    let guard = online(command(
        data.path(),
        &[
            "guard",
            "--account",
            "claude",
            "--limit",
            "5 hours",
            "--below",
            "20",
        ],
    ))
    .output()
    .unwrap();
    assert_eq!(guard.status.code(), Some(0));
    let answers = tools(
        &mut online(command(data.path(), &["mcp"])),
        &[("check_limits", r#"{"account":"claude"}"#)],
    );
    assert!(answers[0].contains("Claude Max"), "{}", answers[0]);
    let claimed: (Option<i64>, Option<i64>) = db
        .query_row("SELECT limits_at, prices_at FROM state", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(claimed, (None, None));
}
