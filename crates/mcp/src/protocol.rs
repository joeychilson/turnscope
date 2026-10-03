//! The Model Context Protocol, over standard input and output.
//!
//! A message is one line of JSON-RPC 2.0. The server answers requests in the
//! order they arrive, never sends requests of its own, and ends when its input
//! does. It writes nothing to its output but answers, each on a line, and the
//! notifications a handover sends.
//!
//! Every message is checked before anything acts on it. A line that is not
//! JSON is answered with a Parse error; one that is JSON but not a JSON-RPC
//! 2.0 request, notification or response, with an Invalid Request error,
//! under the message's id when it has one a request can have and `null`
//! otherwise. As MCP has them, a request's id is a string or an integer, never
//! `null`, and params, where given, are an object. Notifications are never
//! answered, and neither are responses, since the server asks nothing.
//!
//! A session starts with `initialize`, which names the revision the client
//! speaks; its capabilities and who it is go unchecked, since nothing the
//! server does depends on them (see [`negotiate`]), though they are given
//! to the server a session is handed over to. Until `initialize` has been
//! answered only `ping` is served; after, `initialize` is refused, since
//! a session is negotiated once. Requests are served from `initialize`'s
//! answer on, whether or not `notifications/initialized` has come: what that
//! notification gates is requests from the server, which sends none.
//!
//! The server keeps no request open, answering each before it reads the
//! next, so it doesn't track ids: one a client reuses can't be mistaken for
//! another.
//!
//! **What it offers.** Tools (`tools/list`, `tools/call`) and prompts
//! (`prompts/list`, `prompts/get`), which change only when an update replaces
//! this program while it runs: the session is then handed over to the new
//! one, which the client is told lists them again ([`crate::handover`]).
//! A tool's answer is text, its sentences, which are the whole answer, and
//! nothing else: no structured content, and no tool declares an output
//! schema, in any revision. Every agent that connects gives its model a
//! tool's structured content wherever there is some, so figures sent beside
//! the sentences are figures its model reads, at three to four times the
//! sentences' length. Seen 2026-10-03: Claude Code 2.1.288 serializes it as
//! the result and drops the text (in its code, and in a session's
//! transcript, eight answers given to its model as 36,667 characters, whose
//! sentences came to 10,179); Codex does the same
//! (`codex-rs/protocol/src/models.rs`, whose test calls the text
//! "ignored"); OpenCode 2.0.8, whose models call tools from code, is given
//! the structured content as the call's value (in its code, and in a
//! session's history); and Grok Build 1.0.41 gives its model the text and
//! then the structured content (in a headless session). Until 2026-10-03
//! the text also repeated the figures after the sentences, which came to
//! 70% to 85% of it.

use std::io::{self, BufRead, Read, Write};
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::handover::{Program, Successor};
use crate::prompts::Prompt;
use crate::tools::{self, Server, Tool};

/// The protocol revisions the server speaks, newest first. A client asking
/// for one of them gets it; any other gets the newest, and disconnects if it
/// can't speak it.
///
/// What the server offers, tools that take arguments and answer text, and
/// prompts, is in all four, and what later revisions added to it, such as
/// titles and annotations, are fields earlier clients pass over.
const REVISIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// The longest message read, in bytes. Every request the tools take is far
/// shorter; a longer line is refused without being held in memory.
const LONGEST_MESSAGE: u64 = 1 << 20;

/// The most messages a batch holds. No revision bounds a batch, but its
/// answers are written together, and a full page of a conversation for each
/// of 64 comes to about 3 MB.
const MOST_IN_BATCH: usize = 64;

const PARSE_ERROR: i64 = -32_700;
const INVALID_REQUEST: i64 = -32_600;
const METHOD_NOT_FOUND: i64 = -32_601;
const INVALID_PARAMS: i64 = -32_602;

