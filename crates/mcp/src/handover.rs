//! Handing a session over to the server an update leaves.
//!
//! An agent starts `turnscope mcp` once and keeps it for as long as the
//! agent runs, which can be days. An update replaces this program under it,
//! and the new app's engine moves the ledger and the cache on to what this
//! version can't read, so from then on every call would fail until the agent
//! started the server again. On 2026-10-03, a Claude Code session started
//! under 0.1.2 failed every call once the app had updated itself to 0.1.3.
//!
//! So once a session is initialized, the server looks at its program on disk
//! before each message it reads ([`Program`]): a look at the file's
//! metadata. Once another file is there, as an update leaves, it starts that
//! file's `mcp` with the options it was itself started with and hands the
//! session over ([`Successor`]). It initializes the new server as the client
//! initialized it, tells the client its tools and prompts may have changed,
//! so that it lists them again, and from then on passes every line between
//! the two, unread, until the client closes its input. The new server
//! inherits this one's environment and folder, and has the agent among its
//! ancestors, so it knows who is asking as this one did ([`crate::Caller`]).
//!
//! A new server that doesn't start, or doesn't answer `initialize` within
//! [`STARTING`] with the revision this one speaks, is stopped, and this one
//! goes on answering, without trying that file again; why goes to standard
//! error, which agents keep in their logs. Once handed over, a new server
//! that stops leaves the client without one, as any server that stops does:
//! this one stops with it, so the client sees its server stop and fails the
//! requests it was waiting on, rather than wait on them for good.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::{Map, Value, json};

/// How long a new server has to answer `initialize`. It opens the engine
/// first, which builds the cache again when the update changed what it
/// holds.
const STARTING: Duration = Duration::from_secs(20);

/// The id the handover's own `initialize` goes under, whose answer the
/// client never sees.
const HANDOVER: &str = "turnscope-handover";

/// The longest answer to `initialize` read, in bytes: the instructions and
/// a few fields, far shorter.
const LONGEST_INTRODUCTION: u64 = 1 << 16;

/// A file, as told apart from another put at its path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    length: u64,
    /// Seconds and nanoseconds.
    modified: (i64, i64),
}

impl Stamp {
    /// The file at `path` as it stands, following links; `None` when it
    /// can't be looked at, as while an update moves it.
    fn of(path: &Path) -> Option<Stamp> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Stamp {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.size(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
        })
    }
}

/// This program on disk, and the file last seen at its path.
#[derive(Debug)]
pub(crate) struct Program {
    path: PathBuf,
    /// The file the server started from, or the last one a handover to
    /// failed.
    seen: Stamp,
}

impl Program {
    /// The program at `path`, as the file there stands now; `None` when it
    /// can't be looked at, and no handover is ever tried.
    pub(crate) fn at(path: &Path) -> Option<Program> {
        Some(Program {
            path: path.to_owned(),
            seen: Stamp::of(path)?,
        })
    }

    /// A server started from the file now at the program's path, for a
    /// client whose `initialize` gave `initialize` and speaks `revision`,
    /// when another file is there than the one last seen and the server it
    /// starts answers. `None` otherwise: a file the handover to failed is
    /// seen, and not tried again.
    pub(crate) fn successor(
        &mut self,
        options: &[String],
        initialize: &Map<String, Value>,
        revision: &str,
    ) -> Option<Successor> {
        let now = Stamp::of(&self.path).filter(|now| *now != self.seen)?;
        self.seen = now;
        match Successor::start(&self.path, options, initialize, revision) {
            Ok(successor) => Some(successor),
            Err(why) => {
                eprintln!(
                    "turnscope: {} was updated, and this server goes on answering: {why}",
                    self.path.display()
                );
                None
            }
        }
    }
}

