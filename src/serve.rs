//! `turnscope serve`: the daemon the menu bar app talks to, over JSON-RPC
//! 2.0 on a Unix socket, one message a line (docs/protocol.md).

use std::borrow::Cow;
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

/// macOS allows a socket path of 103 bytes at most.
pub fn socket_path(data: &Path) -> PathBuf {
    let path = data.join("serve.sock");
    if path.as_os_str().len() <= 103 {
        return path;
    }
    let hash = crate::sha256_hex(data.as_os_str().as_encoded_bytes(), 8);
    std::env::temp_dir().join(format!("turnscope-{hash}.sock"))
}

enum Message {
    Connected(UnixStream),
    Line(u64, Vec<u8>),
    Gone(u64),
    /// The limits were read at start, or the data was brought up to date:
    /// it may show something new.
    Kept,
    /// Keeping the data current stopped, and why.
    Stopped(String),
    /// A request answered off the loop, and whether it changed the status.
    Answered(u64, Value, bool),
}

struct Client {
    stream: UnixStream,
    greeted: bool,
}

#[derive(Serialize, Deserialize)]
struct AppStatus {
    #[serde(flatten)]
    status: status::Status,
    /// Every agent Turnscope reads, installed or not.
    agents: Vec<status::Agent>,
}

impl AppStatus {
    fn of(db: &Connection, home: &Path, now: i64) -> Result<AppStatus> {
        let status = status::status(db, now)?;
        Ok(AppStatus {
            agents: status::agents(home, &status.accounts),
            status,
        })
    }
}

struct Server {
    db: Connection,
    home: PathBuf,
    data: PathBuf,
    clients: HashMap<u64, Client>,
    /// The status last pushed, and what it shows: all of it but `at`.
    sent: Option<(status::Status, Value)>,
    sender: Sender<Message>,
}

/// How long serve stays with no client: an app restart reconnects first.
const LINGER: Duration = Duration::from_secs(60);

/// How large `serve.log` grows before it starts over. A catch-up that fails
/// each time it's tried writes about 150 bytes, every 5 s at most while
/// agents work: a day of that at least.
const LOG_SIZE: u64 = 1 << 20;

/// Add `line` to `serve.log` in `data`, with the time. No one reads serve's
/// standard error once the app has connected, so what goes wrong while it
/// runs is kept here.
fn log(data: &Path, line: &str) {
    let path = data.join("serve.log");
    let full = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_SIZE);
    let mut options = std::fs::OpenOptions::new();
    match full {
        true => options.write(true).truncate(true),
        false => options.append(true),
    };
    if let Ok(mut file) = options.create(true).open(path) {
        let line = line.replace('\n', " ");
        let _ = writeln!(file, "{:.0} {line}", jiff::Timestamp::now());
    }
}

/// Say `line` on standard error, and in `serve.log`.
fn say(data: &Path, line: &str) {
    eprintln!("turnscope serve: {line}");
    log(data, line);
}

