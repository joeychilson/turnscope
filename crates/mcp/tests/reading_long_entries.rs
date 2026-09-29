//! What a page of a conversation cuts short is read exactly, a slice at a
//! time: every character once, in order, however long the entry and
//! whatever its script.

mod history {
    pub mod files;
}

use serde_json::{Value, json};
use turnscope_engine::Engine;
use turnscope_mcp::Server;

use history::files::write;

const SESSION: &str = "7c1f3e2a-5b8d-4e6f-9a0b-1c2d3e4f5a6b";

/// "Résumé " 700 times, 4,900 characters, and then 27 more: 4,927.
fn reply() -> String {
    format!("{}zanzibar is where it ended.", "Résumé ".repeat(700))
}

/// The command a tool call ran: "printf " and "ab" 500 times, 1,007
/// characters, which Claude Code records as the arguments
/// `{"command":"…"}`, 12 characters before it and 2 after: 1,021.
fn command() -> String {
    format!("printf {}", "ab".repeat(500))
}

/// A line of what the tool returned: l, í, n, e, a, a newline, 日, 本, a
/// space, 🙂 and a full stop, 11 characters, which JSON writes in 1 + 2 + 1
/// + 1 + 1 + 2 + 3 + 3 + 1 + 4 + 1 = 20 bytes.
const LINE: &str = "línea\n日本 🙂.";

/// What the tool returned: [`LINE`] 5,000 times, 55,000 characters.
fn output() -> String {
    LINE.repeat(5_000)
}

/// A server that has read a home holding one Claude Code session: a prompt,
/// a long reply, and a tool call with long arguments and a longer result.
fn server() -> (tempfile::TempDir, tempfile::TempDir, Server) {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let cwd = home.path().join("trip");
    let cwd = cwd.to_string_lossy();
    let lines = [
        json!({"type": "user", "sessionId": SESSION, "timestamp": "2026-09-20T10:00:00.000Z", "cwd": cwd,
               "message": {"role": "user", "content": "Where did the trip end?"}}),
        json!({"type": "assistant", "sessionId": SESSION, "timestamp": "2026-09-20T10:00:10.000Z", "cwd": cwd,
               "requestId": "req_1",
               "message": {"id": "msg_1", "model": "claude-opus-5", "role": "assistant",
                           "usage": {"input_tokens": 10, "output_tokens": 20},
                           "content": [{"type": "text", "text": reply()},
                                       {"type": "tool_use", "id": "toolu_1", "name": "Bash",
                                        "input": {"command": command()}}]}}),
        json!({"type": "user", "sessionId": SESSION, "timestamp": "2026-09-20T10:00:20.000Z", "cwd": cwd,
               "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1",
                                                        "content": output()}]}}),
    ];
    let path = home
        .path()
        .join(format!(".claude/projects/-trip/{SESSION}.jsonl"));
    write(&path, &lines);
    let engine = Engine::open(data.path(), home.path()).unwrap();
    engine.scan().unwrap();
    let server = Server::as_it_stands(engine, "UTC").unwrap();
    (home, data, server)
}

/// What `tool` answers to `arguments`, and whether it failed.
fn call(server: &Server, tool: &str, arguments: Value) -> (String, bool) {
    match server.call(tool, arguments).unwrap() {
        Ok(reply) => (reply.compact(), false),
        Err(failure) => (failure.0, true),
    }
}

/// A slice of one entry, which must be read.
fn slice(server: &Server, arguments: Value) -> Value {
    let mut arguments = arguments;
    arguments["session"] = json!(format!("claude-code:{SESSION}"));
    let (text, failed) = call(server, "read_session", arguments);
    assert!(!failed, "{text}");
    serde_json::from_str(&text).unwrap()
}

/// Every slice of entry `entry`'s `part` from `from` on, following each
/// answer's next_from until one is not truncated.
fn slices(server: &Server, entry: u32, part: &str, from: u64) -> Vec<Value> {
    let mut read = Vec::new();
    let mut next = Some(from);
    while let Some(from) = next {
        let answer = slice(server, json!({"entry": entry, "part": part, "from": from}));
        assert_eq!(answer["from"], from);
        next = answer["next_from"].as_u64();
        assert_eq!(answer["truncated"], next.is_some());
        read.push(answer);
    }
    read
}

/// The text of `slices`, joined.
fn joined(slices: &[Value]) -> String {
    slices
        .iter()
        .map(|slice| slice["text"].as_str().unwrap())
        .collect()
}

/// The first `count` characters of `text`.
fn first(text: &str, count: usize) -> String {
    text.chars().take(count).collect()
}

