//! The engine's failures.

use std::path::{Path, PathBuf};

/// Why the engine could not do what was asked.
///
/// Problems inside an agent's history — an unreadable line, a record of a kind
/// no reader knows, a count out of range — are not errors. They are recorded as
/// diagnostics against the file they came from, and reading goes on.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A file or directory could not be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        /// What was being read or written.
        path: PathBuf,
        /// What the system reported.
        source: std::io::Error,
    },
    /// An agent's own database could not be read.
    #[error("{}: {detail}", path.display())]
    Database {
        /// The database.
        path: PathBuf,
        /// What went wrong.
        detail: String,
    },
    /// A session's conversation can no longer be read: every file that held
    /// it has gone, or the agent deleted it from the database that holds it.
    /// Its usage is still known. A conversation with nothing in it is empty,
    /// not gone.
    #[error("{0}'s conversation is no longer in its agent's files")]
    Gone(String),
    /// The ledger could not be read or written.
    #[error("ledger: {0}")]
    Ledger(#[from] rusqlite::Error),
    /// The ledger was written by a newer version of Turnscope, whose changes
    /// this version cannot read.
    #[error(
        "the ledger is from a newer Turnscope (schema {found}, this version knows up to {known})"
    )]
    NewerLedger {
        /// The schema version the ledger records.
        found: i64,
        /// The newest schema version this build can read.
        known: i64,
    },
    /// The cache was built by a newer version of Turnscope, which works out
    /// what it holds in ways this version doesn't know. It is left for that
    /// version rather than built again, and this one answers nothing from it:
    /// the newer version is the one to run.
    #[error(
        "the cache is from a newer Turnscope (schema {found}, this version knows up to {known}); \
         run the newer version"
    )]
    NewerCache {
        /// The schema version the cache records.
        found: i64,
        /// The newest schema version this build can read.
        known: i64,
    },
    /// A page cursor that no page gave, as [`crate::SessionQuery::after`]
    /// takes one: the asker's mistake, not the cache's.
    #[error("{0:?} is not where a page of sessions ended")]
    Cursor(String),
    /// What was asked can't be done as asked, as hiding an account by what
    /// is no account's id: the asker's mistake, and nothing was changed.
    #[error("{what}: {detail}")]
    Invalid {
        /// What was asked of, as `account`.
        what: &'static str,
        /// Why it can't be done.
        detail: String,
    },
    /// Something the engine itself wrote could not be read back, such as a
    /// reader's saved position in a file.
    #[error("corrupt {what}: {detail}")]
    Corrupt {
        /// What was corrupt.
        what: &'static str,
        /// What was wrong with it.
        detail: String,
    },
}

impl Error {
    /// Something the engine wrote, `what`, read back wrong, as `detail` says.
    pub(crate) fn corrupt(what: &'static str, detail: impl Into<String>) -> Error {
        Error::Corrupt {
            what,
            detail: detail.into(),
        }
    }

    /// A failure to read the agent's database at `path`.
    pub(crate) fn database(path: &Path, error: rusqlite::Error) -> Error {
        Error::Database {
            path: path.to_path_buf(),
            detail: error.to_string(),
        }
    }

    /// A failure to read or write `path`.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Error {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

/// A result whose failure is an [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;