/// Serve `server`'s tools to the client at the other end of `input` and
/// `output` until `input` ends. `program` is this program on disk, which the
/// instructions tell agents to run a tool with from a shell, and which the
/// session is handed over to once an update replaces it: started anew with
/// `options`, those this one was started with, naming the ledger and the
/// home the server reads, which the instructions pass on too.
///
/// # Errors
///
/// Returns the error reading `input` or writing `output` gave, or, once
/// handed over, passing a line between the client and the new server; and
/// an error when the new server stops while the client is still connected,
/// so that the client sees this one stop too.
pub fn serve(
    server: &Server,
    program: Option<&Path>,
    options: &[String],
    mut input: impl BufRead + Send + 'static,
    mut output: impl Write,
) -> io::Result<()> {
    let mut session = Session {
        server,
        executable: program.and_then(Path::to_str),
        program: program.and_then(Program::at),
        options,
        client: None,
    };
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = Read::take(&mut input, LONGEST_MESSAGE + 1).read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(());
        }
        let answer = if line.len() as u64 > LONGEST_MESSAGE && line.last() != Some(&b'\n') {
            input.skip_until(b'\n')?;
            Some(refusal(
                Value::Null,
                INVALID_REQUEST,
                "the message is too long",
            ))
        } else if let Some(successor) = session.successor() {
            return successor.relay(line, input, &mut output);
        } else {
            session.answer(&line)
        };
        if let Some(answer) = answer {
            serde_json::to_writer(&mut output, &answer)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
}

/// A server talking to one client.
struct Session<'a> {
    server: &'a Server,
    /// This program, and the options it was started with, to tell agents
    /// how to run a tool from a shell.
    executable: Option<&'a str>,
    /// This program on disk, to hand the session over to once an update
    /// replaces it.
    program: Option<Program>,
    options: &'a [String],
    /// What the client's `initialize` gave, and the revision it was answered
    /// with: `None` until it has been answered, which every request but
    /// `ping` waits for.
    client: Option<(Map<String, Value>, &'static str)>,
}

