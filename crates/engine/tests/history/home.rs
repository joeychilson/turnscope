//! A home for a test's history, with a data directory beside it for the
//! engines opened over it.

use std::path::Path;

use serde_json::Value;
use tempfile::TempDir;
use turnscope_engine::Engine;

/// A home directory, where agents keep their history, and a data directory,
/// where an engine keeps its ledger and cache: both temporary, and removed
/// when dropped.
pub struct Home {
    dir: TempDir,
    /// The data directory, for a test that handles the ledger's files.
    pub data: TempDir,
}

impl Home {
    /// An empty home and data directory.
    pub fn new() -> Home {
        Home {
            dir: tempfile::tempdir().unwrap(),
            data: tempfile::tempdir().unwrap(),
        }
    }

    /// The home directory, which the agents' paths start from.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// An engine over the home, keeping its ledger in the data directory.
    pub fn open(&self) -> Engine {
        Engine::open(self.data.path(), self.dir.path()).unwrap()
    }
}

/// Write `records` as the whole of the file at `path`, one JSON line each,
/// making its directory.
pub fn write(path: &Path, records: &[Value]) {
    let text: String = records.iter().map(|record| format!("{record}\n")).collect();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
