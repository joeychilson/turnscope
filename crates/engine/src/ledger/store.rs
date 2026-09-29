//! Storing what one read of an artifact found, against that artifact: the
//! artifact and where reading it reached, the sessions and links it names,
//! the reports of responses, the agent's own totals, the trouble found, what
//! was said, and which sessions it holds the transcripts of. A log or a
//! document read again from the start forgets everything it said before, as
//! it holds everything it ever said; a database forgets only what it says
//! again, keeping what it said of rows the agent has since deleted. Every
//! write records what it touched in the change log.

use std::collections::{BTreeSet, HashMap};

use rusqlite::{OptionalExtension, Transaction, params};

use super::changes::{Touch, touch_artifact, touch_batch, touch_replaced_parents};
use super::{Read, pairs, stored};
use crate::agent::{Agent, ArtifactKind, Batch};
use crate::error::{Error, Result};
use crate::session::SessionKey;
use crate::time::Instant;

/// Write what one read of an artifact found, as changes of `revision`.
pub(super) fn write_one(
    transaction: &Transaction,
    revision: i64,
    read: &Read,
    at: Instant,
) -> Result<()> {
    let artifact = store_artifact(transaction, read, at)?;
    touch_batch(transaction, revision, read.agent, &read.batch)?;
    if read.reset {
        match read.kind {
            ArtifactKind::Log | ArtifactKind::Document => {
                forget_all(transaction, revision, artifact)?;
            }
            ArtifactKind::Database => {
                forget_superseded(transaction, revision, artifact, &read.batch)?;
            }
        }
    }
    touch_replaced_parents(transaction, revision, &read.batch)?;
    let mut sessions = Sessions::default();
    store_sessions(transaction, &mut sessions, artifact, &read.batch)?;
    store_observations(
        transaction,
        &mut sessions,
        artifact,
        read.agent,
        &read.batch,
    )?;
    store_reports(transaction, &mut sessions, artifact, &read.batch)?;
    store_diagnostics(transaction, artifact, &read.batch)?;
    store_said(transaction, &mut sessions, artifact, &read.batch)?;
    store_holding(transaction, revision, artifact, &read.batch)?;
    Ok(())
}

/// Forget everything the file `artifact` said, as it is read again from the
/// start: a log or a document holds everything it ever said, so whatever it
/// says now stands for all of it.
fn forget_all(transaction: &Transaction, revision: i64, artifact: i64) -> Result<()> {
    // Whatever it spoke of is to be worked out again.
    touch_artifact(transaction, revision, artifact)?;
    transaction.execute(
        "DELETE FROM said_text WHERE rowid IN (SELECT id FROM said WHERE artifact_id = ?1)",
        [artifact],
    )?;
    transaction.execute("DELETE FROM said WHERE artifact_id = ?1", [artifact])?;
    for table in [
        "observation",
        "report",
        "session_fact",
        "session_link",
        "diagnostic",
        "session_absent",
        "resumption",
    ] {
        transaction.execute(
            &format!("DELETE FROM {table} WHERE artifact_id = ?1"),
            [artifact],
        )?;
    }
    Ok(())
}