impl Session<'_> {
    /// The server to hand the session over to, once it is initialized and an
    /// update has replaced this program.
    fn successor(&mut self) -> Option<Successor> {
        let (initialize, revision) = self.client.as_ref()?;
        self.program
            .as_mut()?
            .successor(self.options, initialize, revision)
    }

    /// The answer to one line, or `None` for one that needs none.
    ///
    /// A line is one message, or a batch of them, answered with a batch of
    /// the answers due. Of the revisions spoken only 2025-03-26 has batches;
    /// one is answered under any, since JSON-RPC 2.0 has them, the client
    /// that sent one waits for its answers, and refusing them would help no
    /// one. As 2025-03-26 says, `initialize` is not sent in one.
    fn answer(&mut self, line: &[u8]) -> Option<Value> {
        let line = line.trim_ascii();
        if line.is_empty() {
            return None;
        }
        let Ok(message) = serde_json::from_slice::<Value>(line) else {
            return Some(refusal(Value::Null, PARSE_ERROR, "the message is not JSON"));
        };
        match message {
            Value::Array(batch) if batch.is_empty() => Some(refusal(
                Value::Null,
                INVALID_REQUEST,
                "a batch holds at least one message",
            )),
            Value::Array(batch) if batch.len() > MOST_IN_BATCH => Some(refusal(
                Value::Null,
                INVALID_REQUEST,
                &format!("a batch holds at most {MOST_IN_BATCH} messages"),
            )),
            Value::Array(batch) => {
                let answers: Vec<Value> = batch
                    .into_iter()
                    .filter_map(|message| self.respond(message, true))
                    .collect();
                (!answers.is_empty()).then_some(Value::Array(answers))
            }
            message => self.respond(message, false),
        }
    }

    /// The answer to one message, `batched` or not, or `None` for one that
    /// needs none.
    fn respond(&mut self, message: Value, batched: bool) -> Option<Value> {
        match Message::of(message) {
            Message::Unanswered => None,
            Message::Invalid { id, reason } => Some(refusal(id, INVALID_REQUEST, reason)),
            Message::Request { id, method, params } => {
                Some(match self.handle(&method, params, batched) {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Err((code, reason)) => refusal(id, code, &reason),
                })
            }
        }
    }

    /// The result of a request. A tool that runs and fails answers a result
    /// marked as an error, which the agent reads; only a request the server
    /// cannot act on is refused.
    fn handle(
        &mut self,
        method: &str,
        mut params: Map<String, Value>,
        batched: bool,
    ) -> Result<Value, (i64, String)> {
        match method {
            "ping" => Ok(json!({})),
            "initialize" if batched => Err((
                INVALID_REQUEST,
                "initialize is sent on its own, not in a batch".into(),
            )),
            "initialize" if self.client.is_some() => Err((
                INVALID_REQUEST,
                "the session is initialized already; initialize comes once".into(),
            )),
            "initialize" => {
                let revision =
                    negotiate(&params).map_err(|reason| (INVALID_PARAMS, reason.into()))?;
                self.client = Some((params, revision));
                Ok(json!({
                    "protocolVersion": revision,
                    "capabilities": {
                        "tools": {"listChanged": true},
                        "prompts": {"listChanged": true},
                    },
                    "serverInfo": {
                        "name": "turnscope",
                        "title": "Turnscope",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "instructions": tools::instructions(self.executable, self.options),
                }))
            }
            _ if self.client.is_none() => Err((
                INVALID_REQUEST,
                format!("{method} waits for initialize, which starts a session"),
            )),
            "tools/list" => Ok(json!({
                "tools": Tool::ALL.map(Tool::describe),
            })),
            "prompts/list" => Ok(json!({
                "prompts": Prompt::ALL.map(Prompt::describe),
            })),
            "prompts/get" => {
                let name = match params.remove("name") {
                    Some(Value::String(name)) => name,
                    _ => return Err((INVALID_PARAMS, "a prompt is asked for by name".into())),
                };
                match params.remove("arguments") {
                    None | Some(Value::Null) => {}
                    Some(Value::Object(arguments)) if arguments.is_empty() => {}
                    Some(_) => {
                        return Err((
                            INVALID_PARAMS,
                            format!("the prompt {name} takes no arguments"),
                        ));
                    }
                }
                Prompt::named(&name)
                    .map(Prompt::get)
                    .ok_or_else(|| (INVALID_PARAMS, format!("there is no prompt named {name}")))
            }
            "tools/call" => {
                let name = match params.remove("name") {
                    Some(Value::String(name)) => name,
                    _ => return Err((INVALID_PARAMS, "a tool call names its tool".into())),
                };
                let arguments = match params.remove("arguments") {
                    None | Some(Value::Null) => Value::Object(Map::new()),
                    Some(arguments @ Value::Object(_)) => arguments,
                    Some(_) => {
                        return Err((INVALID_PARAMS, "a tool's arguments are an object".into()));
                    }
                };
                let answer = self
                    .server
                    .call(&name, arguments)
                    .ok_or_else(|| (INVALID_PARAMS, format!("there is no tool named {name}")))?;
                Ok(match answer {
                    Ok(reply) => json!({
                        "content": [{"type": "text", "text": reply.compact()}],
                        "isError": false,
                    }),
                    Err(failure) => json!({
                        "content": [{"type": "text", "text": failure.0}],
                        "isError": true,
                    }),
                })
            }
            other => Err((METHOD_NOT_FOUND, format!("there is no method {other}"))),
        }
    }
}

