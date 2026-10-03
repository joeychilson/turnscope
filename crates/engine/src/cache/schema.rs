//! The cache's tables, and the SQL that adds usage up: a session's totals,
//! the rollup by quarter hour, each session's account and when it was last
//! active. [`SCHEMA`] sits beside the tables it versions, so a change to
//! either is seen with the other.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use rusqlite::Transaction;

use crate::error::Result;
use crate::session::SessionKey;

/// The schema's version. Increase it with any change to the schema or to how
/// anything in the cache is worked out, as a new agent's usage in it is; the
/// cache is then built again. So builds that read different agents never
/// share a cache: one that passes over an agent's changes never catches up a
/// cache holding that agent's usage.
pub(super) const SCHEMA: i64 = 14;

/// A quarter hour, in milliseconds: the grain of the rollup, and of usage
/// outside the conversation.
pub(crate) const QUARTER: i64 = 15 * 60 * 1000;

/// The start of the quarter hour usage `u` falls in, as the rollup keys it:
/// rounded down, as Rust's `div_euclid` rounds, where SQLite's division
/// rounds toward zero and would put usage before 1970 in the quarter hour
/// after its own.
pub(super) const QUARTER_OF: &str = "(u.at / 900000 - (u.at % 900000 < 0)) * 900000";

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

pub(super) const TABLES: &str = r"
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
pub(super) fn rollup(which: &str) -> String {
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
pub(super) const CLEAR_ROLLUP: &str =
    "DELETE FROM rollup WHERE session = (SELECT id FROM session WHERE key = ?1)";

/// Work out again the rollup of each of the sessions `keys`.
pub(super) fn roll_up(transaction: &Transaction, keys: &BTreeSet<SessionKey>) -> Result<()> {
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
pub(super) fn recount_accounts<'a>(
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
pub(super) fn totals(which: &str) -> String {
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
pub(super) fn session_account(which: &str) -> String {
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
pub(super) fn active(which: &str) -> String {
    format!(
        "UPDATE session SET active = (
             SELECT max(at) FROM (
                 SELECT session.last AS at
                 UNION ALL
                 SELECT d.last FROM lineage l JOIN session d ON d.id = l.session
                 WHERE l.ancestor = session.id)) {which}"
    )
}