/// Forget what the database `artifact` said before of what it says again in
/// `batch`, as it is read again from the start: its reports of the responses
/// the batch reports, its facts, links and totals of the sessions the batch
/// speaks of, and the trouble found reading it. A database drops rows as the
/// agent deletes them, and what it said of those rows is kept, as what a
/// deleted file said is. What was said, for search, is replaced message by
/// message as it is stored.
fn forget_superseded(
    transaction: &Transaction,
    revision: i64,
    artifact: i64,
    batch: &Batch,
) -> Result<()> {
    let mut observation = transaction
        .prepare_cached("DELETE FROM observation WHERE artifact_id = ?1 AND response = ?2")?;
    for reported in batch.observations() {
        observation.execute(params![artifact, reported.response])?;
    }
    let sessions: BTreeSet<&SessionKey> = batch
        .sessions()
        .map(|(key, _)| key)
        .chain(batch.links().map(|link| &link.child))
        .chain(batch.reports().map(|report| &report.session))
        .collect();
    let mut id =
        transaction.prepare_cached("SELECT id FROM session WHERE agent = ?1 AND native = ?2")?;
    // The parent of a link replaced has a tree to work out again.
    let mut touch_parent = transaction.prepare_cached(
        "INSERT INTO touched (revision, kind, agent, key)
         SELECT ?1, ?2, p.agent, p.native FROM session_link l JOIN session p ON p.id = l.parent_id
         WHERE l.artifact_id = ?3 AND l.child_id = ?4",
    )?;
    let mut facts = transaction
        .prepare_cached("DELETE FROM session_fact WHERE artifact_id = ?1 AND session_id = ?2")?;
    let mut links = transaction
        .prepare_cached("DELETE FROM session_link WHERE artifact_id = ?1 AND child_id = ?2")?;
    let mut reports = transaction
        .prepare_cached("DELETE FROM report WHERE artifact_id = ?1 AND session_id = ?2")?;
    for session in sessions {
        let Some(session) = id
            .query_row(params![session.agent().key(), session.native()], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?
        else {
            continue;
        };
        touch_parent.execute(params![revision, Touch::Session.key(), artifact, session])?;
        facts.execute(params![artifact, session])?;
        links.execute(params![artifact, session])?;
        reports.execute(params![artifact, session])?;
    }
    transaction.execute("DELETE FROM diagnostic WHERE artifact_id = ?1", [artifact])?;
    Ok(())
}

/// Add or update the artifact's own row, and return its id.
fn store_artifact(transaction: &Transaction, read: &Read, at: Instant) -> Result<i64> {
    let path = read.path.to_str().ok_or_else(|| {
        Error::corrupt(
            "artifact path",
            format!("{} is not UTF-8", read.path.display()),
        )
    })?;
    let id = transaction.query_row(
        "INSERT INTO artifact (agent, path, kind, device, inode, size, modified, fingerprint,
                               read_to, state, reader_version, present, read_at, closing)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 1, ?12, ?13)
         ON CONFLICT (path) DO UPDATE SET
             agent = excluded.agent, kind = excluded.kind, device = excluded.device,
             inode = excluded.inode, size = excluded.size, modified = excluded.modified,
             fingerprint = excluded.fingerprint, read_to = excluded.read_to,
             state = excluded.state, reader_version = excluded.reader_version, present = 1,
             read_at = excluded.read_at, closing = excluded.closing
         RETURNING id",
        params![
            read.agent.key(),
            path,
            read.kind.key(),
            stored(read.file.device)?,
            stored(read.file.inode)?,
            stored(read.file.size)?,
            read.file.modified,
            read.file.fingerprint,
            stored(read.checkpoint.offset)?,
            read.checkpoint.state,
            read.reader_version,
            at.millis(),
            read.file.closing,
        ],
        |row| row.get(0),
    )?;
    Ok(id)
}

/// Session ids already looked up within one write.
#[derive(Default)]
struct Sessions(HashMap<SessionKey, i64>);

impl Sessions {
    /// The id of the session `key`, adding the session when it is new.
    fn id(&mut self, transaction: &Transaction, key: &SessionKey) -> Result<i64> {
        if let Some(id) = self.0.get(key) {
            return Ok(*id);
        }
        transaction
            .prepare_cached(
                "INSERT INTO session (agent, native) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
            )?
            .execute(params![key.agent().key(), key.native()])?;
        let id = transaction
            .prepare_cached("SELECT id FROM session WHERE agent = ?1 AND native = ?2")?
            .query_row(params![key.agent().key(), key.native()], |row| row.get(0))?;
        self.0.insert(key.clone(), id);
        Ok(id)
    }
}

/// Store facts about sessions, combined with what earlier reads of the
/// artifact found by the rules [`crate::session::SessionFacts`] describes.
fn store_sessions(
    transaction: &Transaction,
    sessions: &mut Sessions,
    artifact: i64,
    batch: &Batch,
) -> Result<()> {
    let mut facts = transaction.prepare_cached(
        "INSERT INTO session_fact
             (artifact_id, session_id, started, last, cwd, branch, version, origin, title, title_rank)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (artifact_id, session_id) DO UPDATE SET
             started = coalesce(min(started, excluded.started), started, excluded.started),
             last = coalesce(max(last, excluded.last), last, excluded.last),
             cwd = coalesce(cwd, excluded.cwd),
             branch = coalesce(excluded.branch, branch),
             version = coalesce(excluded.version, version),
             origin = coalesce(origin, excluded.origin),
             title = CASE WHEN excluded.title IS NOT NULL
                           AND excluded.title_rank >= coalesce(title_rank, 0)
                          THEN excluded.title ELSE title END,
             title_rank = CASE WHEN excluded.title IS NOT NULL
                                AND excluded.title_rank >= coalesce(title_rank, 0)
                               THEN excluded.title_rank ELSE title_rank END",
    )?;
    for (key, session) in batch.sessions() {
        let id = sessions.id(transaction, key)?;
        facts.execute(params![
            artifact,
            id,
            session.started.map(Instant::millis),
            session.last.map(Instant::millis),
            session.cwd,
            session.branch,
            session.version,
            session.origin,
            session.title.as_ref().map(|title| title.text.as_str()),
            session.title.as_ref().map(|title| title.source.rank()),
        ])?;
    }
    let mut links = transaction.prepare_cached(
        "INSERT INTO session_link (artifact_id, child_id, parent_id, kind, launch)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (artifact_id, child_id) DO UPDATE SET
             parent_id = excluded.parent_id, kind = excluded.kind, launch = excluded.launch",
    )?;
    for link in batch.links() {
        let child = sessions.id(transaction, &link.child)?;
        let parent = sessions.id(transaction, &link.parent)?;
        links.execute(params![
            artifact,
            child,
            parent,
            link.kind.key(),
            link.launch
        ])?;
    }
    Ok(())
}

