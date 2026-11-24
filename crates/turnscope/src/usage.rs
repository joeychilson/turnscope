//! What was used and what it cost, and what used a limit.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use jiff::tz::TimeZone;
use rusqlite::{Connection, OptionalExtension, params_from_iter, types::Value};
use serde::{Deserialize, Serialize};

use crate::agents::{self, Tokens};
use crate::time::{self, Period};
use crate::{Error, Result};

#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum By {
    Day,
    Week,
    Month,
    Model,
    Project,
    Agent,
    Account,
    Session,
}

impl By {
    fn period(self) -> Option<Period> {
        match self {
            By::Day => Some(Period::Day),
            By::Week => Some(Period::Week),
            By::Month => Some(Period::Month),
            By::Model | By::Project | By::Agent | By::Account | By::Session => None,
        }
    }

    fn title(self) -> &'static str {
        match self {
            By::Day => "Day",
            By::Week => "Week of",
            By::Month => "Month of",
            By::Model => "Model",
            By::Project => "Project",
            By::Agent => "Agent",
            By::Account => "Account",
            By::Session => "Session",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            By::Day => "day",
            By::Week => "week",
            By::Month => "month",
            By::Model => "model",
            By::Project => "project",
            By::Agent => "agent",
            By::Account => "account",
            By::Session => "session",
        }
    }
}

/// Where usage starts when neither a time nor a limit's window says.
pub const SINCE: &str = "30d";

#[derive(Clone)]
pub struct Query {
    pub since: i64,
    pub until: i64,
    pub by: By,
    pub agent: Option<String>,
    pub account: Option<String>,
    /// Lowercase substrings matched against `response.model`; empty means all models.
    pub models: Vec<String>,
    /// API key fingerprint; filters to responses made under that key.
    pub key: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub since: i64,
    pub until: i64,
    pub by: By,
    pub total: Row,
    pub rows: Vec<Row>,
    pub rest: Option<Row>,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub key: String,
    pub name: Option<String>,
    pub responses: u64,
    pub tokens: Tokens,
    pub cost_usd: f64,
    pub unpriced: u64,
}

impl Row {
    pub fn label(&self, by: By) -> &str {
        match (&self.name, by) {
            (Some(name), _) => name,
            (None, By::Account) if self.key.is_empty() => "no account",
            (None, By::Project) if self.key.is_empty() => "no project",
            (None, By::Model) if self.key.is_empty() => "no model named",
            (None, _) => &self.key,
        }
    }

    fn add(&mut self, other: &Row) {
        self.responses += other.responses;
        self.tokens = self.tokens.zip(other.tokens, |a, b| a + b);
        self.cost_usd += other.cost_usd;
        self.unpriced += other.unpriced;
    }
}

/// A session whose parent was never read is the top of its own tree.
const TREES: &str = "
WITH RECURSIVE tree (root, id) AS (
    SELECT id, id FROM session s
    WHERE parent IS NULL OR NOT EXISTS (SELECT 1 FROM session p WHERE p.id = s.parent)
    UNION ALL
    SELECT tree.root, s.id FROM session s JOIN tree ON s.parent = tree.id
)";

impl Query {
    fn where_clause(&self) -> (String, Vec<Value>) {
        let mut sql = "r.at >= ? AND r.at < ?".to_owned();
        let mut values: Vec<Value> = vec![self.since.into(), self.until.into()];
        for (column, value) in [("agent", &self.agent), ("account", &self.account)] {
            if let Some(value) = value {
                sql += &format!(" AND r.{column} = ?");
                values.push(value.clone().into());
            }
        }
        let (limited, given) = limited(&self.models, self.key.as_deref());
        sql += &format!(" AND {limited}");
        values.extend(given);
        (sql, values)
    }
}

