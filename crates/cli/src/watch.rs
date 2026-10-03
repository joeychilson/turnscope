//! `turnscope watch`: the menu bar app's one line to the engine.
//!
//! The app starts it and keeps it for as long as it runs. It opens the
//! engine and keeps history, prices and limits current ([`Engine::run`]),
//! and speaks JSON, one object to a line, as a language server does:
//!
//! - **Out**, on standard output: `{"feed": ...}`, everything the app shows
//!   ([`crate::feed`]), at once and again whenever any of it changes, but no
//!   more than once in [`SETTLE`], and never twice the same; `{"alert":
//!   ...}` and `{"recap": ...}`, as the engine sends them once each, for the
//!   app to tell as notifications; and `{"reply": {"id": 1, "error": null}}`
//!   to each request, once it is done.
//! - **In**, on standard input: requests, each with an `id` its reply
//!   carries: `{"id": 1, "do": "hide", "account": "...", "hidden": true}`,
//!   `{"do": "connect", "agent": "codex"}`, and `{"do": "panel", "open":
//!   true}` as the panel opens and closes.
//!
//! What used each limit, and the advice drawn from it, is worked out only
//! while the panel is open, the one place it is shown: it is most of what a
//! feed costs, and agents at work have a feed worked out every few seconds.
//! Opening the panel works the feed out again at once.
//!
//! It stops when its input closes, as when the app quits, so it never
//! outlives it. What goes wrong that the app can do nothing about goes to
//! standard error.
//!
//! `contract/` at the repository's root holds a line of each kind, which the
//! app's tests read.

use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant as Clock};

use serde::{Deserialize, Serialize};
use turnscope_engine::{Alert, AlertKind, Change, Engine, Options, WeekEnded};

use crate::feed::{self, Feed};
use crate::{Failure, connect};

/// How long after a change the feed waits for the next, so that a burst of
/// them, as a scan brings, is written once.
const SETTLE: Duration = Duration::from_millis(100);

/// How often the feed is worked out again with nothing changed, as which
/// sessions are active and which accounts in use drifts with the clock.
const TICK: Duration = Duration::from_secs(60);

/// A line written out.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Out<'a> {
    Feed(&'a Feed),
    Alert(Told),
    Recap(Vec<Week>),
    Reply { id: u64, error: Option<String> },
}

/// An alert, for the app to tell.
#[derive(Serialize)]
struct Told {
    account: String,
    title: String,
    label: Option<String>,
    limit: String,
    scope: Option<String>,
    kind: &'static str,
    /// When it runs out, for `running_out`; when it resets, for `used_up`,
    /// `unused` and the quarters left.
    at: Option<String>,
    left: Option<f64>,
}

/// How an account's week went, for the recap.
#[derive(Serialize)]
struct Week {
    account: String,
    title: String,
    label: Option<String>,
    limit: String,
    /// The most of it used, in percent, which can be above 100.
    used: f64,
    /// When it was used up, if it was.
    used_up_at: Option<String>,
    /// The project that took most of it here.
    project: Option<String>,
}

/// A line read in.
#[derive(Debug, Deserialize)]
struct Asked {
    id: u64,
    #[serde(flatten)]
    request: Request,
}

/// What the app asks.
#[derive(Debug, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
enum Request {
    Hide { account: String, hidden: bool },
    Connect { agent: String },
    Panel { open: bool },
}

/// What the loop hears of.
enum Event {
    Change(Change),
    Asked(Asked),
    Done { id: u64, error: Option<String> },
    Closed,
}

