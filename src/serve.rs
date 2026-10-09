//! `turnscope serve`: the daemon the menu bar app talks to, over JSON-RPC
//! 2.0 on a Unix socket, one message a line (docs/protocol.md).
//!
//! The socket is `serve.sock` in the data directory, or for a path past the
//! 103 bytes macOS allows, `$TMPDIR/turnscope-<hash>.sock`. It's the owner's
//! alone (0600, in a 0700 directory). One serve runs per data directory
//! (`serve.lock`); it exits a minute after its last client leaves, so an app
//! restart reconnects without reading anything again.
//!
//! One thread answers requests. Another keeps the data current: it reads
//! what changed 250 ms after the agents' folders do, and each minute reads
//! history, limits when due and prices when due. After each, serve pushes
//! the status, if what it shows changed, with the alerts it raised. Should
//! that thread stop, serve stops too, so the app starts one that keeps it.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use notify::Watcher as _;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Error, Result, agents, alerts, status, usage};

/// The protocol spoken, raised for any change a client can't ignore.
pub const PROTOCOL: u64 = 1;

pub fn socket_path(data: &Path) -> PathBuf {
    let path = data.join("serve.sock");
    if path.as_os_str().len() <= 103 {
        return path;
    }
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(data.as_os_str().as_encoded_bytes());
    let hash: String = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    std::env::temp_dir().join(format!("turnscope-{hash}.sock"))
}

enum Message {
    Connected(UnixStream),
    Line(u64, String),
    Gone(u64),
    /// The data was brought up to date.
    Kept,
    /// Keeping the data current stopped, and why: serve stops with it, so
    /// the app starts one that can.
    Stopped(String),
    /// A request answered off the loop, and whether it changed the status.
    Answered(u64, Value, bool),
}

struct Client {
    stream: UnixStream,
    greeted: bool,
}

/// Let a client go that a write to failed or timed out: shut its socket, so
/// the thread reading it ends and the app sees it close and connects again.
fn drop_client(client: &Client) {
    let _ = client.stream.shutdown(std::net::Shutdown::Both);
}

struct Server {
    db: Connection,
    home: PathBuf,
    clients: HashMap<u64, Client>,
    /// The status last pushed, and what it shows: it less when it was
    /// worked out.
    sent: Option<(status::Status, Value)>,
    sender: Sender<Message>,
}

