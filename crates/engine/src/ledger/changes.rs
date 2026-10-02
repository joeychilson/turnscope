//! The change log: every write takes a revision and records, in the same
//! transaction, which responses and sessions it touched, or that prices, or
//! what sign-ins were held where, changed. The cache records the revision it reflects, works out again only
//! what was touched since, and has the log pruned behind it. The ledger also
//! draws a number when it is made, which a cache records, so a cache built
//! from another ledger is told apart.

use std::collections::HashSet;

use rusqlite::{Transaction, params};

use super::Ledger;
use crate::agent::{Agent, Batch};
use crate::error::{Error, Result};
use crate::session::SessionKey;

/// What changed in the ledger after some revision.
#[derive(Debug, Default)]
pub(crate) struct Touched {
    /// The latest revision covered.
    pub up_to: i64,
    /// Responses whose reports changed.
    pub responses: HashSet<(Agent, String)>,
    /// Sessions whose facts, links, reports or presence changed.
    pub sessions: HashSet<SessionKey>,
    /// Whether any price changed.
    pub prices: bool,
    /// Whether what a place an agent keeps a sign-in held began to differ,
    /// so which account usage drew on may have changed.
    pub sign_ins: bool,
}

impl Ledger {
    /// The number drawn when the ledger was made, which tells it from any
    /// other.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read.
    pub(crate) fn identity(&self) -> Result<i64> {
        Ok(self
            .connection
            .query_row("SELECT value FROM identity WHERE id = 1", [], |row| {
                row.get(0)
            })?)
    }

    /// The latest revision.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read.
    pub(crate) fn revision(&self) -> Result<i64> {
        Ok(self
            .connection
            .query_row("SELECT value FROM revision WHERE id = 1", [], |row| {
                row.get(0)
            })?)
    }

    /// What changed after revision `after`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read, and
    /// [`Error::Corrupt`] when a stored kind of change is unknown.
    pub(crate) fn touched_since(&self, after: i64) -> Result<Touched> {
        let mut touched = Touched {
            up_to: self.revision()?,
            ..Touched::default()
        };
        let mut statement = self.connection.prepare(
            "SELECT DISTINCT kind, agent, key FROM touched WHERE revision > ?1 AND revision <= ?2",
        )?;
        let mut rows = statement.query(params![after, touched.up_to])?;
        while let Some(row) = rows.next()? {
            let kind: String = row.get(0)?;
            let kind =
                Touch::from_key(&kind).ok_or_else(|| Error::corrupt("touched kind", &kind))?;
            match kind {
                Touch::Prices => touched.prices = true,
                Touch::SignIns => touched.sign_ins = true,
                Touch::Response | Touch::Session => {
                    let Some(agent) = Agent::from_key(&row.get::<_, String>(1)?) else {
                        continue;
                    };
                    let key: String = row.get(2)?;
                    if kind == Touch::Response {
                        touched.responses.insert((agent, key));
                    } else {
                        touched.sessions.insert(SessionKey::new(agent, key));
                    }
                }
            }
        }
        Ok(touched)
    }

    /// Forget what changed up to revision `up_to`, which the cache has caught
    /// up on.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be written.
    pub(crate) fn forget_touched(&mut self, up_to: i64) -> Result<()> {
        self.connection
            .execute("DELETE FROM touched WHERE revision <= ?1", [up_to])?;
        // The pragma frees a page each time it is stepped, so it is stepped
        // until it has freed them all.
        let mut vacuum = self.connection.prepare("PRAGMA incremental_vacuum")?;
        let mut freed = vacuum.query([])?;
        while freed.next()?.is_some() {}
        Ok(())
    }
}

/// Take the next revision.
pub(super) fn next_revision(transaction: &Transaction) -> Result<i64> {
    Ok(transaction.query_row(
        "UPDATE revision SET value = value + 1 WHERE id = 1 RETURNING value",
        [],
        |row| row.get(0),
    )?)
}

/// What a write changed, as `touched` records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Touch {
    /// A response whose reports changed.
    Response,
    /// A session whose facts, links, reports or presence changed.
    Session,
    /// Every price.
    Prices,
    /// What sign-ins were held where, which every response's account rests
    /// on.
    SignIns,
}