/// Run until standard input closes.
///
/// # Errors
///
/// Fails when the engine can't be opened or kept current.
pub(crate) fn run(data: &Path, home: &Path) -> Result<(), Failure> {
    let engine = Arc::new(Engine::open(data, home)?);
    let binary = std::env::current_exe()?;
    let (events, heard) = mpsc::channel();
    // Subscribed before it runs, so nothing it says is missed.
    let changes = engine.subscribe();
    let running = engine.run(Options {
        check_prices: true,
        read_limits: true,
    })?;
    relay(&events, "changes", move |events| {
        while let Ok(change) = changes.recv() {
            if events.send(Event::Change(change)).is_err() {
                return;
            }
        }
    });
    relay(&events, "requests", |events| {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let event = match read_request(&line) {
                Ok(asked) => Event::Asked(asked),
                Err((id, error)) => {
                    eprintln!("turnscope: a request it can't read: {error}: {line}");
                    // One with an id is answered, so nothing waits on it.
                    match id {
                        Some(id) => Event::Done {
                            id,
                            error: Some(format!("a request it can't read: {error}")),
                        },
                        None => continue,
                    }
                }
            };
            if events.send(event).is_err() {
                return;
            }
        }
        let _closed = events.send(Event::Closed);
    });
    let mut watch = Watch {
        engine: Arc::clone(&engine),
        binary,
        configs: connect::Configs::of(home),
        events,
        last: None,
        open: false,
    };
    watch.write_feed();
    let mut due: Option<Clock> = None;
    loop {
        let wait = due.map_or(TICK, |due| due.saturating_duration_since(Clock::now()));
        let event = match heard.recv_timeout(wait) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => {
                due = None;
                watch.write_feed();
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let changed = match event {
            Event::Change(Change::Alert(alert)) => {
                watch.write_alert(&alert);
                false
            }
            Event::Change(Change::Recap(weeks)) => {
                watch.write_recap(&weeks);
                false
            }
            Event::Change(Change::Trouble(trouble)) => {
                eprintln!("turnscope: {trouble}");
                false
            }
            Event::Change(_) => true,
            Event::Asked(asked) => watch.answer(asked),
            Event::Done { id, error } => {
                write(&Out::Reply { id, error });
                true
            }
            Event::Closed => break,
        };
        if changed && due.is_none() {
            due = Some(Clock::now() + SETTLE);
        }
    }
    drop(running);
    Ok(())
}

/// What the loop keeps.
struct Watch {
    engine: Arc<Engine>,
    binary: PathBuf,
    /// Where agents keep their MCP servers, read each feed.
    configs: connect::Configs,
    events: Sender<Event>,
    /// The last feed written, but for when it was worked out.
    last: Option<Feed>,
    /// Whether the panel is open, so the feed says what used each limit.
    open: bool,
}

impl Watch {
    /// Write the feed, when it says something the last one didn't.
    fn write_feed(&mut self) {
        let agents = connect::links(&self.configs, &self.binary);
        let feed = match feed::read(&self.engine, agents, self.open) {
            Ok(feed) => feed,
            Err(error) => {
                eprintln!("turnscope: could not read the feed: {error}");
                return;
            }
        };
        let unstamped = Feed {
            at: String::new(),
            ..feed.clone()
        };
        if self.last.as_ref() != Some(&unstamped) {
            write(&Out::Feed(&feed));
            self.last = Some(unstamped);
        }
    }

    /// Do what `asked` asks: at once when it is quick, and on a thread of
    /// its own, replying when done, when it isn't. Whether the feed may have
    /// changed already.
    fn answer(&mut self, asked: Asked) -> bool {
        let id = asked.id;
        let reply = |error: Option<String>| {
            write(&Out::Reply { id, error });
            true
        };
        match asked.request {
            Request::Hide { account, hidden } => reply(
                self.engine
                    .set_account_hidden(&account, hidden)
                    .err()
                    .map(|error| error.to_string()),
            ),
            Request::Connect { agent } => {
                let binary = self.binary.clone();
                self.later(id, move || connect::connect(&agent, &binary));
                false
            }
            // Closing needs no feed of its own: the next leaves out what
            // used each limit.
            Request::Panel { open } => {
                self.open = open;
                write(&Out::Reply { id, error: None });
                open
            }
        }
    }

    /// Do `work` on a thread of its own, and reply to `id` when it is done.
    fn later(&self, id: u64, work: impl FnOnce() -> Result<(), String> + Send + 'static) {
        relay(&self.events, "request", move |events| {
            let error = work().err();
            let _sent = events.send(Event::Done { id, error });
        });
    }

    fn write_alert(&self, alert: &Alert) {
        write(&Out::Alert(Told {
            account: alert.account.clone(),
            title: alert.title.clone(),
            label: alert.label.clone(),
            limit: alert.limit.clone(),
            scope: alert.scope.clone(),
            kind: kind(alert.kind),
            at: alert.when.map(feed::time),
            left: alert.used.map(|used| (100.0 - used).clamp(0.0, 100.0)),
        }));
    }

