//! Questions the app's feed and MCP ask of the cache.
//!
//! Answers come ready to use: usage over time arrives by bucket, those with
//! usage alone, and a session list pages by position, so rows already shown
//! stay put when new sessions arrive. Costs come split
//! into known and unknown ([`Totals::known_cost`]), and formatting is left to
//! whoever asks. A question runs in one read transaction, so its figures
//! always agree with each other.
//!
//! **Speed.** A question reads the rollup, about 7 ms over all history here,
//! and each response only in a quarter hour an end of its span cuts, so the
//! answer is exact: a span from a moment in the past up to now, as a period
//! is compared with, took 600 ms over the 56,235 responses of a week when
//! it read each (2026-09-27). A page of sessions, each with its
//! subagents' usage and how many it ran, takes about 2 ms from the cache's
//! pairs of sessions and index of them by parent, where following the links
//! up at each question took 8 (2026-09-27).
//!
//! **Models.** A list of sessions asked of models, as of a span, keeps to
//! the sessions with such usage, their subagents' counting, since Claude
//! Code's Explore subagents run Haiku where their session doesn't.
//!
//! **Accounts.** Usage is told apart and kept to accounts by the account
//! each response drew on, the one signed in where and when it was made, as
//! the cache keeps it and its rollup sums it: a session made under one
//! account and then another after a switch counts in each for what was made
//! under it. A list of sessions asked of accounts, as of models, keeps to
//! the sessions with such usage, their subagents' counting.
//!
//! **Local time.** Usage is kept by instant. Hours, days, weeks and months are
//! worked out in the zone the question gives, from quarter hours of usage, so
//! a day is whatever the clocks made it: 23 hours when they go forward, 25
//! when they go back. Every zone's offset from UTC is a whole number of
//! quarter hours, so no quarter hour straddles two local hours.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, Row, params_from_iter};

use crate::agent::Agent;
use crate::cache::{AGGREGATES, capped, capped_sum, quarters};
use crate::error::{Error, Result};
use crate::ledger::{optional_instant, unsigned};
use crate::model::ModelKey;
use crate::session::{LinkKind, SessionKey, shell_word};
use crate::time::{Bucket, Instant, QUARTER, Span, Zone, start_of};
use crate::usage::{Tokens, Usd, add_counts};

/// How usage can be told apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dimension {
    /// By agent.
    Agent,
    /// By model, as counted.
    Model,
    /// By project.
    Project,
    /// By session.
    Session,
    /// By the account it drew on, as [`crate::AccountLimits::id`] names it,
    /// usage no account is known to have drawn on being the group `""`, as
    /// usage of no model is.
    Account,
}

/// Which usage to count. An empty list admits everything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    /// Only these agents.
    pub agents: Vec<Agent>,
    /// Only these models.
    pub models: Vec<ModelKey>,
    /// Only these projects, by their root directory, and projects inside
    /// them, so a folder that holds several repositories stands for all of
    /// them. [`crate::Engine::project_root`] gives a directory's root.
    pub projects: Vec<String>,
    /// Only these sessions.
    pub sessions: Vec<SessionKey>,
    /// Only usage that drew on these accounts, by their ids
    /// ([`crate::AccountLimits::id`]): what was made where and when each
    /// was signed in. Usage no account is known to have drawn on is none of
    /// theirs.
    pub accounts: Vec<String>,
}

/// A question about usage. [`UsageQuery::default`] asks for all usage of
/// all time, not told apart.
#[derive(Clone, Debug, Default)]
pub struct UsageQuery {
    /// The time it asks about.
    pub span: Span,
    /// Which usage.
    pub filter: Filter,
    /// Told apart how, if at all.
    pub by: Option<Dimension>,
    /// Totalled over what local length of time, if any: only the buckets
    /// with usage are in the answer, so a span of any length can be split.
    pub every: Option<Bucket>,
}

/// Totals of usage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Tokens, usage outside the conversation included.
    pub tokens: Tokens,
    /// Responses in the transcripts.
    pub responses: u64,
    /// What the priced part cost: at the catalog's list prices, but for
    /// `charged` and `estimated`, the parts that rest on something else.
    pub cost: Usd,
    /// Parts of the usage with no price, which `cost` leaves out.
    pub unpriced: u64,
    /// Whether some of `cost` is approximate: a rate stood in for one the
    /// catalog lacks, or the timing was estimated, as it is for usage outside
    /// the conversation.
    pub approximate: bool,
    /// Tokens used outside the conversation, which `tokens` includes.
    pub outside: u64,
    /// How much of `cost` is what providers charged, as their agents
    /// recorded it: the bill, where an agent keeps it, as Grok Build does.
    pub charged: Usd,
    /// How much of `cost` is agents' own estimates, for models the catalog
    /// doesn't price.
    pub estimated: Usd,
}

impl Totals {
    /// What the usage cost, as far as that is known.
    ///
    /// `None` when some of the usage has no price (`unpriced` is above zero)
    /// and `cost` is zero: the zero then says nothing, since none of the
    /// usage may have been priced, and the totals can't tell that apart from
    /// usage priced at nothing. Otherwise `cost`, which leaves out whatever
    /// `unpriced` counts, so a total with both is a known part of the cost.
    ///
    /// `None` too when the cost reached the largest [`Usd`], as a sum past it
    /// stops there: it is then only known to be at least that.
    pub fn known_cost(&self) -> Option<Usd> {
        (self.cost != Usd::MOST && (self.unpriced == 0 || self.cost.nanos() > 0))
            .then_some(self.cost)
    }