impl Touch {
    /// The kind as the ledger stores it.
    pub(super) fn key(self) -> &'static str {
        match self {
            Touch::Response => "response",
            Touch::Session => "session",
            Touch::Prices => "prices",
            Touch::SignIns => "sign-ins",
        }
    }

    /// The kind with `key`.
    fn from_key(key: &str) -> Option<Touch> {
        [
            Touch::Response,
            Touch::Session,
            Touch::Prices,
            Touch::SignIns,
        ]
        .into_iter()
        .find(|touch| touch.key() == key)
    }
}

/// Record that `revision` changed the response or session `key` of `agent`,
/// as `touch` says, or with [`Touch::Prices`] or [`Touch::SignIns`] and
/// neither, everything of that kind.
pub(super) fn touch(
    transaction: &Transaction,
    revision: i64,
    touch: Touch,
    agent: &str,
    key: &str,
) -> Result<()> {
    transaction
        .prepare_cached("INSERT INTO touched (revision, kind, agent, key) VALUES (?1, ?2, ?3, ?4)")?
        .execute(params![revision, touch.key(), agent, key])?;
    Ok(())
}

/// Record what a batch speaks of as changed at `revision`.
pub(super) fn touch_batch(
    transaction: &Transaction,
    revision: i64,
    agent: Agent,
    batch: &Batch,
) -> Result<()> {
    for observation in batch.observations() {
        touch(
            transaction,
            revision,
            Touch::Response,
            agent.key(),
            &observation.response,
        )?;
    }
    let sessions = batch
        .sessions()
        .map(|(key, _)| key)
        .chain(batch.links().flat_map(|link| [&link.child, &link.parent]))
        .chain(batch.reports().map(|report| &report.session));
    for session in sessions {
        touch(
            transaction,
            revision,
            Touch::Session,
            session.agent().key(),
            session.native(),
        )?;
    }
    Ok(())
}

/// Record as changed at `revision` each session any artifact says a child of
/// `batch`'s links came from, before `batch` says anew where it came from: a
/// parent a child no longer comes from, whichever artifact's link chose it,
/// has lost that child's usage.
pub(super) fn touch_replaced_parents(
    transaction: &Transaction,
    revision: i64,
    batch: &Batch,
) -> Result<()> {
    let mut parents = transaction.prepare_cached(
        "INSERT INTO touched (revision, kind, agent, key)
         SELECT ?1, ?2, p.agent, p.native FROM session_link l
         JOIN session c ON c.id = l.child_id JOIN session p ON p.id = l.parent_id
         WHERE c.agent = ?3 AND c.native = ?4",
    )?;
    for link in batch.links() {
        parents.execute(params![
            revision,
            Touch::Session.key(),
            link.child.agent().key(),
            link.child.native()
        ])?;
    }
    Ok(())
}

/// Record everything the artifact `artifact` has said as changed at
/// `revision`: its responses, and every session it speaks of.
pub(super) fn touch_artifact(
    transaction: &Transaction,
    revision: i64,
    artifact: i64,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO touched (revision, kind, agent, key)
         SELECT ?1, ?3, agent, response FROM observation WHERE artifact_id = ?2",
        params![revision, artifact, Touch::Response.key()],
    )?;
    touch_sessions(transaction, revision, artifact)
}

/// Record every session the artifact `artifact` speaks of as changed at
/// `revision`.
pub(super) fn touch_sessions(
    transaction: &Transaction,
    revision: i64,
    artifact: i64,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO touched (revision, kind, agent, key)
         SELECT ?1, ?3, s.agent, s.native FROM session s WHERE s.id IN (
             SELECT session_id FROM session_fact WHERE artifact_id = ?2
             UNION SELECT session_id FROM observation WHERE artifact_id = ?2
             UNION SELECT session_id FROM report WHERE artifact_id = ?2
             UNION SELECT child_id FROM session_link WHERE artifact_id = ?2
             UNION SELECT parent_id FROM session_link WHERE artifact_id = ?2)",
        params![revision, artifact, Touch::Session.key()],
    )?;
    Ok(())
}
