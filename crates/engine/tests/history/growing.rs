//! Logs that grow, as agents append to them.

use std::io::Write as _;
use std::path::Path;

/// Add `bytes` to the end of the file at `path`, making the file and its
/// directory if need be.
pub fn append(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}
