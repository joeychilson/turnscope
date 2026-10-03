//! An update replaces the program a running server was started from. The
//! session is then handed over to a server started from the new program,
//! once that one answers as this one would, and otherwise this one goes on
//! answering.

use std::fs;
use std::io::{self, BufReader, Cursor, Read};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use serde_json::{Value, json};
use turnscope_engine::Engine;
use turnscope_mcp::{Server, serve};

/// The revision every session here speaks.
const REVISION: &str = "2025-11-25";

/// A stand-in for the new server, as bash 3.2 runs it: it records how it
/// was started and the `initialize` it was given, answers that with
/// `revision`, and answers every other request with which server answered.
fn new_server(revision: &str) -> String {
    format!(
        r#"#!/bin/bash
set -euo pipefail
here=$(dirname "$0")
printf '%s\n' "$*" >> "$here/started"
while IFS= read -r line; do
  case $line in
    *'"method":"initialize"'*)
      printf '%s\n' "$line" > "$here/introduced"
      printf '%s\n' '{{"jsonrpc":"2.0","id":"turnscope-handover","result":{{"protocolVersion":"{revision}","capabilities":{{}},"serverInfo":{{"name":"turnscope","version":"9.9.9"}}}}}}' ;;
    *'"id":'*)
      id=${{line#*\"id\":}}; id=${{id%%,*}}; id=${{id%%\}}*}}
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"answered_by":"the new server"}}}}\n' "$id" ;;
  esac
done
"#
    )
}

/// A program that records that it was started, and stops.
const BROKEN: &str = r#"#!/bin/bash
printf '%s\n' "$*" >> "$(dirname "$0")/started"
exit 1
"#;

/// Put `script` at `path` as an update does: a new file moved over the old.
fn install(path: &Path, script: &str) {
    let staged = path.with_extension("new");
    fs::write(&staged, script).unwrap();
    fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&staged, path).unwrap();
}

/// A client's input: `before`, then, once the server has read all of it
/// and asks for more, `update` done and `after`.
struct Updated<F: FnOnce()> {
    before: Cursor<Vec<u8>>,
    update: Option<F>,
    after: Cursor<Vec<u8>>,
}

impl<F: FnOnce()> Read for Updated<F> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.before.read(buffer)?;
        if read > 0 {
            return Ok(read);
        }
        if let Some(update) = self.update.take() {
            update();
        }
        self.after.read(buffer)
    }
}

/// Lines, each with its newline.
fn lines(messages: &[Value]) -> Vec<u8> {
    messages
        .iter()
        .flat_map(|message| format!("{message}\n").into_bytes())
        .collect()
}

/// Every line a server started from `program` with `options` writes when a
/// client initializes a session, `update` replaces the program, and the
/// client sends `after`.
fn session(
    program: &Path,
    options: &[String],
    update: impl FnOnce(),
    after: &[Value],
) -> Vec<Value> {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let server =
        Server::as_it_stands(Engine::open(data.path(), home.path()).unwrap(), "UTC").unwrap();
    let input = Updated {
        before: Cursor::new(lines(&[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": REVISION, "capabilities": {},
                              "clientInfo": {"name": "claude-code", "version": "2.1.288"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        ])),
        update: Some(update),
        after: Cursor::new(lines(after)),
    };
    let mut output = Vec::new();
    serve(
        &server,
        Some(program),
        options,
        BufReader::new(input),
        &mut output,
    )
    .unwrap();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn a_session_is_handed_over_to_the_server_an_update_leaves() {
    let folder = tempfile::tempdir().unwrap();
    let program = folder.path().join("turnscope");
    install(&program, BROKEN);
    let options = ["--data".to_owned(), "/tmp/a ledger".to_owned()];
    let answers = session(
        &program,
        &options,
        || install(&program, &new_server(REVISION)),
        &[
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "check_limits", "arguments": {}}}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}),
        ],
    );
    assert_eq!(answers[0]["id"], 1);
    assert_eq!(answers[0]["result"]["protocolVersion"], REVISION);
    assert_eq!(
        answers[1..],
        [
            // The client lists the new server's tools and prompts again.
            json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
            json!({"jsonrpc": "2.0", "method": "notifications/prompts/list_changed"}),
            // The request that found the program replaced, and every one
            // after it, is the new server's to answer.
            json!({"jsonrpc": "2.0", "id": 2, "result": {"answered_by": "the new server"}}),
            json!({"jsonrpc": "2.0", "id": 3, "result": {"answered_by": "the new server"}}),
        ]
    );
    // Started once, with the options this one was, and initialized as the
    // client initialized this one.
    let started = fs::read_to_string(folder.path().join("started")).unwrap();
    assert_eq!(started, "mcp --data /tmp/a ledger\n");
    let introduced: Value =
        serde_json::from_str(&fs::read_to_string(folder.path().join("introduced")).unwrap())
            .unwrap();
    assert_eq!(introduced["params"]["protocolVersion"], REVISION);
    assert_eq!(introduced["params"]["clientInfo"]["name"], "claude-code");
}

#[test]
fn a_new_server_that_doesnt_start_leaves_this_one_answering_without_trying_it_again() {
    let folder = tempfile::tempdir().unwrap();
    let program = folder.path().join("turnscope");
    install(&program, &new_server(REVISION));
    let answers = session(
        &program,
        &[],
        || install(&program, BROKEN),
        &[
            json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}),
        ],
    );
    assert_eq!(
        answers[1..],
        [
            json!({"jsonrpc": "2.0", "id": 2, "result": {}}),
            json!({"jsonrpc": "2.0", "id": 3, "result": {}}),
        ]
    );
    let started = fs::read_to_string(folder.path().join("started")).unwrap();
    assert_eq!(started, "mcp\n", "tried once");
}

#[test]
fn a_new_server_speaking_another_revision_leaves_this_one_answering() {
    let folder = tempfile::tempdir().unwrap();
    let program = folder.path().join("turnscope");
    install(&program, BROKEN);
    let answers = session(
        &program,
        &[],
        || install(&program, &new_server("2024-11-05")),
        &[json!({"jsonrpc": "2.0", "id": 2, "method": "ping"})],
    );
    assert_eq!(
        answers[1..],
        [json!({"jsonrpc": "2.0", "id": 2, "result": {}})]
    );
    assert!(folder.path().join("introduced").exists(), "it was asked");
}

#[test]
fn a_program_left_as_it_was_is_never_started() {
    let folder = tempfile::tempdir().unwrap();
    let program = folder.path().join("turnscope");
    install(&program, &new_server(REVISION));
    let answers = session(
        &program,
        &[],
        || {},
        &[json!({"jsonrpc": "2.0", "id": 2, "method": "ping"})],
    );
    assert_eq!(
        answers[1..],
        [json!({"jsonrpc": "2.0", "id": 2, "result": {}})]
    );
    assert!(!folder.path().join("started").exists());
}
