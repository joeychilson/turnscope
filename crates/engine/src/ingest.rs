//! Taking in history: finding the agents' artifacts, deciding how much of each
//! to read, reading in parallel, and writing to the ledger on one thread.
//!
//! **How much to read.** An artifact unchanged since its last read, by the
//! same reader version, is not read. A log that has only grown is read on
//! from where the last read stopped: it is the same file, longer, opening
//! with the same bytes, and the offset is still the start of a line with the
//! same bytes before it, so a 336 MB log that gains a line costs a line.
//! Otherwise what it said is dropped and it is read again from the start: a
//! file replaced, cut short, cut short and written past where it ended, or
//! changed without growing, since an append always grows it. Checking all of
//! what was read would cost the whole file for each line, so a log rewritten
//! only between its ends and grown as well is not seen until it is read
//! whole again; no agent has been seen to do that. A document is read whole
//! at every change. A database is read on from its reader's own marks, and a
//! change to its write-ahead log is a change to it. An artifact read again
//! from the start for a new reader version, or back after it was gone, is
//! read whole.
//!
//! **Reading and writing.** Artifacts are read in parallel on rayon's pool,
//! newest first by when each last changed, and one more thread writes what
//! they found, in groups that commit every [`GROUP_FOR`] or [`GROUP_OF`]
//! records, whichever comes first. The writer is what a first read waits on:
//! one transaction per file spent 5.5 s of 15 committing (2026-09-24),
//! rewriting the index pages files share. Each group commits observations
//! and checkpoints together, so a crash can neither count anything twice nor
//! lose it. Readers hand the writer at most [`QUEUE`] finished reads, and
//! when a write fails, reading stops and the scan returns the failure.
//!
//! In a release build, a first read of Claude Code's 1.92 GB took 0.9 s
//! (2026-09-24); of all 7.55 GB of this Mac's history, 1,905 files, 3.9 to
//! 4.3 s, and a scan with nothing changed under 10 ms (2026-09-29, with the
//! Mac's load around 3).
//!
//! **What can't be read.** An artifact that can't be read is tried again at
//! every read, from where its last good read stopped. A folder that can't be
//! listed, or a file that can't be examined, is reported rather than passed
//! over, and an artifact there isn't marked gone, since that says nothing of
//! whether its sessions are still there.
//!
//! **When files disappear.** An artifact not found is marked absent. What it
//! said stays in the ledger and in every total, and only whether its
//! sessions are still there is worked out again; its conversation can no
//! longer be opened. If it comes back, it is read again from the start.

use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant as Clock};

use rayon::prelude::*;

use crate::agent::{Agent, AgentReader, ArtifactKind, Batch, Checkpoint};
use crate::error::Result;
use crate::jsonl;
use crate::ledger::{FileState, Known, Ledger, Read};
use crate::time::Instant;

/// How many finished reads may wait for the writer before readers pause.
const QUEUE: usize = 64;

/// How many bytes of finished reads may wait for the writer, as
/// [`Batch::weight`] weighs them, before readers pause: 256 MiB, where a
/// first read of everything here peaked at 292 MB in all (2026-09-27). One
/// read always goes, however large, so none waits for room it can't have.
const WAITING: usize = 256 << 20;

/// The longest reads wait to be written together: each group of reads is one
/// transaction, which writes the pages its reads share once rather than once
/// for each read.
const GROUP_FOR: Duration = Duration::from_millis(100);

/// The most records a group of reads holds before it is written.
const GROUP_OF: usize = 10_000;

/// What a scan did.
#[derive(Clone, Debug, Default)]
pub struct ScanReport {
    /// Artifacts read for the first time.
    pub new: usize,
    /// Artifacts read on from where the last read stopped.
    pub resumed: usize,
    /// Artifacts read again from the start, because they were rewritten or
    /// their reader changed.
    pub reread: usize,
    /// Artifacts unchanged since they were last read.
    pub unchanged: usize,
    /// Artifacts found missing, whose history is kept.
    pub absent: usize,
    /// Bytes read.
    pub bytes: u64,
    /// Artifacts that could not be read, and why. Each is tried again on the
    /// next scan from where its last successful read stopped.
    pub failed: Vec<(PathBuf, String)>,
    /// How long the scan took.
    pub elapsed: Duration,
}

