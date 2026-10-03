//! The cache: everything derived from the ledger, in the form queries read.
//!
//! `cache.sqlite` holds each response with its reports combined and priced,
//! each session with what every artifact says of it combined and its totals,
//! and the usage agents' own totals show outside the conversation. Nothing in
//! it is kept for its own sake: a cache of another schema, or one that cannot
//! be read, is deleted and built again from the ledger, and one built from
//! another ledger than the one beside it is built again.
//!
//! **Responses.** A response's reports combine into one: the largest of each
//! count, so streamed repeats and a fork's copies settle on the complete
//! response; the session whose own history holds it, never one that holds a
//! copy; and the latest time any report gives, a copy's included. Each is
//! priced as [`crate::price`] says, with what its cost rests on. A review, as
//! Codex runs one to approve an action, is counted nowhere: no session, no
//! usage and no search hit, though the ledger keeps what it said and doctor,
//! which reports what the files hold, counts it.
//!
//! **Sessions.** Each session has an integer id, its facts, and its totals.
//! Each is paired with itself and with every session it runs within, however
//! deep, so a session's usage with its subagents' is one join: 1,831 pairs
//! for 1,143 sessions on 2026-09-27. A session kept in a folder other than
//! its agent's own ([`crate::Folder`]) has that folder, found from the path
//! of the first of its artifacts, so that it is taken up again there.
//!
//! **Accounts.** Each response, and each stretch of usage outside the
//! conversation, is put down to the account it drew on, the one signed in
//! where and when it was made, as `crate::limits::attribution` finds it from
//! what the ledger kept of sign-ins; and each session to the account most of
//! its own responses drew on. Only when what a place held begins to differ
//! is every response's account worked out again, which on its own changes
//! none of their totals. A doctor run building everything again took 2.73 s
//! where it had taken 2.38 before accounts were kept (2026-09-29, 178K
//! responses, fastest of four each): 0.14 s of it keeping the index usage is
//! found by account with, and 0.14 s finding each session's account.
//!
//! **The rollup** sums usage by quarter hour (UTC), session, provider, model,
//! kind and account: 10,664 rows here on 2026-09-28, before it was kept by
//! account. A quarter hour covers every real time zone's offset, so local
//! hours and days are assembled from it at query time. A session's rollup is
//! summed again whenever any of its usage is put down to another account, so
//! usage by account reads the same from the rollup as from each response.
//!
//! On 2026-09-23, 121K responses from 5.5 GB of history made a 46 MB cache,
//! beside a 50 MB ledger, 8 MB of it the search index.
//!
//! It stays current by catching up. Every ledger write records what it
//! changed; the cache works out again only those responses and sessions, and
//! the ledger then forgets what the cache caught up on. Catching up reads only
//! the sessions touched, their trees' own totals, and the links between all
//! of them for the tree: after one line of a session, about 6 ms, most of it
//! that session's totals (2026-09-24). When more than half of the responses
//! it holds were touched, as by a first read, or prices changed, it builds
//! everything again instead, which comes to the same and is quicker: 3.1 s
//! for 132K responses, where working out each change took 6.3 s
//! (2026-09-24), and 3.8 to 5.0 s for 141K after new prices (2026-09-26, a
//! release build on a busy Mac). Files are never read again for it.
//!
//! **Outside the conversation.** Where an agent's own totals for a session
//! exceed what its transcripts show, the difference is usage the transcripts
//! never recorded, as [`crate::outside`] works it out. It is kept as usage of
//! kind `outside`, spread over the quarter hours the session was active in,
//! in proportion to the session's own usage then, and marked approximate: its
//! amount is the agent's, its timing and price tier are estimates. Where no
//! time is known at all, as when a session's transcripts are gone and only
//! the agent's totals remain, it is kept at none: in the session's totals
//! and in all time, and in no stretch of time, so kept out of the rollup,
//! and priced at its model's first prices.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::agent::{self, AgentReader, SessionReport};
use crate::error::{Error, Result};
use crate::folders;
use crate::ledger::{Ledger, SessionRecord, instant, stored, unsigned, usd};
use crate::limits::Timeline;
use crate::model::ModelKey;
use crate::outside::{self, Transcripts};
use crate::price::{self, Basis, PriceBook, Priceable};
use crate::project;
use crate::session::{LinkKind, SessionKey, Tree};
use crate::sharing;
use crate::time::Instant;
use crate::usage::{Response, Tokens, Usd};

/// The cache's file name, in the data directory.
pub(crate) const FILE: &str = "cache.sqlite";

/// The schema's version. Increase it with any change to the schema or to how
/// anything in the cache is worked out, as a new agent's usage in it is; the
/// cache is then built again. So builds that read different agents never
/// share a cache: one that passes over an agent's changes never catches up a
/// cache holding that agent's usage.
const SCHEMA: i64 = 14;

/// A quarter hour, in milliseconds: the grain of the rollup, and of usage
/// outside the conversation.
pub(crate) const QUARTER: i64 = 15 * 60 * 1000;

/// The start of the quarter hour usage `u` falls in, as the rollup keys it:
/// rounded down, as Rust's `div_euclid` rounds, where SQLite's division
/// rounds toward zero and would put usage before 1970 in the quarter hour
/// after its own.
const QUARTER_OF: &str = "(u.at / 900000 - (u.at % 900000 < 0)) * 900000";

/// The sum of `expression` over a group, as [`AGGREGATES`] adds: an integer
/// that stops at the largest `i64`, where SQLite's `sum` fails the whole
/// question, or a whole catch-up, with an overflow.
///
/// `total` adds in floating point, exactly while the sum stays below 2^53,
/// some 9 quadrillion tokens or $9 million in nano-dollars, and to within a
/// few units past it. A sum at or past the largest `i64` comes out as it,
/// as the sums worked out in Rust stop there too (`Usd::saturating_add`,
/// `Tokens::add`), and in any order.
pub(crate) fn capped_sum(expression: &str) -> String {
    capped(&format!("total({expression})"))
}

/// `expression`, a number, as an integer that stops at the largest `i64`,
/// where SQLite would turn it to floating point.
pub(crate) fn capped(expression: &str) -> String {
    format!("CAST(min({expression}, {}.0) AS INTEGER)", i64::MAX)
}

