//! Storing what the cache works out from the ledger: each response with its
//! reports combined and priced, each session's facts, the sessions each runs
//! within, usage outside the conversation, and the account each drew on.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use rusqlite::{Transaction, params};

use super::schema::recount_accounts;
use crate::agent::{self, AgentReader, SessionReport};
use crate::error::Result;
use crate::folders;
use crate::ledger::{Ledger, SessionRecord, instant, stored, unsigned, usd};
use crate::limits::Timeline;
use crate::model::ModelKey;
use crate::outside::{self, Transcripts};
use crate::price::{self, Basis, PriceBook, Priceable};
use crate::project;
use crate::session::{SessionKey, Tree};
use crate::time::Instant;
use crate::usage::{Response, Tokens, Usd};

/// Session ids already looked up within one catch-up.
#[derive(Default)]
pub(super) struct Ids(HashMap<SessionKey, i64>);

impl Ids {
    /// The id of the session `key`, adding the session, with only its key,
    /// when it is not stored yet. Its facts are stored with it later.
    fn of(&mut self, transaction: &Transaction, key: &SessionKey, tree: &Tree) -> Result<i64> {
        if let Some(id) = self.0.get(key) {
            return Ok(*id);
        }
        let text = key.to_string();
        transaction
            .prepare_cached(
                "INSERT OR IGNORE INTO session (key, agent, native, root, present)
                 VALUES (?1, ?2, ?3, ?4, 1)",
            )?
            .execute(params![
                text,
                key.agent().key(),
                key.native(),
                tree.root(key).to_string()
            ])?;
        let id = transaction
            .prepare_cached("SELECT id FROM session WHERE key = ?1")?
            .query_row([&text], |row| row.get(0))?;
        self.0.insert(key.clone(), id);
        Ok(id)
    }
}

/// Pair each of `keys` with itself and with every session it runs within
/// that is stored, in place of what it was paired with before.
pub(super) fn store_lineage<'a>(
    transaction: &Transaction,
    keys: impl Iterator<Item = &'a SessionKey>,
    tree: &Tree,
) -> Result<()> {
    let mut forget = transaction.prepare_cached(
        "DELETE FROM lineage WHERE session = (SELECT id FROM session WHERE key = ?1)",
    )?;
    let mut pair = transaction.prepare_cached(
        "INSERT OR IGNORE INTO lineage (session, ancestor)
         SELECT s.id, a.id FROM session s JOIN session a ON a.key = ?2 WHERE s.key = ?1",
    )?;
    for key in keys {
        let text = key.to_string();
        forget.execute([&text])?;
        pair.execute([&text, &text])?;
        for above in tree.ancestors(key) {
            pair.execute([text.as_str(), &above.to_string()])?;
        }
    }
    Ok(())
}

/// Which account usage drew on, found as it is stored.
pub(super) struct Drawn<'a> {
    /// What was signed in where, over time.
    pub(super) timeline: Timeline,
    /// The home directory the agents' own folders are in.
    pub(super) home: &'a Path,
    /// The folder each session is kept in, when not its agent's own.
    pub(super) folders: HashMap<SessionKey, PathBuf>,
}

impl Drawn<'_> {
    /// The account usage of `session`, of `provider`, at `at` in
    /// milliseconds, drew on.
    fn account(
        &self,
        session: &SessionKey,
        provider: &str,
        at: Option<i64>,
    ) -> Option<Cow<'_, str>> {
        let agent = session.agent();
        match self.folders.get(session) {
            Some(folder) => self.timeline.account(agent, folder, provider, at),
            None => self
                .timeline
                .account(agent, &folders::own(agent, self.home), provider, at),
        }
    }
}

/// Store one response, priced, and, given what it `drew` on, with the
/// account; without, its account is put down after ([`attribute`]).
pub(super) fn store_response(
    transaction: &Transaction,
    ids: &mut Ids,
    response: &Response,
    book: &PriceBook,
    tree: &Tree,
    drew: Option<&Drawn>,
) -> Result<()> {
    let session = ids.of(transaction, &response.session, tree)?;
    let cost = price::cost(&Priceable::of(response), book);
    let tokens = &response.tokens;
    let account = drew.and_then(|drew| {
        drew.account(
            &response.session,
            &response.provider,
            Some(response.at.millis()),
        )
    });
    transaction
        .prepare_cached(
            "INSERT OR REPLACE INTO usage
                 (agent, response, session, kind, at, provider, model, model_key, input, cache_read,
                  cache_write_5m, cache_write_1h, output, reasoning, prompt, web_searches, priority,
                  recorded, cost, basis, approximate, account)
             VALUES (?1, ?2, ?3, 'response', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                     ?17, ?18, ?19, ?20, ?21)",
        )?
        .execute(params![
            response.agent.key(),
            response.key,
            session,
            response.at.millis(),
            response.provider,
            response.model,
            ModelKey::of(&response.model).as_str(),
            stored(tokens.input)?,
            stored(tokens.cache_read)?,
            stored(tokens.cache_write_5m)?,
            stored(tokens.cache_write_1h)?,
            stored(tokens.output)?,
            stored(tokens.reasoning)?,
            stored(response.prompt)?,
            stored(response.web_searches)?,
            response.priority,
            response.recorded.map(Usd::nanos),
            cost.map(|cost| cost.usd.nanos()),
            cost.map(|cost| cost.basis.key()),
            cost.is_some_and(|cost| cost.approximate),
            account,
        ])?;
    Ok(())
}

