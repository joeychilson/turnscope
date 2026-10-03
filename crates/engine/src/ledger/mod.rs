//! The ledger: what the agents' artifacts said, kept for good.
//!
//! Every row is something one artifact said — a report of a response's usage,
//! facts about a session, a link between sessions, an agent's own totals, or
//! trouble found reading it — stored against that artifact. So a file can be
//! read again from scratch by deleting what it said and reading it anew, and
//! when an agent deletes an artifact, what it said stays. A database read
//! again from the start replaces what it says again, and keeps what it said
//! of rows the agent has since deleted.
//!
//! Nothing here is derived. Merged usage, costs and every total are computed
//! from these rows and can always be computed again.
//!
//! `ledger.sqlite` holds what was observed and can't be made again:
//! artifacts and their checkpoints, sessions, links, reports of responses,
//! agents' own totals, trouble found reading, what was said, the project
//! each directory belonged to, accounts, limit readings, the alerts sent,
//! the catalog and price history, and the last look through every agent's
//! history; and, beside what was observed, the person's own choices
//! ([`settings`]), which no file holds either. What was said, the search
//! index, is here rather than in the cache because, like the reports, it
//! has to outlive the files it was read from. A new reader version reads
//! that agent's present artifacts again;
//! what absent artifacts said, and what a database said of rows it no longer
//! holds, is kept exactly as it was.
//!
//! **The change log** ([`changes`]) records what every write touched, so the
//! cache works out again only that.
//!
//! **Projects.** The project a working directory belonged to is recorded
//! when the directory is first seen, so moving or deleting a repository
//! later doesn't regroup its history.
//!
//! **Agents a build doesn't know.** A newer build sharing the data directory
//! records agents an older one has no reader for. The older build leaves
//! their artifacts alone, never marking them gone, and passes over their rows
//! wherever it reads rows, so it answers of the agents it knows.
//!
//! The schema ([`schema`]) only ever grows, by numbered migrations recorded
//! in SQLite's `user_version`. What a read found is stored as [`store`] says,
//! and read back, combined, as [`combined`] says.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::agent::{Agent, ArtifactKind, Batch, Checkpoint};
use crate::error::{Error, Result};
use crate::search;
use crate::session::SessionKey;
use crate::sharing;
use crate::time::Instant;
use crate::usage::Usd;

mod changes;
mod combined;
mod limits;
mod prices;
mod schema;
mod settings;
mod store;

pub(crate) use self::changes::Touched;
use self::changes::{next_revision, touch_sessions};
pub(crate) use self::combined::SessionRecord;
pub(crate) use self::limits::Reading;
#[cfg(test)]
pub(crate) use self::schema::MIGRATIONS;
use self::schema::migrate;
use self::store::write_one;

/// The ledger's file name, in the data directory.
pub(crate) const FILE: &str = "ledger.sqlite";

/// A look through every agent's history that finished.
pub(crate) struct Look {
    /// When it finished.
    pub at: Instant,
    /// What it could not read, and why.
    pub failed: Vec<(PathBuf, String)>,
}

/// What identifies an artifact's file and tells whether it changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileState {
    /// The device the file is on.
    pub device: u64,
    /// The file's inode, which changes when the file is replaced.
    pub inode: u64,
    /// Its size in bytes.
    pub size: u64,
    /// When it was last modified, in nanoseconds since the epoch.
    pub modified: i64,
    /// A log's first bytes, which change when it is rewritten; none for any
    /// other artifact.
    pub fingerprint: Vec<u8>,
    /// The bytes before where a log was last read to, which change when what
    /// was read is rewritten; `None` for a log read before they were kept or
    /// not read yet, and for any other artifact.
    pub closing: Option<Vec<u8>>,
}

/// An artifact the ledger has read before.
#[derive(Clone, Debug)]
pub(crate) struct Known {
    /// Its row.
    pub id: i64,
    /// Its file as last read.
    pub file: FileState,
    /// Where reading stopped.
    pub checkpoint: Checkpoint,
    /// The version of the reader that read it.
    pub reader_version: u32,
    /// Whether it was there when last looked for.
    pub present: bool,
}

