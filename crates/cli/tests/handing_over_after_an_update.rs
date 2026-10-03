//! What an update does to an agent's server: the program it runs is
//! replaced while a session is open, and the session goes on with the new
//! program, the client told to list its tools and prompts again.

use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

#[test]
fn a_session_goes_on_with_the_program_an_update_leaves() {
    let folder = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let built = env!("CARGO_BIN_EXE_turnscope");
    let program = folder.path().join("turnscope");
    fs::copy(built, &program).unwrap();
    let mut server = Command::new(&program)
        .arg("mcp")
        .arg("--data")
        .arg(data.path())
        .arg("--home")
        .arg(home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = server.stdin.take().unwrap();
    let mut output = BufReader::new(server.stdout.take().unwrap());
    let mut ask = |message: Value| writeln!(input, "{message}").unwrap();
    let mut read = || {
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };

    ask(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
               "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                          "clientInfo": {"name": "claude-code", "version": "2.1.288"}}}));
    assert_eq!(read()["result"]["protocolVersion"], "2025-11-25");
    ask(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // The update: a new file moved over the program.
    let staged = folder.path().join("turnscope.new");
    fs::copy(built, &staged).unwrap();
    fs::rename(&staged, &program).unwrap();

    ask(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "get_usage", "arguments": {}}}));
    assert_eq!(
        read(),
        json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
    );
    assert_eq!(
        read(),
        json!({"jsonrpc": "2.0", "method": "notifications/prompts/list_changed"})
    );
    let answered = read();
    assert_eq!(answered["id"], 2);
    assert_eq!(answered["result"]["isError"], false, "{answered}");
    ask(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}));
    let listed = read();
    assert_eq!(listed["id"], 3);
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 6);

    // The client closing its input stops both servers.
    drop(input);
    assert!(server.wait().unwrap().success());
}