#[test]
fn a_page_says_where_what_it_cut_short_goes_on() {
    let (_home, _data, server) = server();
    let session = format!("claude-code:{SESSION}");
    let (page, _) = call(&server, "read_session", json!({"session": session}));
    // The reply is cut at 4,000 of its 4,927 characters; the arguments at
    // 300 of their 1,021 on the call's one line.
    let text = format!(
        "{}\u{2026} (927 more characters; read_session reads them with {{\"entry\": 1, \"from\": 4000}})",
        first(&reply(), 4_000)
    );
    assert!(page.contains(&text), "{page}");
    let arguments = format!(r#"{{"command":"{}"}}"#, command());
    let input = format!(
        "{}\u{2026} (721 more characters; read_session reads them with {{\"entry\": 2, \"part\": \"input\", \"from\": 300}})",
        first(&arguments, 300)
    );
    assert!(page.contains(&input), "{page}");

    // In full, the arguments fit in 2,000 characters, and the result is cut
    // at 2,000 of its 55,000.
    let (full, _) = call(
        &server,
        "read_session",
        json!({"session": session, "detail": "full"}),
    );
    assert!(full.contains(&format!(": {arguments}\n")), "{full}");
    let output = format!(
        "{}\u{2026} (53000 more characters; read_session reads them with {{\"entry\": 2, \"part\": \"output\", \"from\": 2000}})",
        first(&output(), 2_000)
    );
    assert!(full.contains(&output), "{full}");
}

#[test]
fn an_entry_is_read_a_slice_at_a_time_without_gaps_or_repeats() {
    let (_home, _data, server) = server();

    // The rest of the reply, from where the page cut it: 4,000 is 571 times
    // "Résumé " and 3 characters, so it goes on at "umé ", and holds 927.
    let rest = slice(&server, json!({"entry": 1, "from": 4_000}));
    let text = rest["text"].as_str().unwrap();
    assert_eq!(
        (
            rest["characters"].clone(),
            rest["to"].clone(),
            rest["speaker"].clone(),
            rest["part"].clone()
        ),
        (
            json!(4_927),
            json!(4_927),
            json!("assistant"),
            json!("text")
        )
    );
    assert!(text.starts_with("umé Résumé") && text.ends_with("zanzibar is where it ended."));
    assert_eq!(first(&reply(), 4_000) + text, reply());

    // The result from its start: a page holds 40,000 bytes as JSON writes
    // them, which is 2,000 lines of 20 bytes, 22,000 characters; the last
    // slice holds the 1,000 lines left.
    let read = slices(&server, 2, "output", 0);
    let spans: Vec<(u64, u64)> = read
        .iter()
        .map(|slice| {
            (
                slice["from"].as_u64().unwrap(),
                slice["to"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(spans, [(0, 22_000), (22_000, 44_000), (44_000, 55_000)]);
    assert_eq!(read[0]["text"], LINE.repeat(2_000));
    assert_eq!(read[2]["text"], LINE.repeat(1_000));
    assert!(read.iter().all(|slice| slice["characters"] == 55_000));
    assert_eq!(joined(&read), output());

    // From where the full page cut it, 2,000, which is no line's start, the
    // slices hold the rest exactly.
    let read = slices(&server, 2, "output", 2_000);
    assert_eq!(first(&output(), 2_000) + &joined(&read), output());

    // And the arguments, from 300 of their 1,021.
    let arguments = format!(r#"{{"command":"{}"}}"#, command());
    let read = slices(&server, 2, "input", 300);
    assert_eq!(read.len(), 1);
    assert_eq!(read[0]["tool"], "Bash");
    assert_eq!(first(&arguments, 300) + &joined(&read), arguments);
}

#[test]
fn a_word_past_the_cut_is_found_and_its_passage_read() {
    let (_home, _data, server) = server();
    let found = server
        .call("find_sessions", json!({"words": "zanzibar"}))
        .unwrap()
        .unwrap();
    let entry = &found.data().unwrap()["sessions"][0]["passage"]["entry"];
    assert_eq!(entry, 1);
    let (page, _) = call(
        &server,
        "read_session",
        json!({"session": format!("claude-code:{SESSION}"), "find": "zanzibar"}),
    );
    assert!(
        page.contains("1 mention \"zanzibar\": 1") && !page.contains("zanzibar is"),
        "{page}"
    );
    let rest = slice(&server, json!({"entry": entry, "from": 4_000}));
    assert!(
        rest["text"]
            .as_str()
            .unwrap()
            .contains("zanzibar is where it ended.")
    );
}

#[test]
fn a_slice_asked_of_what_is_not_there_is_refused() {
    let (_home, _data, server) = server();
    let session = format!("claude-code:{SESSION}");
    let refused = |arguments: Value, expected: &str| {
        let mut arguments = arguments;
        arguments["session"] = json!(session);
        let (text, failed) = call(&server, "read_session", arguments);
        assert!(failed && text.contains(expected), "{text}");
    };
    refused(json!({"entry": 3}), "there is no entry 3");
    refused(json!({"entry": 1, "part": "output"}), "is not a tool call");
    refused(json!({"entry": 2}), "whose parts are input and output");
    refused(json!({"entry": 1, "from": 4_928}), "has 4927 characters");
    refused(json!({"entry": 1, "part": "body"}), "part takes text");
    refused(json!({"entry": 1, "offset": 1}), "don't come with it");
    refused(json!({"from": 10}), "come with entry");
    // The end itself is where an empty slice starts.
    let end = slice(&server, json!({"entry": 1, "from": 4_927}));
    assert_eq!(
        (end["text"].clone(), end["truncated"].clone()),
        (json!(""), json!(false))
    );
}
