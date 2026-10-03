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
//! in SQLite's `user_version`. What a read found is stored as [`store`] says.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::agent::{
    Agent, ArtifactKind, Batch, Checkpoint, ModelTotal, ReportScope, SessionReport,
};
use crate::error::{Error, Result};
use crate::search::{self, SearchHit};
use crate::session::{LinkKind, SessionFacts, SessionKey, Title, TitleSource};
use crate::sharing;
use crate::time::Instant;
use crate::usage::{Response, Tokens, Usd};

mod changes;
mod limits;
mod prices;
mod schema;
mod settings;
mod store;

pub(crate) use self::changes::Touched;
use self::changes::{next_revision, touch_sessions};
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

/// A session, with what every artifact says of it combined.
#[derive(Clone, Debug)]
pub(crate) struct SessionRecord {
    /// The session.
    pub key: SessionKey,
    /// What is known of it.
    pub facts: SessionFacts,
    /// The session it came from, and how.
    pub parent: Option<(SessionKey, LinkKind)>,
    /// Whether an artifact holding it is still where it was.
    pub present: bool,
    /// The first by path of the artifacts that say anything of it, those
    /// that give its facts before those that only report its responses, so
    /// it is the same in whatever order they were read; `None` when none is
    /// left in the ledger.
    pub artifact: Option<PathBuf>,
}