/// Store reports of usage, each combined with any earlier report of the same
/// response from the same artifact by the rules of
/// [`crate::usage::Tokens::merge`].
fn store_observations(
    transaction: &Transaction,
    sessions: &mut Sessions,
    artifact: i64,
    agent: Agent,
    batch: &Batch,
) -> Result<()> {
    // Every right-hand side reads the row as it was before the update, so the
    // session is taken from a new report only when it replaces a copy, and
    // the model only where none was named yet.
    let mut statement = transaction.prepare_cached(
        "INSERT INTO observation
             (artifact_id, agent, response, session_id, copy, at, provider, model, input,
              cache_read, cache_write_5m, cache_write_1h, output, reasoning, prompt, web_searches,
              cost, priority)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
         ON CONFLICT (artifact_id, response) DO UPDATE SET
             session_id = CASE WHEN excluded.copy < copy THEN excluded.session_id ELSE session_id END,
             copy = min(copy, excluded.copy),
             model = CASE WHEN model = '' THEN excluded.model ELSE model END,
             at = max(at, excluded.at),
             input = max(input, excluded.input),
             cache_read = max(cache_read, excluded.cache_read),
             cache_write_5m = max(cache_write_5m, excluded.cache_write_5m),
             cache_write_1h = max(cache_write_1h, excluded.cache_write_1h),
             output = max(output, excluded.output),
             reasoning = max(reasoning, excluded.reasoning),
             prompt = max(prompt, excluded.prompt),
             web_searches = max(web_searches, excluded.web_searches),
             cost = coalesce(max(cost, excluded.cost), cost, excluded.cost),
             priority = max(priority, excluded.priority)",
    )?;
    for observation in batch.observations() {
        let session = sessions.id(transaction, &observation.session)?;
        let tokens = &observation.tokens;
        statement.execute(params![
            artifact,
            agent.key(),
            observation.response,
            session,
            observation.copy,
            observation.at.millis(),
            observation.provider,
            observation.model,
            stored(tokens.input)?,
            stored(tokens.cache_read)?,
            stored(tokens.cache_write_5m)?,
            stored(tokens.cache_write_1h)?,
            stored(tokens.output)?,
            stored(tokens.reasoning)?,
            stored(observation.prompt)?,
            stored(observation.web_searches)?,
            observation.cost.map(|cost| cost.nanos()),
            observation.priority,
        ])?;
    }
    Ok(())
}

