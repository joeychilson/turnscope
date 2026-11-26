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

fn turnscope(data: &Path, args: &[&str]) -> Output {
    command(data, args).output().unwrap()
}

fn fed(command: &mut Command, input: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn replies(output: &Output) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
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

/// What the MCP server `command` runs answers each of `calls`, a tool and
/// its arguments, in turn.
fn tools(command: &mut Command, calls: &[(&str, &str)]) -> Vec<String> {
    let mut mcp = Mcp::spawn(command);
    let answers = calls
        .iter()
        .map(|(tool, arguments)| mcp.call(tool, arguments).0)
        .collect();
    mcp.close();
    answers
}

struct Mcp {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    replies: std::io::Lines<BufReader<std::process::ChildStdout>>,
}

impl Mcp {
    fn spawn(command: &mut Command) -> Mcp {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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

    /// What `tool` answers `arguments`, and whether that's an error.
    fn call(&mut self, tool: &str, arguments: &str) -> (String, bool) {
        writeln!(
            self.stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"{tool}","arguments":{arguments}}}}}"#
        )
        .unwrap();
        let reply: serde_json::Value =
            serde_json::from_str(&self.replies.next().unwrap().unwrap()).unwrap();
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        (text.to_owned(), reply["result"]["isError"] == true)
    }

    /// Every account's usage since 2026-09-29.
    fn usage(&mut self) -> (String, bool) {
        self.call("explain_usage", r#"{"all":true,"since":"2026-09-29"}"#)
    }

    fn close(self) {
        drop(self.stdin);
        let output = self.child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn count(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |row| row.get(0)).unwrap()
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
    assert!(turnscope(data.path(), &["accounts"]).status.success());
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
            true,
        ),
        // A line for each account.
        ("status-hours-every", vec!["status", "--hours", "3"], false),
        ("usage", vec!["usage"], true),
        (
            "usage-model",
            vec!["usage", "--since", "2026-09-29", "--by", "model"],
            false,
        ),
        // The rest added up.
        (
            "usage-account",
            vec![
                "usage",
                "--since",
                "2026-09-29",
                "--by",
                "account",
                "--top",
                "3",
            ],
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
        // Each day with its share.
        (
            "usage-limit-day",
            vec![
                "usage",
                "--account",
                "claude",
                "--limit",
                "week",
                "--by",
                "day",
            ],
            false,
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
            let output = turnscope(data.path(), &args);
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
/// each line of their output is a line of some snapshot or MCP answer.
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
    let unseen: Vec<&str> = examples(&mcp, &["```markdown", "```"])
        .into_iter()
        .filter(|line| said(line) && !answered.contains(line))
        .collect();
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
            "--since is now or later, so there's no usage after it: give an earlier --since",
        ),
        (
            vec!["usage", "--since", "2d", "--until", "3d"],
            2,
            "--since is at or after --until, so nothing is between them: give an earlier --since",
        ),
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
        (
            vec!["status", "--hours=-1"],
            2,
            "give a number of hours, 0 or more",
        ),
    ] {
        let output = turnscope(data.path(), &args);
        assert_eq!(output.status.code(), Some(code), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(says),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // Every account's usage has no limit, whose window would run to now;
    // the rest are said by the tool's input schema.
    let answers = tools(
        &mut command(data.path(), &["mcp"]),
        &[
            ("explain_usage", r#"{"all":true,"until":"yesterday"}"#),
            ("check_limits", r#"{"nope":null}"#),
            ("check_limits", r#"{"all":"yes"}"#),
            ("explain_usage", r#"{"account":3}"#),
            ("explain_usage", r#"{"by":"hourly"}"#),
            ("explain_usage", r#"{"top":2.5}"#),
            ("explain_usage", r#"{"top":21}"#),
            ("check_limits", r#"{"hours":-1}"#),
        ],
    );
    assert_eq!(
        answers,
        [
            "until is with since: without since, the usage runs to now",
            "check_limits takes account, all or hours, not nope",
            "all is true or false, not \"yes\"",
            "account is text, not 3",
            "by is session, project, model, agent, account, day, week or month, not \"hourly\"",
            "top is a whole number from 1 to 20, not 2.5",
            "top is a whole number from 1 to 20, not 21",
            "hours is a number of 0 or more, not -1",
        ]
    );
    // A null is an input not given, and so is empty text.
    let answers = tools(
        &mut command(data.path(), &["mcp"]),
        &[
            ("check_limits", r#"{"all":null}"#),
            ("check_limits", r#"{"account":""}"#),
            ("check_limits", "{}"),
        ],
    );
    assert_eq!(answers[0], answers[2]);
    assert_eq!(answers[1], answers[2]);
}

/// A command says so before it reads all of history: the first time, and
/// again once a reader changed. Otherwise it says nothing.
#[test]
fn a_command_says_when_it_reads_all_of_history() {
    let data = tempfile::tempdir().unwrap();
    let said = |args: &[&str]| {
        let output = turnscope(data.path(), args);
        assert!(output.status.success(), "{args:?}");
        String::from_utf8_lossy_owned(output.stderr)
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
    assert!(turnscope(data.path(), &by_session).status.success());
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
    let output = turnscope(data.path(), &["statusline"]);
    assert!(output.status.success() && output.stdout.is_empty());
    let guard = ["guard", "--limit", "week", "--below", "100"];
    assert_eq!(turnscope(data.path(), &guard).status.code(), Some(0));
    assert!(!data.path().join("turnscope.db").exists());
}

/// `connect` and `disconnect` change an agent's settings, not Turnscope's
/// data: they never make the database, and one a newer Turnscope changed
/// doesn't stop them.
#[test]
fn connect_and_disconnect_leave_the_database_alone() {
    let data = tempfile::tempdir().unwrap();
    assert!(turnscope(data.path(), &["connect"]).status.success());
    assert!(!data.path().join("turnscope.db").exists());
    Connection::open(data.path().join("turnscope.db"))
        .unwrap()
        .execute_batch("PRAGMA user_version = 99")
        .unwrap();
    assert!(turnscope(data.path(), &["connect"]).status.success());
    // OpenCode has no command to remove a server with, so none is run.
    let output = turnscope(data.path(), &["disconnect", "opencode"]);
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim_end(),
        "turnscope: OpenCode has no command Turnscope can run to remove a server: remove `turnscope` from its MCP settings by hand."
    );
}

/// Where an agent's command can't be run, as when it isn't installed, the
/// command to run by hand is said: here Claude Code's, its server in a
/// made-up home, with no login shell to find its command on the `PATH`.
#[test]
fn disconnect_says_how_to_remove_the_server_by_hand() {
    let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let settings = json!({"mcpServers": {"turnscope": {"command": "/made-up/turnscope"}}});
    std::fs::write(home.path().join(".claude.json"), settings.to_string()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_turnscope"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/usr/bin/false")
        .env("TURNSCOPE_OFFLINE", "1")
        .arg("--data")
        .arg(data.path())
        .arg("--home")
        .arg(home.path())
        .args(["disconnect", "claude-code"])
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&output.stderr);
    let by_hand = format!(
        "Is Claude Code installed? To remove the server by hand, run: HOME='{}' claude mcp remove --scope user turnscope",
        home.path().display()
    );
    assert!(said.trim_end().ends_with(&by_hand), "{said}");
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
    // A third session of Claude Max's, so one row can be added up with
    // another past the top.
    Connection::open(data.path().join("turnscope.db"))
        .unwrap()
        .execute_batch(
            "INSERT INTO response SELECT agent, 'r-third', 'claude-code:third', copy, file,
                 folder, at, provider, model, input, cache_read, cache_write_5m, cache_write_1h,
                 output, reasoning, prompt, web_searches, priority, recorded, 0.001, account
             FROM response WHERE agent = 'claude-code' LIMIT 1",
        )
        .unwrap();
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
    let guard = |account: &str, limit: &str, below: &str| {
        let args = [
            "guard",
            "--account",
            account,
            "--limit",
            limit,
            "--below",
            below,
        ];
        turnscope(data.path(), &args)
    };
    let blocked = guard("claude", "5 hours", "50");
    assert_eq!(blocked.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&blocked.stderr).trim(),
        "Claude Max's 5-hour limit has 25% left, under 50%. It resets 14:00 (in 2h); at this pace it runs out 13:00 (in 1h)."
    );
    assert_eq!(guard("claude", "5 hours", "20").status.code(), Some(0));
    // Named as the status line shows it, `5h 25%`.
    assert_eq!(guard("claude", "5h", "50").status.code(), Some(2));
    // One it can't follow blocks nothing.
    assert_eq!(guard("claude", "month", "50").status.code(), Some(1));
    // Nor does one mistyped.
    assert_eq!(guard("claude", "5 hours", "150").status.code(), Some(1));
    // One of an account whose limits aren't read passes, saying nothing, as
    // does one before any account is read.
    let quiet = |output: Output| (output.status.code(), output.stderr.is_empty());
    assert_eq!(quiet(guard("supergrok", "week", "50")), (Some(0), true));
    let none = tempfile::tempdir().unwrap();
    drop(turnscope::db::open(none.path()).unwrap());
    let args = [
        "guard",
        "--account",
        "claude",
        "--limit",
        "week",
        "--below",
        "50",
    ];
    assert_eq!(quiet(turnscope(none.path(), &args)), (Some(0), true));
}

/// A new session isn't read yet, as a status line never catches up: its
/// agent's own account shows, not that of whichever agent ran last in its
/// folder.
#[test]
fn a_status_line_for_a_session_not_read_yet_says_its_agents_account() {
    let data = store();
    let new = "9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d";
    let line = |agent: &str, session: &str, folder: &str| {
        let given = format!(r#"{{"session_id": "{session}", "cwd": "{folder}"}}"#);
        let output = fed(
            command_as(agent, data.path(), &["statusline"]).env("CLAUDE_CODE_SESSION_ID", new),
            given.as_bytes(),
        );
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
    let named = turnscope(data.path(), &["statusline", "--account", "supergrok"]);
    assert_eq!(
        String::from_utf8_lossy(&named.stdout).trim(),
        "SuperGrok · sign in again"
    );
    // Claude Code's own session, read, says its account, not that of its
    // agent folder's latest response.
    Connection::open(data.path().join("turnscope.db"))
        .unwrap()
        .execute_batch(
            "INSERT INTO account (id, provider, kind, title)
             VALUES ('anthropic:acct-4:org-4', 'anthropic', 'subscription', 'Claude Pro');
             UPDATE response SET account = 'anthropic:acct-4:org-4'
             WHERE session = 'claude-code:7c1d2e3f-4a5b-4c6d-8e9f-0a1b2c3d4e5f';",
        )
        .unwrap();
    let own = turnscope(data.path(), &["statusline"]);
    assert_eq!(String::from_utf8_lossy(&own.stdout).trim(), claude);
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
            (
                "explain_usage",
                r#"{"account":"claude","limit":"5 hours","since":"1h"}"#,
            ),
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
        (
            &texts[3],
            "since is now or later, so there's no usage after it: give an earlier since",
        ),
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

/// `check_limits` of every account leads with the one with the most room:
/// for OpenCode, signed in to no account, after saying so; given hours, one
/// they fit.
#[test]
fn check_limits_of_every_account_leads_with_the_one_with_most_room() {
    let data = store();
    let answer = tools(
        &mut command_as("opencode", data.path(), &["mcp"]),
        &[("check_limits", "{}")],
    );
    assert!(
        answer[0].starts_with(
            "Which account is yours isn't known, so here is every one.\nMost room: ChatGPT Plus (me@example.com), via Codex: Weekly has 80% left"
        ),
        "{}",
        answer[0]
    );
    // Claude Max by its Weekly alone, with 19h of work left: 5 fit it, and
    // can't be told of ChatGPT Plus, with more left.
    turnscope::db::open(data.path())
        .unwrap()
        .execute(
            "DELETE FROM reading WHERE account = 'anthropic:acct-1:org-1' AND limit_key = 'five_hour'",
            [],
        )
        .unwrap();
    let answer = tools(
        &mut command(data.path(), &["mcp"]),
        &[("check_limits", r#"{"all":true,"hours":5}"#)],
    );
    assert!(
        answer[0]
            .contains("\n\nMost room: Claude Max (me@example.com), yours: Weekly has 56% left"),
        "{}",
        answer[0]
    );
}

/// Claude's 5 hours runs out within the hour, then is used up:
/// `check_limits` for it alone, named or as the caller's own, names an
/// account with room. With every account shown, it doesn't, as `mcp.json`'s
/// snapshot holds.
#[test]
fn check_limits_names_an_account_with_room_when_one_runs_out_or_is_used_up() {
    let data = store();
    let room = "\n\nAnother account has room: ChatGPT Plus (me@example.com) has 80% left (Weekly, via Codex). Call check_limits with all: true to compare.";
    let answer = tools(
        &mut command(data.path(), &["mcp"]),
        &[("check_limits", r#"{"account":"claude"}"#)],
    );
    assert!(answer[0].ends_with(room), "{}", answer[0]);
    turnscope::db::open(data.path())
        .unwrap()
        .execute(
            "INSERT INTO reading (account, limit_key, at, name, used, starts, resets)
             VALUES ('anthropic:acct-1:org-1', 'five_hour', ?1, '5 hours', 100, ?1 - 3 * ?2, ?1 + 2 * ?2)",
            [NOW, HOUR],
        )
        .unwrap();
    let answer = tools(
        &mut command(data.path(), &["mcp"]),
        &[("check_limits", "{}")],
    );
    assert!(answer[0].contains("5 hours is used up"), "{}", answer[0]);
    assert!(answer[0].ends_with(room), "{}", answer[0]);
}

/// `usage` tells its times by the minute they're in, and an `--until` past
/// now counts to now, as `explain_usage`'s does.
#[test]
fn usage_runs_to_now_at_the_latest() {
    let data = store();
    let output = turnscope(
        data.path(),
        &[
            "usage",
            "--agent",
            "grok-build",
            "--since",
            "2026-09-28T23:59:40Z",
            "--until",
            "2030-01-01",
        ],
    );
    let said = String::from_utf8_lossy(&output.stdout);
    assert!(
        said.starts_with("Mon Sep 28 23:59 to Thu Oct 1 12:00: 1 response, $0.02\n"),
        "{said}"
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
    let output = turnscope(data.path(), &["status", "--account", "claude"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(
            "As of 11:25 (35m ago): it couldn't be read since; it's tried again every 5 to 30 minutes."
        ),
        "{text}"
    );
    let output = turnscope(data.path(), &["doctor"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("Claude Max (me@example.com): it couldn't be read since 11:25; it's tried again every 5 to 30 minutes."),
        "{text}"
    );
    // Claude Code has worked on it since, so whether more work fits isn't
    // known; with no work since, it's said as of the read.
    let fit = || {
        let output = turnscope(
            data.path(),
            &["status", "--account", "claude", "--hours", "0.4"],
        );
        let text = String::from_utf8_lossy(&output.stdout);
        text.lines().next().unwrap().to_owned()
    };
    assert_eq!(
        fit(),
        "Claude Max (me@example.com): whether 0.4 more hours of work fit can't be told: 5 hours had 25% left at 11:25, and your agents have worked about 40m since."
    );
    // Its 5 hours reset since the read, at 11:55: the read is still of
    // Weekly.
    db.execute(
        "UPDATE reading SET starts = starts - ?1, resets = resets - ?1
         WHERE account = 'anthropic:acct-1:org-1' AND limit_key = 'five_hour'",
        [2 * HOUR + 5 * HOUR / 60],
    )
    .unwrap();
    assert_eq!(
        fit(),
        "Claude Max (me@example.com): whether 0.4 more hours of work fit can't be told: Weekly had 56% left at 11:25, and your agents have worked about 40m since."
    );
    // And Weekly too, at 11:50: no limit of the read is left.
    db.execute(
        "UPDATE reading SET resets = ?1 WHERE account = 'anthropic:acct-1:org-1' AND limit_key = 'seven_day'",
        [NOW - 10 * HOUR / 60],
    )
    .unwrap();
    assert_eq!(
        fit(),
        "Claude Max (me@example.com): whether 0.4 more hours of work fit can't be told: its limits were last read at 11:25, and your agents have worked about 40m since."
    );
    db.execute(
        "DELETE FROM response WHERE account = 'anthropic:acct-1:org-1' AND at > ?1",
        [NOW - 35 * HOUR / 60],
    )
    .unwrap();
    let said = fit();
    assert!(
        said.ends_with(
            "That's as of 11:25: it couldn't be read since; it's tried again every 5 to 30 minutes."
        ),
        "{said}"
    );
}

/// `check_limits` of one account says whether the hours fit and how it
/// stands in one line, the verdict's range and reset in it, before the
/// account's lines.
#[test]
fn check_limits_of_one_account_says_its_fit_and_verdict_in_one_line() {
    let data = store();
    let answers = tools(
        &mut command(data.path(), &["mcp"]),
        &[
            ("check_limits", r#"{"account":"claude","hours":2}"#),
            ("check_limits", r#"{"account":"supergrok","hours":2}"#),
        ],
    );
    assert!(
        answers[0].starts_with(
            "Claude Max (me@example.com): 2 more hours of work don't fit: 5 hours runs out after about 40m of work (20m–1h 40m); it resets 14:00 (in 2h).\n\nClaude Max (me@example.com) · via"
        ),
        "{}",
        answers[0]
    );
    assert!(
        answers[1].starts_with(
            "SuperGrok (me@example.com): whether 2 more hours of work fit can't be told: its login was refused.\n\nSuperGrok (me@example.com) · via"
        ),
        "{}",
        answers[1]
    );
}

/// Pi's OpenRouter key: $9.00 of its $20.00 weekly limit used, by $0.00 of
/// Pi's usage here.
#[test]
fn a_limit_in_dollars_is_shared_by_dollars() {
    let data = store();
    let elsewhere = "Your agents here account for $0.00 of the $9.00 used; the rest was spent elsewhere with this key.";
    let answer = tools(
        &mut command(data.path(), &["mcp"]),
        &[("explain_usage", r#"{"account":"openrouter","limit":"key"}"#)],
    );
    assert!(
        answer[0].contains(" | $0.00 | 2 | <1% |\n") && answer[0].contains(elsewhere),
        "{}",
        answer[0]
    );
    // Named by the agent that holds the key, too.
    let output = turnscope(
        data.path(),
        &["usage", "--account", "openrouter", "--limit", "pi"],
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.trim_end().ends_with(elsewhere), "{text}");
}

/// Credits never reset, so their usage runs from the month's start, and
/// what of them is used is said beside it: nothing used this month.
#[test]
fn usage_of_credits_says_which_it_is_and_what_of_them_is_used() {
    let data = store();
    let said = "Credits don't reset, so this is this month's usage, since 00:00 (12h ago): nothing yet. $15.00 of them are used; give since, such as 30d, to see what used them.";
    let answer = tools(
        &mut command_as("pi", data.path(), &["mcp"]),
        &[("explain_usage", "{}")],
    );
    assert_eq!(answer[0], format!("OpenRouter API key: {said}"));
    let output = turnscope(
        data.path(),
        &["usage", "--account", "openrouter", "--limit", "credits"],
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.starts_with(&format!(
            "OpenRouter API key · {}\n",
            said.replace("give since", "give --since")
        )),
        "{text}"
    );
}

/// Usage since a time, as yesterday's, is the account's, without the limit
/// whose window is now's.
#[test]
fn explain_usage_since_a_time_leaves_out_the_limit_of_now() {
    let data = store();
    let answer = tools(
        &mut command(data.path(), &["mcp"]),
        &[("explain_usage", r#"{"since":"yesterday","until":"today"}"#)],
    );
    assert!(
        answer[0].starts_with(
            "Usage of Claude Max (me@example.com) since Wed 00:00 (1d 12h ago) to 00:00 (12h ago): $0.03 at list prices over 3 responses.\n\n| Session |"
        ) && !answer[0].contains("Shares of"),
        "{}",
        answer[0]
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
    let output = turnscope(data.path(), &limit);
    let text = String::from_utf8_lossy(&output.stdout);
    // What it used before the reset is no part of the window it's in, and
    // how much of that one is used isn't known: no shares.
    assert!(
        text.contains("5 hours: 100% left, reset 11:00 (1h ago).")
            && text.contains("Thu Oct 1 11:00 to Thu Oct 1 12:00: 12 responses, $0.23\n")
            && text.contains(" Responses   Cost\n"),
        "{text}"
    );
    let output = turnscope(data.path(), &[&limit[..], &["--json"]].concat());
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
        let output = turnscope(data, args);
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
    let hide = turnscope(data.path(), &["accounts", "hide", "claude"]);
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
    // An agent with no account of its own, as OpenCode here, isn't given a
    // hidden one.
    let opencode = |args: &[&str]| command_as("opencode", data.path(), args).output().unwrap();
    assert!(opencode(&["statusline"]).stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&opencode(&["usage", "--limit", "week"]).stderr).trim_end(),
        "turnscope: no account has a limit named week"
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
    assert_eq!(
        count(&db, "SELECT count(*) FROM file WHERE agent = 'codex'"),
        0,
        "it read history into a schema it doesn't know"
    );
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
    assert_eq!(
        count(&db, "SELECT count(*) FROM file WHERE agent = 'codex'"),
        0,
        "it wrote after a newer Turnscope had"
    );
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
    let hide = turnscope(data.path(), &["accounts", "hide", "claude"]);
    assert!(!hide.status.success());
    assert_eq!(
        String::from_utf8_lossy(&hide.stderr).trim_end(),
        "turnscope: a newer Turnscope has updated this data; update Turnscope"
    );
    assert_eq!(count(&db, "SELECT count(*) FROM account WHERE hidden"), 0);
}

/// A message the server can't take gets an error, and the server goes on:
/// a line that isn't UTF-8 once ended it.
#[test]
fn an_mcp_server_answers_a_malformed_message_and_goes_on() {
    let data = store();
    let lines = [
        r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
        r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"check_limits","arguments":null}}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
    ];
    let input = [&b"\xff\xfe\n"[..], (lines.join("\n") + "\n").as_bytes()].concat();
    let replies = replies(&fed(&mut command(data.path(), &["mcp"]), &input));
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
        r#"{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"explain_usage","arguments":{"all":true,"by":"account"}}}"#,
    ];
    let input = requests.join("\n") + "\n";
    let output = fed(&mut command(data.path(), &["mcp"]), input.as_bytes());
    assert!(output.status.success());
    let replies = replies(&output);
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