/// An artifact found on disk.
struct Found {
    /// Which reader it belongs to.
    reader: usize,
    path: PathBuf,
    kind: ArtifactKind,
    file: FileState,
}

impl Found {
    /// The artifact of `kind` at `path`, as `metadata` describes it, which
    /// `reader` reads.
    fn new(reader: usize, path: PathBuf, kind: ArtifactKind, metadata: &fs::Metadata) -> Found {
        Found {
            reader,
            file: state_of(&path, kind, metadata),
            path,
            kind,
        }
    }
}

/// How much of an artifact to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Plan {
    New,
    Resume,
    Reread,
}

/// An artifact to read, and from where.
struct Job {
    found: Found,
    plan: Plan,
    from: Checkpoint,
}

/// Look through `roots`, each a reader's by its place among `readers`, read
/// what is new or changed, and write it to `ledger`, calling `written` after
/// each group of reads is.
///
/// Artifacts are read in parallel, most recently modified first, and written
/// in groups as they finish, at least every tenth of a second, so the newest
/// history is in the ledger long before the oldest. A failure to read one artifact is reported
/// and does not stop the rest. An artifact read before and not found now is
/// marked absent, and what it said is kept.
///
/// # Errors
///
/// Returns an error when the ledger cannot be read or written. Reads written
/// before the failure are kept.
pub(crate) fn scan(
    readers: &[Box<dyn AgentReader>],
    roots: &[(usize, PathBuf)],
    ledger: &mut Ledger,
    written: impl FnMut(&mut Ledger) -> Result<()>,
) -> Result<ScanReport> {
    let mut report = ScanReport::default();
    let found = discover(readers, roots, &mut report);
    process(readers, ledger, found, None, report, written)
}

/// Read what changed at `paths`, as the file system reported them, within
/// `roots`, and write it to `ledger`. A path that is gone marks absent every
/// artifact at or under it; a database's write-ahead log stands for the
/// database.
///
/// # Errors
///
/// As [`scan`].
pub(crate) fn scan_paths(
    readers: &[Box<dyn AgentReader>],
    roots: &[(usize, PathBuf)],
    ledger: &mut Ledger,
    paths: &[PathBuf],
) -> Result<ScanReport> {
    let mut report = ScanReport::default();
    let (found, gone) = locate(readers, roots, paths, &mut report);
    let within: Vec<PathBuf> = found
        .iter()
        .map(|found| found.path.clone())
        .chain(gone.iter().cloned())
        .collect();
    process(readers, ledger, found, Some(&within), report, |_| Ok(()))
}