/// The aggregates every figure of usage is added up with, over usage `u`: a
/// session's totals, the rollup's `c1` to `c13`, and usage answered from each
/// response, so that all three agree. Sums are [`capped_sum`]'s; a sum of
/// some of the usage adds nothing, a null, for the rest.
pub(crate) static AGGREGATES: LazyLock<[String; 13]> = LazyLock::new(|| {
    [
        "count(*) FILTER (WHERE u.kind = 'response')".to_owned(),
        capped_sum("u.input"),
        capped_sum("u.cache_read"),
        capped_sum("u.cache_write_5m"),
        capped_sum("u.cache_write_1h"),
        capped_sum("u.output"),
        capped_sum("u.reasoning"),
        capped_sum("u.cost"),
        "count(*) - count(u.cost)".to_owned(),
        "coalesce(max(u.approximate), 0)".to_owned(),
        capped_sum(
            "CASE WHEN u.kind = 'outside'
             THEN u.input + u.cache_read + u.cache_write_5m + u.cache_write_1h + u.output END",
        ),
        capped_sum("CASE WHEN u.basis = 'charged' THEN u.cost END"),
        capped_sum("CASE WHEN u.basis = 'agent' THEN u.cost END"),
    ]
});

const TABLES: &str = r"
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE TABLE usage (
    agent TEXT NOT NULL,
    response TEXT NOT NULL,
    session INTEGER NOT NULL,
    kind TEXT NOT NULL,
    -- NULL only for usage outside the conversation at no time known.
    at INTEGER,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    model_key TEXT NOT NULL,
    input INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cache_write_5m INTEGER NOT NULL,
    cache_write_1h INTEGER NOT NULL,
    output INTEGER NOT NULL,
    reasoning INTEGER NOT NULL,
    prompt INTEGER NOT NULL,
    web_searches INTEGER NOT NULL,
    priority INTEGER NOT NULL,
    recorded INTEGER,
    cost INTEGER,
    basis TEXT,
    approximate INTEGER NOT NULL,
    -- The account it drew on, as `crate::limits::attribution` finds it; NULL
    -- for none known.
    account TEXT,
    PRIMARY KEY (agent, response)
) STRICT, WITHOUT ROWID;
CREATE INDEX usage_at ON usage (at);
CREATE INDEX usage_session ON usage (session);
-- Covering what a limit's window reads of each response, so a week of
-- them is read from the index alone.
CREATE INDEX usage_account ON usage (account, at, cost, model_key, session);

CREATE TABLE session (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE,
    agent TEXT NOT NULL,
    native TEXT NOT NULL,
    title TEXT,
    cwd TEXT,
    project TEXT,
    project_name TEXT,
    branch TEXT,
    started INTEGER,
    last INTEGER,
    -- The latest `last` of the session and of every session run within it,
    -- however deep: when any of its work was last done.
    active INTEGER,
    parent TEXT,
    link TEXT,
    root TEXT NOT NULL,
    present INTEGER NOT NULL,
    -- The folder of its agent's it is kept in, as a second account's is;
    -- NULL for its agent's own.
    folder TEXT,
    -- The account most of its own responses drew on, then most of its
    -- usage outside them, then the first by id; NULL for none known.
    account TEXT,
    responses INTEGER NOT NULL DEFAULT 0,
    input INTEGER NOT NULL DEFAULT 0,
    cache_read INTEGER NOT NULL DEFAULT 0,
    cache_write_5m INTEGER NOT NULL DEFAULT 0,
    cache_write_1h INTEGER NOT NULL DEFAULT 0,
    output INTEGER NOT NULL DEFAULT 0,
    reasoning INTEGER NOT NULL DEFAULT 0,
    cost INTEGER NOT NULL DEFAULT 0,
    unpriced INTEGER NOT NULL DEFAULT 0,
    approximate INTEGER NOT NULL DEFAULT 0,
    outside INTEGER NOT NULL DEFAULT 0,
    charged INTEGER NOT NULL DEFAULT 0,
    estimated INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX session_root ON session (root);
CREATE INDEX session_parent ON session (parent);
CREATE INDEX session_active ON session (active);

-- Each session paired with itself and with every session it runs within, up
-- subagent and review links however deep: what a session's usage counts in.
CREATE TABLE lineage (
    session INTEGER NOT NULL,
    ancestor INTEGER NOT NULL,
    PRIMARY KEY (session, ancestor)
) STRICT, WITHOUT ROWID;
-- A session's tree, found from its top.
CREATE INDEX lineage_ancestor ON lineage (ancestor, session);

CREATE TABLE rollup (
    quarter INTEGER NOT NULL,
    session INTEGER NOT NULL,
    agent TEXT NOT NULL,
    provider TEXT NOT NULL,
    model_key TEXT NOT NULL,
    kind TEXT NOT NULL,
    -- The account it drew on, as usage keeps it, and '' for none known.
    account TEXT NOT NULL,
    c1 INTEGER NOT NULL,
    c2 INTEGER NOT NULL,
    c3 INTEGER NOT NULL,
    c4 INTEGER NOT NULL,
    c5 INTEGER NOT NULL,
    c6 INTEGER NOT NULL,
    c7 INTEGER NOT NULL,
    c8 INTEGER NOT NULL,
    c9 INTEGER NOT NULL,
    c10 INTEGER NOT NULL,
    c11 INTEGER NOT NULL,
    c12 INTEGER NOT NULL,
    c13 INTEGER NOT NULL,
    PRIMARY KEY (quarter, session, provider, model_key, kind, account)
) STRICT, WITHOUT ROWID;
CREATE INDEX rollup_session ON rollup (session);
-- Usage summed by quarter hour, session, provider, model, kind and account,
-- with AGGREGATES as c1 to c13. Most questions are answered from here, which holds
-- a small fraction of the rows usage does.
";

/// Work out the rollup of the usage `u` that `which`, a condition, keeps:
/// of what has a time, as usage at no time known is in no quarter hour.
fn rollup(which: &str) -> String {
    format!(
        "INSERT INTO rollup {}",
        quarters(&format!("WHERE u.at IS NOT NULL AND {which}"))
    )
}

/// The usage `u` that `which`, a `WHERE` clause or nothing, keeps, summed
/// into rows as the rollup holds them, with its columns' names: what the
/// rollup is built from, and what a question reads where the rollup can't
/// split a quarter hour.
pub(crate) fn quarters(which: &str) -> String {
    let aggregates: Vec<String> = AGGREGATES
        .iter()
        .enumerate()
        .map(|(index, expression)| format!("{expression} AS c{}", index + 1))
        .collect();
    format!(
        "SELECT {QUARTER_OF} AS quarter, u.session AS session, u.agent AS agent,
                u.provider AS provider, u.model_key AS model_key, u.kind AS kind,
                coalesce(u.account, '') AS account, {}
         FROM usage u {which} GROUP BY 1, 2, 4, 5, 6, 7",
        aggregates.join(", ")
    )
}

/// Forget the rollup of the session whose key is `?1`.
const CLEAR_ROLLUP: &str =
    "DELETE FROM rollup WHERE session = (SELECT id FROM session WHERE key = ?1)";

/// Work out again the rollup of each of the sessions `keys`.
fn roll_up(transaction: &Transaction, keys: &BTreeSet<SessionKey>) -> Result<()> {
    let mut clear = transaction.prepare_cached(CLEAR_ROLLUP)?;
    let mut rollup = transaction.prepare_cached(&rollup(
        "u.session = (SELECT id FROM session WHERE key = ?1)",
    ))?;
    for key in keys {
        let key = key.to_string();
        clear.execute([&key])?;
        rollup.execute([&key])?;
    }
    Ok(())
}

/// Work out again the account of each of the sessions `keys`, from its
/// usage as it stands.
fn recount_accounts<'a>(
    transaction: &Transaction,
    keys: impl IntoIterator<Item = &'a SessionKey>,
) -> Result<()> {
    let mut account = transaction.prepare_cached(&session_account("WHERE key = ?1"))?;
    for key in keys {
        account.execute([key.to_string()])?;
    }
    Ok(())
}

