//! What was used and what it cost, and what used a limit.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params_from_iter, types::Value};
use serde::Serialize;

use crate::agents::{self, Tokens};
use crate::time::{self, Period};
use crate::{Error, Result};

#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum, Serialize)]
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

    /// The title of the column its rows are under.
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

    /// What one row is, singular, for the rest row's label.
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

pub struct Query {
    pub since: i64,
    pub until: i64,
    pub by: By,
    pub agent: Option<String>,
    pub account: Option<String>,
    /// Only responses of a model whose id holds one of these, in lowercase.
    /// Every model's, when empty.
    pub models: Vec<String>,
    /// Only responses made in an agent folder signed in with this key, by
    /// its fingerprint, when they were made.
    pub key: Option<String>,
    pub top: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub since: i64,
    pub until: i64,
    pub by: By,
    pub total: Row,
    /// The largest first, or for time, the earliest first.
    pub rows: Vec<Row>,
    /// The rest, past the top rows, added up: for time, the earliest.
    pub rest: Option<Row>,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub key: String,
    /// What people call it, where that isn't the key.
    pub name: Option<String>,
    pub responses: u64,
    pub tokens: Tokens,
    /// Of the responses with a known price.
    pub cost_usd: f64,
    pub unpriced: u64,
}

impl Row {
    /// What to call it, grouped `by`: an empty key is usage on no account,
    /// in no project, or of no model named.
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
    let mut filters = vec!["r.at >= ?".to_owned(), "r.at < ?".to_owned()];
    let mut values: Vec<Value> = vec![query.since.into(), query.until.into()];
    for (filter, value) in [
        ("r.agent = ?", &query.agent),
        ("r.account = ?", &query.account),
    ] {
        if let Some(value) = value {
            filters.push(filter.to_owned());
            values.push(value.clone().into());
        }
    }
    let (limited, given) = limited(&query.models, query.key.as_ref());
    filters.extend(limited);
    values.extend(given);
    let sql = format!(
        "{trees} SELECT {key}, {name}, count(*), sum(r.input), sum(r.cache_read), sum(r.cache_write_5m),
             sum(r.cache_write_1h), sum(r.output), sum(r.reasoning), total(r.cost), count(*) - count(r.cost)
         FROM response r {join} WHERE {} GROUP BY 1",
        filters.join(" AND ")
    );
    let mut groups: BTreeMap<String, Row> = BTreeMap::new();
    let mut statement = db.prepare(&sql)?;
    let mut rows = statement.query(params_from_iter(values))?;
    while let Some(row) = rows.next()? {
        let count = |at: usize| row.get::<_, i64>(at).map(|count| count as u64);
        let found = Row {
            key: String::new(),
            name: row.get(1)?,
            responses: count(2)?,
            tokens: crate::db::tokens(row, 3)?,
            cost_usd: row.get(9)?,
            unpriced: count(10)?,
        };
        let key = if let Some(period) = query.by.period() {
            time::local(time::start_of(period, row.get(0)?))
                .date()
                .to_string()
        } else if query.by == By::Model {
            // One model through several providers: OpenRouter's
            // `google/gemini-3.8-flash` is Google's `gemini-3.8-flash`.
            let model: String = row.get(0)?;
            model.rsplit('/').next().unwrap_or(&model).to_owned()
        } else {
            row.get(0)?
        };
        let group = groups.entry(key.clone()).or_default();
        if query.by == By::Agent {
            group.name = Some(agents::name(&key).to_owned());
        }
        group.name = group.name.take().or(found.name.clone());
        group.key = key;
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
    let time = query.by.period().is_some();
    if !time {
        rows.sort_by(|a, b| {
            b.cost_usd
                .total_cmp(&a.cost_usd)
                .then(b.responses.cmp(&a.responses))
        });
    }
    let rest = (rows.len() > query.top).then(|| {
        let folded: Vec<Row> = match time {
            true => rows.drain(..rows.len() - query.top).collect(),
            false => rows.drain(query.top..).collect(),
        };
        let mut rest = Row {
            key: format!("{} {}", folded.len(), if time { "earlier" } else { "more" }),
            ..Row::default()
        };
        for row in &folded {
            rest.add(row);
        }
        rest
    });
    Ok(Usage {
        since: query.since,
        until: query.until,
        by: query.by,
        total,
        rows,
        rest,
    })
}

/// A count with its thousands marked: `196,813`.
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

/// What usage cost, with how many responses had no known price, which
/// aren't free: `$0.47`, `$0.47 (+3 unpriced)`.
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

/// To the point, as shares are approximate.
pub fn share(points: f64) -> String {
    if points < 0.5 {
        "<1%".to_owned()
    } else {
        format!("{points:.0}%")
    }
}

/// The points of a limit each of a usage's rows took, in the rows' order,
/// and its rest row's. They're known for every row or for none, as each is
/// its part of the points used.
pub struct Shares {
    pub rows: Vec<f64>,
    /// None with no rest row.
    pub rest: Option<f64>,
}

pub fn text(usage: &Usage, shares: Option<(&crate::status::Limit, &Shares)>) -> String {
    let date = |at: i64| {
        let minute = (at + time::MINUTE / 2) / time::MINUTE * time::MINUTE;
        time::local(minute).strftime("%a %b %-d %H:%M").to_string()
    };
    let mut out = format!(
        "{} to {}: {} responses, {}\n",
        date(usage.since),
        date(usage.until),
        count(usage.total.responses),
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
    out += &format!(
        "\n  {what:<wide$}  {:>9}  {:>cost_wide$}{}\n",
        "Responses",
        "Cost",
        share_head
            .as_ref()
            .map_or(String::new(), |head| format!("  {head}"))
    );
    for ((row, points, label), cost) in rows.iter().zip(&costs) {
        let points = match (&share_head, points) {
            (Some(head), Some(points)) => {
                format!("  {:>width$}", share(*points), width = head.len())
            }
            _ => String::new(),
        };
        out += &format!(
            "  {label:<wide$}  {:>9}  {cost:>cost_wide$}{points}{}\n",
            count(row.responses),
            unpriced(row.unpriced)
        );
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
    let cell = |points: Option<f64>| {
        points.map_or(String::new(), |points| format!(" {} |", share(points)))
    };
    let rest_line = usage.rest.as_ref().map(|rest| {
        let points = cell(shares.and_then(|(_, shares)| shares.rest));
        // Its key, made in `usage`, counts the rows it adds up: `3 more` or `3 earlier`.
        let noun = usage.by.noun();
        let rows = match rest.key.as_str() {
            "1 more" | "1 earlier" => format!("{} {noun}", rest.key),
            other => format!("{other} {noun}s"),
        };
        format!(
            "| {rows} | {} | {} |{points}\n",
            cost(rest.cost_usd, rest.unpriced),
            count(rest.responses)
        )
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
        let points = cell(shares.map(|(_, shares)| shares.rows[at]));
        out += &format!(
            "| {name} | {} | {} |{points}\n",
            cost(row.cost_usd, row.unpriced),
            count(row.responses)
        );
    }
    if !time && let Some(line) = &rest_line {
        out += line;
    }
    out
}

/// What used a limit: `account`'s usage in `limit`'s window, or
/// `between` a start and an end, each row in the window with the points of
/// the limit it took: its share of the cost, times the points used. Approximate, as providers say how much of a
/// limit is used but not by what. With no limit, the account's usage.
///
/// A limit whose window reset since it was read is in a new window, and how
/// much of that one is used isn't known. So its usage counts from the
/// reset, with no shares, over the start it was read with, which listed the
/// sessions of the window before, each at `<1%` of a limit shown as 100%
/// left.
///
/// Where the window's length is known, it counts from no earlier than one
/// length back, as a reset longer ago began a window that has ended too.
/// Chosen over the reset alone, which counted every window since: with
/// Claude's 5-hour limit read 30 hours before and reset 28 hours before,
/// `usage --limit "5 hours"` listed a session from 27 hours before.
///
/// A window that rolls counts the last of its length, as its provider gives
/// it (`Provider::rolls`), whenever it resets: OpenCode Go's 5 hours, with no
/// start, listed a week of sessions.
///
/// Usage with no known price gives no shares (`unshared`): a share is of
/// cost, so a model not yet priced took `<1%` and the priced rows its
/// points, at each model's launch until models.dev lists it.
pub fn breakdown(
    db: &Connection,
    account: &crate::status::Account,
    limit: Option<&crate::status::Limit>,
    between: Option<(i64, i64)>,
    by: By,
    top: usize,
    now: i64,
) -> Result<(Usage, Option<Shares>)> {
    let window = limit.and_then(crate::status::Limit::started);
    let rolls = limit.and_then(|limit| {
        crate::providers::by_id(&account.provider)?
            .rolls(&limit.key)
            .map(|length| now - length)
    });
    let start = rolls.or(window).or_else(|| {
        let limit = limit?;
        let reset = limit.reset_since?;
        let length = limit
            .window
            .and_then(|window| Some(window.resets.at - window.starts_at?));
        Some(length.map_or(reset, |length| reset.max(now - length)))
    });
    let usage = usage(
        db,
        &Query {
            // A key's spend counts by the month; a limit with no window
            // start otherwise over the last week.
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
            key: key(limit.map_or("", |limit| &limit.key)),
            top,
        },
    )?;
    // Points, for the limit's own window alone.
    let used = limit
        .filter(|_| between.is_none() && window.is_some() && usage.total.cost_usd > 0.0)
        .filter(|_| usage.total.unpriced == 0)
        .map(|limit| f64::from(100 - limit.left_percent));
    let shares = used.map(|used| {
        let points = |row: &Row| row.cost_usd / usage.total.cost_usd * used;
        Shares {
            rows: usage.rows.iter().map(points).collect(),
            rest: usage.rest.as_ref().map(points),
        }
    });
    Ok((usage, shares))
}

/// Why `usage` of `limit`'s window gives no shares, when it's that some of
/// it has no known price.
pub fn unshared(usage: &Usage, limit: &crate::status::Limit) -> Option<String> {
    let responses = match usage.total.unpriced {
        0 => return None,
        1 => "1 response".to_owned(),
        n => format!("{} responses", count(n)),
    };
    limit.started().map(|_| {
        format!(
            "No shares of {} are given: {responses} with no known price, and a share is of cost.",
            limit.named()
        )
    })
}

/// What a limit on some models is used by: responses of a model whose id
/// names one, as "Opus" is in `claude-opus-5`. A scope that names several
/// joins them with "and", as Google's "Claude and GPT" does, and any of
/// them counts. A scope that names no model, as "Other apps", counts none.
/// Every model, with no scope.
pub fn models(scope: Option<&str>) -> Vec<String> {
    scope
        .into_iter()
        .flat_map(|scope| scope.split(" and "))
        .map(|model| model.trim().to_lowercase().replace(' ', "-"))
        .collect()
}

/// What a key's own limit, `limit_key`, is used by: the responses made with
/// that key, by its fingerprint. An API-key account holds every key to its
/// provider, and Pi's usage on another key was listed under OpenCode's.
pub fn key(limit_key: &str) -> Option<String> {
    limit_key.strip_prefix("key-").map(str::to_owned)
}

/// The filters a response `r` passes to be of `models` and `key`, with the
/// values of their `?`s, in order.
pub fn limited(models: &[String], key: Option<&String>) -> (Vec<String>, Vec<Value>) {
    let mut filters = Vec::new();
    let mut values: Vec<Value> = Vec::new();
    if !models.is_empty() {
        let any = vec!["instr(lower(r.model), ?) > 0"; models.len()];
        filters.push(format!("({})", any.join(" OR ")));
        values.extend(models.iter().map(|model| model.clone().into()));
    }
    // The key its folder's sign-in held when it was made, as `ingest` works
    // out its account.
    if let Some(key) = key {
        filters.push(format!("(SELECT key {}) = ?", crate::prices::SIGN_IN_AT));
        values.push(key.clone().into());
    }
    (filters, values)
}

/// The project and agent of the session `id`.
pub fn brief(db: &Connection, id: &str) -> Result<(Option<String>, String)> {
    db.prepare_cached("SELECT project, agent FROM session WHERE id = ?1")?
        .query_row([id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("no session is {id}")))
}

pub fn session_account(db: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
    db.query_row(
        "SELECT account FROM response WHERE session = ?1 AND account IS NOT NULL ORDER BY at DESC LIMIT 1",
        [id],
        |row| row.get(0),
    )
    .optional()
}

/// The first 8 characters of the agent's own id, over the whole id, some 45
/// characters a row in an answer to an agent. Nothing takes an id: it only
/// tells two sessions with one title apart, which 8 characters do.
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

/// A title is the session's own words, often what a person asked there, and
/// an agent reading the answer must not take them for Turnscope's. So each
/// is quoted where it stands, over one note at the top that a reader is far
/// past by the last row. Double quotes, as the session's id beside it is in
/// backticks; JSON's escapes keep a quote inside from ending it early.
fn title(title: Option<&str>) -> String {
    title.map_or("(untitled)".to_owned(), |title| {
        serde_json::Value::String(short(title, 80)).to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Account;

    /// The accounts of the example status, and when it's as of.
    fn example() -> (Vec<Account>, i64) {
        let example: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/status.json")).unwrap();
        let accounts = serde_json::from_value(example["accounts"].clone()).unwrap();
        (accounts, example["at"].as_i64().unwrap())
    }

    /// A store with `agent`'s responses on `account`: the id of each, which
    /// is its session's too, its model, its cost and when it was.
    fn store(
        account: &Account,
        agent: &str,
        responses: &[(&str, &str, f64, i64)],
    ) -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).unwrap();
        for (id, model, cost, at) in responses {
            db.execute(
                "INSERT INTO response (agent, id, session, copy, file, folder, at, provider, model, input,
                     cache_read, cache_write_5m, cache_write_1h, output, reasoning, prompt, web_searches,
                     priority, cost, account)
                 VALUES (?1, ?2, ?2, 0, 1, 'f', ?3, ?4, ?5, 1, 0, 0, 0, 1, 0, 1, 0, 0, ?6, ?7)",
                rusqlite::params![agent, id, at, account.provider, model, cost, account.id],
            )
            .unwrap();
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
        let (usage, shares) =
            breakdown(&db, account, Some(opus), None, By::Model, 10, now).unwrap();
        assert_eq!(keys(&usage), ["claude-opus-5"]);
        assert_eq!(shares.unwrap().rows, [f64::from(100 - opus.left_percent)]);

        // A limit on several counts any of them.
        let mut either = opus.clone();
        either.scope = Some("Sonnet and Opus".to_owned());
        let (usage, _) = breakdown(&db, account, Some(&either), None, By::Model, 10, now).unwrap();
        assert_eq!(keys(&usage), ["claude-opus-5", "claude-sonnet-5"]);
        either.scope = Some("Haiku and GPT".to_owned());
        let (usage, _) = breakdown(&db, account, Some(&either), None, By::Model, 10, now).unwrap();
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
        let (usage, _) =
            breakdown(&db, account, Some(&rolling), None, By::Session, 10, now).unwrap();
        assert_eq!(usage.since, now - 5 * time::HOUR);
        assert_eq!(keys(&usage), ["this hour", "four hours ago"]);
        // Its reset still ahead.
        rolling.window = Some(crate::status::LimitWindow {
            starts_at: None,
            resets: crate::status::Moment::of(now + time::HOUR, now),
        });
        rolling.reset_since = None;
        let (usage, _) =
            breakdown(&db, account, Some(&rolling), None, By::Session, 10, now).unwrap();
        assert_eq!(keys(&usage), ["this hour", "four hours ago"]);
    }

    /// ChatGPT's Weekly in the example reset ten minutes ago, after its last
    /// read: it's shown as 100% left, with the start of the window before.
    #[test]
    fn a_limit_whose_window_reset_since_its_read_counts_from_the_reset_with_no_shares() {
        let (accounts, now) = example();
        let account = &accounts[1];
        let weekly = &account.limits[0];
        let reset = weekly.reset_since.unwrap();
        assert!(weekly.window.unwrap().starts_at.unwrap() < reset && reset < now);
        let responses = [
            ("before", "gpt-5.5", 3.0, reset - time::HOUR),
            ("after", "gpt-5.5", 1.0, reset + time::MINUTE),
            ("this week", "gpt-5.5", 2.0, now - 2 * time::DAY),
            ("ended", "gpt-5.5", 4.0, now - 8 * time::DAY),
        ];
        let (_dir, db) = store(account, "codex", &responses);
        let (usage, shares) =
            breakdown(&db, account, Some(weekly), None, By::Session, 10, now).unwrap();
        assert_eq!(usage.since, reset);
        // The one before used the window that ended, and no points of this
        // one are known to be used.
        assert_eq!(keys(&usage), ["after"]);
        assert!(shares.is_none());

        // Read last 16 days ago, in a week that reset 9 days ago: the week
        // that began then has ended too, and this one began 7 days ago at
        // the earliest.
        let mut late = weekly.clone();
        let reset = now - 9 * time::DAY;
        late.window = Some(crate::status::LimitWindow {
            starts_at: Some(reset - 7 * time::DAY),
            resets: crate::status::Moment::of(reset, now),
        });
        late.reset_since = Some(reset);
        let (usage, shares) =
            breakdown(&db, account, Some(&late), None, By::Session, 10, now).unwrap();
        assert_eq!(usage.since, now - 7 * time::DAY);
        assert_eq!(keys(&usage), ["before", "this week", "after"]);
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
        let (usage, shares) =
            breakdown(&db, account, Some(weekly), None, By::Model, 1, now).unwrap();
        let shares = shares.unwrap();
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
        let (usage, _) = breakdown(&db, account, Some(weekly), None, By::Model, 2, now).unwrap();
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
        let (usage, _) = breakdown(&db, account, Some(limit), None, By::Agent, 5, now).unwrap();
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
        let shares = |db: &Connection| {
            breakdown(db, account, Some(weekly), None, By::Model, 5, now).unwrap()
        };
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

    /// A title is what someone typed: a `|` in it stays in its cell.
    #[test]
    fn a_bar_in_a_title_stays_in_its_cell() {
        let usage = Usage {
            since: 0,
            until: 1,
            by: By::Session,
            total: Row::default(),
            rows: vec![Row {
                key: "0123456789".to_owned(),
                name: Some("ok | $9.99 | 9 |".to_owned()),
                responses: 1,
                ..Row::default()
            }],
            rest: None,
        };
        assert_eq!(
            markdown(&usage, None, None).lines().last(),
            Some(r#"| `01234567` "ok \| $9.99 \| 9 \|" | $0.00 | 1 |"#)
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
    fn a_title_is_quoted_as_json_writes_a_string() {
        assert_eq!(title(None), "(untitled)");
        assert_eq!(title(Some("Say \"hi\"\n  now")), r#""Say \"hi\" now""#);
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