/// Which folder of its agent's each session is kept in, as its artifacts'
/// paths say.
pub(super) struct Kept<'a> {
    readers: Vec<Box<dyn AgentReader>>,
    home: &'a Path,
}

impl<'a> Kept<'a> {
    /// Sessions' folders among the agents' folders under `home`.
    pub(super) fn in_home(home: &'a Path) -> Kept<'a> {
        Kept {
            readers: agent::readers(),
            home,
        }
    }

    /// The folder `record` is kept in when it isn't its agent's own, as a
    /// second account's is: `None` for a session kept in its agent's own
    /// folder, or whose artifacts the ledger no longer holds.
    pub(super) fn folder(&self, record: &SessionRecord) -> Option<String> {
        let agent = record.key.agent();
        let reader = self.readers.iter().find(|reader| reader.agent() == agent)?;
        let folder = agent::folder_of(reader.as_ref(), record.artifact.as_deref()?)?;
        (folder != folders::own(agent, self.home))
            .then(|| folder.to_str().map(str::to_owned))
            .flatten()
    }
}

/// Store a session's facts, where it came from, its project, and the folder
/// it is kept in.
pub(super) fn store_session(
    transaction: &Transaction,
    record: &SessionRecord,
    tree: &Tree,
    ledger: &Ledger,
    kept: &Kept,
) -> Result<()> {
    let facts = &record.facts;
    let project = facts
        .cwd
        .as_deref()
        .map(|cwd| ledger.project_of(cwd, |path| project::resolve(path, kept.home)))
        .transpose()?
        // As its folder is spelled on disk, which the ledger may have found
        // it by another spelling of.
        .map(|root| project::spelled(&root));
    let (parent, link) = record
        .parent
        .as_ref()
        .map_or((None, None), |(parent, kind)| {
            (Some(parent.to_string()), Some(kind.key()))
        });
    transaction
        .prepare_cached(
            "INSERT INTO session (key, agent, native, title, cwd, project, project_name, branch,
                                  started, last, parent, link, root, present, folder)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT (key) DO UPDATE SET
                 title = excluded.title, cwd = excluded.cwd, project = excluded.project,
                 project_name = excluded.project_name, branch = excluded.branch,
                 started = excluded.started, last = excluded.last, parent = excluded.parent,
                 link = excluded.link, root = excluded.root, present = excluded.present,
                 folder = excluded.folder",
        )?
        .execute(params![
            record.key.to_string(),
            record.key.agent().key(),
            record.key.native(),
            facts.title.as_ref().map(|title| title.text.as_str()),
            facts.cwd,
            project.as_ref().map(|(root, _)| root.as_str()),
            project.as_ref().map(|(_, name)| name.as_str()),
            facts.branch,
            facts.started.map(Instant::millis),
            facts.last.map(Instant::millis),
            parent,
            link,
            tree.root(&record.key).to_string(),
            record.present,
            kept.folder(record),
        ])?;
    Ok(())
}

/// Put the usage of each of `sessions`, or of every session, down to the
/// account it drew on by `timeline`, the agents' own folders being under
/// `home`, and each such session to the account most of its own usage drew
/// on. Gives the sessions any of whose usage changed account.
pub(super) fn attribute(
    transaction: &Transaction,
    timeline: &Timeline,
    home: &Path,
    sessions: Option<&BTreeSet<SessionKey>>,
) -> Result<BTreeSet<SessionKey>> {
    const USAGE: &str = "SELECT u.agent, u.response, u.provider, u.at, u.account, s.key, s.folder
                         FROM usage u JOIN session s ON s.id = u.session";
    let mut changed: BTreeSet<SessionKey> = BTreeSet::new();
    let mut put = transaction
        .prepare_cached("UPDATE usage SET account = ?3 WHERE agent = ?1 AND response = ?2")?;
    let mut visit = |row: &rusqlite::Row| -> Result<()> {
        let agent_key: String = row.get(0)?;
        let Some(agent) = crate::agent::Agent::from_key(&agent_key) else {
            return Ok(());
        };
        let provider: String = row.get(2)?;
        let at: Option<i64> = row.get(3)?;
        let was: Option<String> = row.get(4)?;
        let folder = row
            .get::<_, Option<String>>(6)?
            .map_or_else(|| folders::own(agent, home), PathBuf::from);
        let account = timeline.account(agent, &folder, &provider, at);
        if account.as_deref() != was.as_deref() {
            put.execute(params![agent_key, row.get::<_, String>(1)?, account])?;
            if let Some(key) = SessionKey::parse(&row.get::<_, String>(5)?) {
                changed.insert(key);
            }
        }
        Ok(())
    };
    match sessions {
        None => {
            let mut statement = transaction.prepare(USAGE)?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                visit(row)?;
            }
        }
        Some(sessions) => {
            let mut statement = transaction.prepare_cached(&format!("{USAGE} WHERE s.key = ?1"))?;
            for key in sessions {
                let mut rows = statement.query([key.to_string()])?;
                while let Some(row) = rows.next()? {
                    visit(row)?;
                }
            }
        }
    }
    recount_accounts(transaction, &changed)?;
    Ok(changed)
}