/// Work out the totals of the sessions `which`, a `WHERE` clause or nothing,
/// keeps, from their usage.
fn totals(which: &str) -> String {
    format!(
        "UPDATE session SET (responses, input, cache_read, cache_write_5m, cache_write_1h, output,
                             reasoning, cost, unpriced, approximate, outside, charged, estimated)
             = (SELECT {} FROM usage u WHERE u.session = session.id) {which}",
        AGGREGATES.join(", ")
    )
}

/// Work out the account of each session `which`, a `WHERE` clause or
/// nothing, keeps, from its usage's: the one most of its responses drew on,
/// then most of its usage outside them, then the first by id.
fn session_account(which: &str) -> String {
    format!(
        "UPDATE session SET account = (
             SELECT u.account FROM usage u WHERE u.session = session.id AND u.account IS NOT NULL
             GROUP BY u.account
             ORDER BY count(*) FILTER (WHERE u.kind = 'response') DESC, count(*) DESC, u.account
             LIMIT 1) {which}"
    )
}

/// Work out when the sessions `which`, a `WHERE` clause or nothing, keeps
/// were last active, they or any session run within them, from their own
/// `last` and those their lineage pairs beneath them.
fn active(which: &str) -> String {
    format!(
        "UPDATE session SET active = (
             SELECT max(at) FROM (
                 SELECT session.last AS at
                 UNION ALL
                 SELECT d.last FROM lineage l JOIN session d ON d.id = l.session
                 WHERE l.ancestor = session.id)) {which}"
    )
}

/// What catching up changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Caught {
    /// The ledger revision the cache now reflects.
    pub revision: i64,
    /// Whether the cache was built again from nothing.
    pub rebuilt: bool,
    /// Whether prices changed, so every cost was worked out again.
    pub prices: bool,
    /// Sessions whose facts or totals changed.
    pub sessions: BTreeSet<SessionKey>,
}

/// The cache's database.
pub(crate) struct Cache {
    connection: Connection,
    /// Where it is.
    path: PathBuf,
    /// The device and inode of the file opened there, which tell whether
    /// another build has put a file of its own in its place since.
    file: (u64, u64),
}