/// Plan, read and write `found`, and mark absent every artifact `readers`
/// read before that was not found, calling `written` after each group of
/// reads is written. Only artifacts at or under `within`, when given, are
/// looked up, as `found` and the paths gone are all there.
fn process(
    readers: &[Box<dyn AgentReader>],
    ledger: &mut Ledger,
    found: Vec<Found>,
    within: Option<&[PathBuf]>,
    mut report: ScanReport,
    written: impl FnMut(&mut Ledger) -> Result<()>,
) -> Result<ScanReport> {
    let started = Clock::now();
    let agents: Vec<Agent> = readers.iter().map(|reader| reader.agent()).collect();
    let mut known = ledger.known(&agents, within)?;
    let mut jobs = Vec::new();
    for found in found {
        // The ledger keeps paths as text, so a file whose name isn't UTF-8,
        // which APFS allows none of but another volume may, can't be kept;
        // it is said, and the rest are read.
        if found.path.to_str().is_none() {
            report
                .failed
                .push((found.path, "its path is not UTF-8".to_owned()));
            continue;
        }
        let previous = known.remove(&found.path);
        let reader = &readers[found.reader];
        match plan(reader.as_ref(), &found, previous.as_ref()) {
            Ok(Some((plan, from))) => jobs.push(Job { found, plan, from }),
            Ok(None) => report.unchanged += 1,
            Err(error) => report.failed.push((found.path, error.to_string())),
        }
    }
    // An artifact not found is gone, unless it is at or under what couldn't
    // be looked at, which says nothing of whether it is still there.
    let absent: Vec<i64> = known
        .iter()
        .filter(|(path, artifact)| {
            artifact.present
                && !report
                    .failed
                    .iter()
                    .any(|(unseen, _)| path.starts_with(unseen))
        })
        .map(|(_, artifact)| artifact.id)
        .collect();
    report.absent = absent.len();
    ledger.mark_absent(&absent)?;

    jobs.sort_by_key(|job| std::cmp::Reverse(job.found.file.modified));
    let (sender, finished) = mpsc::sync_channel::<Finished>(QUEUE);
    let budget = Budget::new(WAITING);
    std::thread::scope(|scope| {
        let waiting = &budget;
        scope.spawn(move || {
            // Reading stops once the writer has gone, as it does when a write
            // fails, since nothing more can be written.
            jobs.into_par_iter()
                .try_for_each_with(sender, |sender, job| {
                    let read = read(readers, job);
                    waiting.take(weight(&read));
                    sender.send(read).ok()
                })
        });
        let _closing = Closing(&budget);
        write(finished, readers, ledger, &mut report, written, &budget)
    })?;
    report.elapsed = started.elapsed();
    Ok(report)
}

/// A read of one artifact, finished or failed.
type Finished = (Job, Result<(Checkpoint, Batch)>);

/// What a finished read weighs while it waits to be written.
fn weight(read: &Finished) -> usize {
    read.1.as_ref().map_or(0, |(_, batch)| batch.weight())
}

/// Room for finished reads to wait in, by weight.
struct Budget {
    state: Mutex<Waiting>,
    room: Condvar,
    most: usize,
}

/// What waits for the writer.
#[derive(Default)]
struct Waiting {
    /// The weight of the reads waiting.
    weight: usize,
    /// Whether the writer has gone, so none waits for room again.
    closed: bool,
}

impl Budget {
    fn new(most: usize) -> Budget {
        Budget {
            state: Mutex::new(Waiting::default()),
            room: Condvar::new(),
            most,
        }
    }

    fn waiting(&self) -> MutexGuard<'_, Waiting> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wait until `weight` more fits, or nothing waits, or the writer has
    /// gone, and count it in.
    fn take(&self, weight: usize) {
        let mut waiting = self.waiting();
        while waiting.weight > 0
            && waiting.weight.saturating_add(weight) > self.most
            && !waiting.closed
        {
            waiting = self
                .room
                .wait(waiting)
                .unwrap_or_else(PoisonError::into_inner);
        }
        waiting.weight = waiting.weight.saturating_add(weight);
    }

    /// Count `weight` out, as written.
    fn give(&self, weight: usize) {
        let mut waiting = self.waiting();
        waiting.weight = waiting.weight.saturating_sub(weight);
        self.room.notify_all();
    }

    /// Let every reader waiting for room go, as the writer has gone.
    fn close(&self) {
        self.waiting().closed = true;
        self.room.notify_all();
    }
}

/// Closes the budget when the writer has gone, however it ends, so no
/// reader waits for room that will never come.
struct Closing<'a>(&'a Budget);

impl Drop for Closing<'_> {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// Read the artifact `job` names from where it says.
fn read(readers: &[Box<dyn AgentReader>], mut job: Job) -> Finished {
    let reader = &readers[job.found.reader];
    let mut batch = Batch::default();
    // A log's opening bytes are kept as they were when read, and those
    // before where the read stopped, to tell next time whether it was
    // rewritten since. Any other artifact is read again whole, or on from
    // its reader's own marks, whatever became of its bytes.
    let log = job.found.kind == ArtifactKind::Log;
    let mut read = || {
        if log {
            job.found.file.fingerprint = jsonl::opening(&job.found.path)?;
        }
        let end = reader.read(&job.found.path, &job.from, &mut batch)?;
        if log {
            job.found.file.closing = Some(jsonl::closing(&job.found.path, end.offset)?);
        }
        Ok(end)
    };
    let result = read();
    (job, result.map(|end| (end, batch)))
}