pub fn usage(db: &Connection, query: &Query) -> Result<Usage> {
    // Time is grouped by quarter hours here and into local days in Rust: every
    // time zone's offset is a whole number of them.
    let (key, name, join) = match query.by {
        By::Day | By::Week | By::Month => ("r.at / 900000 * 900000", "NULL", ""),
        By::Model => ("r.model", "NULL", ""),
        By::Agent => ("r.agent", "NULL", ""),
        By::Account => (
            "coalesce(r.account, '')",
            crate::status::Account::FULL_TITLE,
            "LEFT JOIN account a ON a.id = r.account",
        ),
        By::Project => (
            "coalesce(s.project, '')",
            "NULL",
            "LEFT JOIN session s ON s.id = r.session",
        ),
        // A subagent's usage counts toward the session at the top of its
        // tree. A fork's or a continuation's is its own.
        By::Session => (
            "coalesce(t.root, r.session)",
            "s.title",
            "LEFT JOIN tree t ON t.id = r.session LEFT JOIN session s ON s.id = coalesce(t.root, r.session)",
        ),
    };
    let trees = match query.by {
        By::Session => TREES,
        _ => "",
    };
    let (where_sql, values) = query.where_clause();
    let sql = format!(
        "{trees} SELECT {key}, {name}, count(*), sum(r.input), sum(r.cache_read), sum(r.cache_write_5m),
             sum(r.cache_write_1h), sum(r.output), sum(r.reasoning), total(r.cost), count(*) - count(r.cost)
         FROM response r {join} WHERE {where_sql} GROUP BY 1",
    );
    let period = query.by.period().map(|period| (period, TimeZone::system()));
    let mut groups: BTreeMap<String, Row> = BTreeMap::new();
    let mut statement = db.prepare(&sql)?;
    let mut rows = statement.query(params_from_iter(values))?;
    while let Some(row) = rows.next()? {
        let count = |at: usize| row.get::<_, i64>(at).map(|count| count as u64);
        let found = Row {
            key: String::new(),
            name: None,
            responses: count(2)?,
            tokens: crate::db::tokens(row, 3)?,
            cost_usd: row.get(9)?,
            unpriced: count(10)?,
        };
        let key = if let Some((period, zone)) = &period {
            time::period_date(*period, row.get(0)?, zone).to_string()
        } else if query.by == By::Model {
            // One model through several providers: OpenRouter's
            // `google/gemini-3.8-flash` is Google's `gemini-3.8-flash`.
            let model = row.get_ref(0)?.as_str()?;
            model.rsplit('/').next().unwrap_or(model).to_owned()
        } else {
            row.get(0)?
        };
        let group = groups.entry(key).or_insert_with_key(|key| Row {
            key: key.clone(),
            name: (query.by == By::Agent).then(|| agents::name(key).to_owned()),
            ..Row::default()
        });
        if group.name.is_none() {
            group.name = row.get(1)?;
        }
        group.add(&found);
    }
    let mut total = Row {
        key: "total".to_owned(),
        ..Row::default()
    };
    let mut rows: Vec<Row> = groups.into_values().collect();
    for row in &rows {
        total.add(row);
    }
    if query.by.period().is_none() {
        rows.sort_by(|a, b| {
            b.cost_usd
                .total_cmp(&a.cost_usd)
                .then(b.responses.cmp(&a.responses))
        });
    }
    Ok(Usage {
        since: query.since,
        until: query.until,
        by: query.by,
        total,
        rows,
        rest: None,
    })
}

pub fn fold(usage: &mut Usage, shares: Option<&mut Shares>, top: usize) {
    if usage.rows.len() <= top {
        return;
    }
    let time = usage.by.period().is_some();
    let range = if time {
        0..usage.rows.len() - top
    } else {
        top..usage.rows.len()
    };
    let mut rest = Row {
        key: format!("{} {}", range.len(), if time { "earlier" } else { "more" }),
        ..Row::default()
    };
    for row in usage.rows.drain(range.clone()) {
        rest.add(&row);
    }
    usage.rest = Some(rest);
    if let Some(shares) = shares {
        shares.rest = Some(shares.rows.drain(range).sum());
    }
}