impl Cache {
    /// Open the cache at `path`, building it anew when it has an older
    /// schema or cannot be read.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NewerCache`] when a newer build made it, which is
    /// left as it is; [`Error::Io`] when a stale cache cannot be deleted; and
    /// [`Error::Ledger`] when a new one cannot be created.
    pub(crate) fn open(path: &Path) -> Result<Cache> {
        if let Some((connection, schema)) = Cache::existing(path) {
            if schema == SCHEMA {
                return Cache::at(path, connection);
            }
            // A newer build's cache is left for it, as its ledger is.
            refuse_newer(schema)?;
        }
        for stale in [
            path.to_path_buf(),
            sharing::side_file(path, "-wal"),
            sharing::side_file(path, "-shm"),
        ] {
            match std::fs::remove_file(&stale) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(Error::io(stale, error)),
            }
        }
        let connection = connect(path)?;
        connection.execute_batch(TABLES)?;
        connection.execute(
            "INSERT INTO meta (key, value) VALUES ('schema', ?1), ('revision', 0)",
            [SCHEMA],
        )?;
        Cache::at(path, connection)
    }

    /// The cache at `path` and the schema it records, when it exists and can
    /// be read.
    fn existing(path: &Path) -> Option<(Connection, i64)> {
        if !path.exists() {
            return None;
        }
        let connection = connect(path).ok()?;
        let schema = meta(&connection, "schema").ok()?;
        Some((connection, schema))
    }

    /// The cache at `path` for reading only, as it stands: nothing is
    /// created or built, and every write fails.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NewerCache`] when a newer build made it, whose
    /// contents this build's questions would misread; [`Error::Ledger`] when
    /// it cannot be opened; and [`Error::Io`] when its file cannot be looked
    /// at.
    pub(crate) fn reader(path: &Path) -> Result<Cache> {
        let connection = sharing::reader(path)?;
        refuse_newer(meta(&connection, "schema")?)?;
        Cache::at(path, connection)
    }

    /// Whether the file it was opened at is still the one at its path: not
    /// replaced since by another build sharing the data directory, whose
    /// cache a connection to this one would never see.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] when the path cannot be looked at.
    pub(crate) fn current(&self) -> Result<bool> {
        Ok(file_at(&self.path)? == Some(self.file))
    }

    /// The cache `connection` opened at `path`.
    fn at(path: &Path, connection: Connection) -> Result<Cache> {
        Ok(Cache {
            connection,
            path: path.to_path_buf(),
            file: file_at(path)?.ok_or_else(|| {
                Error::io(path, std::io::Error::from(std::io::ErrorKind::NotFound))
            })?,
        })
    }

    /// The database, for reading.
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Bring the cache up to the ledger's latest revision, pricing usage from
    /// `book` and finding projects for directories under `home`.
    ///
    /// # Errors
    ///
    /// Returns an error when the ledger or the cache cannot be read or
    /// written. The cache is then left at the revision it last reached, and
    /// the next catch-up tries again from there.
    pub(crate) fn catch_up(
        &mut self,
        ledger: &mut Ledger,
        book: &PriceBook,
        home: &Path,
    ) -> Result<Caught> {
        // Another build sharing the data directory may have put a cache of
        // its own in this one's place since it was opened: a newer build's is
        // refused, and an older one's built anew. Nothing is written through
        // a connection to a file no longer there, and the ledger forgets
        // nothing on its behalf.
        if !self.current()? {
            let path = self.path.clone();
            *self = Cache::open(&path)?;
        }
        let reached = meta(&self.connection, "revision")?;
        let built_from = meta(&self.connection, "ledger").optional()?;
        // A cache built from another ledger, as from one deleted and made
        // anew, or from a later state of this one, as when a copy of it is
        // restored, holds what this ledger doesn't say.
        let caught = if built_from != Some(ledger.identity()?) || reached > ledger.revision()? {
            self.rebuild(ledger, book, home)?
        } else {
            let touched = ledger.touched_since(reached)?;
            if touched.up_to == reached {
                return Ok(Caught {
                    revision: reached,
                    ..Caught::default()
                });
            }
            // New prices change every cost, and where most of what the cache
            // holds changed, as when history is first read, building it all
            // again is quicker than working out each change, and comes to
            // the same. What it holds is counted from sessions' totals, which
            // count the same responses: counting them in usage read every
            // row, 31 ms, or 690 from disk, at every catch-up (2026-09-30,
            // 169K responses).
            let held: i64 = self.connection.query_row(
                "SELECT coalesce(sum(responses), 0) FROM session",
                [],
                |row| row.get(0),
            )?;
            if touched.prices
                || i64::try_from(touched.responses.len()).unwrap_or(i64::MAX) > held / 2
            {
                Caught {
                    prices: touched.prices,
                    ..self.rebuild(ledger, book, home)?
                }
            } else {
                self.update(ledger, book, home, touched)?
            }
        };
        ledger.forget_touched(caught.revision)?;
        Ok(caught)
    }

    /// Build everything from the ledger.
    fn rebuild(&mut self, ledger: &Ledger, book: &PriceBook, home: &Path) -> Result<Caught> {
        let revision = ledger.revision()?;
        let responses = ledger.responses()?;
        let parents = ledger.parents()?;
        let records = ledger.sessions(&parents)?;
        let reports = ledger.reports()?;
        let transaction = self.connection.transaction()?;
        transaction.execute_batch(
            "DELETE FROM rollup; DELETE FROM usage; DELETE FROM lineage; DELETE FROM session;",
        )?;
        let tree = Tree::of(&parents);
        let skipped = reviews(&parents);
        let mut ids = Ids::default();
        let kept = Kept::in_home(home);
        for record in records
            .iter()
            .filter(|record| !skipped.contains(&record.key))
        {
            store_session(&transaction, record, &tree, ledger, &kept)?;
        }
        // Each account is put down as the usage is stored, where putting it
        // down after took a second and a half for 178K responses
        // (2026-09-29).
        let drawn = Drawn {
            timeline: Timeline::read(ledger)?,
            home,
            folders: records
                .iter()
                .filter_map(|record| {
                    Some((record.key.clone(), PathBuf::from(kept.folder(record)?)))
                })
                .collect(),
        };
        for response in responses
            .iter()
            .filter(|response| !skipped.contains(&response.session))
        {
            store_response(&transaction, &mut ids, response, book, &tree, Some(&drawn))?;
        }
        for report in reports
            .iter()
            .filter(|report| !skipped.contains(&report.session))
        {
            store_outside(&transaction, &mut ids, report, &tree, book, Some(&drawn))?;
        }
        let stored: Vec<String> = transaction
            .prepare("SELECT key FROM session")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        store_lineage(
            &transaction,
            stored
                .iter()
                .filter_map(|key| SessionKey::parse(key))
                .collect::<Vec<_>>()
                .iter(),
            &tree,
        )?;
        transaction.execute(&totals(""), [])?;
        transaction.execute(&rollup("1"), [])?;
        transaction.execute(&active(""), [])?;
        transaction.execute(&session_account(""), [])?;
        set_meta(&transaction, "ledger", ledger.identity()?)?;
        set_meta(&transaction, "revision", revision)?;
        transaction.commit()?;
        Ok(Caught {
            revision,
            rebuilt: true,
            prices: false,
            sessions: records.into_iter().map(|record| record.key).collect(),
        })
    }

    /// Work out again what changed after the revision the cache reflects, in
    /// one transaction with the revision it reaches, so a failure leaves the
    /// cache where it was and the next catch-up works it out again.
    fn update(
        &mut self,
        ledger: &Ledger,
        book: &PriceBook,
        home: &Path,
        touched: crate::ledger::Touched,
    ) -> Result<Caught> {
        let parents = ledger.parents()?;
        let tree = Tree::of(&parents);
        let skipped = reviews(&parents);
        // A session counted now that the cache holds nothing of, as one no
        // longer a review holds nothing, has every response of its stored,
        // not only those touched: a link that changed touched none of them.
        let mut wanted = touched.responses.clone();
        let mut uncounted = Vec::new();
        {
            let mut held = self
                .connection
                .prepare_cached("SELECT 1 FROM session WHERE key = ?1")?;
            for key in touched
                .sessions
                .iter()
                .filter(|key| !skipped.contains(*key))
            {
                if !held.exists([key.to_string()])? {
                    uncounted.push(key.clone());
                }
            }
        }
        wanted.extend(ledger.responses_in(&uncounted)?);
        let responses = ledger.responses_of(&wanted)?;
        let mut ids = Ids::default();

        let transaction = self.connection.transaction()?;
        // The sessions the touched responses belonged to before, and belong to
        // now, both have totals to work out again.
        let mut affected: BTreeSet<SessionKey> = touched.sessions.into_iter().collect();
        // Those that lost usage, which its account may rest on.
        let mut lost: BTreeSet<SessionKey> = BTreeSet::new();
        {
            let mut before = transaction.prepare_cached(
                "SELECT s.key FROM usage u JOIN session s ON s.id = u.session
                 WHERE u.agent = ?1 AND u.response = ?2 AND u.kind = 'response'",
            )?;
            let mut forget = transaction
                .prepare_cached("DELETE FROM usage WHERE agent = ?1 AND response = ?2")?;
            for (agent, key) in &touched.responses {
                let previous: Option<String> = before
                    .query_row(params![agent.key(), key], |row| row.get(0))
                    .optional()?;
                if let Some(session) = previous.as_deref().and_then(SessionKey::parse) {
                    affected.insert(session.clone());
                    lost.insert(session);
                }
                forget.execute(params![agent.key(), key])?;
            }
        }
        for response in responses
            .iter()
            .filter(|response| !skipped.contains(&response.session))
        {
            store_response(&transaction, &mut ids, response, book, &tree, None)?;
            affected.insert(response.session.clone());
        }
        {
            // A session found to be a review since is counted nowhere, with
            // whatever of it was counted before.
            let mut id_of = transaction.prepare_cached("SELECT id FROM session WHERE key = ?1")?;
            let mut forget_rollup =
                transaction.prepare_cached("DELETE FROM rollup WHERE session = ?1")?;
            let mut forget_usage =
                transaction.prepare_cached("DELETE FROM usage WHERE session = ?1")?;
            let mut forget_lineage = transaction
                .prepare_cached("DELETE FROM lineage WHERE ?1 IN (session, ancestor)")?;
            let mut forget = transaction.prepare_cached("DELETE FROM session WHERE id = ?1")?;
            for key in affected.iter().filter(|key| skipped.contains(*key)) {
                let id: Option<i64> = id_of
                    .query_row([key.to_string()], |row| row.get(0))
                    .optional()?;
                if let Some(id) = id {
                    forget_rollup.execute([id])?;
                    forget_usage.execute([id])?;
                    forget_lineage.execute([id])?;
                    forget.execute([id])?;
                }
            }
        }

        // Sessions whose own facts changed are stored again, and every session
        // under them takes the root they are under now, which a link found
        // late can change.
        let records = ledger.sessions_of(
            affected.iter().filter(|key| !skipped.contains(*key)),
            &parents,
        )?;
        let kept = Kept::in_home(home);
        for record in &records {
            store_session(&transaction, record, &tree, ledger, &kept)?;
        }
        {
            let mut rooted = transaction
                .prepare_cached("UPDATE session SET root = ?2 WHERE key = ?1 AND root != ?2")?;
            for key in &affected {
                let root = tree.root(key).to_string();
                for below in tree.below(key).iter().skip(1) {
                    rooted.execute(params![below.to_string(), root])?;
                }
            }
        }

        // Every session whose totals may have changed has its usage outside
        // the conversation worked out again, from the top of its tree: what
        // was kept of it goes, and what the reports that stand now show is
        // kept, so none outlives the report it came from.
        let mut roots: BTreeSet<SessionKey> = affected
            .iter()
            .flat_map(|key| tree.ancestors(key))
            .collect();
        roots.extend(affected.iter().cloned());
        {
            let mut clear = transaction.prepare_cached(
                "DELETE FROM usage
                 WHERE kind = 'outside' AND session = (SELECT id FROM session WHERE key = ?1)",
            )?;
            for key in &roots {
                if clear.execute([key.to_string()])? > 0 {
                    affected.insert(key.clone());
                    lost.insert(key.clone());
                }
            }
        }
        for report in ledger
            .reports_of(&roots)?
            .iter()
            .filter(|report| !skipped.contains(&report.session))
        {
            store_outside(&transaction, &mut ids, report, &tree, book, None)?;
            affected.insert(report.session.clone());
        }
        // What a session runs within changes with its links and those above
        // it, and a session stored for the first time is now there to count
        // in: each changed session and every one under it is paired again.
        let paired: BTreeSet<SessionKey> = affected
            .iter()
            .filter(|key| !skipped.contains(*key))
            .flat_map(|key| tree.below(key))
            .collect();
        store_lineage(&transaction, paired.iter(), &tree)?;
        // What was stored again drew on the accounts signed in where and when
        // it was made, which the rollup sums it by, and when what was signed
        // in where changed, so may every response: those whose account
        // changed are rolled up again. A session that lost usage draws on
        // what its usage left draws on, none once all of it went elsewhere,
        // as a rebuild finds.
        let changed = attribute(
            &transaction,
            &Timeline::read(ledger)?,
            home,
            (!touched.sign_ins).then_some(&affected),
        )?;
        recount_accounts(&transaction, lost.difference(&changed))?;
        {
            let mut totals = transaction.prepare_cached(&totals("WHERE key = ?1"))?;
            for key in &affected {
                totals.execute([key.to_string()])?;
            }
        }
        roll_up(&transaction, &affected.union(&changed).cloned().collect())?;
        // A session's usage with its subagents', and when it was last active,
        // change with theirs, so every session above one that changed has
        // changed too.
        let above: Vec<SessionKey> = affected
            .iter()
            .flat_map(|key| tree.ancestors(key))
            .collect();
        affected.extend(above);
        {
            let mut active = transaction.prepare_cached(&active("WHERE key = ?1"))?;
            for key in &affected {
                active.execute([key.to_string()])?;
            }
        }
        set_meta(&transaction, "revision", touched.up_to)?;
        transaction.commit()?;
        affected.extend(changed);
        Ok(Caught {
            revision: touched.up_to,
            sessions: affected,
            ..Caught::default()
        })
    }
}