    /// Count `other` in as well. Counts and cost add, stopping at the largest
    /// the cache can hold (`i64::MAX`), as its sums do, so a total comes out
    /// the same in any order; a cost that stops there is past knowing, and
    /// [`Totals::known_cost`] says so.
    pub(crate) fn add(&mut self, other: &Totals) {
        self.tokens.add(&other.tokens);
        self.responses = add_counts(self.responses, other.responses);
        self.cost = self.cost.saturating_add(other.cost);
        self.unpriced = add_counts(self.unpriced, other.unpriced);
        self.approximate |= other.approximate;
        self.outside = add_counts(self.outside, other.outside);
        self.charged = self.charged.saturating_add(other.charged);
        self.estimated = self.estimated.saturating_add(other.estimated);
    }
}

/// One row of an answer about usage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageRow {
    /// What the row is told apart by, as a key: an agent's key, a model, a
    /// project's root, a session's key, an account's id. `None` when the
    /// question told nothing apart.
    pub group: Option<String>,
    /// What to call the group: a project's name, a session's title.
    pub label: Option<String>,
    /// The start of the bucket, when the question asked for buckets; `None`
    /// too for usage at no time known, which is in no bucket, and in the
    /// total.
    pub start: Option<Instant>,
    /// The usage.
    pub totals: Totals,
}

/// An answer about usage.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageTable {
    /// Rows with usage, by group and then bucket.
    pub rows: Vec<UsageRow>,
    /// Everything the rows add up to.
    pub total: Totals,
}

/// Where a question's usage is read from: quarter-hour sums of usage, as
/// the rollup keeps them, as `r`, each placed in time by its quarter hour,
/// `r.quarter`.
struct Source {
    /// The table, or a query standing for one, aliased `r`.
    table: String,
    /// The values of the placeholders in `table`, in order.
    values: Vec<Sql>,
    /// Whether `table` holds only the usage within the span already, so it
    /// isn't asked for the span again.
    spanned: bool,
}

impl Source {
    /// The rollup's quarter-hour sums.
    fn rollup() -> Source {
        Source {
            table: "rollup r".to_owned(),
            values: Vec::new(),
            spanned: false,
        }
    }

    /// All of it: the rollup's quarter-hour sums, and the usage at no time
    /// known, which the rollup leaves out, summed as it sums, its quarter
    /// hour none.
    fn all_time() -> Source {
        Source {
            table: format!(
                "({} UNION ALL {}) r",
                from_rollup(),
                quarters("WHERE u.at IS NULL")
            ),
            ..Source::rollup()
        }
    }

    /// Quarter-hour sums of the usage in `span`: the rollup's for the quarter
    /// hours the span holds whole, and in a quarter hour an end of it cuts,
    /// which the rollup can't split, each response's within the span, summed
    /// as the rollup sums them. So an answer is exact whatever the span, and
    /// reads at most two quarter hours of responses: a span on quarter hours,
    /// as every local hour and day is, reads the rollup alone.
    ///
    /// All time holds the usage at no time known too, when there is any, as
    /// one look along the usage's index of times says. Reading it beside
    /// the rollup costs a question of all time up to 2 ms of its 5 to 10
    /// (2026-09-27), which only history holding some pays.
    fn for_span(connection: &Connection, span: &Span) -> Result<Source> {
        let cut = |at: Option<Instant>| {
            at.map(Instant::millis)
                .filter(|at| at.rem_euclid(QUARTER) != 0)
        };
        let (cut_from, cut_until) = (cut(span.from), cut(span.until));
        if span.from.is_none() && span.until.is_none() {
            let timeless: bool = connection
                .prepare_cached("SELECT EXISTS (SELECT 1 FROM usage WHERE at IS NULL)")?
                .query_row([], |row| row.get(0))?;
            return Ok(if timeless {
                Source::all_time()
            } else {
                Source::rollup()
            });
        }
        if cut_from.is_none() && cut_until.is_none() {
            return Ok(Source::rollup());
        }
        // The quarter hours within the span whole: from the first to start in
        // it, up to the start of the one its end cuts.
        let whole_from = span.from.map(|from| {
            let start = from.millis().div_euclid(QUARTER).saturating_mul(QUARTER);
            match cut_from {
                Some(_) => start.saturating_add(QUARTER),
                None => start,
            }
        });
        let whole_until = span
            .until
            .map(|until| until.millis().div_euclid(QUARTER).saturating_mul(QUARTER));
        let mut values = Vec::new();
        let mut parts = Vec::new();
        let mut cuts: Vec<(i64, i64)> = Vec::new();
        match (span.from, span.until, whole_from, whole_until) {
            // Both ends cut one quarter hour, or two with none whole between.
            (Some(from), Some(until), Some(first), Some(last)) if first >= last => {
                cuts.push((from.millis(), until.millis()));
            }
            _ => {
                let mut whole = from_rollup();
                whole.push_str(" WHERE 1");
                if let Some(first) = whole_from {
                    whole.push_str(" AND quarter >= ?");
                    values.push(Sql::Integer(first));
                }
                if let Some(last) = whole_until {
                    whole.push_str(" AND quarter < ?");
                    values.push(Sql::Integer(last));
                }
                parts.push(whole);
                if let (Some(from), Some(first)) = (cut_from, whole_from) {
                    cuts.push((from, first));
                }
                if let (Some(until), Some(last)) = (cut_until, whole_until) {
                    cuts.push((last, until));
                }
            }
        }
        let within: Vec<&str> = cuts.iter().map(|_| "(u.at >= ? AND u.at < ?)").collect();
        values.extend(
            cuts.iter()
                .flat_map(|(from, until)| [Sql::Integer(*from), Sql::Integer(*until)]),
        );
        parts.push(quarters(&format!("WHERE {}", within.join(" OR "))));
        Ok(Source {
            table: format!("({}) r", parts.join(" UNION ALL ")),
            values,
            spanned: true,
        })
    }

