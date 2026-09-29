//! Agents' files, as the tests write them.

use std::path::Path;

use serde_json::Value;

/// Write `lines` as the whole of the file at `path`, one JSON line each,
/// making its directory.
pub fn write(path: &Path, lines: &[Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    std::fs::write(path, text).unwrap();
}