/// Open a connection to the cache.
fn connect(path: &Path) -> Result<Connection> {
    let connection = sharing::open(path)?;
    sharing::share(&connection)?;
    Ok(connection)
}

/// The device and inode of the file at `path`, or `None` when there is none.
fn file_at(path: &Path) -> Result<Option<(u64, u64)>> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(Some((metadata.dev(), metadata.ino()))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::io(path, error)),
    }
}

/// Keep `value` as the cache's `key` in its meta table, as [`meta`] reads it.
fn set_meta(transaction: &Transaction, key: &str, value: i64) -> Result<()> {
    transaction.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![key, value],
    )?;
    Ok(())
}

/// The cache's `key` in its meta table: its schema, the revision it has
/// reached, or the ledger it was built from.
fn meta(connection: &Connection, key: &str) -> rusqlite::Result<i64> {
    connection.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
        row.get(0)
    })
}

/// Refuse a cache a newer build made, of `schema`: what it holds is that
/// build's, which this build's questions would misread.
fn refuse_newer(schema: i64) -> Result<()> {
    if schema > SCHEMA {
        return Err(Error::NewerCache {
            found: schema,
            known: SCHEMA,
        });
    }
    Ok(())
}

/// Session ids already looked up within one catch-up.
#[derive(Default)]
struct Ids(HashMap<SessionKey, i64>);

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
fn store_lineage<'a>(
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
struct Drawn<'a> {
    /// What was signed in where, over time.
    timeline: Timeline,
    /// The home directory the agents' own folders are in.
    home: &'a Path,
    /// The folder each session is kept in, when not its agent's own.
    folders: HashMap<SessionKey, PathBuf>,
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
fn store_response(
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
struct Kept<'a> {
    readers: Vec<Box<dyn AgentReader>>,
    home: &'a Path,
}

