//! A server talking to a client over the protocol, as an agent talks to it.

use std::io::Cursor;

use serde_json::{Value, json};
use turnscope_mcp::{Server, serve};

/// Every answer `server` gives to `messages`, one per line, in a session
/// begun as a client speaking `revision` begins one.
pub fn talk(server: &Server, revision: &str, messages: &[Value]) -> Vec<Value> {
    let handshake = [
        json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
               "params": {"protocolVersion": revision, "capabilities": {"roots": {}},
                          "clientInfo": {"name": "claude-code", "version": "2.1.288"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    ];
    let input: String = handshake
        .iter()
        .chain(messages)
        .map(|message| format!("{message}\n"))
        .collect();
    let mut output = Vec::new();
    serve(server, None, &[], Cursor::new(input), &mut output).unwrap();
    let mut answers: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let begun = answers.remove(0);
    assert!(begun["result"]["protocolVersion"].is_string(), "{begun}");
    answers
}

/// What a tool answered: the text an agent is given over the protocol, and
/// the figures behind it, which `turnscope call` shows a person.
pub struct Answered {
    /// The text: the sentences, which are the whole answer.
    pub text: String,
    /// The figures, or null for an answer that has none, as a refusal.
    pub data: Value,
    /// Whether it is an error.
    pub failed: bool,
}

impl Answered {
    /// The sentences.
    pub fn said(&self) -> &str {
        &self.text
    }
}

/// `tool`'s answer to `arguments`: its text over the protocol, and its
/// figures from the tool itself.
pub fn call(server: &Server, tool: &str, arguments: Value) -> Answered {
    let answers = talk(
        server,
        "2025-06-18",
        &[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                 "params": {"name": tool, "arguments": arguments.clone()}})],
    );
    let result = &answers[0]["result"];
    // An agent is given the text alone.
    assert_eq!(
        result.as_object().map(|fields| fields.len()),
        Some(2),
        "{tool}: {result}"
    );
    let figures = match server.call(tool, arguments) {
        Some(Ok(reply)) => reply.data().cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    };
    Answered {
        text: result["content"][0]["text"].as_str().unwrap().to_owned(),
        data: figures,
        failed: result["isError"].as_bool().unwrap(),
    }
}

/// `tool`'s answer to `arguments`, which must not be an error.
pub fn answer(server: &Server, tool: &str, arguments: Value) -> Answered {
    let answered = call(server, tool, arguments);
    assert!(!answered.failed, "{tool} failed: {}", answered.text);
    answered
}

/// Why `tool` refused `arguments`, which it must.
pub fn refusal(server: &Server, tool: &str, arguments: Value) -> String {
    let answered = call(server, tool, arguments);
    assert!(answered.failed, "{tool} answered: {}", answered.text);
    answered.text
}