/// One read of one artifact, ready to be written.
pub(crate) struct Read {
    /// Whose artifact it is.
    pub agent: Agent,
    /// Where it is.
    pub path: PathBuf,
    /// How it changes.
    pub kind: ArtifactKind,
    /// Its file as read.
    pub file: FileState,
    /// The version of the reader that read it.
    pub reader_version: u32,
    /// Whether it was read again from the start, so that what it says
    /// replaces what it said before: all of it, for a log or a document, and
    /// for a database, what it says again.
    pub reset: bool,
    /// Where reading stopped.
    pub checkpoint: Checkpoint,
    /// What was read.
    pub batch: Batch,
}

/// The ledger's database.
pub(crate) struct Ledger {
    connection: Connection,
}

impl Ledger {
    /// Open the ledger at `path`, creating it or bringing its schema up to
    /// date.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NewerLedger`] when the ledger was written by a newer
    /// build, and [`Error::Ledger`] when it cannot be opened or migrated.
    pub(crate) fn open(path: &Path) -> Result<Ledger> {
        let mut connection = sharing::open(path)?;
        // Space freed by forgetting what the cache caught up on is given back
        // as it goes. This can only be chosen before the first table exists.
        let fresh: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if fresh == 0 {
            connection.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
        }
        sharing::share(&connection)?;
        connection.pragma_update(None, "foreign_keys", true)?;
        migrate(&mut connection)?;
        terms(&connection)?;
        Ok(Ledger { connection })
    }

    /// The ledger at `path` for reading only, as it stands: nothing is
    /// created or migrated, and every write fails.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when it cannot be opened.
    pub(crate) fn reader(path: &Path) -> Result<Ledger> {
        let connection = sharing::reader(path)?;
        terms(&connection)?;
        Ok(Ledger { connection })
    }

    /// The database, for reading.
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Every artifact of `agents` read before, by path; or, given `within`,
    /// those at or under those paths. Another agent's, as a newer build's,
    /// is none of this build's to plan or to mark gone.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read, and
    /// [`Error::Corrupt`] when a stored value is out of range.
    pub(crate) fn known(
        &self,
        agents: &[Agent],
        within: Option<&[PathBuf]>,
    ) -> Result<HashMap<PathBuf, Known>> {
        const COLUMNS: &str = "SELECT id, path, device, inode, size, modified, fingerprint,
                                      read_to, state, reader_version, present, closing
                               FROM artifact WHERE agent IN (SELECT value FROM json_each(?1))";
        let agents =
            serde_json::Value::from(agents.iter().map(|agent| agent.key()).collect::<Vec<_>>())
                .to_string();
        let mut known = HashMap::new();
        let Some(within) = within else {
            let mut statement = self.connection.prepare(COLUMNS)?;
            let mut rows = statement.query([agents])?;
            while let Some(row) = rows.next()? {
                let (path, artifact) = known_row(row)?;
                known.insert(path, artifact);
            }
            return Ok(known);
        };
        // The path itself, or one under it: those that sort from `path/` up
        // to `path0`, '0' being the character after '/'.
        let mut statement = self.connection.prepare_cached(&format!(
            "{COLUMNS} AND (path = ?2 OR (path >= ?3 AND path < ?4))"
        ))?;
        for path in within {
            let Some(text) = path.to_str() else { continue };
            let text = text.trim_end_matches('/');
            let mut rows = statement.query(params![
                agents,
                text,
                format!("{text}/"),
                format!("{text}0")
            ])?;
            while let Some(row) = rows.next()? {
                let (path, artifact) = known_row(row)?;
                known.insert(path, artifact);
            }
        }
        Ok(known)
    }