    fn write_recap(&self, weeks: &[WeekEnded]) {
        let weeks = weeks
            .iter()
            .map(|week| Week {
                account: week.account.clone(),
                title: week.title.clone(),
                label: week.label.clone(),
                limit: week.limit.clone(),
                used: week.window.used,
                used_up_at: week.window.reached.map(feed::time),
                project: week.project.clone(),
            })
            .collect();
        write(&Out::Recap(weeks));
    }
}

fn kind(kind: AlertKind) -> &'static str {
    match kind {
        AlertKind::RunningOut => "running_out",
        AlertKind::Reached => "used_up",
        AlertKind::Available => "back",
        AlertKind::Unused => "unused",
        AlertKind::ThreeQuartersLeft => "three_quarters_left",
        AlertKind::HalfLeft => "half_left",
        AlertKind::QuarterLeft => "quarter_left",
    }
}

/// The request `line` asks, or why it can't be read, with its id when it
/// has one.
fn read_request(line: &str) -> Result<Asked, (Option<u64>, serde_json::Error)> {
    serde_json::from_str::<Asked>(line).map_err(|error| {
        let id = serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|value| value.get("id")?.as_u64());
        (id, error)
    })
}

/// Run `work` on a thread named `name`, with a sender of its own.
fn relay(events: &Sender<Event>, name: &str, work: impl FnOnce(Sender<Event>) + Send + 'static) {
    let events = events.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || work(events))
    {
        eprintln!("turnscope: could not start {name}: {error}");
    }
}

/// Write `out` as a line. Once standard output has closed, the app has gone,
/// and so does this.
fn write(out: &Out<'_>) {
    let Ok(mut line) = serde_json::to_string(out) else {
        return;
    };
    line.push('\n');
    let mut stdout = std::io::stdout().lock();
    if stdout
        .write_all(line.as_bytes())
        .and_then(|()| stdout.flush())
        .is_err()
    {
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Asked, Out, Request, Told, Week, read_request};

    fn contract(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../contract")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn every_request_in_the_contract_is_understood() {
        let asked: Vec<Asked> = contract("in.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(matches!(
            &asked[0],
            Asked { id: 1, request: Request::Hide { account, hidden: true } } if account == "api:openrouter"
        ));
        assert!(matches!(&asked[1].request, Request::Connect { agent } if agent == "codex"));
        assert!(matches!(asked[2].request, Request::Panel { open: true }));
        assert_eq!(asked.len(), 3);
        // One it doesn't know is refused, not taken for another, and keeps
        // its id to be answered by.
        assert!(matches!(
            read_request(r#"{"id":4,"do":"check_update"}"#),
            Err((Some(4), _))
        ));
        assert!(matches!(read_request("not json"), Err((None, _))));
    }

    #[test]
    fn every_line_written_is_the_contract() {
        let lines = [
            Out::Alert(Told {
                account: "claude:a".into(),
                title: "Claude Max".into(),
                label: Some("joey@example.com".into()),
                limit: "5 hours".into(),
                scope: None,
                kind: "running_out",
                at: Some("2026-09-30T14:00:00Z".into()),
                left: Some(20.0),
            }),
            Out::Recap(vec![Week {
                account: "claude:a".into(),
                title: "Claude Max".into(),
                label: Some("joey@example.com".into()),
                limit: "Weekly".into(),
                used: 84.0,
                used_up_at: None,
                project: Some("atlas".into()),
            }]),
            Out::Reply {
                id: 2,
                error: Some("codex isn't on this Mac's PATH".into()),
            },
            Out::Reply { id: 3, error: None },
        ];
        let written: String = lines
            .iter()
            .map(|line| serde_json::to_string(line).unwrap() + "\n")
            .collect();
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contract/out.jsonl");
        if std::env::var_os("TURNSCOPE_WRITE_CONTRACT").is_some() {
            std::fs::write(&path, &written).unwrap();
        }
        assert_eq!(written, contract("out.jsonl"));
    }
}