/// Store agents' own totals, each replacing whatever the artifact reported of
/// the same session before.
fn store_reports(
    transaction: &Transaction,
    sessions: &mut Sessions,
    artifact: i64,
    batch: &Batch,
) -> Result<()> {
    let mut clear = transaction
        .prepare_cached("DELETE FROM report WHERE artifact_id = ?1 AND session_id = ?2")?;
    let mut insert = transaction.prepare_cached(
        "INSERT INTO report
             (artifact_id, session_id, scope, at, model, input, cache_read, cache_write, output,
              web_searches, cost)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;
    for report in batch.reports() {
        let session = sessions.id(transaction, &report.session)?;
        clear.execute(params![artifact, session])?;
        for total in &report.models {
            insert.execute(params![
                artifact,
                session,
                report.scope.key(),
                report.at.map(Instant::millis),
                total.model.as_deref().unwrap_or(""),
                stored(total.input)?,
                stored(total.cache_read)?,
                stored(total.cache_write)?,
                stored(total.output)?,
                stored(total.web_searches)?,
                total.cost.map(|cost| cost.nanos()),
            ])?;
        }
    }
    Ok(())
}

/// Index what was said, for search, and record where the artifact took up
/// sessions' numbered history, which decides what of it search finds.
fn store_said(
    transaction: &Transaction,
    sessions: &mut Sessions,
    artifact: i64,
    batch: &Batch,
) -> Result<()> {
    // The rows of `said` a message named `?2` of the artifact `?1` holds, and
    // those of the session `?2` in messages `?3`, a JSON list, doesn't name.
    const GIVEN: &str = "SELECT id FROM said WHERE artifact_id = ?1 AND message = ?2";
    const UNHELD: &str = "SELECT id FROM said WHERE artifact_id = ?1 AND session_id = ?2
                          AND message NOT IN (SELECT value FROM json_each(?3))";
    let mut row = transaction.prepare_cached(
        "INSERT INTO said (artifact_id, session_id, at, message, ordinal)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    let mut index =
        transaction.prepare_cached("INSERT INTO said_text (rowid, text) VALUES (?1, ?2)")?;
    // A message given again replaces what it said before, even with nothing.
    for message in batch.given() {
        for query in [
            format!("DELETE FROM said_text WHERE rowid IN ({GIVEN})"),
            format!("DELETE FROM said WHERE id IN ({GIVEN})"),
        ] {
            transaction
                .prepare_cached(&query)?
                .execute(params![artifact, message])?;
        }
    }
    // A message a session no longer holds said nothing it shows.
    for (session, messages) in batch.held_messages() {
        let session = sessions.id(transaction, session)?;
        let held = serde_json::Value::from_iter(messages.iter().map(String::as_str)).to_string();
        for query in [
            format!("DELETE FROM said_text WHERE rowid IN ({UNHELD})"),
            format!("DELETE FROM said WHERE id IN ({UNHELD})"),
        ] {
            transaction
                .prepare_cached(&query)?
                .execute(params![artifact, session, held])?;
        }
    }
    for said in batch.said() {
        let session = sessions.id(transaction, &said.session)?;
        row.execute(params![
            artifact,
            session,
            said.at.map(Instant::millis),
            said.message,
            said.ordinal
        ])?;
        index.execute(params![transaction.last_insert_rowid(), said.text])?;
    }
    let mut resumption = transaction.prepare_cached(
        "INSERT INTO resumption (artifact_id, session_id, at, base) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (artifact_id, session_id) DO UPDATE SET at = excluded.at, base = excluded.base",
    )?;
    for (session, resumed) in batch.resumed() {
        let session = sessions.id(transaction, session)?;
        resumption.execute(params![
            artifact,
            session,
            resumed.at.millis(),
            resumed.base
        ])?;
    }
    Ok(())
}

/// Record which of the sessions the artifact `artifact` speaks of it holds
/// the transcripts of, when its read said, as changes of `revision`: every
/// other is absent from it, now or when read before, until it holds it
/// again. Only the sessions whose standing changed are touched.
fn store_holding(
    transaction: &Transaction,
    revision: i64,
    artifact: i64,
    batch: &Batch,
) -> Result<()> {
    // The sessions held, as `?2` names them.
    const HELD: &str = "SELECT id FROM session WHERE (agent, native) IN
                            (SELECT value ->> 0, value ->> 1 FROM json_each(?2))";
    let Some(holding) = batch.holding() else {
        return Ok(());
    };
    let held = pairs(holding.iter().map(|key| (key.agent().key(), key.native())));
    // The sessions it speaks of and doesn't hold.
    let unheld = format!(
        "SELECT session_id FROM session_fact WHERE artifact_id = ?1
         UNION SELECT session_id FROM observation WHERE artifact_id = ?1
         EXCEPT {HELD}"
    );
    // Those it holds again, and those it holds no more.
    transaction
        .prepare_cached(&format!(
            "INSERT INTO touched (revision, kind, agent, key)
             SELECT ?3, ?4, s.agent, s.native FROM session s
             WHERE s.id IN (SELECT session_id FROM session_absent WHERE artifact_id = ?1)
               AND s.id IN ({HELD})"
        ))?
        .execute(params![artifact, held, revision, Touch::Session.key()])?;
    transaction
        .prepare_cached(&format!(
            "DELETE FROM session_absent WHERE artifact_id = ?1 AND session_id IN ({HELD})"
        ))?
        .execute(params![artifact, held])?;
    transaction
        .prepare_cached(&format!(
            "INSERT INTO touched (revision, kind, agent, key)
             SELECT ?3, ?4, s.agent, s.native FROM session s
             WHERE s.id IN ({unheld})
               AND s.id NOT IN (SELECT session_id FROM session_absent WHERE artifact_id = ?1)"
        ))?
        .execute(params![artifact, held, revision, Touch::Session.key()])?;
    transaction
        .prepare_cached(&format!(
            "INSERT INTO session_absent (artifact_id, session_id)
             SELECT ?1, session_id FROM ({unheld}) WHERE true
             ON CONFLICT DO NOTHING"
        ))?
        .execute(params![artifact, held])?;
    Ok(())
}

/// Store trouble found, adding to what earlier reads of the artifact found.
fn store_diagnostics(transaction: &Transaction, artifact: i64, batch: &Batch) -> Result<()> {
    let mut statement = transaction.prepare_cached(
        "INSERT INTO diagnostic (artifact_id, kind, detail, count, first)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (artifact_id, kind, detail) DO UPDATE SET
             count = count + excluded.count, first = min(first, excluded.first)",
    )?;
    for ((kind, detail), seen) in batch.diagnostics() {
        statement.execute(params![
            artifact,
            kind.key(),
            detail,
            stored(seen.count)?,
            stored(seen.first)?
        ])?;
    }
    Ok(())
}