pub fn run(home: &Path, data: &Path) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(data)?;
    let lock = std::fs::File::create(data.join("serve.lock"))?;
    if lock.try_lock().is_err() {
        eprintln!("turnscope serve: already running for {}", data.display());
        return Ok(());
    }
    let logged = data.to_path_buf();
    let panicked = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        log(&logged, &panic.to_string());
        panicked(panic);
    }));
    // Before the socket, so a database this serve can't open, as one a
    // newer Turnscope changed, is said on standard error, where the app
    // reads why a serve it started couldn't. Opened after it, the app
    // could connect first, and then only saw the connection close.
    let db = crate::db::open(data).inspect_err(|error| log(data, &error.to_string()))?;
    let socket = socket_path(data);
    // Left by a serve that ended without removing it: none holds the lock.
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).map_err(|error| {
        let error = format!("can't listen on {}: {error}", socket.display());
        log(data, &error);
        Error::Failed(error)
    })?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;

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
        db,
        home: home.to_path_buf(),
        data: data.to_path_buf(),
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
            Err(_) => "it panicked".to_owned(),
        };
        let _ = sender.send(Message::Stopped(why));
    });
    let mut alone = Some(Instant::now());
    let mut next = 0;
    loop {
        let wait = alone.map_or(Duration::from_secs(3600), |since| {
            LINGER.saturating_sub(since.elapsed())
        });
        match messages.recv_timeout(wait) {
            Ok(Message::Connected(stream)) => {
                // Out of file descriptors, say: let this client go, and
                // serve on.
                let Ok(reading) = stream.try_clone() else {
                    continue;
                };
                next += 1;
                let sender = server.sender.clone();
                std::thread::spawn(move || {
                    let mut reading = BufReader::new(reading);
                    let mut line = Vec::new();
                    // Bytes, so a line that isn't UTF-8 is answered as one
                    // that isn't JSON, and the client stays.
                    while reading
                        .read_until(b'\n', &mut line)
                        .is_ok_and(|read| read > 0)
                    {
                        if sender
                            .send(Message::Line(next, std::mem::take(&mut line)))
                            .is_err()
                        {
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
                server.stopped(&why);
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
            Err(RecvTimeoutError::Timeout) if alone.is_some() => break,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

/// The least time from one catch-up's end to the next one's start, while
/// agents write in their folders: 5 s, over catching up as soon as writes
/// settle. Many agents at work at once never let writes settle, and each
/// catch-up looks at every history file. Over 5,000 sessions, with a
/// stand-in agent writing ten of them ten times a second for 30 s, serve ran
/// 9.8G instructions without the gap and 3.2G with it. The cost: the panel
/// shows what an agent wrote up to 5 s late.
const GAP: Duration = Duration::from_secs(5);

/// Catches up when the agents' folders change, and each minute, as horizons
/// and outlooks move with the clock.
fn keep(home: &Path, data: &Path, kept: &Sender<Message>) -> Result<()> {
    let mut db = crate::db::open(data)?;
    let (changed, changes) = mpsc::channel();
    // Any write, not only to history: macOS reports a write when its file
    // is closed, and Codex keeps a session's file open, and OpenCode its
    // database, while they work. Their other writes are what show it.
    // But not a SQLite database's `-shm` file, which Turnscope writes too
    // when it opens the database: counted, a database that fails every read,
    // with a log left beside it, started a catch-up every 5 s for good (21 in
    // 100 s, measured 2026-11-11). An agent's writes to its database are in
    // the `-wal` file.
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let ours = event.is_ok_and(|event| {
            !event.paths.is_empty()
                && event
                    .paths
                    .iter()
                    .all(|path| path.to_string_lossy().ends_with("-shm"))
        });
        if !ours {
            let _ = changed.send(());
        }
    })
    .map_err(|error| Error::Failed(error.to_string()))?;
    for agent in agents::ALL {
        for (n, folder) in agent.folders(home).iter().enumerate() {
            // The agent's own folder may be another program's, as `~/.gemini`
            // is the Gemini command line's too: every write there would
            // start a catch-up. Until the agent is installed it's looked
            // through each minute, as a folder not there yet is.
            if n == 0 && !agent.installed(folder) {
                continue;
            }
            let _ = watcher.watch(folder, notify::RecursiveMode::Recursive);
        }
    }
    // The minute and SQLite's `data_version` when the status was last
    // pushed. `data_version` moves when another connection writes, as an
    // agent's MCP server does. A catch-up that read nothing new, in the same
    // minute, with no other write since, doesn't push: as when an agent
    // writes anything but history in its folder. Chosen over pushing after
    // every catch-up: with a stand-in agent writing so three times a second
    // for 30 s, serve ran 11.3G instructions that way and 0.4G with this.
    // The cost: an agent's MCP settings changed outside the app show at the
    // next minute.
    let mut pushed = None;
    // The limits first, and the main thread told, over telling it only once
    // the first catch-up is done: on a first launch that reads all of
    // history (12 s for a made-up one of 5,000 files, 176 MB), and the app
    // showed no account until it ended. The catch-up then finds the limits
    // read.
    if crate::online() {
        let read = crate::limits::refresh(&mut db, home).unwrap_or_else(|error| {
            say(data, &error.to_string());
            true
        });
        if read && kept.send(Message::Kept).is_err() {
            return Ok(());
        }
    }
    loop {
        let read = match crate::catch_up(&mut db, home) {
            Ok(read) => read,
            Err(error) => {
                // Once a newer Turnscope has changed the schema, this one
                // reads nothing more, so it stops: the app says why, and the
                // serve it starts next can't open the data, and says to
                // update. Going on, it showed what it read before as current.
                crate::db::revision(&db)?;
                say(data, &error.to_string());
                // One that failed may have read some.
                true
            }
        };
        let now = (
            crate::now().div_euclid(crate::time::MINUTE),
            db.pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))?,
        );
        if read || pushed != Some(now) {
            if kept.send(Message::Kept).is_err() {
                return Ok(());
            }
            pushed = Some(now);
        }
        let waited = Instant::now();
        match changes.recv_timeout(Duration::from_secs(60)) {
            Ok(()) => {
                // Let a burst of writes settle, a second at most.
                let start = Instant::now();
                while changes.recv_timeout(Duration::from_millis(250)).is_ok()
                    && start.elapsed() < Duration::from_secs(1)
                {}
                // The next catch-up reads what's written meanwhile.
                std::thread::sleep(GAP.saturating_sub(waited.elapsed()));
                while changes.try_recv().is_ok() {}
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

impl Server {
    fn push(&mut self) {
        let app = match AppStatus::of(&self.db, &self.home, crate::now()) {
            Ok(app) => app,
            Err(error) => return say(&self.data, &error.to_string()),
        };
        let mut shown = serde_json::to_value(&app).unwrap_or_default();
        if let Some(fields) = shown.as_object_mut() {
            fields.remove("at");
        }
        if self.sent.as_ref().is_some_and(|(_, sent)| *sent == shown) {
            return;
        }
        let raised = alerts::raise(&mut self.db, &app.status);
        self.broadcast("status", json!(app));
        match raised {
            Ok(raised) => {
                for alert in raised {
                    self.broadcast("alert", json!(alert));
                }
                self.sent = Some((app.status, shown));
            }
            // Not sent, so the next push raises them again.
            Err(error) => say(&self.data, &format!("alerts: {error}")),
        }
    }

    /// Tell every client why the data can't be kept current, before serve
    /// exits: once the app has connected, no one reads what serve says on
    /// standard error.
    fn stopped(&mut self, why: &str) {
        log(
            &self.data,
            &format!("keeping the data current stopped: {why}"),
        );
        self.broadcast(
            "stopped",
            json!(Stopped {
                why: why.to_owned()
            }),
        );
    }

    fn broadcast(&mut self, method: &str, params: Value) {
        let line = json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string();
        self.clients.retain(|_, client| {
            let sent = !client.greeted || writeln!(client.stream, "{line}").is_ok();
            if !sent {
                // Shut, so the thread reading it ends, and the app sees it
                // close and connects again.
                let _ = client.stream.shutdown(std::net::Shutdown::Both);
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
            // As in `broadcast`.
            let _ = client.stream.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Answer a line; `false` once asked to shut down.
    fn answer(&mut self, client: u64, line: &[u8]) -> bool {
        let reply = |id: Value, result: std::result::Result<Value, Value>| match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        };
        let refused = |code: i64, message: String| json!({ "code": code, "message": message });
        let request: Value = match serde_json::from_slice(line) {
            Ok(request) => request,
            Err(error) => {
                self.send(
                    client,
                    &reply(
                        Value::Null,
                        Err(refused(-32700, format!("not JSON: {error}"))),
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
                    Err(refused(-32600, "a request needs a method".to_owned())),
                ),
            );
            return true;
        };
        // A notification asks for no answer, and none is acted on.
        let Some(id) = request.get("id").cloned() else {
            return true;
        };
        let params = request.get("params").cloned().unwrap_or_default();
        fn given<T: for<'a> Deserialize<'a>>(params: Value) -> std::result::Result<T, Value> {
            serde_json::from_value(params).map_err(|error| {
                json!({ "code": -32602, "message": format!("parameters that don't fit: {error}") })
            })
        }
        let failed = move |error: Error| refused(-32000, error.to_string());
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
            "hello" => self.hello(client, now).map_err(failed),
            _ if !greeted => Err(refused(-32002, "send hello first".to_owned())),
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
                    agents::by_id(&asked.agent)
                        .ok_or_else(|| refused(-32602, format!("no agent is {}", asked.agent)))
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
            other => Err(refused(-32601, format!("no method {other}"))),
        };
        let changed = result.is_ok() && matches!(method, "settings.set" | "account.hide");
        self.send(client, &reply(id, result));
        if changed {
            self.push();
        }
        true
    }

    /// Greet `client`: the engine this is, the whole status, and the alerts
    /// waiting to be shown. It takes no parameters, and any sent are passed
    /// over: the app only talks to the engine it ships with, which it tells
    /// by `server`.
    fn hello(&mut self, client: u64, now: i64) -> Result<Value> {
        let status = AppStatus::of(&self.db, &self.home, now)?;
        let alerts = alerts::pending(&self.db, &status.status, now)?;
        if let Some(client) = self.clients.get_mut(&client) {
            client.greeted = true;
        }
        let server = format!("turnscope {}", env!("CARGO_PKG_VERSION"));
        Ok(json!({ "server": server, "status": status, "alerts": alerts }))
    }

    /// From the status last pushed: the one the app shows.
    fn breakdown(&self, account: &str, limit: &str, now: i64) -> Result<Vec<UsedMost>> {
        let status = match &self.sent {
            Some((status, _)) => Cow::Borrowed(status),
            None => Cow::Owned(status::status(&self.db, now)?),
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
        let (used, shares) = usage::breakdown(
            &self.db,
            found.0,
            Some(found.1),
            None,
            usage::By::Session,
            3,
            now,
        )?;
        let mut sessions = Vec::new();
        for (at, row) in used.rows.iter().enumerate() {
            let (project, agent) = usage::brief(&self.db, &row.key)?;
            sessions.push(UsedMost {
                session: row.key.clone(),
                project: project
                    .as_deref()
                    .and_then(|project| project.rsplit('/').next())
                    .map(str::to_owned),
                // By session, a row's name is its session's title.
                title: row.name.clone(),
                agent,
                share_percent: shares.as_ref().map(|shares| shares.rows[at]),
            });
        }
        Ok(sessions)
    }
}

/// Why serve stopped, as `stopped` tells it.
#[derive(Serialize, Deserialize)]
struct Stopped {
    /// What went wrong, as `a newer Turnscope has updated this data; update
    /// Turnscope`.
    why: String,
}

/// A session that used a limit, as `limit.breakdown` lists it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UsedMost {
    /// The session's id: `claude-code:0f6e3f6a-713c`.
    session: String,
    title: Option<String>,
    /// The name of its project's folder.
    project: Option<String>,
    /// The agent's id.
    agent: String,
    /// About how many points of the limit it used: its share of the cost,
    /// times the points used. None when the window's start isn't known, the
    /// window reset since the limit was read, or nothing used had a price.
    share_percent: Option<f64>,
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
        fn again<T: serde::Serialize + serde::de::DeserializeOwned>(value: &Value) {
            let read: T = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(&serde_json::to_value(read).unwrap(), value);
        }
        again::<super::AppStatus>(&example("status.notification.json")["params"]);
        again::<super::Stopped>(&example("stopped.notification.json")["params"]);
        again::<crate::alerts::Alert>(&example("alert.notification.json")["params"]);
        let hello = example("hello.result.json");
        again::<super::AppStatus>(&hello["result"]["status"]);
        for each in hello["result"]["alerts"].as_array().unwrap() {
            again::<crate::alerts::Alert>(each);
        }
    }

    /// The app reads an enum value it doesn't know as an error, so each
    /// value of each enum it reads is in an example, where both sides'
    /// tests read it. An enum's values are those serde names when it
    /// refuses one it doesn't know, so a new value fails here until an
    /// example holds it.
    #[test]
    fn the_contracts_examples_hold_every_value_of_every_enum() {
        use crate::{alerts, providers, status};
        use serde::de::DeserializeOwned;
        use serde_json::json;
        use std::collections::BTreeSet;

        fn values<T: DeserializeOwned>(unknown: Value) -> Vec<String> {
            let refused = serde_json::from_value::<T>(unknown).err().unwrap();
            let said = refused.to_string();
            let expected = said.split_once("expected").map_or("", |(_, rest)| rest);
            expected
                .split('`')
                .skip(1)
                .step_by(2)
                .map(str::to_owned)
                .collect()
        }
        // Each string in `value`, with the key it's under and the key of the
        // object holding it. An array's items are held by the array's key.
        fn strings(value: &Value, holder: &str, found: &mut BTreeSet<(String, String, String)>) {
            match value {
                Value::Object(fields) => {
                    for (key, field) in fields {
                        if let Value::String(text) = field {
                            found.insert((holder.to_owned(), key.clone(), text.clone()));
                        }
                        strings(field, key, found);
                    }
                }
                Value::Array(items) => {
                    for item in items {
                        strings(item, holder, found);
                    }
                }
                _ => {}
            }
        }
        let mut found = BTreeSet::new();
        let folder = format!("{}/contract", env!("CARGO_MANIFEST_DIR"));
        for file in std::fs::read_dir(folder).unwrap() {
            let path = file.unwrap().path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let text = std::fs::read_to_string(path).unwrap();
            strings(&serde_json::from_str(&text).unwrap(), "", &mut found);
        }
        // Each enum, and where its values are: the key holding the object
        // they're in, and their own key.
        type At = fn(&str, &str) -> bool;
        let enums: [(&str, Vec<String>, At); 8] = [
            (
                "Connection",
                values::<status::Connection>(json!("")),
                |_, key| key == "connection",
            ),
            (
                "State",
                values::<status::State>(json!({ "kind": "" })),
                |holder, key| (holder, key) == ("state", "kind"),
            ),
            (
                "Stale",
                values::<status::Stale>(json!("")),
                |holder, key| (holder, key) == ("state", "why"),
            ),
            ("Horizon", values::<status::Horizon>(json!("")), |_, key| {
                key == "horizon"
            }),
            (
                "Outlook",
                values::<status::Outlook>(json!({ "kind": "" })),
                |holder, key| (holder, key) == ("outlook", "kind"),
            ),
            (
                "Unknown",
                values::<status::Unknown>(json!("")),
                |holder, key| (holder, key) == ("outlook", "reason"),
            ),
            (
                "an account's Kind",
                values::<providers::Kind>(json!("")),
                |holder, key| (holder, key) == ("accounts", "kind"),
            ),
            (
                "an alert's Kind",
                values::<alerts::Kind>(json!("")),
                |holder, key| key == "kind" && ["params", "alerts"].contains(&holder),
            ),
        ];
        for (name, values, at) in enums {
            assert!(!values.is_empty(), "serde named no values of {name}");
            for value in values {
                let held = found
                    .iter()
                    .any(|(holder, key, text)| at(holder, key) && *text == value);
                assert!(held, "no example in contract/ holds {name} `{value}`");
            }
        }
    }

    /// A database serve can't open, as one a newer Turnscope changed, is
    /// said before it listens: the app reads a starting serve's standard
    /// error only until it connects.
    #[test]
    fn a_database_it_cant_open_is_said_before_it_listens() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        drop(crate::db::open(data.path()).unwrap());
        let newer = rusqlite::Connection::open(data.path().join("turnscope.sqlite")).unwrap();
        newer.pragma_update(None, "user_version", 99).unwrap();
        let error = super::run(home.path(), data.path())
            .unwrap_err()
            .to_string();
        assert_eq!(error, crate::db::newer().to_string());
        assert!(!super::socket_path(data.path()).exists());
        let logged = std::fs::read_to_string(data.path().join("serve.log")).unwrap();
        assert!(logged.trim_end().ends_with(&error), "{logged}");
    }

    #[test]
    fn the_log_starts_over_once_it_is_full() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("serve.log");
        std::fs::write(&path, vec![b'x'; super::LOG_SIZE as usize + 1]).unwrap();
        super::log(data.path(), "it failed\nat once");
        let logged = std::fs::read_to_string(&path).unwrap();
        // One line, the time first.
        assert_eq!(logged.lines().count(), 1, "{logged}");
        assert!(
            logged.trim_end().ends_with("Z it failed at once"),
            "{logged}"
        );
    }

    /// Once a newer Turnscope has changed the schema, keeping the data
    /// current stops at the next catch-up, so serve exits and the app says
    /// why.
    #[test]
    fn keeping_the_data_current_stops_once_the_schema_is_newer() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // Claude Code is installed, so its folder is watched.
        let folder = home.path().join(".claude/projects/-work-a");
        std::fs::create_dir_all(&folder).unwrap();
        let db = crate::db::open(data.path()).unwrap();
        // Nothing is asked over the network: the prices and limits count
        // as read just now.
        db.execute(
            "UPDATE state SET prices_at = ?1, limits_at = ?1",
            [crate::now()],
        )
        .unwrap();
        let (sender, messages) = std::sync::mpsc::channel();
        let (home_kept, data_kept) = (home.path().to_path_buf(), data.path().to_path_buf());
        let (ended, end) = std::sync::mpsc::channel();
        std::thread::spawn(move || ended.send(super::keep(&home_kept, &data_kept, &sender)));
        let first = messages.recv_timeout(super::Duration::from_secs(10));
        assert!(matches!(first, Ok(super::Message::Kept)));
        db.pragma_update(None, "user_version", 99).unwrap();
        // A write in the folder, so it catches up, 5 s after the last.
        std::fs::write(folder.join("s1.jsonl"), "{}\n").unwrap();
        let stopped = end.recv_timeout(super::Duration::from_secs(20));
        let why = stopped.expect("it went on").unwrap_err().to_string();
        assert_eq!(why, crate::db::newer().to_string());
    }

    #[test]
    fn why_it_stopped_is_told_to_each_client_and_logged() {
        use std::io::BufRead as _;
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (stream, app) = super::UnixStream::pair().unwrap();
        let mut server = super::Server {
            db: crate::db::open(data.path()).unwrap(),
            home: home.path().to_path_buf(),
            data: data.path().to_path_buf(),
            clients: [(
                1,
                super::Client {
                    stream,
                    greeted: true,
                },
            )]
            .into(),
            sent: None,
            sender: std::sync::mpsc::channel().0,
        };
        let why = "a newer Turnscope has updated this data; update Turnscope";
        server.stopped(why);
        let mut line = String::new();
        std::io::BufReader::new(app).read_line(&mut line).unwrap();
        let told: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            told,
            serde_json::json!({ "jsonrpc": "2.0", "method": "stopped", "params": { "why": why } })
        );
        let logged = std::fs::read_to_string(data.path().join("serve.log")).unwrap();
        assert!(
            logged.contains(&format!("keeping the data current stopped: {why}")),
            "{logged}"
        );
    }

    /// The main thread is told once the limits are read, before the first
    /// read of history. That read can't begin here: its lock is held, as
    /// another Turnscope reading history holds it.
    #[test]
    fn the_main_thread_is_told_of_the_limits_before_history_is_read() {
        // Offline, no limits are read, so nothing is told before history.
        if !crate::online() {
            return;
        }
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut db = crate::db::open(data.path()).unwrap();
        // Nothing is asked over the network: the home holds no login to
        // read a limit with, and the prices count as read just now.
        db.execute("UPDATE state SET prices_at = ?1", [crate::now()])
            .unwrap();
        // What each agent folder holds is known by now, and the limits are
        // due again: a folder's first sign-in takes the lock too, to work
        // out whose its usage is (`ingest::derive`).
        crate::limits::refresh(&mut db, home.path()).unwrap();
        db.execute("UPDATE state SET limits_at = NULL", []).unwrap();
        let reading = crate::db::lock(&data.path().join("ingest.lock")).unwrap();
        let (sender, messages) = std::sync::mpsc::channel();
        let (home_kept, data_kept) = (home.path().to_path_buf(), data.path().to_path_buf());
        std::thread::spawn(move || super::keep(&home_kept, &data_kept, &sender));
        // Wait to be told, and say when the limits were read, and whether
        // the history was, by then.
        let told = || -> (Option<i64>, bool) {
            let told = messages.recv_timeout(super::Duration::from_secs(10));
            assert!(matches!(told, Ok(super::Message::Kept)));
            db.query_row(
                "SELECT limits_at, history_at IS NOT NULL FROM state",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };
        let (limits, history) = told();
        assert!(limits.is_some() && !history);
        drop(reading);
        assert_eq!(told(), (limits, true));
    }

    /// A status is pushed when what it shows changed, and only then: not
    /// after a catch-up that read history but changed nothing shown.
    #[test]
    fn a_status_is_pushed_only_when_what_it_shows_changed() {
        use std::io::BufRead as _;
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // Claude Code is installed from the start, with no history yet.
        let folder = home.path().join(".claude/projects/-work-a");
        std::fs::create_dir_all(&folder).unwrap();
        let (stream, app) = super::UnixStream::pair().unwrap();
        app.set_nonblocking(true).unwrap();
        let mut app = std::io::BufReader::new(app);
        let mut server = super::Server {
            db: crate::db::open(data.path()).unwrap(),
            home: home.path().to_path_buf(),
            data: data.path().to_path_buf(),
            clients: [(
                1,
                super::Client {
                    stream,
                    greeted: true,
                },
            )]
            .into(),
            sent: None,
            sender: std::sync::mpsc::channel().0,
        };
        // Push, and say whether the first read had finished in each status
        // the app was sent.
        let mut pushed = |server: &mut super::Server| -> Vec<Value> {
            server.push();
            (&mut app)
                .lines()
                .map_while(Result::ok)
                .map(|line| serde_json::from_str::<Value>(&line).unwrap())
                .map(|message| message["params"]["historyRead"].clone())
                .collect()
        };

        assert_eq!(pushed(&mut server), [false]);
        // The first read finishes, though there's nothing to read.
        crate::ingest::ingest(&mut server.db, home.path()).unwrap();
        assert_eq!(pushed(&mut server), [true]);
        // A session that drew on no account: read, and nothing shown changed.
        let line = serde_json::json!({"type": "user", "sessionId": "s1",
            "timestamp": "2026-09-30T09:00:00Z", "message": {"role": "user", "content": "Hi."}});
        std::fs::write(folder.join("s1.jsonl"), format!("{line}\n")).unwrap();
        assert!(crate::ingest::ingest(&mut server.db, home.path()).unwrap());
        assert!(pushed(&mut server).is_empty());
        let mut settings = crate::alerts::Settings::default();
        settings.notify.reset = false;
        crate::alerts::save_settings(&server.db, &settings).unwrap();
        assert_eq!(pushed(&mut server), [true]);
    }
}