/// A message, as JSON-RPC 2.0 and MCP shape it.
enum Message {
    /// A request, which is answered.
    Request {
        id: Value,
        method: String,
        params: Map<String, Value>,
    },
    /// A notification, or a response to a request the server never sent:
    /// neither is answered.
    Unanswered,
    /// None of those, answered with an Invalid Request error under `id`: the
    /// message's own when a request could have it, and `null` otherwise.
    Invalid { id: Value, reason: &'static str },
}

impl Message {
    /// What `message` is.
    fn of(message: Value) -> Message {
        let Value::Object(mut message) = message else {
            return Message::Invalid {
                id: Value::Null,
                reason: "a message is a JSON object",
            };
        };
        let id = message.remove("id");
        let answer_to = id.clone().filter(is_id).unwrap_or(Value::Null);
        let invalid = |reason| Message::Invalid {
            id: answer_to.clone(),
            reason,
        };
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return invalid("a message says \"jsonrpc\": \"2.0\"");
        }
        match message.remove("method") {
            Some(Value::String(method)) => {
                let params = match message.remove("params") {
                    None => Map::new(),
                    Some(Value::Object(params)) => params,
                    Some(_) => return invalid("a message's params are an object"),
                };
                match id {
                    None => Message::Unanswered,
                    Some(id) if is_id(&id) => Message::Request { id, method, params },
                    Some(_) => invalid("a request's id is a string or an integer"),
                }
            }
            Some(_) => invalid("a message's method is a string"),
            None => {
                // A response carries its request's id and either a result or
                // an error; an error's id is null when the request's could
                // not be read.
                let response = match (message.get("result"), message.get("error")) {
                    (Some(Value::Object(_)), None) => id.as_ref().is_some_and(is_id),
                    (None, Some(Value::Object(error))) => {
                        id.as_ref().is_some_and(|id| id.is_null() || is_id(id))
                            && error
                                .get("code")
                                .is_some_and(|code| code.as_i64().is_some())
                            && error.get("message").is_some_and(Value::is_string)
                    }
                    _ => false,
                };
                if response {
                    Message::Unanswered
                } else {
                    invalid("a message is a request, a notification or a response")
                }
            }
        }
    }
}

/// Whether `id` is one a request can have: a string, or an integer written
/// without a fraction.
fn is_id(id: &Value) -> bool {
    match id {
        Value::String(_) => true,
        Value::Number(number) => number.is_i64() || number.is_u64(),
        _ => false,
    }
}

/// The revision to speak with a client whose `initialize` gave `params`: the
/// one it asked for when the server speaks it, and otherwise the newest the
/// server speaks. Why not, when `params` name no revision.
///
/// The client's capabilities and who it is aren't checked: the server asks
/// nothing of a client, so a client that leaves them out, or gives them in a
/// shape of its own, loses nothing by it, where refusing it would leave it
/// without the server.
fn negotiate(params: &Map<String, Value>) -> Result<&'static str, &'static str> {
    let Some(Value::String(asked)) = params.get("protocolVersion") else {
        return Err("initialize names the protocolVersion the client speaks, as a string");
    };
    Ok(REVISIONS
        .into_iter()
        .find(|revision| revision == asked)
        .unwrap_or(REVISIONS[0]))
}

/// A JSON-RPC error answering the request `id`.
fn refusal(id: Value, code: i64, reason: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": reason}})
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use serde_json::{Value, json};
    use turnscope_engine::Engine;

    use super::{LONGEST_MESSAGE, serve};
    use crate::tools::Server;

    /// A line holding a ping with `id` after as many spaces as make it
    /// `length` bytes, and then its newline.
    fn ping(id: u64, length: usize) -> Vec<u8> {
        let message = json!({"jsonrpc": "2.0", "id": id, "method": "ping"}).to_string();
        let mut line = vec![b' '; length - message.len()];
        line.extend(message.bytes());
        line.push(b'\n');
        line
    }

    #[test]
    fn a_message_too_long_is_refused_and_the_next_still_answered() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let engine = Engine::open(data.path(), home.path()).unwrap();
        let server = Server::as_it_stands(engine, "UTC").unwrap();
        let longest = usize::try_from(LONGEST_MESSAGE).unwrap();
        // The second ping lies past the longest message read, so it is
        // refused with the rest of its line rather than answered.
        let input = [ping(1, longest), ping(2, longest + 64), ping(3, 64)].concat();
        let mut output = Vec::new();
        serve(&server, None, &[], Cursor::new(input), &mut output).unwrap();
        let answers: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            answers,
            [
                json!({"jsonrpc": "2.0", "id": 1, "result": {}}),
                json!({"jsonrpc": "2.0", "id": null,
                       "error": {"code": -32_600, "message": "the message is too long"}}),
                json!({"jsonrpc": "2.0", "id": 3, "result": {}}),
            ]
        );
    }
}