/// Write the reads `finished` gives to `ledger` as they come, in groups,
/// calling `written` after each group is written, and count them in `report`.
///
/// It owns `finished`, so that once it returns, as it does when a write
/// fails, readers waiting for room in the queue are let go rather than
/// waiting for it forever.
fn write(
    finished: mpsc::Receiver<Finished>,
    readers: &[Box<dyn AgentReader>],
    ledger: &mut Ledger,
    report: &mut ScanReport,
    mut written: impl FnMut(&mut Ledger) -> Result<()>,
    budget: &Budget,
) -> Result<()> {
    let mut group: Vec<Read> = Vec::new();
    // Write the group, and give back the room its reads took.
    let mut commit = |ledger: &mut Ledger, group: &mut Vec<Read>| -> Result<()> {
        ledger.write(group, Instant::now())?;
        budget.give(group.iter().map(|read| read.batch.weight()).sum());
        group.clear();
        written(ledger)
    };
    let mut records = 0;
    let mut since = Clock::now();
    loop {
        let received = if group.is_empty() {
            finished.recv().ok()
        } else {
            match finished.recv_timeout(GROUP_FOR.saturating_sub(since.elapsed())) {
                Ok(received) => Some(received),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    commit(ledger, &mut group)?;
                    records = 0;
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => None,
            }
        };
        let Some((job, result)) = received else { break };
        match result {
            Ok((checkpoint, batch)) => {
                let reader = &readers[job.found.reader];
                report.bytes = report
                    .bytes
                    .saturating_add(checkpoint.offset.saturating_sub(job.from.offset));
                match job.plan {
                    Plan::New => report.new += 1,
                    Plan::Resume => report.resumed += 1,
                    Plan::Reread => report.reread += 1,
                }
                if group.is_empty() {
                    since = Clock::now();
                }
                records += batch.records();
                group.push(Read {
                    agent: reader.agent(),
                    path: job.found.path,
                    kind: job.found.kind,
                    file: job.found.file,
                    reader_version: reader.version(),
                    reset: job.plan == Plan::Reread,
                    checkpoint,
                    batch,
                });
                if records >= GROUP_OF || since.elapsed() >= GROUP_FOR {
                    commit(ledger, &mut group)?;
                    records = 0;
                }
            }
            Err(error) => report.failed.push((job.found.path, error.to_string())),
        }
    }
    if !group.is_empty() {
        commit(ledger, &mut group)?;
    }
    Ok(())
}

/// The artifacts at `paths`, and the paths that are gone.
///
/// The file system reports paths with symbolic links resolved, as
/// `/private/var` for `/var`, so a path under a root's resolved form is taken
/// as under the root itself.
fn locate(
    readers: &[Box<dyn AgentReader>],
    roots: &[(usize, PathBuf)],
    paths: &[PathBuf],
    report: &mut ScanReport,
) -> (Vec<Found>, Vec<PathBuf>) {
    let roots: Vec<(usize, PathBuf, Option<PathBuf>)> = roots
        .iter()
        .map(|(index, root)| {
            let resolved = fs::canonicalize(root)
                .ok()
                .filter(|resolved| resolved != root);
            (*index, root.clone(), resolved)
        })
        .collect();
    // A path as it is known under its root, with the root it is under.
    let place = |path: &Path| -> Option<(usize, PathBuf, PathBuf)> {
        roots.iter().find_map(|(index, root, resolved)| {
            if path.starts_with(root) {
                return Some((*index, root.clone(), path.to_path_buf()));
            }
            let resolved = resolved.as_ref()?;
            let within = path.strip_prefix(resolved).ok()?;
            Some((*index, root.clone(), root.join(within)))
        })
    };
    let mut found = Vec::new();
    let mut gone = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in paths {
        let Some((reader, root, candidate)) = place(&database_of(path)) else {
            continue;
        };
        if !seen.insert(candidate.clone()) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                gone.push(candidate);
                continue;
            }
            Err(error) => {
                report.failed.push((candidate, error.to_string()));
                continue;
            }
        };
        if metadata.is_dir() {
            // A directory made or moved in whole, as a root that has just
            // appeared or a project's folder moved into one, is what its
            // change names, not what it holds.
            walk(readers, reader, &root, candidate, &mut found, report);
        } else if metadata.is_file()
            && let Some(kind) = readers[reader].classify(&root, &candidate)
        {
            found.push(Found::new(reader, candidate, kind, &metadata));
        }
    }
    // A file named and walked to as well is read once.
    let mut named = std::collections::HashSet::new();
    found.retain(|artifact| named.insert(artifact.path.clone()));
    (found, gone)
}