    /// Write what reads of artifacts found, all at once or not at all.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be written; nothing of
    /// the reads is kept, and each artifact is read again from where the last
    /// successful write left it.
    pub(crate) fn write(&mut self, reads: &[Read], at: Instant) -> Result<()> {
        let transaction = self.connection.transaction()?;
        let revision = next_revision(&transaction)?;
        for read in reads {
            write_one(&transaction, revision, read, at)?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Record a look through every agent's history that finished `at`, and
    /// what it could not read, `failed`, in place of the last.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be written.
    pub(crate) fn record_look(&mut self, at: Instant, failed: &[(PathBuf, String)]) -> Result<()> {
        let failed = serde_json::Value::Array(
            failed
                .iter()
                .map(|(path, why)| serde_json::json!([path.to_string_lossy(), why]))
                .collect(),
        )
        .to_string();
        self.connection.execute(
            "INSERT INTO look (id, at, failed) VALUES (1, ?1, ?2)
             ON CONFLICT (id) DO UPDATE SET at = excluded.at, failed = excluded.failed",
            params![at.millis(), failed],
        )?;
        Ok(())
    }

    /// The last look through every agent's history to finish, when there has
    /// been one: when it finished, and what it could not read.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read, and
    /// [`Error::Corrupt`] when what it holds does not read.
    pub(crate) fn last_look(&self) -> Result<Option<Look>> {
        let look: Option<(i64, String)> = self
            .connection
            .query_row("SELECT at, failed FROM look WHERE id = 1", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
        let Some((at, failed)) = look else {
            return Ok(None);
        };
        let at = instant(at, "look time")?;
        let failed: Vec<(String, String)> = serde_json::from_str(&failed)
            .map_err(|error| Error::corrupt("look failures", error.to_string()))?;
        Ok(Some(Look {
            at,
            failed: failed
                .into_iter()
                .map(|(path, why)| (PathBuf::from(path), why))
                .collect(),
        }))
    }

    /// Record that the artifacts `ids` were no longer where they were. What
    /// they said is kept, and only whether its sessions are still there
    /// changes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be written.
    pub(crate) fn mark_absent(&mut self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let transaction = self.connection.transaction()?;
        let revision = next_revision(&transaction)?;
        {
            let mut statement =
                transaction.prepare("UPDATE artifact SET present = 0 WHERE id = ?1")?;
            for id in ids {
                touch_sessions(&transaction, revision, *id)?;
                statement.execute([id])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }
}

/// An artifact as [`Ledger::known`] reads it, and its path.
fn known_row(row: &rusqlite::Row) -> Result<(PathBuf, Known)> {
    let path: String = row.get(1)?;
    let artifact = Known {
        id: row.get(0)?,
        file: FileState {
            device: unsigned(row.get(2)?, "artifact device")?,
            inode: unsigned(row.get(3)?, "artifact inode")?,
            size: unsigned(row.get(4)?, "artifact size")?,
            modified: row.get(5)?,
            fingerprint: row.get(6)?,
            closing: row.get(11)?,
        },
        checkpoint: Checkpoint {
            offset: unsigned(row.get(7)?, "artifact offset")?,
            state: row.get(8)?,
        },
        reader_version: u32::try_from(row.get::<_, i64>(9)?)
            .map_err(|_| Error::corrupt("artifact reader version", "out of range"))?,
        present: row.get(10)?,
    };
    Ok((PathBuf::from(path), artifact))
}

/// List the words of the index of what was said, for a search to look up
/// their other forms in, as [`search::TERMS`] in `connection`'s `temp`
/// schema, which is the connection's own: nothing is written to the ledger,
/// and a connection that only reads can make it.
///
/// # Errors
///
/// Returns [`Error::Ledger`] when SQLite refuses it.
fn terms(connection: &Connection) -> Result<()> {
    connection.execute_batch(&format!(
        "CREATE VIRTUAL TABLE temp.{} USING fts5vocab(main, said_text, row)",
        search::TERMS
    ))?;
    Ok(())
}

/// `pairs` as a JSON list of two-item lists, a parameter that names many
/// rows at once: `json_each` gives each back, as `value ->> 0` and
/// `value ->> 1`.
fn pairs<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    let pairs = pairs
        .map(|(first, second)| serde_json::Value::Array(vec![first.into(), second.into()]))
        .collect();
    serde_json::Value::Array(pairs).to_string()
}

/// The sessions `keys`, as [`pairs`] of their agents' keys and native ids.
fn session_pairs<'a>(keys: impl IntoIterator<Item = &'a SessionKey>) -> String {
    pairs(
        keys.into_iter()
            .map(|key| (key.agent().key(), key.native())),
    )
}

/// SQL testing two columns against the [`pairs`] parameter number
/// `parameter` names, as `(agent, native) IN …` does.
fn among_pairs(parameter: usize) -> String {
    format!("IN (SELECT value ->> 0, value ->> 1 FROM json_each(?{parameter}))")
}

/// The session whose agent's key and native id are in columns `at` and the
/// next of `row`: `None` for an agent this build doesn't know.
fn session_at(row: &rusqlite::Row, at: usize) -> Result<Option<SessionKey>> {
    let Some(agent) = Agent::from_key(&row.get::<_, String>(at)?) else {
        return Ok(None);
    };
    Ok(Some(SessionKey::new(agent, row.get::<_, String>(at + 1)?)))
}

/// A stored instant, as the ledger and the cache store instants: in
/// milliseconds.
///
/// # Errors
///
/// Returns [`Error::Corrupt`], naming `what`, when no instant is that far
/// from the epoch.
pub(crate) fn instant(millis: i64, what: &'static str) -> Result<Instant> {
    Instant::from_millis(millis).ok_or_else(|| Error::corrupt(what, millis.to_string()))
}

/// A stored instant, when one is stored, as [`instant`] reads it.
///
/// # Errors
///
/// As [`instant`].
pub(crate) fn optional_instant(millis: Option<i64>, what: &'static str) -> Result<Option<Instant>> {
    millis.map(|millis| instant(millis, what)).transpose()
}

/// A stored amount, when one is stored, in nano-dollars, as the ledger and
/// the cache store amounts.
///
/// # Errors
///
/// Returns [`Error::Corrupt`], naming `what`, when it is below zero.
pub(crate) fn usd(nanos: Option<i64>, what: &'static str) -> Result<Option<Usd>> {
    nanos
        .map(|nanos| Usd::from_nanos(nanos).ok_or_else(|| Error::corrupt(what, nanos.to_string())))
        .transpose()
}

/// A stored integer that must not be negative, as every count is, in the
/// ledger and the cache alike.
///
/// # Errors
///
/// Returns [`Error::Corrupt`], naming `what`, when it is negative.
pub(crate) fn unsigned(value: i64, what: &'static str) -> Result<u64> {
    u64::try_from(value).map_err(|_| Error::corrupt(what, format!("{value} is negative")))
}

/// A count as the ledger and the cache store it.
///
/// Counts are at most [`crate::usage::LARGEST_COUNT`] and byte offsets at most
/// a file's size, so every value stored fits; one that did not would be a
/// reader's bug, and is refused rather than written wrong.
///
/// # Errors
///
/// Returns [`Error::Corrupt`] when it is past `i64::MAX`.
pub(crate) fn stored(value: u64) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| Error::corrupt("count", format!("{value} is too large to store")))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;

    use super::{FileState, Ledger, MIGRATIONS, Read};
    use crate::agent::{
        Agent, ArtifactKind, Batch, Checkpoint, ModelTotal, Observation, ReportScope, SessionReport,
    };
    use crate::error::Error;
    use crate::session::{LinkKind, SessionKey, SessionLink};
    use crate::time::Instant;
    use crate::usage::Tokens;

    /// A ledger of its own, in a folder kept as long as it is.
    pub(crate) fn scratch() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("ledger.sqlite")).unwrap();
        (dir, ledger)
    }

    /// A read of `agent`'s log at `path` that found `batch`.
    pub(crate) fn read(agent: Agent, path: &str, batch: Batch) -> Read {
        Read {
            agent,
            path: PathBuf::from(path),
            kind: ArtifactKind::Log,
            file: FileState {
                device: 1,
                inode: 1,
                size: 0,
                modified: 0,
                fingerprint: Vec::new(),
                closing: None,
            },
            reader_version: 1,
            reset: false,
            checkpoint: Checkpoint::default(),
            batch,
        }
    }

    #[test]
    fn what_artifacts_say_of_a_session_combines_alike_whichever_was_read_first() {
        let key = |native: &str| SessionKey::new(Agent::ClaudeCode, native);
        let at = Instant::parse("2026-09-14T12:00:00Z").unwrap();
        // Two files that say different things of one session at one moment:
        // who ran it, its branch and its totals.
        let said = |parent: &str, branch: &str, input: u64| {
            let mut batch = Batch::default();
            let facts = batch.session_mut(&key("s"));
            facts.saw(at);
            facts.branch = Some(branch.to_owned());
            batch.link(SessionLink {
                child: key("s"),
                parent: key(parent),
                kind: LinkKind::Subagent,
                launch: None,
            });
            batch.report(SessionReport {
                session: key("s"),
                scope: ReportScope::Session,
                at: Some(at),
                models: vec![ModelTotal {
                    model: None,
                    input,
                    cache_read: 0,
                    cache_write: 0,
                    output: 0,
                    web_searches: 0,
                    cost: None,
                }],
            });
            batch
        };
        let ledger = |order: [(&str, Batch); 2]| {
            let (dir, mut ledger) = scratch();
            for (path, batch) in order {
                ledger
                    .write(&[read(Agent::ClaudeCode, path, batch)], at)
                    .unwrap();
            }
            (dir, ledger)
        };
        let (_first_dir, first) = ledger([
            ("/a.jsonl", said("p1", "one", 1)),
            ("/b.jsonl", said("p2", "two", 2)),
        ]);
        let (_second_dir, second) = ledger([
            ("/b.jsonl", said("p2", "two", 2)),
            ("/a.jsonl", said("p1", "one", 1)),
        ]);

        let parents = first.parents().unwrap();
        assert_eq!(parents, second.parents().unwrap());
        assert_eq!(parents[&key("s")].0, key("p1"));
        let sessions = |ledger: &Ledger| format!("{:?}", ledger.sessions(&parents).unwrap());
        assert_eq!(sessions(&first), sessions(&second));
        let reports = first.reports().unwrap();
        assert_eq!(reports, second.reports().unwrap());
        assert_eq!(reports[0].models[0].input, 2);
    }

    #[test]
    fn a_ledger_from_a_newer_build_is_refused_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite");
        drop(Ledger::open(&path).unwrap());
        let newer = i64::try_from(MIGRATIONS.len()).unwrap() + 1;
        let version = || -> i64 {
            rusqlite::Connection::open(&path)
                .unwrap()
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap()
        };
        rusqlite::Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", newer)
            .unwrap();
        match Ledger::open(&path) {
            Err(Error::NewerLedger { found, .. }) => assert_eq!(found, newer),
            other => panic!("expected a refusal, got {:?}", other.map(|_| ())),
        }
        assert_eq!(version(), newer);
    }

    #[test]
    fn a_change_of_a_kind_no_write_records_is_corrupt() {
        let (_dir, ledger) = scratch();
        ledger
            .connection()
            .execute_batch(
                "UPDATE revision SET value = 1;
                 INSERT INTO touched (revision, kind, agent, key) VALUES (1, 'sessions', 'codex', '01a0');",
            )
            .unwrap();
        assert!(matches!(
            ledger.touched_since(0),
            Err(Error::Corrupt {
                what: "touched kind",
                ..
            })
        ));
    }

    #[test]
    fn a_copy_never_decides_whose_a_response_is() {
        let key = |native: &str| SessionKey::new(Agent::ClaudeCode, native);
        let at = Instant::parse("2026-09-14T12:00:00Z").unwrap();
        let observed = |session: &str, copy: bool| {
            let mut batch = Batch::default();
            batch.observe(Observation {
                response: "msg_1".to_owned(),
                session: key(session),
                copy,
                at,
                provider: "anthropic".to_owned(),
                model: "claude-opus-5".to_owned(),
                tokens: Tokens::default(),
                prompt: 0,
                web_searches: 0,
                cost: None,
                priority: false,
            });
            batch
        };
        let (_dir, mut ledger) = scratch();
        // One file reports the response as its session's own, and later as a
        // copy another session made; another file, of a session whose key
        // sorts first, only copied it.
        for (path, batch) in [
            ("/own.jsonl", observed("own", false)),
            ("/own.jsonl", observed("copier", true)),
            ("/copy.jsonl", observed("a-copier", true)),
        ] {
            ledger
                .write(&[read(Agent::ClaudeCode, path, batch)], at)
                .unwrap();
        }
        let responses = ledger.responses().unwrap();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].session, key("own"));
    }

    #[test]
    fn a_session_whose_words_have_no_time_is_found_by_when_it_was_active() {
        let (_dir, mut ledger) = scratch();
        let at = |text: &str| Instant::parse(text).unwrap();
        let key = |native: &str| SessionKey::new(Agent::ClaudeCode, native);
        // One session said it at 09:00; another, recording no time for what
        // was said, was active until noon.
        let mut timed = Batch::default();
        timed.say(
            &key("timed"),
            Some(at("2026-09-14T09:00:00Z")),
            "the migration",
        );
        let mut untimed = Batch::default();
        untimed.say(&key("untimed"), None, "the migration");
        untimed
            .session_mut(&key("untimed"))
            .saw(at("2026-09-14T12:00:00Z"));
        let reads = [
            read(Agent::ClaudeCode, "/timed.jsonl", timed),
            read(Agent::ClaudeCode, "/untimed.jsonl", untimed),
        ];
        ledger.write(&reads, at("2026-09-14T12:00:00Z")).unwrap();

        let found: Vec<SessionKey> = ledger
            .search("migration")
            .unwrap()
            .into_iter()
            .map(|(hit, _)| hit.session)
            .collect();
        assert_eq!(found, [key("untimed"), key("timed")]);
    }

    #[test]
    fn a_report_of_a_scope_no_reader_gives_is_corrupt() {
        let (_dir, ledger) = scratch();
        ledger
            .connection()
            .execute_batch(
                "INSERT INTO artifact (id, agent, path, kind, device, inode, size, modified, fingerprint,
                                       read_to, state, reader_version, present, read_at)
                 VALUES (1, 'claude-code', '/s.jsonl', 'log', 1, 1, 0, 0, x'', 0, x'', 1, 1, 0);
                 INSERT INTO session (id, agent, native) VALUES (1, 'claude-code', 's');
                 INSERT INTO report (artifact_id, session_id, scope, model, input, cache_read,
                                     cache_write, output, web_searches)
                 VALUES (1, 1, 'subtree', '', 0, 0, 0, 0, 0);",
            )
            .unwrap();
        assert!(matches!(
            ledger.reports(),
            Err(Error::Corrupt {
                what: "report scope",
                ..
            })
        ));
    }

    #[test]
    fn a_link_given_anew_touches_the_parent_it_came_from_before() {
        let (_dir, mut ledger) = scratch();
        let key = |native: &str| SessionKey::new(Agent::ClaudeCode, native);
        let linking = |parent: &str| {
            let mut batch = Batch::default();
            batch.link(SessionLink {
                child: key("child"),
                parent: key(parent),
                kind: LinkKind::Subagent,
                launch: None,
            });
            read(Agent::ClaudeCode, "/child.jsonl", batch)
        };
        ledger.write(&[linking("first")], Instant::now()).unwrap();
        let after = ledger.revision().unwrap();
        // The same artifact, read on, says the child came from another.
        ledger.write(&[linking("second")], Instant::now()).unwrap();
        let touched = ledger.touched_since(after).unwrap().sessions;
        for session in ["child", "first", "second"] {
            assert!(touched.contains(&key(session)), "{session}: {touched:?}");
        }
    }
}