pub fn run(home: &Path, data: &Path, linger: Duration) -> Result<()> {
    let failed = |error: std::io::Error| Error::Failed(error.to_string());
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(data)
        .map_err(failed)?;
    let lock = std::fs::File::create(data.join("serve.lock")).map_err(failed)?;
    if lock.try_lock().is_err() {
        eprintln!("turnscope serve: already running for {}", data.display());
        return Ok(());
    }
    let socket = socket_path(data);
    // Left by a serve that ended without removing it: none holds the lock.
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)
        .map_err(|error| Error::Failed(format!("can't listen on {}: {error}", socket.display())))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).map_err(failed)?;

    let (sender, messages) = mpsc::channel();
    let accepting = sender.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            if accepting.send(Message::Connected(stream)).is_err() {
                return;
            }
        }
    });
    let mut server = Server {
        db: crate::db::open(data)?,
        home: home.to_path_buf(),
        clients: HashMap::new(),
        sent: None,
        sender: sender.clone(),
    };
    let (home_kept, data_kept) = (home.to_path_buf(), data.to_path_buf());
    std::thread::spawn(move || {
        // Stopped, by an error or a panic, it can't be kept current: serve
        // exits, and the app starts one that can.
        let kept = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            keep(&home_kept, &data_kept, &sender)
        }));
        let why = match kept {
            Ok(Ok(())) => return,
            Ok(Err(error)) => error.to_string(),
            Err(_) => "keeping the data current panicked".to_owned(),
        };
        let _ = sender.send(Message::Stopped(why));
    });
    let mut alone = Some(Instant::now());
    let mut next = 0;
    loop {
        let wait = alone.map_or(Duration::from_secs(3600), |since| {
            linger.saturating_sub(since.elapsed())
        });
        match messages.recv_timeout(wait) {
            Ok(Message::Connected(stream)) => {
                next += 1;
                let (reading, sender) =
                    (stream.try_clone().map_err(failed)?, server.sender.clone());
                std::thread::spawn(move || {
                    for line in BufReader::new(reading).lines() {
                        let Ok(line) = line else { break };
                        if sender.send(Message::Line(next, line)).is_err() {
                            return;
                        }
                    }
                    let _ = sender.send(Message::Gone(next));
                });
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                server.clients.insert(
                    next,
                    Client {
                        stream,
                        greeted: false,
                    },
                );
                alone = None;
            }
            Ok(Message::Line(client, line)) => {
                if !server.answer(client, &line) {
                    break;
                }
            }
            Ok(Message::Gone(client)) => {
                server.clients.remove(&client);
                if server.clients.is_empty() {
                    alone = Some(Instant::now());
                }
            }
            Ok(Message::Kept) => server.push(),
            Ok(Message::Stopped(why)) => {
                let _ = std::fs::remove_file(&socket);
                return Err(Error::Failed(format!(
                    "keeping the data current stopped: {why}"
                )));
            }
            Ok(Message::Answered(client, reply, changed)) => {
                server.send(client, &reply);
                if changed {
                    server.push();
                }
            }
            // The linger, with no client: an app restart reconnects first.
            Err(RecvTimeoutError::Timeout) if alone.is_some() => break,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

/// Keep the data current: 250 ms after the agents' folders change, and each
/// minute, as horizons and outlooks move with the clock.
fn keep(home: &Path, data: &Path, kept: &Sender<Message>) -> Result<()> {
    let mut db = crate::db::open(data)?;
    let (changed, changes) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |_: notify::Result<notify::Event>| {
        let _ = changed.send(());
    })
    .map_err(|error| Error::Failed(error.to_string()))?;
    for agent in agents::ALL {
        for folder in agent.folders(home) {
            // A folder not there yet is looked through each minute.
            let _ = watcher.watch(&folder, notify::RecursiveMode::Recursive);
        }
    }
    loop {
        if let Err(error) = crate::catch_up(&mut db, home) {
            eprintln!("turnscope serve: {error}");
        }
        if kept.send(Message::Kept).is_err() {
            return Ok(());
        }
        match changes.recv_timeout(Duration::from_secs(60)) {
            Ok(()) => {
                // Let a burst of writes settle, a second at most.
                let start = Instant::now();
                while changes.recv_timeout(Duration::from_millis(250)).is_ok()
                    && start.elapsed() < Duration::from_secs(1)
                {}
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

impl Server {
    /// Push the status to every client, if what it shows changed, with the
    /// alerts it raised.
    fn push(&mut self) {
        let status = match status::status(&self.db, &self.home, crate::now()) {
            Ok(status) => status,
            Err(error) => return eprintln!("turnscope serve: {error}"),
        };
        let mut shown = serde_json::to_value(&status).unwrap_or_default();
        if let Some(fields) = shown.as_object_mut() {
            fields.remove("at");
            // Set at every catch-up; what's shown is only whether the first
            // read is done.
            fields.insert(
                "historyReadAt".to_owned(),
                json!(status.history_read_at.is_some()),
            );
        }
        if self.sent.as_ref().is_some_and(|(_, sent)| *sent == shown) {
            return;
        }
        let raised = alerts::raise(&mut self.db, &status);
        self.broadcast("status", json!(status));
        match raised {
            Ok(raised) => {
                for alert in raised {
                    self.broadcast("alert", json!(alert));
                }
                self.sent = Some((status, shown));
            }
            // Not sent, so the next push raises them again.
            Err(error) => eprintln!("turnscope serve: alerts: {error}"),
        }
    }

    fn broadcast(&mut self, method: &str, params: Value) {
        let line = json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string();
        self.clients.retain(|_, client| {
            let sent = !client.greeted || writeln!(client.stream, "{line}").is_ok();
            if !sent {
                drop_client(client);
            }
            sent
        });
    }

    fn send(&mut self, client: u64, message: &Value) {
        let gone = self
            .clients
            .get_mut(&client)
            .is_some_and(|to| writeln!(to.stream, "{message}").is_err());
        if gone && let Some(client) = self.clients.remove(&client) {
            drop_client(&client);
        }
    }

    /// Answer a line; `false` once asked to shut down.
    fn answer(&mut self, client: u64, line: &str) -> bool {
        let reply =
            |id: Value, result: std::result::Result<Value, (i64, String, Option<Value>)>| {
                match result {
                    Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                    Err((code, message, data)) => {
                        let mut error = json!({ "code": code, "message": message });
                        if let Some(data) = data {
                            error["data"] = data;
                        }
                        json!({ "jsonrpc": "2.0", "id": id, "error": error })
                    }
                }
            };
        let request: Value = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(error) => {
                self.send(
                    client,
                    &reply(
                        Value::Null,
                        Err((-32700, format!("not JSON: {error}"), None)),
                    ),
                );
                return true;
            }
        };
        let Some(method) = request["method"].as_str() else {
            let id = request.get("id").cloned().unwrap_or_default();
            self.send(
                client,
                &reply(
                    id,
                    Err((-32600, "a request needs a method".to_owned(), None)),
                ),
            );
            return true;
        };
        // A notification asks for no answer, and none is acted on.
        let Some(id) = request.get("id").cloned() else {
            return true;
        };
        let params = request.get("params").cloned().unwrap_or_default();
        fn given<T: for<'a> Deserialize<'a>>(
            params: Value,
        ) -> std::result::Result<T, (i64, String, Option<Value>)> {
            serde_json::from_value(params)
                .map_err(|error| (-32602, format!("parameters that don't fit: {error}"), None))
        }
        let failed = |error: Error| (-32000, error.to_string(), None);
        let greeted = self
            .clients
            .get(&client)
            .is_some_and(|client| client.greeted);
        let now = crate::now();
        let result = match method {
            "shutdown" => {
                self.send(client, &reply(id, Ok(json!({}))));
                return false;
            }
            "hello" => {
                #[derive(Deserialize)]
                struct Hello {
                    protocol: u64,
                }
                given::<Hello>(params).and_then(|hello| {
                    if hello.protocol != PROTOCOL {
                        let message = format!(
                            "this turnscope speaks protocol {PROTOCOL}, not {}",
                            hello.protocol
                        );
                        return Err((-32001, message, Some(json!({ "supported": [PROTOCOL] }))));
                    }
                    let status = status::status(&self.db, &self.home, now).map_err(failed)?;
                    let alerts = alerts::pending(&self.db, now).map_err(failed)?;
                    if let Some(client) = self.clients.get_mut(&client) {
                        client.greeted = true;
                    }
                    let server = format!("turnscope {}", env!("CARGO_PKG_VERSION"));
                    Ok(json!({ "server": server, "status": status, "alerts": alerts }))
                })
            }
            _ if !greeted => Err((-32002, "send hello first".to_owned(), None)),
            "alerts.ack" => {
                #[derive(Deserialize)]
                struct Ack {
                    ids: Vec<i64>,
                }
                given::<Ack>(params).and_then(|ack| {
                    let acknowledged = alerts::acknowledge(&self.db, &ack.ids).map_err(failed)?;
                    Ok(json!({ "acknowledged": acknowledged }))
                })
            }
            "limit.breakdown" => {
                #[derive(Deserialize)]
                struct Asked {
                    account: String,
                    limit: String,
                }
                given::<Asked>(params).and_then(|asked| {
                    let sessions = self
                        .breakdown(&asked.account, &asked.limit, now)
                        .map_err(failed)?;
                    Ok(json!({ "sessions": sessions }))
                })
            }
            "settings.set" => {
                #[derive(Deserialize)]
                struct Asked {
                    settings: crate::alerts::Settings,
                }
                given::<Asked>(params).and_then(|asked| {
                    crate::alerts::save_settings(&self.db, &asked.settings).map_err(failed)?;
                    Ok(json!({}))
                })
            }
            "account.hide" => {
                #[derive(Deserialize)]
                struct Asked {
                    account: String,
                    hidden: bool,
                }
                given::<Asked>(params).and_then(|asked| {
                    status::hide(&self.db, &asked.account, asked.hidden).map_err(failed)?;
                    Ok(json!({}))
                })
            }
            "agent.connect" | "agent.disconnect" => {
                #[derive(Deserialize)]
                struct Asked {
                    agent: String,
                }
                match given::<Asked>(params).and_then(|asked| {
                    agents::by_id(&asked.agent).ok_or((
                        -32602,
                        format!("no agent is {}", asked.agent),
                        None,
                    ))
                }) {
                    // It runs the agent's own commands, for up to minutes:
                    // answered when done, so nothing else waits.
                    Ok(agent) => {
                        let (sender, home) = (self.sender.clone(), self.home.clone());
                        let connect = method == "agent.connect";
                        std::thread::spawn(move || {
                            let done = match connect {
                                true => crate::connect::connect(agent, &home),
                                false => crate::connect::disconnect(agent, &home),
                            };
                            let changed = done.is_ok();
                            let answer = reply(id, done.map(|()| json!({})).map_err(failed));
                            let _ = sender.send(Message::Answered(client, answer, changed));
                        });
                        return true;
                    }
                    Err(error) => Err(error),
                }
            }
            other => Err((-32601, format!("no method {other}"), None)),
        };
        let changed = result.is_ok() && matches!(method, "settings.set" | "account.hide");
        self.send(client, &reply(id, result));
        if changed {
            self.push();
        }
        true
    }

    /// The sessions that used most of a limit in its window, with the points
    /// of it each took, of the status last pushed.
    fn breakdown(&self, account: &str, limit: &str, now: i64) -> Result<Vec<UsedMost>> {
        let worked_out;
        let status = match &self.sent {
            Some((status, _)) => status,
            None => {
                worked_out = status::status(&self.db, &self.home, now)?;
                &worked_out
            }
        };
        let found = status
            .accounts
            .iter()
            .find(|found| found.id == account)
            .and_then(|account| {
                Some((
                    account,
                    account.limits.iter().find(|found| found.key == limit)?,
                ))
            })
            .ok_or_else(|| Error::NotFound(format!("no limit {limit} of {account}")))?;
        let (used, points) =
            usage::breakdown(&self.db, found.0, found.1, None, usage::By::Session, 3, now)?;
        let mut sessions = Vec::new();
        for (row, points) in used.rows.iter().zip(points) {
            let (title, project, agent, running) = crate::sessions::brief(&self.db, &row.key, now)?;
            sessions.push(UsedMost {
                session: row.key.clone(),
                project: project
                    .as_deref()
                    .and_then(|project| project.rsplit('/').next())
                    .map(str::to_owned),
                title,
                agent,
                share_percent: points,
                running,
            });
        }
        Ok(sessions)
    }
}

/// A session that used a limit, as `limit.breakdown` lists it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UsedMost {
    session: String,
    title: Option<String>,
    /// Its folder's name.
    project: Option<String>,
    agent: String,
    /// Points of the limit it took; `None` where the window's start isn't
    /// known, as of credits or a window that rolls.
    share_percent: Option<f64>,
    running: bool,
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    /// Each example in `contract/` that carries a status or an alert,
    /// read as the Rust types and written again as it was: the app's tests
    /// do the same with its types, so the two sides can't drift.
    #[test]
    fn the_contracts_examples_are_read_and_written_again_as_they_are() {
        let example = |name: &str| -> Value {
            let path = format!("{}/contract/{name}", env!("CARGO_MANIFEST_DIR"));
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
        };
        let again =
            |value: &Value, read: fn(Value) -> Value| assert_eq!(&read(value.clone()), value);
        let status = |value: Value| {
            serde_json::to_value(serde_json::from_value::<crate::status::Status>(value).unwrap())
                .unwrap()
        };
        let alert = |value: Value| {
            serde_json::to_value(serde_json::from_value::<crate::alerts::Alert>(value).unwrap())
                .unwrap()
        };
        again(&example("status.notification.json")["params"], status);
        again(&example("alert.notification.json")["params"], alert);
        let hello = example("hello.result.json");
        again(&hello["result"]["status"], status);
        for each in hello["result"]["alerts"].as_array().unwrap() {
            again(each, alert);
        }
    }
}