    /// A query selecting `selected` from the rows, and their sessions `s`, in
    /// `span` that `filter` admits, its values added to `values`.
    fn select(
        &self,
        selected: &str,
        span: &Span,
        filter: &Filter,
        values: &mut Vec<Sql>,
    ) -> String {
        let mut sql = format!(
            "SELECT {selected} FROM {} JOIN session s ON s.id = r.session WHERE 1",
            self.table
        );
        values.extend(self.values.iter().cloned());
        constrain(&mut sql, values, self, span, filter);
        sql
    }
}

/// Each model with usage, by its key, in order of the keys. Usage outside
/// the conversation that no model is named for is no model's.
///
/// # Errors
///
/// Returns [`Error::Ledger`] when the cache cannot be read.
pub(crate) fn models(connection: &Connection) -> Result<Vec<ModelKey>> {
    let mut statement = connection.prepare_cached(
        "SELECT DISTINCT model_key FROM rollup WHERE model_key != '' ORDER BY model_key",
    )?;
    let models = statement
        .query_map([], |row| row.get(0).map(ModelKey::stored))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(models)
}

/// A group's label and usage, as an answer is put together.
#[derive(Default)]
struct Labelled {
    label: Option<String>,
    totals: Totals,
}

/// Answer a question about usage.
///
/// # Errors
///
/// Returns [`Error::Ledger`] when the cache cannot be read, and
/// [`Error::Corrupt`] when a stored value is out of range.
pub(crate) fn usage(
    connection: &Connection,
    question: &UsageQuery,
    zone: &Zone,
) -> Result<UsageTable> {
    let source = Source::for_span(connection, &question.span)?;
    let quartered = question.every.is_some();
    let (group, label) = match question.by {
        None => ("NULL", "NULL"),
        Some(by) => grouping(by),
    };
    let mut values: Vec<Sql> = Vec::new();
    let mut sql = source.select(
        &format!(
            "{}, {group}, {label}, {}",
            if quartered { "r.quarter" } else { "0" },
            combined("r", false)
        ),
        &question.span,
        &question.filter,
        &mut values,
    );
    sql.push_str(" GROUP BY 1, 2");

    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query(params_from_iter(values))?;
    let mut table = UsageTable::default();
    // By group and bucket start: the group's label, and its usage.
    let mut grouped: BTreeMap<(Option<String>, Option<Instant>), Labelled> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let totals = totals(row, 3)?;
        table.total.add(&totals);
        let start = match question.every {
            // Usage at no time known is in the total and in no bucket.
            Some(every) => match row.get::<_, Option<i64>>(0)? {
                Some(millis) => {
                    let quarter = Instant::from_millis(millis)
                        .ok_or_else(|| Error::corrupt("usage time", "out of range"))?;
                    start_of(every, quarter, zone)
                }
                None => None,
            },
            None => None,
        };
        let entry = grouped.entry((row.get(1)?, start)).or_default();
        if entry.label.is_none() {
            entry.label = row.get(2)?;
        }
        entry.totals.add(&totals);
    }
    table.rows = grouped
        .into_iter()
        .map(|((group, start), labelled)| UsageRow {
            group,
            label: labelled.label,
            start,
            totals: labelled.totals,
        })
        .collect();
    Ok(table)
}

/// What a group of `by` is keyed by in a query over a [`Source`] and
/// sessions `s`, and the label it takes.
fn grouping(by: Dimension) -> (&'static str, &'static str) {
    match by {
        Dimension::Agent => ("r.agent", "NULL"),
        Dimension::Model => ("r.model_key", "NULL"),
        Dimension::Project => ("coalesce(s.project, s.cwd, '')", "max(s.project_name)"),
        Dimension::Session => ("s.key", "max(coalesce(s.title, s.native))"),
        Dimension::Account => ("r.account", "NULL"),
    }
}

/// A session's usage for all time as the cache keeps it beside the session,
/// named as [`AGGREGATES`] are: the same figures, added up the same way.
const TOTALS_KEPT: &str = "s.responses AS c1, s.input AS c2, s.cache_read AS c3,
    s.cache_write_5m AS c4, s.cache_write_1h AS c5, s.output AS c6, s.reasoning AS c7,
    s.cost AS c8, s.unpriced AS c9, s.approximate AS c10, s.outside AS c11, s.charged AS c12,
    s.estimated AS c13";

/// The index in [`AGGREGATES`] of whether any cost is approximate, which
/// combines by `max` where every other aggregate combines by `sum`.
const APPROXIMATE: usize = 9;

