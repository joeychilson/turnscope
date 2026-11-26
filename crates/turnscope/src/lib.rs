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
mod platform;
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
    let home = absolute(home.unwrap_or_else(platform::home));
    let data = absolute(data.unwrap_or_else(|| platform::data_dir(&home)));
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
    /// nothing more (`db::newer`). `replaced`: this copy's binary was
    /// replaced since it started, so starting it again updates it. Exit 1.
    Newer { replaced: bool },
    /// Another Turnscope was reading history for longer than this one
    /// waited (`ingest::ingest`), so an MCP answer says its usage is only
    /// part of it. Exit 1.
    Busy,
}

pub type Result<T> = std::result::Result<T, Error>;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(match self {
            Error::Usage(message) | Error::NotFound(message) | Error::Failed(message) => message,
            Error::Busy => "another Turnscope is reading your agents' history",
            Error::Newer { replaced: true } => {
                "Turnscope was updated after this one started; start it again"
            }
            Error::Newer { replaced: false } => {
                "a newer Turnscope has updated this data; update Turnscope"
            }
        })
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

/// The first `bytes` of `data`'s SHA-256, in hex.
pub fn sha256_hex(data: &[u8], bytes: usize) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(data)[..bytes]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
            limits::refresh(db, home, limits::Ask::Scheduled),
        ),
        false => (false, Ok(false)),
    };
    let read = ingest::ingest(db, home, patience)?;
    Ok(limits? || priced || read)
}

pub fn online() -> bool {
    std::env::var_os("TURNSCOPE_OFFLINE").is_none_or(|offline| offline == "0")
}

/// Diagnostic notes on logins, stale accounts, unreadable history files and
/// unpriced models.
pub fn doctor(db: &rusqlite::Connection, home: &Path, now: i64) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    for agent in agents::ALL {
        for folder in agent.installed_folders(home) {
            if let Err(error) = agent.logins(&folder, home) {
                notes.push(format!(
                    "{}'s logins in {} can't be read: {error}",
                    agent.info().name,
                    folder.display()
                ));
            }
        }
    }
    let status = status::status(db, now)?;
    // An account no agent here is signed in to has nothing to fix.
    for account in &status.accounts {
        if account.state.why() != Some(status::Stale::SignedOut)
            && let Some(stale) = status::stale(account, false, status.at)
        {
            notes.push(format!("{}: {stale}.", account.full_title()));
        }
    }
    for (path, why) in files::<String>(
        db,
        "SELECT path, failed FROM file WHERE failed IS NOT NULL ORDER BY path",
    )? {
        // A reader's error often names its file already.
        let why = if why.contains(&path) {
            why
        } else {
            format!("{path}: {why}")
        };
        notes.push(format!(
            "{why}. It's tried again each time history is read."
        ));
    }
    for (path, skipped) in files::<i64>(
        db,
        "SELECT path, skipped FROM file WHERE skipped > 0 ORDER BY skipped DESC",
    )? {
        notes.push(format!(
            "{path}: {} couldn't be read; their agent may write a newer format.",
            usage::counted(skipped as u64, "line")
        ));
    }
    // Before any price is read, every model is unpriced: one note says so.
    let priced: bool = db.query_row("SELECT EXISTS (SELECT 1 FROM price)", [], |row| row.get(0))?;
    if !priced {
        notes.push(
            "Prices haven't been read from models.dev yet: costs are unknown until they are."
                .to_owned(),
        );
    } else {
        let unpriced: Vec<(String, i64)> = db
            .prepare("SELECT model, count(*) FROM response WHERE cost IS NULL GROUP BY model ORDER BY count(*) DESC LIMIT 5")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (model, count) in unpriced {
            let model = if model.is_empty() {
                "no model named".to_owned()
            } else {
                model
            };
            notes.push(format!(
                "{model}: {} with no known price; models.dev doesn't list it.",
                usage::counted(count as u64, "response")
            ));
        }
    }
    Ok(notes)
}

/// What `app.log` in `data` says went wrong in the last day, the latest five,
/// as notes for `doctor`: the app is started from its icon, and limits are
/// read in agents' hooks, where no one sees their errors.
pub fn logged(data: &Path, now: i64) -> Vec<String> {
    let log = std::fs::read_to_string(data.join("app.log")).unwrap_or_default();
    let mut logged: Vec<&str> = log
        .lines()
        .rev()
        .filter(|line| {
            line.split_once(' ')
                .and_then(|(at, _)| time::millis(at))
                .is_some_and(|at| at > now - time::DAY)
        })
        .take(5)
        .collect();
    logged.reverse();
    logged
        .iter()
        .map(|line| format!("Logged: {line}"))
        .collect()
}

/// The first ten history files `sql` lists with what it says of each, of
/// those still there: a deleted one isn't read again, and one in a folder
/// that can't be looked into may still be there. Filtered before the ten,
/// so deleted files never hide one still there.
fn files<T: rusqlite::types::FromSql>(
    db: &rusqlite::Connection,
    sql: &str,
) -> Result<Vec<(String, T)>> {
    Ok(db
        .prepare(sql)?
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get(1)?)))?
        .filter(|row| {
            row.as_ref().map_or(true, |(path, _)| {
                Path::new(path).try_exists().unwrap_or(true)
            })
        })
        .take(10)
        .collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Eleven files that can't be read, one more than doctor names.
    #[cfg(unix)]
    #[test]
    fn doctor_names_a_file_that_cant_be_read_until_its_deleted() {
        use std::os::unix::fs::PermissionsExt as _;
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let folder = home.path().join(".claude/projects/-work-a");
        std::fs::create_dir_all(&folder).unwrap();
        let locked: Vec<PathBuf> = (0..11)
            .map(|n| folder.join(format!("s{n:02}.jsonl")))
            .collect();
        for file in &locked {
            std::fs::write(file, "{}\n").unwrap();
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        let mut db = db::open(data.path()).unwrap();
        let mut notes = || {
            ingest::ingest(&mut db, home.path(), None).unwrap();
            doctor(&db, home.path(), now()).unwrap().join("\n")
        };
        assert!(notes().contains("s00.jsonl"));
        // The ten it named are deleted, and don't hide the last.
        for file in &locked[..10] {
            std::fs::remove_file(file).unwrap();
        }
        let named = notes();
        assert!(
            named.contains("s10.jsonl") && !named.contains("s00.jsonl"),
            "{named}"
        );
        std::fs::remove_file(&locked[10]).unwrap();
        assert!(!notes().contains("s10.jsonl"));
    }
}