/// The database a SQLite side file belongs to: `x.db-wal`, `x.db-shm` and
/// `x.db-journal` stand for `x.db`; any other path for itself.
fn database_of(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    for suffix in ["-wal", "-shm", "-journal"] {
        if let Some(database) = text.strip_suffix(suffix) {
            return PathBuf::from(database);
        }
    }
    path.to_path_buf()
}

/// Every artifact under `roots`, each a reader's by its place among
/// `readers`. A root that does not exist is passed over; a directory that
/// cannot be read is reported.
fn discover(
    readers: &[Box<dyn AgentReader>],
    roots: &[(usize, PathBuf)],
    report: &mut ScanReport,
) -> Vec<Found> {
    let mut found = Vec::new();
    for (index, root) in roots {
        let index = *index;
        // A root may be a single file rather than a directory.
        if let Ok(metadata) = fs::symlink_metadata(root)
            && metadata.is_file()
        {
            if let Some(artifact) = readers[index].classify(root, root) {
                found.push(Found::new(index, root.clone(), artifact, &metadata));
            }
            continue;
        }
        walk(readers, index, root, root.clone(), &mut found, report);
    }
    found
}

/// Every artifact of `readers[index]` under the directory `from`, within its
/// `root`, added to `found`. A directory that cannot be read is reported.
fn walk(
    readers: &[Box<dyn AgentReader>],
    index: usize,
    root: &Path,
    from: PathBuf,
    found: &mut Vec<Found>,
    report: &mut ScanReport,
) {
    let reader = &readers[index];
    let mut pending = vec![from];
    while let Some(dir) = pending.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                report.failed.push((dir, error.to_string()));
                continue;
            }
        };
        for entry in entries {
            // What can't be looked at is said, so that what it holds is
            // neither passed over unsaid nor taken as gone.
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    report.failed.push((dir.clone(), error.to_string()));
                    continue;
                }
            };
            let path = entry.path();
            // Symbolic links are not followed, so a link cannot lead a scan
            // in circles or outside the agent's own directory.
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(error) => {
                    report.failed.push((path, error.to_string()));
                    continue;
                }
            };
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file()
                && let Some(artifact) = reader.classify(root, &path)
            {
                match entry.metadata() {
                    Ok(metadata) => found.push(Found::new(index, path, artifact, &metadata)),
                    Err(error) => report.failed.push((path, error.to_string())),
                }
            }
        }
    }
}

/// A file's identity and size, without its fingerprint or closing, which are read only
/// when the file has changed.
///
/// A SQLite database in WAL mode takes new writes in its `-wal` file, and its
/// own file changes only when they are checkpointed into it, so a database's
/// size and time include its write-ahead log's.
fn state_of(path: &Path, kind: ArtifactKind, metadata: &fs::Metadata) -> FileState {
    let modified = |metadata: &fs::Metadata| {
        metadata
            .mtime()
            .saturating_mul(1_000_000_000)
            .saturating_add(metadata.mtime_nsec())
    };
    let mut state = FileState {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.size(),
        modified: modified(metadata),
        fingerprint: Vec::new(),
        closing: None,
    };
    if kind == ArtifactKind::Database
        && let Ok(log) = fs::metadata(crate::sharing::side_file(path, "-wal"))
    {
        state.size = state.size.saturating_add(log.size());
        state.modified = state.modified.max(modified(&log));
    }
    state
}