pub fn count(number: u64) -> String {
    let digits = number.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

pub fn counted(n: u64, singular: &str) -> String {
    match n {
        1 => format!("1 {singular}"),
        n => format!("{} {singular}s", count(n)),
    }
}

pub fn cost(usd: f64, unpriced_count: u64) -> String {
    dollars(usd) + &unpriced(unpriced_count)
}

fn unpriced(n: u64) -> String {
    match n {
        0 => String::new(),
        _ => format!(" (+{} unpriced)", count(n)),
    }
}

pub fn dollars(amount: f64) -> String {
    let cents = (amount.max(0.0) * 100.0).round() as u64;
    format!("${}.{:02}", count(cents / 100), cents % 100)
}

pub fn share(points: f64) -> String {
    if points < 0.5 {
        "<1%".to_owned()
    } else {
        format!("{points:.0}%")
    }
}

pub struct Shares {
    pub rows: Vec<f64>,
    pub rest: Option<f64>,
}

pub fn text(usage: &Usage, shares: Option<(&crate::status::Limit, &Shares)>) -> String {
    let date = |at: i64| time::local(at).strftime("%a %b %-d %H:%M").to_string();
    let mut out = format!(
        "{} to {}: {}, {}\n",
        date(usage.since),
        date(usage.until),
        counted(usage.total.responses, "response"),
        cost(usage.total.cost_usd, usage.total.unpriced),
    );
    let time = usage.by.period().is_some();
    let label = |row: &Row| match (&usage.by, &row.name) {
        (By::Session, Some(name)) => format!("{} {name}", start(&row.key)),
        (By::Session, None) => start(&row.key),
        _ => row.label(usage.by).to_owned(),
    };
    let mut rows: Vec<(&Row, Option<f64>, String)> = usage
        .rows
        .iter()
        .enumerate()
        .map(|(at, row)| {
            let points = shares.map(|(_, shares)| shares.rows[at]);
            (row, points, short(&label(row), 55))
        })
        .collect();
    // Its key counts the rows it adds up, `3551 more`, and isn't a session.
    if let Some(rest) = &usage.rest {
        let points = shares.and_then(|(_, shares)| shares.rest);
        rows.insert(
            if time { 0 } else { rows.len() },
            (rest, points, rest.label(usage.by).to_owned()),
        );
    }
    if rows.is_empty() {
        return out + "Nothing was used.";
    }
    let what = usage.by.title();
    let wide = rows
        .iter()
        .map(|(_, _, label)| label.chars().count())
        .chain([what.len()])
        .max()
        .unwrap_or_default();
    let costs: Vec<String> = rows.iter().map(|(row, ..)| dollars(row.cost_usd)).collect();
    let cost_wide = costs.iter().map(String::len).max().unwrap_or(4).max(4);
    let share_head = shares.map(|(limit, _)| format!("% of {}", limit.named()));
    let _ = write!(
        out,
        "\n  {what:<wide$}  {:>9}  {:>cost_wide$}",
        "Responses", "Cost"
    );
    if let Some(head) = &share_head {
        let _ = write!(out, "  {head}");
    }
    out.push('\n');
    for ((row, points, label), cost) in rows.iter().zip(&costs) {
        let _ = write!(
            out,
            "  {label:<wide$}  {:>9}  {cost:>cost_wide$}",
            count(row.responses)
        );
        if let (Some(head), Some(points)) = (&share_head, points) {
            let _ = write!(out, "  {:>width$}", share(*points), width = head.len());
        }
        let _ = writeln!(out, "{}", unpriced(row.unpriced));
    }
    out + "\nCosts are at list prices: what the same tokens cost on each provider's API."
}

/// Usage as a Markdown table. The asking session, `yours`, is marked, where
/// its agent names it.
pub fn markdown(
    usage: &Usage,
    shares: Option<(&crate::status::Limit, &Shares)>,
    yours: Option<&str>,
) -> String {
    let what = usage.by.title();
    let mut out = match shares {
        Some((limit, _)) => format!(
            "| {what} | Cost | Responses | % of {} |\n|---|--:|--:|--:|\n",
            limit.named()
        ),
        None => format!("| {what} | Cost | Responses |\n|---|--:|--:|\n"),
    };
    let rest_line = usage.rest.as_ref().map(|rest| {
        // Its key, made in `fold`, counts the rows it adds up: `3 more` or `3 earlier`.
        let noun = usage.by.noun();
        let rows = match rest.key.as_str() {
            "1 more" | "1 earlier" => format!("{} {noun}", rest.key),
            other => format!("{other} {noun}s"),
        };
        let mut line = format!(
            "| {rows} | {} | {} |",
            cost(rest.cost_usd, rest.unpriced),
            count(rest.responses)
        );
        if let Some(points) = shares.and_then(|(_, shares)| shares.rest) {
            let _ = write!(line, " {} |", share(points));
        }
        line.push('\n');
        line
    });
    let time = usage.by.period().is_some();
    if time && let Some(line) = &rest_line {
        out += line;
    }
    for (at, row) in usage.rows.iter().enumerate() {
        let name = match usage.by {
            // Its name is its title, which a session may not have yet.
            By::Session => format!(
                "`{}` {}{}",
                start(&row.key),
                title(row.name.as_deref()),
                if Some(row.key.as_str()) == yours {
                    " (yours)"
                } else {
                    ""
                }
            ),
            _ => row.label(usage.by).to_owned(),
        };
        // A `|` in a title or path would end its cell: one titled
        // `ok | $0.01 | 1 | 1% |` wrote cells of its own.
        let name = name.replace('|', "\\|");
        let _ = write!(
            out,
            "| {name} | {} | {} |",
            cost(row.cost_usd, row.unpriced),
            count(row.responses)
        );
        if let Some((_, shares)) = shares {
            let _ = write!(out, " {} |", share(shares.rows[at]));
        }
        out.push('\n');
    }
    if !time && let Some(line) = &rest_line {
        out += line;
    }
    out
}

/// What used a limit: `account`'s usage in `limit`'s window, or `between` a
/// start and an end, with the points of the limit each row took in the
/// window (`shares`). With no limit, the account's usage.
pub fn breakdown(
    db: &Connection,
    account: &crate::status::Account,
    limit: Option<&crate::status::Limit>,
    between: Option<(i64, i64)>,
    by: By,
    now: i64,
) -> Result<(Usage, Option<Shares>)> {
    let usage = usage(db, &query(account, limit, between, by, now))?;
    let shares = limit
        .filter(|_| between.is_none())
        .and_then(|limit| shares(&usage, limit));
    Ok((usage, shares))
}

/// `account`'s usage of `limit`, in its window or `between` a start and an
/// end. A window that reset since it was read counts from the reset, or one
/// length back where that's later. A window that rolls counts the last of
/// its length (`Provider::rolls`). With no window start, a key's spend
/// counts by the month, anything else over the last week.
pub fn query(
    account: &crate::status::Account,
    limit: Option<&crate::status::Limit>,
    between: Option<(i64, i64)>,
    by: By,
    now: i64,
) -> Query {
    let rolls = limit.and_then(|limit| {
        crate::providers::by_id(&account.provider)?
            .rolls(&limit.key)
            .map(|length| now - length)
    });
    let start = rolls
        .or_else(|| limit.and_then(crate::status::Limit::started))
        .or_else(|| {
            let limit = limit?;
            let reset = limit.reset_since?;
            Some(reset.max(limit.length().map_or(reset, |length| now - length)))
        });
    Query {
        since: between
            .map(|(since, _)| since)
            .or(start)
            .unwrap_or_else(|| match account.kind {
                crate::providers::Kind::ApiKey => time::start_of(time::Period::Month, now),
                crate::providers::Kind::Subscription => now - 7 * time::DAY,
            }),
        until: between.map_or(now, |(_, until)| until),
        by,
        agent: None,
        account: Some(account.id.clone()),
        models: models(limit.and_then(|limit| limit.scope.as_deref())),
        key: limit
            .and_then(|limit| crate::providers::key_limit_fingerprint(&limit.key))
            .map(str::to_owned),
    }
}

/// The points of `limit` each row of `usage` of its window took: its share
/// of the cost, times the points used. Approximate, as providers say how
/// much of a limit is used but not by what. None for a window that reset
/// since it was read, as how much of the new one is used isn't known, and
/// for usage with no known price (`unshared`): a share is of cost.
pub fn shares(usage: &Usage, limit: &crate::status::Limit) -> Option<Shares> {
    limit.started()?;
    if usage.total.unpriced > 0 || usage.total.cost_usd <= 0.0 {
        return None;
    }
    let used = f64::from(100 - limit.left_percent);
    let points = |row: &Row| row.cost_usd / usage.total.cost_usd * used;
    Some(Shares {
        rows: usage.rows.iter().map(points).collect(),
        rest: usage.rest.as_ref().map(points),
    })
}

/// Why `usage` of `limit`'s window gives no shares, when it's that some of
/// it has no known price.
pub fn unshared(usage: &Usage, limit: &crate::status::Limit) -> Option<String> {
    if usage.total.unpriced == 0 {
        return None;
    }
    let responses = counted(usage.total.unpriced, "response");
    limit.started().map(|_| {
        format!(
            "No shares of {} are given: {responses} with no known price, and a share is of cost.",
            limit.named()
        )
    })
}

pub fn models(scope: Option<&str>) -> Vec<String> {
    scope
        .into_iter()
        .flat_map(|scope| scope.split(" and "))
        .map(|model| model.trim().to_lowercase().replace(' ', "-"))
        .collect()
}

/// Whether a response `r` counts toward a limit on `models` or one `key`'s,
/// as SQL, and its values: true of every response for a limit on neither.
pub fn limited(models: &[String], key: Option<&str>) -> (String, Vec<Value>) {
    let mut sql = "1".to_owned();
    let mut values: Vec<Value> = Vec::new();
    if !models.is_empty() {
        let any = vec!["instr(lower(r.model), ?) > 0"; models.len()];
        sql += &format!(" AND ({})", any.join(" OR "));
        values.extend(models.iter().map(|model| model.clone().into()));
    }
    if let Some(key) = key {
        sql += &format!(" AND (SELECT key {}) = ?", crate::limits::SIGN_IN_AT);
        values.push(key.to_owned().into());
    }
    (sql, values)
}

pub fn brief(db: &Connection, id: &str) -> Result<(Option<String>, String)> {
    db.prepare_cached("SELECT project, agent FROM session WHERE id = ?1")?
        .query_row([id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("no session is {id}")))
}

pub fn session_account(db: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
    db.prepare_cached(
        "SELECT account FROM response WHERE session = ?1 AND account IS NOT NULL ORDER BY at DESC LIMIT 1",
    )?
    .query_row([id], |row| row.get(0))
    .optional()
}

/// First 8 chars of the agent's native id (over the full ~45-char id): only
/// disambiguates duplicate titles.
fn start(id: &str) -> String {
    let native = id.split_once(':').map_or(id, |(_, native)| native);
    native.chars().take(8).collect()
}

fn short(text: &str, most: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= most {
        return line;
    }
    let kept: String = line.chars().take(most).collect();
    format!("{}…", kept.trim_end())
}

/// Quotes each user-authored title as a JSON string where it appears (over a
/// single header warning) so an agent reading the table can't mistake it for
/// Turnscope's own output or break out with embedded quotes.
fn title(title: Option<&str>) -> String {
    title.map_or("(untitled)".to_owned(), |title| {
        serde_json::Value::String(short(title, 80)).to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Account;

    fn example() -> (Vec<Account>, i64) {
        let example: crate::status::Status =
            serde_json::from_str(include_str!("../tests/fixtures/status.json")).unwrap();
        (example.accounts, example.at)
    }

    fn store(
        account: &Account,
        agent: &str,
        responses: &[(&str, &str, f64, i64)],
    ) -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).unwrap();
        let from = (
            agent,
            "f",
            account.provider.as_str(),
            Some(account.id.as_str()),
        );
        for (id, model, cost, at) in responses {
            crate::db::test::response(&db, from, id, *at, model, Some(*cost));
        }
        (dir, db)
    }

    fn keys(usage: &Usage) -> Vec<&str> {
        usage.rows.iter().map(|row| row.key.as_str()).collect()
    }

    #[test]
    fn a_limit_on_one_model_counts_that_models_responses_alone() {
        let (accounts, now) = example();
        let account = &accounts[0];
        let opus = account
            .limits
            .iter()
            .find(|limit| limit.scope.as_deref() == Some("Opus"))
            .unwrap();
        let at = now - time::HOUR;
        let responses = [
            ("r1", "claude-opus-5", 3.0, at),
            ("r2", "claude-sonnet-5", 1.0, at),
        ];
        let (_dir, db) = store(account, "claude-code", &responses);
        let (usage, shares) = breakdown(&db, account, Some(opus), None, By::Model, now).unwrap();
        assert_eq!(keys(&usage), ["claude-opus-5"]);
        assert_eq!(shares.unwrap().rows, [f64::from(100 - opus.left_percent)]);

        // A limit on several counts any of them.
        let mut either = opus.clone();
        either.scope = Some("Sonnet and Opus".to_owned());
        let (usage, _) = breakdown(&db, account, Some(&either), None, By::Model, now).unwrap();
        assert_eq!(keys(&usage), ["claude-opus-5", "claude-sonnet-5"]);
        either.scope = Some("Haiku and GPT".to_owned());
        let (usage, _) = breakdown(&db, account, Some(&either), None, By::Model, now).unwrap();
        assert!(usage.rows.is_empty());
    }

    /// OpenCode Go's 5 hours, a window that rolls, whose reset passed since
    /// it was read: what used it is the use of the last five hours.
    #[test]
    fn a_window_that_rolls_counts_the_last_of_its_length() {
        let (accounts, now) = example();
        let account = &accounts[3];
        let mut rolling = accounts[0].limits[0].clone();
        rolling.key = "rolling".to_owned();
        let reset = now - time::HOUR;
        rolling.window = Some(crate::status::LimitWindow {
            starts_at: None,
            resets: crate::status::Moment::of(reset, now),
        });
        rolling.reset_since = Some(reset);
        let responses = [
            ("this hour", "glm-5", 2.0, now - time::MINUTE),
            ("four hours ago", "glm-5", 1.0, now - 4 * time::HOUR),
            ("six hours ago", "glm-5", 1.0, now - 6 * time::HOUR),
            ("two days ago", "glm-5", 1.0, now - 2 * time::DAY),
        ];
        let (_dir, db) = store(account, "opencode", &responses);
        let (usage, _) = breakdown(&db, account, Some(&rolling), None, By::Session, now).unwrap();
        assert_eq!(usage.since, now - 5 * time::HOUR);
        assert_eq!(keys(&usage), ["this hour", "four hours ago"]);
        // Its reset still ahead.
        rolling.window = Some(crate::status::LimitWindow {
            starts_at: None,
            resets: crate::status::Moment::of(now + time::HOUR, now),
        });
        rolling.reset_since = None;
        let (usage, _) = breakdown(&db, account, Some(&rolling), None, By::Session, now).unwrap();
        assert_eq!(keys(&usage), ["this hour", "four hours ago"]);
    }

    /// ChatGPT's Weekly, read last 16 days ago, in a week that reset 9 days
    /// ago: the week that began then has ended too, and this one began 7
    /// days ago at the earliest.
    #[test]
    fn a_limit_that_reset_over_a_window_ago_counts_from_one_window_back() {
        let (accounts, now) = example();
        let account = &accounts[1];
        let responses = [
            ("this week", "gpt-5.5", 2.0, now - 2 * time::DAY),
            ("ended", "gpt-5.5", 4.0, now - 8 * time::DAY),
        ];
        let (_dir, db) = store(account, "codex", &responses);
        let mut late = account.limits[0].clone();
        let reset = now - 9 * time::DAY;
        late.window = Some(crate::status::LimitWindow {
            starts_at: Some(reset - 7 * time::DAY),
            resets: crate::status::Moment::of(reset, now),
        });
        late.reset_since = Some(reset);
        let (usage, shares) = breakdown(&db, account, Some(&late), None, By::Session, now).unwrap();
        assert_eq!(usage.since, now - 7 * time::DAY);
        assert_eq!(keys(&usage), ["this week"]);
        assert!(shares.is_none());
    }

    /// Claude's Weekly in the example has 60% left.
    #[test]
    fn the_rest_row_has_its_share_of_a_limit_as_text_and_as_markdown() {
        let (accounts, now) = example();
        let account = &accounts[0];
        let weekly = account.limit("week").unwrap();
        let at = now - time::HOUR;
        let responses = [
            ("r1", "claude-opus-5", 3.0, at),
            ("r2", "claude-sonnet-5", 0.75, at),
            ("r3", "claude-haiku-5", 0.25, at),
        ];
        let (_dir, db) = store(account, "claude-code", &responses);
        let (mut usage, shares) =
            breakdown(&db, account, Some(weekly), None, By::Model, now).unwrap();
        let mut shares = shares.unwrap();
        fold(&mut usage, Some(&mut shares), 1);
        // Of the 40 points used, three quarters of the cost and the rest's
        // quarter.
        assert_eq!(
            (shares.rows.as_slice(), shares.rest),
            (&[30.0][..], Some(10.0))
        );
        let text = text(&usage, Some((weekly, &shares)));
        assert!(
            text.contains("  2 more                 2  $1.00          10%\n"),
            "{text}"
        );
        let markdown = markdown(&usage, Some((weekly, &shares)), None);
        assert!(
            markdown.contains("| 2 more models | $1.00 | 2 | 10% |\n"),
            "{markdown}"
        );
        let (mut usage, _) = breakdown(&db, account, Some(weekly), None, By::Model, now).unwrap();
        fold(&mut usage, None, 2);
        let markdown = super::markdown(&usage, None, None);
        assert!(
            markdown.contains("| 1 more model | $0.25 | 1 |\n"),
            "{markdown}"
        );
    }

    /// OpenRouter's key held by OpenCode, beside Pi's on another key: its
    /// limit was used by OpenCode's responses alone.
    #[test]
    fn a_keys_limit_counts_that_keys_responses_alone() {
        let (accounts, now) = example();
        let account = &accounts[2];
        let limit = account.limit("key-fedcba").unwrap();
        let at = now - time::HOUR;
        let (_dir, db) = store(
            account,
            "opencode",
            &[("r1", "glm-5", 3.0, at), ("r2", "glm-5", 1.0, at)],
        );
        db.execute_batch(
            "UPDATE response SET agent = 'pi', folder = 'pi' WHERE id = 'r2';
             INSERT INTO sign_in VALUES ('opencode', 'f', 'openrouter', 0, 'openrouter:api', 'fedcba9876543210');
             INSERT INTO sign_in VALUES ('pi', 'pi', 'openrouter', 0, 'openrouter:api', '0123456789abcdef');",
        )
        .unwrap();
        let (usage, _) = breakdown(&db, account, Some(limit), None, By::Agent, now).unwrap();
        assert_eq!(keys(&usage), ["opencode"]);
    }

    /// A model not yet priced beside one priced: no shares, and why.
    #[test]
    fn usage_with_no_known_price_gives_no_shares() {
        let (accounts, now) = example();
        let account = &accounts[0];
        let weekly = account.limit("week").unwrap();
        let at = now - time::HOUR;
        let responses = [
            ("r1", "claude-opus-5", 0.1, at),
            ("r2", "claude-new-5", 0.0, at),
        ];
        let (_dir, db) = store(account, "claude-code", &responses);
        let shares =
            |db: &Connection| breakdown(db, account, Some(weekly), None, By::Model, now).unwrap();
        assert!(shares(&db).1.is_some());
        db.execute("UPDATE response SET cost = NULL WHERE id = 'r2'", [])
            .unwrap();
        let (usage, none) = shares(&db);
        assert!(none.is_none());
        assert_eq!(
            unshared(&usage, weekly).as_deref(),
            Some(
                "No shares of Weekly are given: 1 response with no known price, and a share is of cost."
            )
        );
    }

    /// A title is what someone typed: it's one string, in its own cell.
    #[test]
    fn a_title_stays_a_quoted_string_in_its_cell() {
        let row = |name: Option<&str>| Row {
            key: "0123456789".to_owned(),
            name: name.map(str::to_owned),
            responses: 1,
            ..Row::default()
        };
        let usage = Usage {
            since: 0,
            until: 1,
            by: By::Session,
            total: Row::default(),
            rows: vec![row(Some("Say \"ok\" | $9.99 |\n  9 |")), row(None)],
            rest: None,
        };
        assert_eq!(
            markdown(&usage, None, None)
                .lines()
                .skip(2)
                .collect::<Vec<_>>(),
            [
                r#"| `01234567` "Say \"ok\" \| $9.99 \| 9 \|" | $0.00 | 1 |"#,
                "| `01234567` (untitled) | $0.00 | 1 |"
            ]
        );
    }

    /// Over 999 sessions, the rest's key is longer than the 8 characters a
    /// session's id is cut to.
    #[test]
    fn the_rest_of_many_sessions_is_named_in_full() {
        let row = |key: &str, name: Option<&str>| Row {
            key: key.to_owned(),
            name: name.map(str::to_owned),
            responses: 1,
            ..Row::default()
        };
        let usage = Usage {
            since: 0,
            until: 0,
            by: By::Session,
            total: row("total", None),
            rows: vec![row("codex:7c1d2e3f-4a5b", Some("Fix it"))],
            rest: Some(row("3551 more", None)),
        };
        let text = text(&usage, None);
        assert!(text.contains("  7c1d2e3f Fix it  "), "{text}");
        assert!(text.contains("  3551 more  "), "{text}");
    }

    #[test]
    fn markdown_by_time_puts_earlier_first_with_its_period_name() {
        let row = |key: &str, cost_usd: f64, responses: u64| Row {
            key: key.to_owned(),
            cost_usd,
            responses,
            ..Row::default()
        };
        for (by, rest_key, expected_rest) in [
            (By::Day, "1 earlier", "| 1 earlier day | $1.00 | 1 |"),
            (By::Week, "2 earlier", "| 2 earlier weeks | $1.00 | 1 |"),
            (By::Month, "3 earlier", "| 3 earlier months | $1.00 | 1 |"),
        ] {
            let usage = Usage {
                since: 0,
                until: 1,
                by,
                total: row("total", 3.0, 2),
                rows: vec![row("2026-10-01", 2.0, 1)],
                rest: Some(row(rest_key, 1.0, 1)),
            };
            let table = markdown(&usage, None, None);
            let lines: Vec<&str> = table.lines().collect();
            assert_eq!(
                &lines[2..],
                [expected_rest, "| 2026-10-01 | $2.00 | 1 |"],
                "{table}"
            );
        }
    }
}
