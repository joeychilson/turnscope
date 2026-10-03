//! Sharing a data directory between processes.
//!
//! The app, an MCP server for each agent session that starts one, and the
//! command line can all open the same data directory. Any of them reads at any
//! time: SQLite's write-ahead log gives each query a consistent view while
//! another process writes. Writing is one process at a time, and at most one
//! keeps the directory current.
//!
//! - **Reading.** A question of several statements runs in one read
//!   transaction, and questions are answered from read-only connections of
//!   their own, kept between questions, never from those writes hold.
//!   Measured 2026-09-24 while a first read of this Mac's history ran:
//!   asking for limits waited the whole read, 60 s, on the writer's
//!   connection, and from its own, the 95th percentile of 963 askings was
//!   0.5 ms for limits, 22 ms for a page of sessions and 53 ms for a
//!   conversation.
//! - **Writing.** Every write to the ledger or the cache, from reading an
//!   artifact to taking new prices, holds `writer.lock` as well as the
//!   process's own ledger mutex. Reading an artifact takes its checkpoint,
//!   parses what follows and records it with the next checkpoint; two
//!   processes doing that at once recorded lines twice, 79 prompts for 60 in
//!   the test that checks this. A writer takes the prices again whenever the
//!   ledger's catalog is newer than the one its prices came from, since
//!   another process may have taken it in.
//! - **Keeping.** A running engine holds `keeper.lock` for as long as it runs,
//!   and only that process watches for changes, asks for prices and reads
//!   limits. A process that finds the lock held knows what it reads is kept
//!   current; one that finds it free reads what changed for itself before it
//!   answers. A second running engine waits and takes over when the first
//!   stops: at once, since the first touches `keeper.lock` as it lets go and
//!   the second watches the file, and within two seconds of a keeper that
//!   ended without, as one that crashed does.
//!
//! Both are `flock` locks, which the system releases however a process ends.
//! Each database is shared through its own write-ahead log, and every
//! connection waits its turn while another writes.
//!
//! **Versions.** Builds of different versions can share a directory, as an
//! MCP server started before an update does with the app after it. An
//! artifact a newer reader recorded is left alone by an older one, as is
//! every artifact of an agent a build has no reader for, which it never marks
//! gone; and an account of a subscription, or anything of an agent, that a
//! build doesn't know is passed over in what it answers. Builds of different
//! cache schemas can't share one cache, and share one change log, so each
//! writer checks before catching up that the cache file at its path is still
//! the one it opened, and opens the one there now if another build replaced
//! it, refusing a newer one; so it never writes to a file that is gone, nor
//! prunes the change log for a cache no one reads. Readers check the same
//! before each question, one `stat`: connections kept to a replaced cache
//! are let go, since they would go on answering as that file last stood,
//! and one from a newer build is refused with
//! [`Error::NewerCache`], so an older process
//! answers nothing it would misread rather than something stale.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, OpenFlags};

use crate::error::{Error, Result};

/// The lock every writer holds.
pub(crate) const WRITER: &str = "writer.lock";

/// The lock a running engine holds.
pub(crate) const KEEPER: &str = "keeper.lock";

/// How long a connection waits for another process's write to finish.
const BUSY: Duration = Duration::from_secs(5);

/// A lock file in the data directory.
///
/// A lock belongs to an open file, and taking it again through the same file
/// succeeds without waiting, so one handle must not be shared by code that
/// takes it independently. Every use of a `Lock` is serialized by its owner.
#[derive(Debug)]
pub(crate) struct Lock {
    path: PathBuf,
    file: File,
}