/// The aggregates of the rows of `table` combined: summed as [`capped_sum`]
/// sums, except whether any cost is approximate; named `c1` to `c13` when
/// `named`.
fn combined(table: &str, named: bool) -> String {
    (1..=AGGREGATES.len())
        .map(|column| {
            let value = format!("{table}.c{column}");
            let combined = if column == APPROXIMATE + 1 {
                format!("max({value})")
            } else {
                capped_sum(&value)
            };
            match named {
                true => format!("{combined} AS c{column}"),
                false => combined,
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every token in the aggregates of `table`, counting reasoning once, as part
/// of output.
fn tokens(table: &str) -> String {
    format!("{table}.c2 + {table}.c3 + {table}.c4 + {table}.c5 + {table}.c6")
}

/// The tokens of the aggregates of `table` as an order sorts them: stopping
/// at `i64::MAX`, as sums do, rather than past it, where SQLite's integers
/// turn to floating point and reading the order fails.
fn tokens_ordered(table: &str) -> String {
    capped(&tokens(table))
}

/// The rollup's quarter-hour sums, each with what it sums by and its named
/// aggregates, as a `SELECT` a condition can follow.
fn from_rollup() -> String {
    format!(
        "SELECT quarter, session, agent, provider, model_key, kind, account, {} FROM rollup",
        columns("rollup")
    )
}

/// The named aggregates of `table`, in order.
fn columns(table: &str) -> String {
    (1..=AGGREGATES.len())
        .map(|column| format!("{table}.c{column}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Read the aggregates [`AGGREGATES`] lists, starting at column `at`.
fn totals(row: &Row, at: usize) -> Result<Totals> {
    let count = |index: usize| -> Result<u64> { unsigned(row.get(at + index)?, "usage total") };
    let usd = |index: usize, what: &'static str| -> Result<Usd> {
        let nanos: i64 = row.get(at + index)?;
        Usd::from_nanos(nanos).ok_or_else(|| Error::corrupt(what, nanos.to_string()))
    };
    Ok(Totals {
        responses: count(0)?,
        tokens: Tokens {
            input: count(1)?,
            cache_read: count(2)?,
            cache_write_5m: count(3)?,
            cache_write_1h: count(4)?,
            output: count(5)?,
            reasoning: count(6)?,
        },
        cost: usd(7, "usage cost")?,
        unpriced: count(8)?,
        approximate: row.get(at + APPROXIMATE)?,
        outside: count(10)?,
        charged: usd(11, "charged cost")?,
        estimated: usd(12, "estimated cost")?,
    })
}

/// Add the span and filter to a query over `source` and sessions `s`.
fn constrain(
    sql: &mut String,
    values: &mut Vec<Sql>,
    source: &Source,
    span: &Span,
    filter: &Filter,
) {
    if let Some(from) = span.from.filter(|_| !source.spanned) {
        sql.push_str(" AND r.quarter >= ?");
        values.push(Sql::Integer(from.millis()));
    }
    if let Some(until) = span.until.filter(|_| !source.spanned) {
        sql.push_str(" AND r.quarter < ?");
        values.push(Sql::Integer(until.millis()));
    }
    one_of(sql, values, "r.agent", agents(&filter.agents));
    one_of(
        sql,
        values,
        "r.model_key",
        filter.models.iter().map(|model| model.as_str().to_owned()),
    );
    one_of(sql, values, "r.account", filter.accounts.iter().cloned());
    one_of(
        sql,
        values,
        "s.key",
        filter.sessions.iter().map(ToString::to_string),
    );
    in_projects(sql, values, &filter.projects);
}

/// Keep only rows whose `column` is one of `keys`; any row when there are
/// none.
fn one_of(
    sql: &mut String,
    values: &mut Vec<Sql>,
    column: &str,
    keys: impl ExactSizeIterator<Item = String>,
) {
    if keys.len() == 0 {
        return;
    }
    let marks = vec!["?"; keys.len()].join(", ");
    let _ = write!(sql, " AND {column} IN ({marks})");
    values.extend(keys.map(Sql::Text));
}

/// `agents`' keys, as the cache keeps them.
fn agents(agents: &[Agent]) -> impl ExactSizeIterator<Item = String> + '_ {
    agents.iter().map(|agent| agent.key().to_owned())
}

/// Keep only sessions in `projects`, by their root directory, and in projects
/// inside them; any session when there are none.
fn in_projects(sql: &mut String, values: &mut Vec<Sql>, projects: &[String]) {
    if projects.is_empty() {
        return;
    }
    // The root itself, or a path under it: those that sort from `root/` up
    // to `root0`, '0' being the character after '/'. The column is what a
    // split by project keys its groups by.
    let column = "coalesce(s.project, s.cwd, '')";
    let each = format!("({column} = ? OR ({column} >= ? AND {column} < ?))");
    let _ = write!(sql, " AND ({})", vec![each; projects.len()].join(" OR "));
    for root in projects {
        let root = root.trim_end_matches('/');
        values.push(Sql::Text(root.to_owned()));
        values.push(Sql::Text(format!("{root}/")));
        values.push(Sql::Text(format!("{root}0")));
    }
}

/// Which of `sessions` a list of sessions over `span` and `filter` holds,
/// subagents and sessions without usage among them: the list's own rules,
/// so a search admits what the list would.
///
/// # Errors
///
/// Returns [`Error::Ledger`] when the cache cannot be read.
pub(crate) fn admitted(
    connection: &Connection,
    sessions: &[SessionKey],
    span: &Span,
    filter: &Filter,
) -> Result<HashSet<SessionKey>> {
    // Sessions the filter names itself narrow these further.
    let named: HashSet<&SessionKey> = filter.sessions.iter().collect();
    let candidates: Vec<SessionKey> = sessions
        .iter()
        .filter(|session| named.is_empty() || named.contains(session))
        .cloned()
        .collect();
    Ok(held(connection, &candidates, span, filter, false)?
        .into_iter()
        .map(|row| row.key)
        .collect())
}

/// The sessions `keys` name, subagents and sessions without usage among
/// them, with their usage for all time and the models each used, by key:
/// none for none, and none for a key no session has.
///
/// # Errors
///
/// Returns [`Error::Ledger`] when the cache cannot be read, and
/// [`Error::Corrupt`] when a stored value is out of range.
pub(crate) fn rows(
    connection: &Connection,
    keys: &[SessionKey],
) -> Result<HashMap<SessionKey, SessionRow>> {
    Ok(
        held(connection, keys, &Span::default(), &Filter::default(), true)?
            .into_iter()
            .map(|row| (row.key.clone(), row))
            .collect(),
    )
}

/// Which of `sessions` a list over `span` and `filter` holds, subagents and
/// sessions without usage among them, with the models each used when
/// `models`: none for none, where a list naming no sessions would hold
/// every one.
fn held(
    connection: &Connection,
    sessions: &[SessionKey],
    span: &Span,
    filter: &Filter,
    models: bool,
) -> Result<Vec<SessionRow>> {
    let mut rows = Vec::new();
    // A page lists at most 1,000 sessions, and a list of as many named
    // sessions lists no more, so each is one page.
    for chunk in sessions.chunks(1_000) {
        let question = SessionQuery {
            span: *span,
            filter: Filter {
                sessions: chunk.to_vec(),
                ..filter.clone()
            },
            subagents: true,
            empty: true,
            limit: 1_000,
            ..SessionQuery::default()
        };
        let mut page = listed(connection, &question)?;
        if models {
            models_of(connection, &mut page.items)?;
        }
        rows.extend(page.items);
    }
    Ok(rows)
}

/// How a list of sessions is ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionOrder {
    /// Most recently active first.
    Recent,
    /// Most tokens first.
    Tokens,
}

impl SessionOrder {
    /// Its name, as a page's cursor records it.
    fn key(self) -> &'static str {
        match self {
            SessionOrder::Recent => "recent",
            SessionOrder::Tokens => "tokens",
        }
    }
}

/// How recently a session, or one run within it, must have been active for
/// it to be running now: long enough to span a model's slowest reply and a
/// short pause between turns. Five minutes, in milliseconds.
const RUNNING: i64 = 5 * 60 * 1000;

/// Since when a session must have been active, it or a session run within
/// it, to be running at `now`: five minutes before, that moment itself
/// included. `None` only for a `now` within five minutes of the earliest
/// instant there is.
pub fn running_since(now: Instant) -> Option<Instant> {
    Instant::from_millis(now.millis().saturating_sub(RUNNING))
}

/// A question about sessions. [`SessionQuery::default`] asks for every
/// session with something in it, most recently active first, a page of 100.
#[derive(Clone, Debug)]
pub struct SessionQuery {
    /// Sessions with usage in this span, whose totals then count only it.
    /// Unbounded, every session, with its totals for all time.
    pub span: Span,
    /// Which sessions.
    pub filter: Filter,
    /// Whether subagents are listed apart from the sessions that ran them,
    /// rather than only counted in their totals. Reviews, as Codex runs to
    /// approve actions, are neither listed nor counted.
    pub subagents: bool,
    /// Whether sessions in which nothing happened, with no response and no
    /// title, are listed too. Agents leave many, opened and closed.
    pub empty: bool,
    /// Only the subagents this session ran itself, which are then listed
    /// whatever `subagents` says.
    pub subagents_of: Option<SessionKey>,
    /// The order.
    pub order: SessionOrder,
    /// How many to list on a page, from 1 to 1,000: fewer is taken as 1,
    /// and more as 1,000.
    pub limit: usize,
    /// Where the previous page ended, as its [`Page::next`] said, for the
    /// page after it in the same order.
    pub after: Option<String>,
    /// Only sessions active since this, they or a session run within them,
    /// as [`SessionRow::active`] says: those running now, since
    /// [`running_since`] now.
    pub active_since: Option<Instant>,
}

impl Default for SessionQuery {
    fn default() -> SessionQuery {
        SessionQuery {
            span: Span::default(),
            filter: Filter::default(),
            subagents: false,
            empty: false,
            subagents_of: None,
            order: SessionOrder::Recent,
            limit: 100,
            after: None,
            active_since: None,
        }
    }
}

/// A session, as a list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    /// The session.
    pub key: SessionKey,
    /// Its title, when it has one.
    pub title: Option<String>,
    /// Its project's name.
    pub project: Option<String>,
    /// The directory it started in.
    pub cwd: Option<String>,
    /// Its git branch.
    pub branch: Option<String>,
    /// When it started.
    pub started: Option<Instant>,
    /// When it or any session run within it was last active, however deep:
    /// when any of its work was last done. In a list of a span, its latest
    /// activity within the span: to the millisecond when that was its
    /// latest of all, and otherwise, as when it went on past the span's end,
    /// the end of the last quarter hour it used anything in.
    pub active: Option<Instant>,
    /// The session it came from, and how.
    pub parent: Option<(SessionKey, LinkKind)>,
    /// Whether its transcript can still be read.
    pub present: bool,
    /// Its own usage.
    pub totals: Totals,
    /// Its usage with that of every session run within it: its subagents,
    /// theirs, and so on however deep.
    pub with_subagents: Totals,
    /// The models its own usage went through, most tokens first.
    pub models: Vec<ModelKey>,
    /// How many subagents it ran itself.
    pub subagents: u32,
    /// The folder its agent keeps it in when that isn't the agent's own, as
    /// a second account's is ([`crate::Folder`]); `None` for the agent's own.
    pub folder: Option<PathBuf>,
    /// The account its own usage drew on, as [`crate::AccountLimits::id`]:
    /// the one signed in where and when it was made, and of a session that
    /// drew on several, the one most of its responses did. `None` when none
    /// is known, as before any look for sign-ins, or for use of a sign-in to
    /// nothing Turnscope reads.
    pub account: Option<String>,
}

impl SessionRow {
    /// Whether it is running at `now`: active since [`running_since`] then,
    /// it or a session run within it.
    pub fn running(&self, now: Instant) -> bool {
        self.active
            .zip(running_since(now))
            .is_some_and(|(active, since)| active >= since)
    }

    /// The shell command that picks the session up again in its agent, from
    /// the folder it ran in, and with the agent pointed at the folder it
    /// keeps the session in when that isn't its own; `None` for a subagent,
    /// which no one returns to.
    pub fn resume(&self) -> Option<String> {
        if matches!(&self.parent, Some((_, LinkKind::Subagent))) {
            return None;
        }
        let mut command = self.key.agent().resume(self.key.native());
        if let (Some(folder), Some(variable)) = (&self.folder, self.key.agent().folder_variable()) {
            command = format!(
                "{variable}={} {command}",
                shell_word(&folder.to_string_lossy())
            );
        }
        Some(match &self.cwd {
            Some(cwd) => format!("cd {} && {command}", shell_word(cwd)),
            None => command,
        })
    }
}

/// A page of answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page<T> {
    /// The answers on this page.
    pub items: Vec<T>,
    /// What to pass as `after` for the next page, when there is one.
    pub next: Option<String>,
}

/// List sessions.
///
/// # Errors
///
/// Returns [`Error::Ledger`] when the cache cannot be read,
/// [`Error::Corrupt`] when a stored value is out of range, and
/// [`Error::Cursor`] when `after` is not where a page ended.
pub(crate) fn sessions(
    connection: &Connection,
    question: &SessionQuery,
) -> Result<Page<SessionRow>> {
    let mut page = listed(connection, question)?;
    models_of(connection, &mut page.items)?;
    Ok(page)
}

/// The page of sessions `question` asks for, without the models each used,
/// which only a list shows: a search asks only which sessions a list holds.
fn listed(connection: &Connection, question: &SessionQuery) -> Result<Page<SessionRow>> {
    // Only sessions with usage that answers the question, their own or their
    // subagents', are listed when it asks of a span, of models or of
    // accounts; otherwise every session is, those without usage included.
    // Claude Code's Explore subagents run Haiku, so a session can have usage
    // of Haiku only through them.
    let spanned = question.span.from.is_some() || question.span.until.is_some();
    let bounded =
        spanned || !question.filter.models.is_empty() || !question.filter.accounts.is_empty();
    let mut values: Vec<Sql> = Vec::new();

    // Each session's own usage in the span, and each session's with every
    // session run within it, however deep. Every session keeps its usage for
    // all time beside it, added up as the rollup would add it, so a question
    // of all time reads that rather than adding up the rollup: 2 ms rather
    // than 20 for a page (2026-09-24). Sessions asked for are kept to below, so that the usage
    // of their subagents still counts in their trees.
    let usage_filter = Filter {
        sessions: Vec::new(),
        ..question.filter.clone()
    };
    let mut sql = if bounded {
        let source = Source::for_span(connection, &question.span)?;
        let own = source.select(
            &format!(
                "r.session AS id, {}, max(r.quarter) AS q",
                combined("r", true)
            ),
            &question.span,
            &usage_filter,
            &mut values,
        );
        // Grouped by `+session`, which no index serves, so SQLite reads just
        // the span's quarter hours and sorts them, rather than every quarter
        // hour in the rollup by its index of sessions: 1 ms rather than 11
        // for a day here, and never slower (2026-09-27).
        format!("WITH own AS ({own} GROUP BY +r.session)")
    } else {
        let mut own =
            format!("WITH own AS (SELECT s.id AS id, {TOTALS_KEPT} FROM session s WHERE 1");
        one_of(
            &mut own,
            &mut values,
            "s.agent",
            agents(&usage_filter.agents),
        );
        in_projects(&mut own, &mut values, &usage_filter.projects);
        own.push(')');
        own
    };
    // When a session was last active: within the span, for a list of one,
    // exactly when its latest activity of all falls in the span, and
    // otherwise the end of its tree's last quarter hour of usage in the span.
    let activity = if spanned {
        let mut within = String::from("s.active IS NOT NULL");
        let mut last = format!("tree.q + {}", QUARTER - 1);
        if let Some(from) = question.span.from {
            let _ = write!(within, " AND s.active >= {}", from.millis());
        }
        if let Some(until) = question.span.until {
            let _ = write!(within, " AND s.active < {}", until.millis());
            last = format!("min({last}, {})", until.millis().saturating_sub(1));
        }
        format!("CASE WHEN {within} THEN s.active ELSE {last} END")
    } else {
        "s.active".to_owned()
    };
    let order = match question.order {
        SessionOrder::Recent => format!("coalesce({activity}, 0)"),
        SessionOrder::Tokens => format!(
            "coalesce({}, {}, 0)",
            tokens_ordered("tree"),
            tokens_ordered("own")
        ),
    };
    // A session's usage counts in its own tree and in that of every session
    // it runs within, as the cache's lineage pairs them.
    let _ = write!(
        sql,
        ",
         tree AS (SELECT l.ancestor AS id, {}{} FROM own JOIN lineage l ON l.session = own.id
                  GROUP BY l.ancestor)
         SELECT {order}, s.key, s.title, s.project_name, s.cwd, s.branch, s.started,
                s.parent, s.link, s.present, {}, {},
                (SELECT count(*) FROM session c WHERE c.parent = s.key AND c.link = 'subagent'),
                {activity}, s.folder, s.account
         FROM session s LEFT JOIN own ON own.id = s.id LEFT JOIN tree ON tree.id = s.id WHERE 1",
        combined("own", true),
        if bounded { ", max(own.q) AS q" } else { "" },
        columns("own"),
        columns("tree"),
    );
    if bounded {
        sql.push_str(" AND (own.id IS NOT NULL OR tree.id IS NOT NULL)");
    }
    if let Some(since) = question.active_since {
        sql.push_str(" AND s.active >= ?");
        values.push(Sql::Integer(since.millis()));
    }
    if let Some(parent) = &question.subagents_of {
        sql.push_str(" AND s.parent = ? AND s.link = 'subagent'");
        values.push(Sql::Text(parent.to_string()));
    } else if !question.subagents {
        sql.push_str(" AND (s.link IS NULL OR s.link != 'subagent')");
    }
    // A session is empty with no title and no response of its own or
    // beneath it: one whose subagents did all its work is no empty session.
    if !question.empty {
        sql.push_str(" AND (coalesce(tree.c1, 0) > 0 OR s.title IS NOT NULL)");
    }
    // The usage above is filtered already; these keep sessions without usage
    // to the agents and projects asked for too.
    one_of(
        &mut sql,
        &mut values,
        "s.agent",
        agents(&question.filter.agents),
    );
    in_projects(&mut sql, &mut values, &question.filter.projects);
    one_of(
        &mut sql,
        &mut values,
        "s.key",
        question.filter.sessions.iter().map(ToString::to_string),
    );
    // A cursor is `order:value|key`, and pages on only in its own order: a
    // value of one order read as another's would page from nowhere.
    if let Some(after) = question.after.as_deref() {
        let (value, key) = after
            .strip_prefix(question.order.key())
            .and_then(|rest| rest.strip_prefix(':'))
            .and_then(|rest| rest.split_once('|'))
            .and_then(|(value, key)| Some((value.parse::<i64>().ok()?, key)))
            .ok_or_else(|| Error::Cursor(after.to_owned()))?;
        let _ = write!(sql, " AND ({order}, s.key) < (?, ?)");
        values.push(Sql::Integer(value));
        values.push(Sql::Text(key.to_owned()));
    }
    let limit = question.limit.clamp(1, 1_000);
    let _ = write!(sql, " ORDER BY 1 DESC, s.key DESC LIMIT {}", limit + 1);

    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query(params_from_iter(values))?;
    let mut page = Page {
        items: Vec::new(),
        next: None,
    };
    let mut last = None;
    while let Some(row) = rows.next()? {
        if page.items.len() == limit {
            page.next = last;
            break;
        }
        let value: i64 = row.get(0)?;
        let key_text: String = row.get(1)?;
        let Some(key) = SessionKey::parse(&key_text) else {
            continue;
        };
        let parent = match (
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<String>>(8)?,
        ) {
            (Some(parent), Some(link)) => {
                let kind =
                    LinkKind::from_key(&link).ok_or_else(|| Error::corrupt("cached link", link))?;
                SessionKey::parse(&parent).map(|parent| (parent, kind))
            }
            _ => None,
        };
        // A session with no usage in the span, its own or beneath it, has
        // no aggregates.
        let totals_at = |at: usize| -> Result<Option<Totals>> {
            match row.get::<_, Option<i64>>(at)? {
                None => Ok(None),
                Some(_) => totals(row, at).map(Some),
            }
        };
        let own = totals_at(10)?.unwrap_or_default();
        let subagents: i64 = row.get(10 + 2 * AGGREGATES.len())?;
        let active = optional_instant(
            row.get(11 + 2 * AGGREGATES.len())?,
            "cached session activity",
        )?;
        page.items.push(SessionRow {
            key,
            title: row.get(2)?,
            project: row.get(3)?,
            cwd: row.get(4)?,
            branch: row.get(5)?,
            started: optional_instant(row.get(6)?, "cached session start")?,
            active,
            parent,
            present: row.get(9)?,
            with_subagents: totals_at(10 + AGGREGATES.len())?.unwrap_or(own),
            totals: own,
            models: Vec::new(),
            subagents: u32::try_from(subagents)
                .map_err(|_| Error::corrupt("subagent count", subagents.to_string()))?,
            folder: row
                .get::<_, Option<String>>(12 + 2 * AGGREGATES.len())?
                .map(PathBuf::from),
            account: row.get(13 + 2 * AGGREGATES.len())?,
        });
        last = Some(format!("{}:{value}|{key_text}", question.order.key()));
    }
    Ok(page)
}

/// Fill in each of `rows`' models, most tokens first, from the rollup; usage
/// outside the conversation that no model is named for is left out.
fn models_of(connection: &Connection, rows: &mut [SessionRow]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let place: HashMap<String, usize> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| (row.key.to_string(), index))
        .collect();
    // A page's keys, at most 1,000, are far fewer than the values SQLite
    // takes in one statement, 32,766.
    let marks = vec!["?"; place.len()].join(", ");
    let mut statement = connection.prepare(&format!(
        "SELECT s.key, r.model_key FROM rollup r JOIN session s ON s.id = r.session
         WHERE s.key IN ({marks}) AND r.model_key != ''
         GROUP BY 1, 2 ORDER BY 1, {} DESC, 2",
        capped_sum(&tokens("r"))
    ))?;
    let mut found = statement.query(params_from_iter(
        place.keys().map(|key| Sql::Text(key.clone())),
    ))?;
    while let Some(row) = found.next()? {
        let key: String = row.get(0)?;
        if let Some(index) = place.get(&key) {
            rows[*index].models.push(ModelKey::stored(row.get(1)?));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::{Totals, tokens_ordered};
    use crate::usage::Usd;

    /// Each of `rows`, its token aggregates c2 to c6, as `expression` of the
    /// table `t` orders it.
    fn ordered(expression: &str, rows: &[[i64; 5]]) -> Vec<i64> {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE t (c2 INTEGER, c3 INTEGER, c4 INTEGER, c5 INTEGER, c6 INTEGER)",
            )
            .unwrap();
        for tokens in rows {
            connection
                .execute(
                    "INSERT INTO t VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![tokens[0], tokens[1], tokens[2], tokens[3], tokens[4]],
                )
                .unwrap();
        }
        let mut statement = connection
            .prepare(&format!("SELECT {expression} FROM t ORDER BY rowid"))
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn tokens_order_stops_at_the_largest_integer_rather_than_failing() {
        // 1 + 2 + 3 + 4 + 5 = 15; five of 3e18 would be 1.5e19, past
        // i64::MAX (about 9.22e18), where SQLite's sum turns to floating point.
        let huge = 3_000_000_000_000_000_000;
        assert_eq!(
            ordered(&tokens_ordered("t"), &[[1, 2, 3, 4, 5], [huge; 5]]),
            [15, i64::MAX]
        );
    }

    #[test]
    fn a_cost_of_nothing_beside_unpriced_usage_is_unknown() {
        let priced = Totals {
            responses: 2,
            cost: Usd::from_nanos(1_500_000).unwrap(),
            ..Totals::default()
        };
        assert_eq!(priced.known_cost(), Usd::from_nanos(1_500_000));
        let partly = Totals {
            unpriced: 1,
            ..priced
        };
        assert_eq!(partly.known_cost(), Usd::from_nanos(1_500_000));
        let unpriced = Totals {
            responses: 1,
            unpriced: 1,
            ..Totals::default()
        };
        assert_eq!(unpriced.known_cost(), None);
        // No usage at all cost nothing.
        assert_eq!(Totals::default().known_cost(), Usd::from_nanos(0));
    }

    #[test]
    fn totals_add_up_alike_in_any_order_and_stop_where_the_cache_does() {
        let costing = |dollars: i64| Totals {
            responses: 1,
            cost: Usd::from_nanos(dollars * 1_000_000_000).unwrap(),
            ..Totals::default()
        };
        let add = |parts: &[i64]| {
            let mut sum = Totals::default();
            for part in parts {
                sum.add(&costing(*part));
            }
            sum
        };
        let billion = 1_000_000_000;
        // Nine billion dollars is 9 x 10^18 nano-dollars, below i64::MAX,
        // 9,223,372,036,854,775,807: known, and the same either way round.
        let nine = [billion; 9];
        assert_eq!(
            add(&nine).known_cost(),
            Usd::from_nanos(9 * billion * billion)
        );
        // Half a billion more, 9.5 x 10^18, passes it: first or last, the sum
        // stops there and its cost is past knowing.
        let mut last = nine.to_vec();
        last.push(billion / 2);
        let mut first = vec![billion / 2];
        first.extend(nine);
        assert_eq!(add(&last), add(&first));
        assert_eq!(add(&last).cost, Usd::MOST);
        assert_eq!(add(&last).known_cost(), None);
        assert_eq!(add(&last).responses, 10);

        // Half of i64::MAX, rounded up, is 4,611,686,018,427,387,904, and
        // twice that passes it by one: counts stop there too.
        let half = Totals {
            responses: 4_611_686_018_427_387_904,
            unpriced: 4_611_686_018_427_387_904,
            outside: 4_611_686_018_427_387_904,
            ..Totals::default()
        };
        let mut sum = half;
        sum.add(&half);
        let most = 9_223_372_036_854_775_807;
        assert_eq!(
            (sum.responses, sum.unpriced, sum.outside),
            (most, most, most)
        );
    }
}
