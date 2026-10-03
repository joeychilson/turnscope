//! Reading what the artifacts said, combined: each response with its
//! reports, each session with what every artifact says of it, the links
//! between sessions, agents' own totals, what was said, for search, and the
//! project each directory belonged to. Combining is order-independent, in
//! SQL as in the cache's own versions of each rule.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use rusqlite::{OptionalExtension, params, params_from_iter};

use super::{
    Ledger, among_pairs, instant, optional_instant, pairs, session_at, session_pairs, unsigned, usd,
};
use crate::agent::{Agent, ModelTotal, ReportScope, SessionReport};
use crate::error::{Error, Result};
use crate::search::{self, SearchHit};
use crate::session::{LinkKind, SessionFacts, SessionKey, Title, TitleSource};
use crate::usage::{Response, Tokens};

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