/// Work out again the usage `report` shows outside the conversation, from
/// what the cache holds of the sessions it covers, and, given what it
/// `drew` on, with its accounts; without, they are put down after
/// ([`attribute`]).
pub(super) fn store_outside(
    transaction: &Transaction,
    ids: &mut Ids,
    report: &SessionReport,
    tree: &Tree,
    book: &PriceBook,
    drew: Option<&Drawn>,
) -> Result<()> {
    let root = report.session.to_string();
    let root_id = ids.of(transaction, &report.session, tree)?;
    let covered = outside::covered(report, tree);
    let mut transcripts = Transcripts::default();
    {
        let mut read = transaction.prepare_cached(
            "SELECT u.provider, u.model, u.at, u.input, u.cache_read, u.cache_write_5m,
                    u.cache_write_1h, u.output, u.reasoning, u.recorded
             FROM usage u JOIN session s ON s.id = u.session WHERE s.key = ?1 AND u.kind = 'response'",
        )?;
        for session in &covered {
            let mut rows = read.query([session.to_string()])?;
            while let Some(row) = rows.next()? {
                let provider: String = row.get(0)?;
                let model: String = row.get(1)?;
                let at = instant(row.get(2)?, "cached usage time")?;
                let tokens = Tokens {
                    input: unsigned(row.get(3)?, "cached count")?,
                    cache_read: unsigned(row.get(4)?, "cached count")?,
                    cache_write_5m: unsigned(row.get(5)?, "cached count")?,
                    cache_write_1h: unsigned(row.get(6)?, "cached count")?,
                    output: unsigned(row.get(7)?, "cached count")?,
                    reasoning: unsigned(row.get(8)?, "cached count")?,
                };
                let recorded = usd(row.get(9)?, "cached cost")?;
                transcripts.add(&provider, &model, at, &tokens, recorded);
            }
        }
    }
    // With no transcripts to spread it over, it is put when the agent
    // reported it, or else when the session was last active.
    let when = match report.at {
        Some(at) => Some(at),
        None => transaction
            .prepare_cached("SELECT coalesce(last, started) FROM session WHERE id = ?1")?
            .query_row([root_id], |row| row.get::<_, Option<i64>>(0))?
            .map(|millis| instant(millis, "cached session time"))
            .transpose()?,
    };
    for part in outside::outside(report, &transcripts, when) {
        // What the agent's own totals say it cost is its estimate.
        let cost = part.cost.map(|usd| (usd, Basis::Agent)).or_else(|| {
            price::cost(
                &Priceable {
                    agent: report.session.agent(),
                    at: part.quarter,
                    provider: &part.provider,
                    model: &part.model,
                    tokens: &part.tokens,
                    prompt: 0,
                    web_searches: 0,
                    priority: false,
                    recorded: None,
                },
                book,
            )
            .map(|cost| (cost.usd, cost.basis))
        });
        let key = part.key.as_ref().map(ModelKey::as_str);
        let quarter = part.quarter.map(Instant::millis);
        let account = drew.and_then(|drew| drew.account(&report.session, &part.provider, quarter));
        transaction
            .prepare_cached(
                "INSERT INTO usage
                     (agent, response, session, kind, at, provider, model, model_key, input, cache_read,
                      cache_write_5m, cache_write_1h, output, reasoning, prompt, web_searches, priority,
                      recorded, cost, basis, approximate, account)
                 VALUES (?1, ?2, ?3, 'outside', ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, 0, 0, 0, NULL,
                         ?13, ?14, 1, ?15)",
            )?
            .execute(params![
                report.session.agent().key(),
                format!(
                    "outside:{root}:{}:{}",
                    key.unwrap_or("*"),
                    quarter.map_or_else(|| "none".to_owned(), |quarter| quarter.to_string())
                ),
                root_id,
                quarter,
                part.provider,
                part.model,
                key.unwrap_or(""),
                stored(part.tokens.input)?,
                stored(part.tokens.cache_read)?,
                stored(part.tokens.cache_write_5m)?,
                stored(part.tokens.cache_write_1h)?,
                stored(part.tokens.output)?,
                cost.map(|(usd, _)| usd.nanos()),
                cost.map(|(_, basis)| basis.key()),
                account,
            ])?;
    }
    Ok(())
}