impl Ledger {
    /// Every response any artifact reports in `sessions`, by its agent and
    /// key, whichever session it is counted in once its reports combine.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read.
    pub(crate) fn responses_in(&self, sessions: &[SessionKey]) -> Result<HashSet<(Agent, String)>> {
        let wanted = session_pairs(sessions);
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT DISTINCT o.agent, o.response FROM observation o
             JOIN session s ON s.id = o.session_id
             WHERE (s.agent, s.native) {}",
            among_pairs(1)
        ))?;
        let mut rows = statement.query([wanted])?;
        let mut keys = HashSet::new();
        while let Some(row) = rows.next()? {
            if let Some(agent) = Agent::from_key(&row.get::<_, String>(0)?) {
                keys.insert((agent, row.get(1)?));
            }
        }
        Ok(keys)
    }

    /// The responses `keys` name, with their reports combined as
    /// [`Ledger::responses`] combines them. A key with no reports left gives
    /// nothing.
    ///
    /// # Errors
    ///
    /// As [`Ledger::responses`].
    pub(crate) fn responses_of(&self, keys: &HashSet<(Agent, String)>) -> Result<Vec<Response>> {
        let wanted = pairs(keys.iter().map(|(agent, key)| (agent.key(), key.as_str())));
        self.merged(Some(&wanted))
    }

    /// Where every session that came from another came from, and how: of
    /// the links every artifact records for it, the first by kind as stored
    /// and then by the parent's key, so the answer doesn't depend on which
    /// artifact was read first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read, and
    /// [`Error::Corrupt`] when a stored value is out of range.
    pub(crate) fn parents(&self) -> Result<HashMap<SessionKey, (SessionKey, LinkKind)>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT c.agent, c.native, p.agent, p.native, l.kind FROM session_link l
             JOIN session c ON c.id = l.child_id JOIN session p ON p.id = l.parent_id
             ORDER BY l.kind, p.agent, p.native",
        )?;
        let mut rows = statement.query([])?;
        let mut parents = HashMap::new();
        while let Some(row) = rows.next()? {
            let kind: String = row.get(4)?;
            let kind =
                LinkKind::from_key(&kind).ok_or_else(|| Error::corrupt("link kind", &kind))?;
            let (Some(child), Some(parent)) = (session_at(row, 0)?, session_at(row, 2)?) else {
                continue;
            };
            parents.entry(child).or_insert((parent, kind));
        }
        Ok(parents)
    }

    /// The subagents `parent` started whose links record the key of the call
    /// that started each, by that key. A subagent's link is the one
    /// [`Ledger::parents`] chooses, and its key the least any artifact records
    /// for that link, so the answer is the same in whatever order artifacts
    /// were read. A key two subagents share names neither: which of them the
    /// call started is then unknown.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read, and
    /// [`Error::Corrupt`] when a stored value is out of range.
    pub(crate) fn launched_from(&self, parent: &SessionKey) -> Result<HashMap<String, SessionKey>> {
        // Every link of every session linked to `parent`, each session's
        // first in the order `parents` chooses by.
        let mut statement = self.connection.prepare_cached(
            "SELECT c.agent, c.native, p.agent, p.native, l.kind, l.launch FROM session_link l
             JOIN session c ON c.id = l.child_id JOIN session p ON p.id = l.parent_id
             WHERE l.child_id IN (
                 SELECT m.child_id FROM session_link m JOIN session q ON q.id = m.parent_id
                 WHERE q.agent = ?1 AND q.native = ?2)
             ORDER BY c.agent, c.native, l.kind, p.agent, p.native, l.launch IS NULL, l.launch",
        )?;
        let mut rows = statement.query(params![parent.agent().key(), parent.native()])?;
        let mut chosen: Option<SessionKey> = None;
        let mut launched = HashMap::new();
        let mut shared = HashSet::new();
        while let Some(row) = rows.next()? {
            // A link of an agent this build doesn't know is passed over, as
            // `parents` passes it over.
            let (Some(child), Some(from)) = (session_at(row, 0)?, session_at(row, 2)?) else {
                continue;
            };
            if chosen.as_ref() == Some(&child) {
                continue;
            }
            chosen = Some(child.clone());
            let kind: String = row.get(4)?;
            let kind =
                LinkKind::from_key(&kind).ok_or_else(|| Error::corrupt("link kind", &kind))?;
            let Some(launch) = row.get::<_, Option<String>>(5)? else {
                continue;
            };
            if kind != LinkKind::Subagent || from != *parent {
                continue;
            }
            if launched.insert(launch.clone(), child).is_some() {
                shared.insert(launch);
            }
        }
        for launch in shared {
            launched.remove(&launch);
        }
        Ok(launched)
    }

    /// Every session, with what every artifact says of it combined, and where
    /// it came from as `parents` says.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read, and
    /// [`Error::Corrupt`] when a stored value is out of range.
    pub(crate) fn sessions(
        &self,
        parents: &HashMap<SessionKey, (SessionKey, LinkKind)>,
    ) -> Result<Vec<SessionRecord>> {
        self.records(None, parents)
    }

    /// The sessions `keys` names, as [`Ledger::sessions`] gives them; a key
    /// the ledger doesn't know gives nothing.
    ///
    /// # Errors
    ///
    /// As [`Ledger::sessions`].
    pub(crate) fn sessions_of<'a>(
        &self,
        keys: impl IntoIterator<Item = &'a SessionKey>,
        parents: &HashMap<SessionKey, (SessionKey, LinkKind)>,
    ) -> Result<Vec<SessionRecord>> {
        self.records(Some(&session_pairs(keys)), parents)
    }

    /// The sessions `wanted` names, as [`pairs`] of their agents and native
    /// ids, or every session.
    fn records(
        &self,
        wanted: Option<&str>,
        parents: &HashMap<SessionKey, (SessionKey, LinkKind)>,
    ) -> Result<Vec<SessionRecord>> {
        let wanted_sessions = format!(
            "IN (SELECT id FROM session WHERE (agent, native) {})",
            among_pairs(1)
        );
        let (sessions, facts) = if wanted.is_some() {
            (
                format!("WHERE s.id {wanted_sessions}"),
                format!("WHERE f.session_id {wanted_sessions}"),
            )
        } else {
            (String::new(), String::new())
        };
        let mut records: HashMap<i64, SessionRecord> = HashMap::new();
        let mut statement = self.connection.prepare(&format!(
            "SELECT s.id, s.agent, s.native,
                    EXISTS (SELECT 1 FROM artifact a WHERE a.present = 1 AND a.id IN (
                        SELECT artifact_id FROM session_fact WHERE session_id = s.id
                        UNION SELECT artifact_id FROM observation WHERE session_id = s.id)
                        AND NOT EXISTS (SELECT 1 FROM session_absent x
                                        WHERE x.artifact_id = a.id AND x.session_id = s.id)),
                    coalesce(
                        (SELECT min(a.path) FROM session_fact f JOIN artifact a ON a.id = f.artifact_id
                         WHERE f.session_id = s.id),
                        (SELECT min(a.path) FROM observation o JOIN artifact a ON a.id = o.artifact_id
                         WHERE o.session_id = s.id))
             FROM session s {sessions}"
        ))?;
        let mut rows = statement.query(params_from_iter(wanted))?;
        while let Some(row) = rows.next()? {
            let Some(key) = session_at(row, 1)? else {
                continue;
            };
            records.insert(
                row.get(0)?,
                SessionRecord {
                    parent: parents.get(&key).cloned(),
                    key,
                    facts: SessionFacts::default(),
                    present: row.get(3)?,
                    artifact: row.get::<_, Option<String>>(4)?.map(PathBuf::from),
                },
            );
        }

        // Each artifact's facts, oldest first and then by the artifact's
        // path, combined as a read of one artifact combines them, so the
        // result does not depend on which artifact was read first.
        let mut statement = self.connection.prepare(&format!(
            "SELECT f.session_id, f.started, f.last, f.cwd, f.branch, f.version, f.origin, f.title,
                    f.title_rank
             FROM session_fact f JOIN artifact a ON a.id = f.artifact_id {facts}
             ORDER BY coalesce(f.started, f.last), a.path"
        ))?;
        let mut rows = statement.query(params_from_iter(wanted))?;
        while let Some(row) = rows.next()? {
            let Some(record) = records.get_mut(&row.get(0)?) else {
                continue;
            };
            let title = match (
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<i64>>(8)?,
            ) {
                (Some(text), Some(rank)) => Some(Title {
                    source: TitleSource::from_rank(rank)
                        .ok_or_else(|| Error::corrupt("title rank", rank.to_string()))?,
                    text,
                }),
                _ => None,
            };
            record.facts.absorb(SessionFacts {
                started: optional_instant(row.get(1)?, "session start")?,
                last: optional_instant(row.get(2)?, "session activity")?,
                cwd: row.get(3)?,
                branch: row.get(4)?,
                version: row.get(5)?,
                origin: row.get(6)?,
                title,
            });
        }
        let mut records: Vec<SessionRecord> = records.into_values().collect();
        records.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(records)
    }

    /// Every artifact that speaks of `session`, is still where it was, and
    /// holds its transcript, as far as the artifact last said, in path
    /// order.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read.
    pub(crate) fn artifacts_of(&self, session: &SessionKey) -> Result<Vec<PathBuf>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT a.path FROM artifact a JOIN session s ON s.agent = ?1 AND s.native = ?2
             WHERE a.present = 1
               AND a.id IN (SELECT artifact_id FROM session_fact WHERE session_id = s.id
                            UNION SELECT artifact_id FROM observation WHERE session_id = s.id)
               AND NOT EXISTS (SELECT 1 FROM session_absent x
                               WHERE x.artifact_id = a.id AND x.session_id = s.id)
             ORDER BY a.path",
        )?;
        let paths = statement
            .query_map(params![session.agent().key(), session.native()], |row| {
                row.get::<_, String>(0)
            })?
            .map(|path| path.map(PathBuf::from))
            .collect::<rusqlite::Result<_>>()?;
        Ok(paths)
    }

    /// Every session in which something said matches `words`, as
    /// [`crate::Engine::search`] describes, without titles, which the cache
    /// gives, and each with where it is placed: its latest mention, or, where
    /// none has a time, its latest activity. Most recently placed first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read.
    pub(crate) fn search(&self, words: &str) -> Result<Vec<(SearchHit, Option<i64>)>> {
        let Some(expression) = search::expression(&self.connection, words)? else {
            return Ok(Vec::new());
        };
        // A review, which the cache counts nowhere, takes no place among
        // them: a session whose link standing, as `parents` takes it, the
        // least kind's, is a review's. Where nothing matched has a time, as
        // nothing Grok Build records as said has, the session's latest
        // activity places it. What matched is counted from the artifacts
        // still there when any are, so a file moved, as Codex moves a thread
        // to its archive, isn't counted again where it was; from all of them
        // once none is, since history outlives the files it was read from.
        // What a later artifact took back is not found, as its conversation
        // doesn't show it: the rule of `Resumed::takes_back`, over every
        // artifact's resumptions.
        let mut statement = self.connection.prepare_cached(
            "SELECT s.agent, s.native,
                    CASE WHEN count(*) FILTER (WHERE a.present) > 0
                         THEN count(*) FILTER (WHERE a.present) ELSE count(*) END,
                    max(d.at),
                    coalesce(max(d.at),
                             (SELECT max(coalesce(f.last, f.started)) FROM session_fact f
                              WHERE f.session_id = d.session_id)) AS placed
             FROM said_text JOIN said d ON d.id = said_text.rowid JOIN session s ON s.id = d.session_id
                  JOIN artifact a ON a.id = d.artifact_id
             WHERE said_text MATCH ?1
               AND (d.ordinal IS NULL OR d.at IS NULL OR NOT EXISTS (
                   SELECT 1 FROM resumption cut
                   WHERE cut.session_id = d.session_id AND d.ordinal >= cut.base AND d.at < cut.at
                     AND NOT EXISTS (SELECT 1 FROM resumption fresh
                                     WHERE fresh.session_id = d.session_id AND fresh.base IS NULL
                                       AND d.at < fresh.at AND fresh.at < cut.at)))
             GROUP BY d.session_id
             HAVING d.session_id NOT IN (SELECT child_id FROM session_link GROUP BY child_id
                                         HAVING min(kind) = 'review')
             ORDER BY placed DESC NULLS LAST, s.agent, s.native",
        )?;
        let mut rows = statement.query(params![expression])?;
        let mut found = Vec::new();
        while let Some(row) = rows.next()? {
            let Some(session) = session_at(row, 0)? else {
                continue;
            };
            let hit = SearchHit {
                session,
                matches: unsigned(row.get(2)?, "matches")?,
                last: optional_instant(row.get(3)?, "said time")?,
            };
            found.push((hit, row.get(4)?));
        }
        Ok(found)
    }

    /// Every session's own totals, as the agent last reported them: of the
    /// reports every artifact keeps of it, the latest, and of those reported
    /// at once, the last by the artifact's path, so the answer doesn't depend
    /// on which artifact was read first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read, and
    /// [`Error::Corrupt`] when a stored value is out of range.
    pub(crate) fn reports(&self) -> Result<Vec<SessionReport>> {
        self.reported(None)
    }

    /// The own totals of the sessions `keys` names, as [`Ledger::reports`]
    /// gives them; a session with none gives nothing.
    ///
    /// # Errors
    ///
    /// As [`Ledger::reports`].
    pub(crate) fn reports_of<'a>(
        &self,
        keys: impl IntoIterator<Item = &'a SessionKey>,
    ) -> Result<Vec<SessionReport>> {
        self.reported(Some(&session_pairs(keys)))
    }

    /// The own totals of the sessions `wanted` names, as [`pairs`] of their
    /// agents and native ids, or of every session.
    fn reported(&self, wanted: Option<&str>) -> Result<Vec<SessionReport>> {
        let condition = if wanted.is_some() {
            format!("WHERE (s.agent, s.native) {}", among_pairs(1))
        } else {
            String::new()
        };
        let mut statement = self.connection.prepare(&format!(
            "SELECT s.agent, s.native, r.artifact_id, r.scope, r.at, r.model, r.input, r.cache_read,
                    r.cache_write, r.output, r.web_searches, r.cost
             FROM report r JOIN session s ON s.id = r.session_id JOIN artifact a ON a.id = r.artifact_id
             {condition}
             ORDER BY s.agent, s.native, r.at, a.path, r.model"
        ))?;
        let mut rows = statement.query(params_from_iter(wanted))?;
        // The latest report of each session stands, whole.
        let mut latest: BTreeMap<SessionKey, (i64, SessionReport)> = BTreeMap::new();
        while let Some(row) = rows.next()? {
            let Some(session) = session_at(row, 0)? else {
                continue;
            };
            let artifact: i64 = row.get(2)?;
            let scope: String = row.get(3)?;
            let scope = ReportScope::from_key(&scope)
                .ok_or_else(|| Error::corrupt("report scope", &scope))?;
            let at = optional_instant(row.get(4)?, "report time")?;
            let model: String = row.get(5)?;
            let cost = usd(row.get(11)?, "reported cost")?;
            let total = ModelTotal {
                model: (!model.is_empty()).then_some(model),
                input: unsigned(row.get(6)?, "reported input")?,
                cache_read: unsigned(row.get(7)?, "reported cache read")?,
                cache_write: unsigned(row.get(8)?, "reported cache write")?,
                output: unsigned(row.get(9)?, "reported output")?,
                web_searches: unsigned(row.get(10)?, "reported web searches")?,
                cost,
            };
            let entry = latest.entry(session.clone()).or_insert_with(|| {
                (
                    artifact,
                    SessionReport {
                        session,
                        scope,
                        at,
                        models: Vec::new(),
                    },
                )
            });
            if entry.0 != artifact {
                entry.0 = artifact;
                entry.1.models.clear();
                entry.1.at = at;
            }
            entry.1.scope = scope;
            entry.1.models.push(total);
        }
        Ok(latest.into_values().map(|(_, reported)| reported).collect())
    }

    /// The root of the project `path` belongs to: as recorded when the
    /// directory was first seen, or as `resolve` finds it now, with its name,
    /// which is then recorded.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read or written.
    pub(crate) fn project_of(
        &self,
        path: &str,
        resolve: impl FnOnce(&str) -> (String, String),
    ) -> Result<String> {
        let known = self
            .connection
            .prepare_cached("SELECT project FROM directory WHERE path = ?1")?
            .query_row([path], |row| row.get(0))
            .optional()?;
        if let Some(known) = known {
            return Ok(known);
        }
        let (project, name) = resolve(path);
        self.connection
            .prepare_cached(
                "INSERT OR IGNORE INTO directory (path, project, name) VALUES (?1, ?2, ?3)",
            )?
            .execute(params![path, project, name])?;
        Ok(project)
    }

    /// Every response, with its reports combined: the largest of each count,
    /// and the session whose own report of it is not a copy, the lowest key
    /// settling the case that never arises of two.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read, and
    /// [`Error::Corrupt`] when a stored value is out of range.
    pub(crate) fn responses(&self) -> Result<Vec<Response>> {
        self.merged(None)
    }

    /// The responses `wanted` names, as [`pairs`] of their agents and keys,
    /// or every response, with their reports combined.
    fn merged(&self, wanted: Option<&str>) -> Result<Vec<Response>> {
        let condition = if wanted.is_some() {
            format!("WHERE (o.agent, o.response) {}", among_pairs(1))
        } else {
            String::new()
        };
        let mut statement = self.connection.prepare(&format!(
            "SELECT o.agent, o.response,
                    coalesce(min(CASE WHEN o.copy = 0 THEN s.native END), min(s.native)),
                    max(o.at), max(o.provider), max(o.model), max(o.input), max(o.cache_read),
                    max(o.cache_write_5m), max(o.cache_write_1h), max(o.output), max(o.reasoning),
                    max(o.prompt), max(o.web_searches), max(o.priority), max(o.cost)
             FROM observation o JOIN session s ON s.id = o.session_id
             {condition}
             GROUP BY o.agent, o.response"
        ))?;
        let mut rows = statement.query(params_from_iter(wanted))?;
        let mut responses = Vec::new();
        while let Some(row) = rows.next()? {
            let Some(agent) = Agent::from_key(&row.get::<_, String>(0)?) else {
                continue;
            };
            let at: i64 = row.get(3)?;
            let cost: Option<i64> = row.get(15)?;
            responses.push(Response {
                agent,
                key: row.get(1)?,
                session: SessionKey::new(agent, row.get::<_, String>(2)?),
                at: instant(at, "response time")?,
                provider: row.get(4)?,
                model: row.get(5)?,
                tokens: Tokens {
                    input: unsigned(row.get(6)?, "input")?,
                    cache_read: unsigned(row.get(7)?, "cache read")?,
                    cache_write_5m: unsigned(row.get(8)?, "cache write")?,
                    cache_write_1h: unsigned(row.get(9)?, "cache write")?,
                    output: unsigned(row.get(10)?, "output")?,
                    reasoning: unsigned(row.get(11)?, "reasoning")?,
                },
                prompt: unsigned(row.get(12)?, "prompt")?,
                web_searches: unsigned(row.get(13)?, "web searches")?,
                priority: row.get(14)?,
                recorded: usd(cost, "recorded cost")?,
            });
        }
        Ok(responses)
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