/// How much of `found` to read, and from where; `None` when it has not
/// changed since it was last read.
fn plan(
    reader: &dyn AgentReader,
    found: &Found,
    previous: Option<&Known>,
) -> Result<Option<(Plan, Checkpoint)>> {
    let current = reader.version();
    let Some(previous) = previous else {
        return Ok(Some((Plan::New, Checkpoint::default())));
    };
    // A newer build read it, and is the one to read it again: an older build
    // still running beside it, such as an MCP server started before an update,
    // would otherwise read it back its own way, and each would undo the other.
    if previous.reader_version > current {
        return Ok(None);
    }
    let same_file =
        previous.file.device == found.file.device && previous.file.inode == found.file.inode;
    // A log that holds something its reader read none of, as a subagent's
    // log that waited for the description written beside it, is read on
    // though it hasn't changed: its reader may read it now.
    let unread =
        found.kind == ArtifactKind::Log && previous.checkpoint.offset == 0 && found.file.size > 0;
    if previous.reader_version == current
        && previous.present
        && same_file
        && previous.file.size == found.file.size
        && previous.file.modified == found.file.modified
        && !unread
    {
        return Ok(None);
    }
    // One back after it was gone is read again whole, so that everything it
    // speaks of is worked out again as there.
    if previous.reader_version != current
        || !same_file
        || !previous.present
        || found.kind == ArtifactKind::Document
    {
        return Ok(Some((Plan::Reread, Checkpoint::default())));
    }
    // A database is read on from the reader's own marks.
    if found.kind == ArtifactKind::Database {
        return Ok(Some((Plan::Resume, previous.checkpoint.clone())));
    }
    // A log that changed without growing was rewritten, as was one that
    // was not only appended to.
    if (unread || found.file.size != previous.file.size)
        && jsonl::appended(
            &found.path,
            &previous.file.fingerprint,
            previous.file.closing.as_deref(),
            found.file.size,
            previous.checkpoint.offset,
        )?
    {
        Ok(Some((Plan::Resume, previous.checkpoint.clone())))
    } else {
        Ok(Some((Plan::Reread, Checkpoint::default())))
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::{Budget, Closing, Found, GROUP_OF, Plan, QUEUE, plan, scan};
    use crate::agent::{self, Agent, AgentReader, ArtifactKind, Batch, Checkpoint, DiagnosticKind};
    use crate::error::Result;
    use crate::ledger::{FileState, Known, Ledger};
    use crate::session::SessionKey;
    use crate::transcript::Transcript;

    /// Takes every file under the home directory for a log, and reads from
    /// each as much trouble as fills a group of reads on its own, so the
    /// first read to reach the writer is written at once.
    struct Filling;

    impl AgentReader for Filling {
        fn agent(&self) -> Agent {
            Agent::Pi
        }

        fn version(&self) -> u32 {
            1
        }

        fn roots(&self, folder: &Path) -> Vec<PathBuf> {
            vec![folder.to_path_buf()]
        }

        fn classify(&self, _: &Path, _: &Path) -> Option<ArtifactKind> {
            Some(ArtifactKind::Log)
        }

        fn read(&self, _: &Path, from: &Checkpoint, batch: &mut Batch) -> Result<Checkpoint> {
            for line in 0..GROUP_OF {
                batch.note(DiagnosticKind::Invalid, line.to_string(), 0);
            }
            Ok(from.clone())
        }

        fn conversation(&self, _: &SessionKey, _: &[PathBuf]) -> Result<Transcript> {
            Ok(Transcript::default())
        }
    }

    #[test]
    fn a_scan_whose_write_fails_stops_and_says_so() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        // More artifacts than wait in the queue, so readers wait for room
        // when the writer stops.
        for n in 0..QUEUE * 3 {
            std::fs::write(home.path().join(format!("{n}.jsonl")), "").unwrap();
        }
        let path = data.path().join("ledger.sqlite");
        drop(Ledger::open(&path).unwrap());
        let home = home.path().to_path_buf();
        let (done, scanned) = mpsc::channel();
        std::thread::spawn(move || {
            // Opened only to read, so the first write fails.
            let mut ledger = Ledger::reader(&path).unwrap();
            let readers: Vec<Box<dyn AgentReader>> = vec![Box::new(Filling)];
            let result = scan(&readers, &[(0, home)], &mut ledger, |_| Ok(()));
            done.send(result.is_err()).unwrap();
        });
        let failed = scanned
            .recv_timeout(Duration::from_secs(20))
            .expect("the scan ends");
        assert!(failed);
    }

    #[test]
    fn an_artifact_is_read_again_by_a_newer_reader_but_not_an_older_one() {
        let reader = &agent::readers()[0];
        let file = |size| FileState {
            device: 1,
            inode: 7,
            size,
            modified: 1_000,
            fingerprint: Vec::new(),
            closing: None,
        };
        let found = Found {
            reader: 0,
            path: PathBuf::from("/nonexistent/session.jsonl"),
            kind: ArtifactKind::Log,
            file: file(2_048),
        };
        let known = |reader_version| Known {
            id: 1,
            file: file(1_024),
            checkpoint: Checkpoint {
                offset: 1_024,
                state: Vec::new(),
            },
            reader_version,
            present: true,
        };
        let current = reader.version();

        let older = plan(reader.as_ref(), &found, Some(&known(current - 1))).unwrap();
        assert_eq!(older.map(|(plan, _)| plan), Some(Plan::Reread));
        // A newer build recorded it; this one leaves it for that build.
        let newer = plan(reader.as_ref(), &found, Some(&known(current + 1))).unwrap();
        assert_eq!(newer.map(|(plan, _)| plan), None);
    }

    #[test]
    fn a_log_its_reader_read_none_of_is_read_on_though_unchanged() {
        let reader = &agent::readers()[0];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-a1.jsonl");
        std::fs::write(&path, "{}\n".repeat(682) + "{}").unwrap();
        let file = FileState {
            device: 1,
            inode: 7,
            size: 2_048,
            modified: 1_000,
            fingerprint: Vec::new(),
            closing: None,
        };
        let found = Found {
            reader: 0,
            path,
            kind: ArtifactKind::Log,
            file: file.clone(),
        };
        let known = |offset| Known {
            id: 1,
            file: file.clone(),
            checkpoint: Checkpoint {
                offset,
                state: Vec::new(),
            },
            reader_version: reader.version(),
            present: true,
        };
        // Its reader waited, and read none of its 2,048 bytes.
        let waited = plan(reader.as_ref(), &found, Some(&known(0))).unwrap();
        assert_eq!(waited.map(|(plan, _)| plan), Some(Plan::Resume));
        // Read to its end, it is left alone until it changes.
        let read = plan(reader.as_ref(), &found, Some(&known(2_048))).unwrap();
        assert_eq!(read.map(|(plan, _)| plan), None);
    }

    #[test]
    fn reads_wait_for_room_but_one_always_goes_and_none_once_the_writer_is_gone() {
        let budget = Budget::new(100);
        // A read heavier than all the room goes, since nothing waits.
        budget.take(150);
        // Another waits until that one is written.
        let waited = std::thread::scope(|scope| {
            let reader = scope.spawn(|| {
                let started = std::time::Instant::now();
                budget.take(10);
                started.elapsed()
            });
            std::thread::sleep(Duration::from_millis(100));
            budget.give(150);
            reader.join().unwrap()
        });
        assert!(waited >= Duration::from_millis(90), "{waited:?}");
        // 10 + 80 = 90 fits; 50 more doesn't, and waits until the writer has
        // gone, which lets it go.
        budget.take(80);
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| budget.take(50));
            std::thread::sleep(Duration::from_millis(50));
            drop(Closing(&budget));
            reader.join().unwrap();
        });
    }
}