impl Lock {
    /// Open the lock file at `path`, creating it if need be.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file cannot be opened or created.
    pub(crate) fn open(path: &Path) -> Result<Lock> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(|error| Error::io(path, error))?;
        Ok(Lock {
            path: path.to_path_buf(),
            file,
        })
    }

    /// Wait until no other process holds the lock, and hold it until the
    /// returned guard is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the system refuses the lock.
    pub(crate) fn hold(&self) -> Result<Held<'_>> {
        self.file
            .lock()
            .map_err(|error| Error::io(&self.path, error))?;
        Ok(Held { file: &self.file })
    }

    /// Hold the lock if no other process does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the system refuses the lock for a reason
    /// other than another holder.
    pub(crate) fn try_hold(&self) -> Result<Option<Held<'_>>> {
        match self.file.try_lock() {
            Ok(()) => Ok(Some(Held { file: &self.file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(Error::io(&self.path, error)),
        }
    }

    /// Mark the lock file changed, as a process that let go of the lock does
    /// for one watching the file to try it again at once. Letting go itself
    /// changes nothing a watcher sees.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the file's time can't be set.
    pub(crate) fn touch(&self) -> Result<()> {
        self.file
            .set_modified(SystemTime::now())
            .map_err(|error| Error::io(&self.path, error))
    }
}

/// Whether some process, this one included, holds the lock at `path`, judged
/// through a handle of its own so that no handle in use is disturbed.
///
/// The probe takes the lock shared, which only a holder's refuses, so two
/// processes probing at once don't take each other for holders. It lets go
/// as the handle closes.
///
/// # Errors
///
/// Returns [`Error::Io`] when the lock file cannot be opened or the system
/// refuses the lock for a reason other than another holder.
pub(crate) fn held(path: &Path) -> Result<bool> {
    match Lock::open(path)?.file.try_lock_shared() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(Error::io(path, error)),
    }
}

/// Open the database at `path` to write to it, waiting its turn from the
/// first statement on.
///
/// # Errors
///
/// Returns [`Error::Ledger`] when it cannot be opened.
pub(crate) fn open(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(BUSY)?;
    Ok(connection)
}

/// Share `connection`'s database through its write-ahead log, so that every
/// process reads while one writes. A writing connection keeps a 64 MB page
/// cache, where SQLite's 2 MB default read the indexes' pages again as they
/// grew, and gives its pages back once each piece of work is done. The log
/// is cut back to 4 MB each time it starts over: it keeps the size of the
/// largest write otherwise, which left the cache's at 112 MB beside a
/// 111 MB cache after a first read (2026-09-30).
///
/// # Errors
///
/// Returns [`Error::Ledger`] when the database refuses.
pub(crate) fn share(connection: &Connection) -> Result<()> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "cache_size", -65536)?;
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    connection.pragma_update(None, "journal_size_limit", 4 << 20)?;
    Ok(())
}

/// A file SQLite keeps beside the database at `path`: its write-ahead log,
/// `-wal`, or the log's index, `-shm`.
pub(crate) fn side_file(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Open the database at `path` for reading only, as it stands: nothing is
/// created, and every write fails.
///
/// # Errors
///
/// Returns [`Error::Ledger`] when it cannot be opened.
pub(crate) fn reader(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(BUSY)?;
    Ok(connection)
}

/// A lock, held until dropped.
#[derive(Debug)]
pub(crate) struct Held<'a> {
    file: &'a File,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        // Unlocking fails only for a descriptor that is no longer open, and
        // closing a descriptor releases its lock.
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::{Lock, held};

    #[test]
    fn one_holder_at_a_time_and_released_when_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("writer.lock");
        let first = Lock::open(&path).unwrap();
        let second = Lock::open(&path).unwrap();
        assert!(!held(&path).unwrap());

        let holding = first.hold().unwrap();
        assert!(held(&path).unwrap());
        assert!(second.try_hold().unwrap().is_none());

        drop(holding);
        assert!(!held(&path).unwrap());
        assert!(second.try_hold().unwrap().is_some());
    }

    #[test]
    fn a_process_asking_whether_the_lock_is_held_is_no_holder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keeper.lock");
        // Another process asking at the same moment holds it as a probe does.
        let asking = Lock::open(&path).unwrap();
        asking.file.try_lock_shared().unwrap();
        assert!(!held(&path).unwrap());
    }
}