impl<'a> Kept<'a> {
    /// Sessions' folders among the agents' folders under `home`.
    fn in_home(home: &'a Path) -> Kept<'a> {
        Kept {
            readers: agent::readers(),
            home,
        }
    }

    /// The folder `record` is kept in when it isn't its agent's own, as a
    /// second account's is: `None` for a session kept in its agent's own
    /// folder, or whose artifacts the ledger no longer holds.
    fn folder(&self, record: &SessionRecord) -> Option<String> {
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
fn store_session(
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
fn attribute(
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

/// The sessions `parents` says review a request for approval, as
/// Codex's reviews do, rather than doing any of the work: the ledger keeps
/// what they said, and nothing counts them. They use a model of their own,
/// with no price, and say only whether an action may go ahead.
fn reviews(parents: &HashMap<SessionKey, (SessionKey, LinkKind)>) -> HashSet<SessionKey> {
    parents
        .iter()
        .filter(|(_, (_, kind))| *kind == LinkKind::Review)
        .map(|(child, _)| child.clone())
        .collect()
}

/// Work out again the usage `report` shows outside the conversation, from
/// what the cache holds of the sessions it covers, and, given what it
/// `drew` on, with its accounts; without, they are put down after
/// ([`attribute`]).
fn store_outside(
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::{Cache, Caught, SCHEMA};
    use crate::agent::{Agent, Batch, Observation};
    use crate::catalog::Catalog;
    use crate::error::Error;
    use crate::ledger::tests::{read, scratch};
    use crate::ledger::{Ledger, Read};
    use crate::limits::{Held, Place, Seen};
    use crate::session::{LinkKind, SessionKey, SessionLink};
    use crate::time::Instant;
    use crate::usage::{Tokens, Usd};

    /// A catalog of claude-opus-5, released on 1 September, at `output`
    /// dollars a million, as read on `as_of`.
    fn catalog(output: f64, as_of: &str) -> Catalog {
        let listing = json!({"anthropic": {"models": {"claude-opus-5": {
            "name": "Claude Opus 5", "release_date": "2026-09-01", "last_updated": "2026-09-01",
            "cost": {"input": 5, "output": output, "cache_read": 0.5, "cache_write": 6.25}}}}});
        Catalog::parse(listing.to_string().as_bytes())
            .unwrap()
            .read_at(Instant::from_date(as_of).unwrap())
    }

    /// Bring `cache` up to `ledger`, at its prices, for a home of /home.
    fn catch_up(ledger: &mut Ledger, cache: &mut Cache) -> Caught {
        let book = ledger.price_book().unwrap();
        cache.catch_up(ledger, &book, Path::new("/home")).unwrap()
    }

    /// What found the response `key` of `session`, from `provider`'s
    /// `model`, of a million output tokens, at noon on 14 September, whose
    /// cost the agent recorded as `recorded`; `copy` when the session only
    /// copied it.
    fn observed(
        session: &SessionKey,
        key: &str,
        (provider, model): (&str, &str),
        recorded: Option<Usd>,
        copy: bool,
    ) -> Batch {
        let mut batch = Batch::default();
        batch.observe(Observation {
            response: key.to_owned(),
            session: session.clone(),
            copy,
            at: Instant::parse("2026-09-14T12:00:00Z").unwrap(),
            provider: provider.to_owned(),
            model: model.to_owned(),
            tokens: Tokens {
                output: 1_000_000,
                ..Tokens::default()
            },
            prompt: 0,
            web_searches: 0,
            cost: recorded,
            priority: false,
        });
        batch
    }

    const OPUS: (&str, &str) = ("anthropic", "claude-opus-5");

    /// A read of one Claude Code response, `key`, of Claude Opus 5, in the
    /// session `s`.
    fn response(key: &str) -> Read {
        let session = SessionKey::new(Agent::ClaudeCode, "s");
        let found = observed(&session, key, OPUS, None, false);
        read(
            Agent::ClaudeCode,
            "/home/.claude/projects/-work/s.jsonl",
            found,
        )
    }

    /// A read of one response of `agent`'s, `key`, in a session of the same
    /// name, from `provider`'s `model`, whose cost the agent recorded as
    /// `recorded`.
    fn costed(agent: Agent, key: &str, served: (&str, &str), recorded: Option<Usd>) -> Read {
        let found = observed(&SessionKey::new(agent, key), key, served, recorded, false);
        read(agent, &format!("/home/{key}.jsonl"), found)
    }

    /// A read of `path` that says only that `child` came from `parent` as
    /// `kind`.
    fn linked(path: &str, child: &SessionKey, parent: &SessionKey, kind: LinkKind) -> Read {
        let mut batch = Batch::default();
        batch.link(SessionLink {
            child: child.clone(),
            parent: parent.clone(),
            kind,
            launch: None,
        });
        read(Agent::ClaudeCode, path, batch)
    }

    /// The account the cache puts `session` down to.
    fn account_of(cache: &Cache, session: &SessionKey) -> Option<String> {
        cache
            .connection()
            .query_row(
                "SELECT account FROM session WHERE key = ?1",
                [session.to_string()],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn a_session_whose_response_went_to_another_draws_on_none_as_a_rebuild_finds() {
        let (dir, mut ledger) = scratch();
        let mut cache = Cache::open(&dir.path().join("cache.sqlite")).unwrap();
        let at = Instant::parse("2026-09-14T12:00:00Z").unwrap();
        // ~/.claude held a sign-in to claude:work throughout.
        let place = Place {
            agent: Agent::ClaudeCode,
            folder: PathBuf::from("/home/.claude"),
            provider: "anthropic".to_owned(),
        };
        let held = Held::Account("claude:work".to_owned());
        ledger.record_sign_ins(&[Seen { place, held }], at).unwrap();
        // A fork copied the response `shared` before the session that made
        // it was read; three responses of another session make that session,
        // read later, a change the cache catches up on rather than one it is
        // built again for.
        let fork = SessionKey::new(Agent::ClaudeCode, "fork");
        let own = SessionKey::new(Agent::ClaudeCode, "own");
        let path = |session: &str| format!("/home/.claude/projects/-work/{session}.jsonl");
        let copied = observed(&fork, "shared", OPUS, None, true);
        ledger
            .write(
                &[
                    response("a"),
                    response("b"),
                    response("c"),
                    read(Agent::ClaudeCode, &path("fork"), copied),
                ],
                at,
            )
            .unwrap();
        assert!(catch_up(&mut ledger, &mut cache).rebuilt);
        assert_eq!(account_of(&cache, &fork).as_deref(), Some("claude:work"));

        let made = observed(&own, "shared", OPUS, None, false);
        ledger
            .write(&[read(Agent::ClaudeCode, &path("own"), made)], at)
            .unwrap();
        assert!(!catch_up(&mut ledger, &mut cache).rebuilt);
        // The fork holds no response of its own now, so draws on no
        // account, as a cache built at once finds.
        assert_eq!(account_of(&cache, &fork), None);
        assert_eq!(account_of(&cache, &own).as_deref(), Some("claude:work"));
        let mut fresh = Cache::open(&dir.path().join("fresh.sqlite")).unwrap();
        catch_up(&mut ledger, &mut fresh);
        assert_eq!(account_of(&fresh, &fork), None);
    }

    #[test]
    fn a_session_no_longer_a_review_is_counted_again_as_a_rebuild_counts_it() {
        let (dir, mut ledger) = scratch();
        let mut cache = Cache::open(&dir.path().join("cache.sqlite")).unwrap();
        ledger
            .take_catalog(&catalog(25.0, "2026-09-02"), "bundled", None)
            .unwrap();
        let child = SessionKey::new(Agent::ClaudeCode, "c");
        let parent = SessionKey::new(Agent::ClaudeCode, "p");
        let usage_rows = |cache: &Cache| -> i64 {
            cache
                .connection()
                .query_row("SELECT count(*) FROM usage", [], |row| row.get(0))
                .unwrap()
        };
        // The child's one response, counted.
        ledger
            .write(
                &[costed(Agent::ClaudeCode, "c", OPUS, None)],
                Instant::now(),
            )
            .unwrap();
        catch_up(&mut ledger, &mut cache);
        assert_eq!(usage_rows(&cache), 1);
        // One artifact says it reviewed the parent: counted nowhere.
        ledger
            .write(
                &[linked("/home/r.jsonl", &child, &parent, LinkKind::Review)],
                Instant::now(),
            )
            .unwrap();
        catch_up(&mut ledger, &mut cache);
        assert_eq!(usage_rows(&cache), 0);
        // Another says it forked from it, which a link stands by before a
        // review: counted again, as a cache built at once counts it.
        ledger
            .write(
                &[linked("/home/f.jsonl", &child, &parent, LinkKind::Fork)],
                Instant::now(),
            )
            .unwrap();
        catch_up(&mut ledger, &mut cache);
        assert_eq!(usage_rows(&cache), 1);
        let mut fresh = Cache::open(&dir.path().join("fresh.sqlite")).unwrap();
        catch_up(&mut ledger, &mut fresh);
        assert_eq!(usage_rows(&fresh), 1);
    }

    #[test]
    fn a_parent_another_artifact_takes_a_child_from_is_caught_up_as_a_rebuild_finds() {
        let (dir, mut ledger) = scratch();
        let mut cache = Cache::open(&dir.path().join("cache.sqlite")).unwrap();
        let active = |cache: &Cache, session: &str| -> Option<i64> {
            cache
                .connection()
                .query_row(
                    "SELECT active FROM session WHERE key = ?1",
                    [format!("claude-code:{session}")],
                    |row| row.get(0),
                )
                .unwrap()
        };
        // Two sessions answered at noon, and a subagent at 15:00, which one
        // artifact says `old` ran.
        let noon = Instant::parse("2026-09-14T12:00:00Z").unwrap();
        let three = Instant::parse("2026-09-14T15:00:00Z").unwrap();
        let answered = |session: &SessionKey, at: Instant| -> Read {
            let mut batch = Batch::default();
            let found = observed(session, session.native(), OPUS, None, false);
            for observation in found.observations() {
                batch.observe(Observation {
                    at,
                    ..observation.clone()
                });
            }
            batch.session_mut(session).saw(at);
            read(
                Agent::ClaudeCode,
                &format!("/home/{}.jsonl", session.native()),
                batch,
            )
        };
        let child = SessionKey::new(Agent::ClaudeCode, "c");
        let old = SessionKey::new(Agent::ClaudeCode, "old");
        let new = SessionKey::new(Agent::ClaudeCode, "new");
        ledger
            .write(
                &[
                    answered(&old, noon),
                    answered(&new, noon),
                    answered(&child, three),
                    linked("/home/a.jsonl", &child, &old, LinkKind::Subagent),
                ],
                noon,
            )
            .unwrap();
        catch_up(&mut ledger, &mut cache);
        assert_eq!(active(&cache, "old"), Some(three.millis()));
        // Another artifact says `new` ran it, which a link stands by before
        // `old`, by the parent's key: `old` was last active at noon, as a
        // cache built at once finds.
        ledger
            .write(
                &[linked("/home/b.jsonl", &child, &new, LinkKind::Subagent)],
                noon,
            )
            .unwrap();
        assert!(!catch_up(&mut ledger, &mut cache).rebuilt);
        assert_eq!(active(&cache, "new"), Some(three.millis()));
        assert_eq!(active(&cache, "old"), Some(noon.millis()));
        let mut fresh = Cache::open(&dir.path().join("fresh.sqlite")).unwrap();
        catch_up(&mut ledger, &mut fresh);
        assert_eq!(active(&fresh, "old"), Some(noon.millis()));
    }

    #[test]
    fn a_cost_keeps_what_part_of_it_was_charged_or_estimated() {
        let (dir, mut ledger) = scratch();
        let mut cache = Cache::open(&dir.path().join("cache.sqlite")).unwrap();
        ledger
            .take_catalog(&catalog(25.0, "2026-09-02"), "bundled", None)
            .unwrap();
        let dollars = |cents: i64| Usd::from_nanos(cents * 10_000_000);
        ledger
            .write(
                &[
                    // A million output tokens of Claude Opus 5 at the
                    // catalog's $25 a million: $25.
                    costed(Agent::ClaudeCode, "c", OPUS, None),
                    // What xAI charged, as Grok Build recorded it: $7.50.
                    costed(Agent::Grok, "g", ("xai", "grok-4.6-build"), dollars(750)),
                    // OpenCode's own estimate for a model the catalog
                    // doesn't price: $0.06.
                    costed(Agent::OpenCode, "o", ("opencode", "unlisted"), dollars(6)),
                ],
                Instant::now(),
            )
            .unwrap();
        let book = ledger.price_book().unwrap();
        cache
            .catch_up(&mut ledger, &book, Path::new("/home"))
            .unwrap();
        // $25 + $7.50 + $0.06 = $32.56 in all, of which $7.50 was charged
        // and $0.06 estimated; the sessions and the rollup say the same.
        let parts = |sql: &str| -> (i64, i64, i64) {
            cache
                .connection()
                .query_row(sql, [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap()
        };
        let expected = (32_560_000_000, 7_500_000_000, 60_000_000);
        assert_eq!(
            parts("SELECT sum(cost), sum(charged), sum(estimated) FROM session"),
            expected
        );
        assert_eq!(
            parts("SELECT sum(c8), sum(c12), sum(c13) FROM rollup"),
            expected
        );
    }

    #[test]
    fn new_prices_reach_every_cost() {
        let (dir, mut ledger) = scratch();
        let mut cache = Cache::open(&dir.path().join("cache.sqlite")).unwrap();
        ledger.write(&[response("msg_1")], Instant::now()).unwrap();
        let mut catch_up = |ledger: &mut Ledger| {
            let book = ledger.price_book().unwrap();
            let caught = cache.catch_up(ledger, &book, Path::new("/home")).unwrap();
            let cost: i64 = cache
                .connection()
                .query_row("SELECT cost FROM usage", [], |row| row.get(0))
                .unwrap();
            (caught.prices, cost)
        };

        // A million output tokens at $25 a million: $25.
        ledger
            .take_catalog(&catalog(25.0, "2026-09-02"), "bundled", None)
            .unwrap();
        assert_eq!(catch_up(&mut ledger), (false, 25_000_000_000));
        // Corrected within two weeks of the release to $20, for all usage.
        ledger
            .take_catalog(&catalog(20.0, "2026-09-10"), "models.dev", None)
            .unwrap();
        assert_eq!(catch_up(&mut ledger), (true, 20_000_000_000));
    }

    /// Put at `path`, in place of what is there, a cache as a build of
    /// `schema` leaves one.
    fn cache_of_schema(path: &Path, schema: i64) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(crate::sharing::side_file(path, suffix));
        }
        rusqlite::Connection::open(path)
            .unwrap()
            .execute_batch(&format!(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL) STRICT, WITHOUT ROWID;
                 INSERT INTO meta (key, value) VALUES ('schema', {schema});"
            ))
            .unwrap();
    }

    /// The schema the cache at `path` records.
    fn schema_at(path: &Path) -> i64 {
        rusqlite::Connection::open(path)
            .unwrap()
            .query_row("SELECT value FROM meta WHERE key = 'schema'", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn a_cache_from_a_newer_build_is_refused_untouched_and_not_read_from() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite");
        cache_of_schema(&path, SCHEMA + 1);
        assert!(matches!(
            Cache::open(&path),
            Err(Error::NewerCache { found, .. }) if found == SCHEMA + 1
        ));
        assert!(matches!(
            Cache::reader(&path),
            Err(Error::NewerCache { found, .. }) if found == SCHEMA + 1
        ));
        assert_eq!(schema_at(&path), SCHEMA + 1);
        cache_of_schema(&path, SCHEMA);
        assert!(Cache::reader(&path).is_ok());
    }

    #[test]
    fn a_cache_another_build_put_in_its_place_is_not_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite");
        let mut ledger = Ledger::open(&dir.path().join("ledger.sqlite")).unwrap();
        let mut cache = Cache::open(&path).unwrap();
        let book = ledger.price_book().unwrap();
        let home = Path::new("/home");
        ledger.write(&[response("msg_1")], Instant::now()).unwrap();
        cache.catch_up(&mut ledger, &book, home).unwrap();

        // A newer build's cache put in its place, and then more history: the
        // newer cache is left alone, and so is the ledger's record of what
        // changed, for the newer build to catch up on.
        cache_of_schema(&path, SCHEMA + 1);
        ledger.write(&[response("msg_2")], Instant::now()).unwrap();
        assert!(matches!(
            cache.catch_up(&mut ledger, &book, home),
            Err(Error::NewerCache { .. })
        ));
        assert_eq!(schema_at(&path), SCHEMA + 1);
        assert_eq!(ledger.touched_since(0).unwrap().responses.len(), 1);

        // An older build's is built anew, at the path, from the ledger.
        cache_of_schema(&path, SCHEMA - 1);
        assert!(cache.catch_up(&mut ledger, &book, home).unwrap().rebuilt);
        let held: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT count(*) FROM usage", [], |row| row.get(0))
            .unwrap();
        assert_eq!(held, 2);
    }

    #[test]
    fn sums_stop_where_rust_sums_stop_and_are_exact_below() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let sum = |values: &[i64]| -> i64 {
            let rows = values
                .iter()
                .map(|value| format!("SELECT {value} AS v"))
                .collect::<Vec<_>>()
                .join(" UNION ALL ");
            connection
                .query_row(
                    &format!("SELECT {} FROM ({rows})", super::capped_sum("v")),
                    [],
                    |row| row.get(0),
                )
                .unwrap()
        };
        let rust = |values: &[i64]| {
            values
                .iter()
                .map(|value| Usd::from_nanos(*value).unwrap())
                .fold(Usd::default(), Usd::saturating_add)
                .nanos()
        };
        let billion_dollars = 1_000_000_000_000_000_000;
        // Nine billion dollars and half a billion more, 9.5 x 10^18 nano-
        // dollars: past i64::MAX, so both stop there, and SQLite's own sum
        // would have failed.
        let past: Vec<i64> = [billion_dollars; 9]
            .into_iter()
            .chain([billion_dollars / 2])
            .collect();
        assert_eq!(sum(&past), i64::MAX);
        assert_eq!(rust(&past), i64::MAX);
        // 2^53 - 1 and 1 more, where a float first rounds: exact, and equal.
        let edge = [9_007_199_254_740_991, 1];
        assert_eq!(sum(&edge), 9_007_199_254_740_992);
        assert_eq!(rust(&edge), sum(&edge));
        // Nothing at all sums to zero.
        assert_eq!(
            connection
                .query_row(
                    &format!(
                        "SELECT {} FROM (SELECT 1 AS v WHERE 0)",
                        super::capped_sum("v")
                    ),
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }
}
