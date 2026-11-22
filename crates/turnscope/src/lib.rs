//! Turnscope's engine: it reads coding agents' history and logins, and their
//! providers' limits, and works out what every app, the command line and
//! the MCP server show. See docs/architecture.md.
#![forbid(unsafe_code)]

pub mod agents;
pub mod alerts;
pub mod connect;
pub mod db;
pub mod engine;
pub mod forecast;
pub mod ingest;
pub mod limits;
pub mod prices;
pub mod process;
pub mod providers;
pub mod status;
pub mod time;
pub mod usage;

use std::path::{Path, PathBuf};

/// The home whose agents are read, and the data directory, unless they're
/// given: `$HOME`, and `Library/Application Support/com.joeychilson.turnscope`
/// in that home. Absolute, as paths under them are stored and read from
/// other folders.
pub fn places(home: Option<PathBuf>, data: Option<PathBuf>) -> (PathBuf, PathBuf) {
    let absolute = |path: PathBuf| std::path::absolute(&path).unwrap_or(path);
    let home = absolute(
        home.or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_default(),
    );
    let data = absolute(
        data.unwrap_or_else(|| home.join("Library/Application Support/com.joeychilson.turnscope")),
    );
    (home, data)
}

#[derive(Debug)]
pub enum Error {
    /// Arguments that can't be followed. Exit 2.
    Usage(String),
    /// What was named doesn't exist. Exit 3.
    NotFound(String),
    /// Exit 1; the message says how to fix it.
    Failed(String),
    /// A newer Turnscope has written to the data, so this one writes
    /// nothing more (`db::newer`). Exit 1.
    Newer(String),
    /// Another Turnscope was reading history for longer than this one
    /// waited (`ingest::ingest`), so an MCP answer says its usage is only
    /// part of it. Exit 1.
    Busy(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let (Error::Usage(message)
        | Error::NotFound(message)
        | Error::Failed(message)
        | Error::Newer(message)
        | Error::Busy(message)) = self;
        f.write_str(message)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Error {
        Error::Failed(format!("the database: {error}"))
    }
}

impl From<rusqlite::types::FromSqlError> for Error {
    fn from(error: rusqlite::types::FromSqlError) -> Error {
        Error::Failed(format!("the database: {error}"))
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Error {
        Error::Failed(error.to_string())
    }
}

/// Now, in UTC milliseconds; `TURNSCOPE_NOW` fixes it, so output can be
/// reproduced.
pub fn now() -> i64 {
    std::env::var("TURNSCOPE_NOW")
        .ok()
        .and_then(|now| now.parse().ok())
        .unwrap_or_else(|| jiff::Timestamp::now().as_millisecond())
}

pub fn sha256_hex(data: &[u8], bytes: usize) -> String {
    use sha2::Digest as _;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = sha2::Sha256::digest(data);
    let mut out = String::with_capacity(bytes * 2);
    for &byte in &digest[..bytes] {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Bring the database up to date: prices and limits when due, then what's
/// new in the agents' history, priced and attributed as it's written. A
/// provider out of reach leaves things as they were. The history is read
/// even when the limits can't be, and their error is given after. Whether
/// there was anything new: prices, limits or history.
///
/// With `patience`, history isn't read when another process has been
/// reading it for longer, as an agent's MCP server shouldn't wait minutes
/// for the app's first read: the error says so.
pub fn catch_up(
    db: &mut rusqlite::Connection,
    home: &Path,
    patience: Option<std::time::Duration>,
) -> Result<bool> {
    let (priced, limits) = match online() {
        true => (
            prices::refresh(db).unwrap_or(false),
            limits::refresh(db, home, false),
        ),
        false => (false, Ok(false)),
    };
    let read = ingest::ingest(db, home, patience)?;
    Ok(limits? || priced || read)
}

pub fn online() -> bool {
    std::env::var_os("TURNSCOPE_OFFLINE").is_none_or(|offline| offline == "0")
}