/// A newer server, initialized, that the session is handed over to.
#[derive(Debug)]
pub(crate) struct Successor {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Successor {
    /// The server `path` starts as `mcp` with `options`, initialized as
    /// `initialize` asks, once it has answered with `revision`; or why not,
    /// having stopped it.
    fn start(
        path: &Path,
        options: &[String],
        initialize: &Map<String, Value>,
        revision: &str,
    ) -> Result<Successor, String> {
        let mut child = Command::new(path)
            .arg("mcp")
            .args(options)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| format!("it could not be started: {error}"))?;
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            stop(child);
            return Err("its input and output were not given to this server".into());
        };
        match introduce(input, output, initialize, revision) {
            Ok((input, output)) => Ok(Successor {
                child,
                input,
                output,
            }),
            Err(why) => {
                stop(child);
                Err(why)
            }
        }
    }

    /// Tell the client that its tools and prompts may have changed; pass the
    /// new server `pending`, the line that found this server out of date,
    /// and every line `input` holds after it, and pass the client every line
    /// the new server writes, until the new server's output ends. That is
    /// once `input` has ended and the new server stopped with it; or the new
    /// server stopped first, and the session ends with it.
    ///
    /// `input` is read on a thread of its own, since nothing ends a read of
    /// a client that stays open: once the new server has stopped first, the
    /// relay returns without it, and the thread ends with this process.
    ///
    /// # Errors
    ///
    /// Returns an error when the new server stopped while the client was
    /// still connected, which ends this server too, so that the client sees
    /// its server stop rather than wait on requests no one will answer; and
    /// otherwise the error passing a line gave.
    pub(crate) fn relay(
        self,
        pending: Vec<u8>,
        mut input: impl BufRead + Send + 'static,
        output: &mut impl Write,
    ) -> io::Result<()> {
        let Successor {
            mut child,
            input: mut onward,
            output: mut back,
        } = self;
        for method in [
            "notifications/tools/list_changed",
            "notifications/prompts/list_changed",
        ] {
            serde_json::to_writer(&mut *output, &json!({"jsonrpc": "2.0", "method": method}))?;
            output.write_all(b"\n")?;
        }
        output.flush()?;
        let (sent, client_ended) = mpsc::channel();
        thread::spawn(move || {
            let passed_on = onward
                .write_all(&pending)
                .and_then(|()| io::copy(&mut input, &mut onward))
                .map(drop);
            // Said before the new server's input closes, so that once its
            // output ends, which it does only after, the relay knows which
            // stopped first.
            sent.send(passed_on).ok();
            drop(onward);
        });
        let passed_back = lines(&mut back, output);
        if passed_back.is_err() {
            // The client no longer reads, so nothing the new server says
            // goes anywhere.
            stop(child);
            return passed_back;
        }
        let status = child.wait()?;
        match client_ended.try_recv() {
            Ok(passed_on) if status.success() => passed_on.and(passed_back),
            // Stopped before the client's input ended, or failing as a line
            // was passed to it.
            _ => Err(io::Error::other(format!(
                "the newer server this session was handed over to stopped ({status}), so \
                 this one stops with it"
            ))),
        }
    }
}

/// Send `initialize` to a new server on `input` and read its answer from
/// `output`, then tell it the session is initialized: its input and output
/// once it answered with `revision`, or why not.
fn introduce(
    mut input: ChildStdin,
    output: ChildStdout,
    initialize: &Map<String, Value>,
    revision: &str,
) -> Result<(ChildStdin, BufReader<ChildStdout>), String> {
    let asked =
        json!({"jsonrpc": "2.0", "id": HANDOVER, "method": "initialize", "params": initialize});
    writeln!(input, "{asked}").map_err(|error| format!("it took no message: {error}"))?;
    // The answer is read on a thread of its own, so that a server that
    // never answers is given up on. Once it is, the thread's read ends as
    // the server is stopped, and what it read goes nowhere.
    let (sent, answered) = mpsc::channel();
    thread::spawn(move || {
        let mut output = BufReader::new(output);
        let mut line = Vec::new();
        let read = Read::take(&mut output, LONGEST_INTRODUCTION).read_until(b'\n', &mut line);
        sent.send((output, read.map(|_| line))).ok();
    });
    let (output, line) = answered
        .recv_timeout(STARTING)
        .map_err(|_| format!("it did not answer within {} s", STARTING.as_secs()))?;
    let line = line.map_err(|error| format!("its answer could not be read: {error}"))?;
    let answer: Value = serde_json::from_slice(&line)
        .map_err(|_| "it answered initialize with something other than JSON".to_owned())?;
    let spoken = answer["result"]["protocolVersion"].as_str();
    if answer["id"] != HANDOVER || spoken != Some(revision) {
        return Err(format!(
            "it answered initialize with {spoken:?}, not the revision {revision} this session speaks"
        ));
    }
    writeln!(
        input,
        r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
    )
    .map_err(|error| format!("it took no message: {error}"))?;
    Ok((input, output))
}

/// Pass what `from` writes to `to` a line at a time, each as soon as it is
/// whole, until `from` ends.
fn lines(from: &mut impl BufRead, to: &mut impl Write) -> io::Result<()> {
    let mut line = Vec::new();
    loop {
        line.clear();
        if from.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        to.write_all(&line)?;
        to.flush()?;
    }
}

/// Stop a new server the session isn't handed over to, saying so if it
/// can't be.
fn stop(mut child: Child) {
    if let Err(error) = child.kill().and_then(|()| child.wait().map(drop)) {
        eprintln!("turnscope: a newer server could not be stopped: {error}");
    }
}
