//! What was used and what it cost, grouped by time, model, project, agent,
//! account or session.

use std::collections::BTreeMap;

use rusqlite::{Connection, params_from_iter, types::Value};
use serde::Serialize;

use crate::Result;
use crate::agents::{self, Tokens};
use crate::time::{self, Period};

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

pub struct Query {
    pub since: i64,
    pub until: i64,
    pub by: By,
    pub agent: Option<String>,
    pub account: Option<String>,
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

#[derive(Serialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub key: String,
    /// What people call it, where that isn't the key.
    pub name: Option<String>,
    pub responses: u64,
    pub tokens: Tokens,
    /// Of the responses with a known price.
    pub cost_usd: f64,
    /// Responses with no known price.
    pub unpriced: u64,
}

impl Row {
    fn add(&mut self, other: &Row) {
        self.responses += other.responses;
        let (tokens, more) = (&mut self.tokens, &other.tokens);
        tokens.input += more.input;
        tokens.cache_read += more.cache_read;
        tokens.cache_write_5m += more.cache_write_5m;
        tokens.cache_write_1h += more.cache_write_1h;
        tokens.output += more.output;
        tokens.reasoning += more.reasoning;
        self.cost_usd += other.cost_usd;
        self.unpriced += other.unpriced;
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
            "a.title || coalesce(' (' || a.label || ')', '')",
            "LEFT JOIN account a ON a.id = r.account",
        ),
        By::Project => (
            "coalesce(s.project, '')",
            "NULL",
            "LEFT JOIN session s ON s.id = r.session",
        ),
        // A subagent's or fork's usage counts toward the session it came
        // from, as finding sessions counts it.
        By::Session => (
            "coalesce(t.root, r.session)",
            "s.title",
            "LEFT JOIN tree t ON t.id = r.session LEFT JOIN session s ON s.id = coalesce(t.root, r.session)",
        ),
    };
    let trees = match query.by {
        By::Session => crate::sessions::TREES,
        _ => "",
    };
    let mut filters = vec!["r.at >= ?", "r.at < ?"];
    let mut values: Vec<Value> = vec![query.since.into(), query.until.into()];
    if let Some(agent) = &query.agent {
        filters.push("r.agent = ?");
        values.push(agent.clone().into());
    }
    if let Some(account) = &query.account {
        filters.push("r.account = ?");
        values.push(account.clone().into());
    }
    let sql = format!(
        "{trees} SELECT {key}, {name}, count(*), sum(r.input), sum(r.cache_read), sum(r.cache_write_5m),
             sum(r.cache_write_1h), sum(r.output), sum(r.reasoning), total(r.cost), sum(r.cost IS NULL)
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
            tokens: Tokens {
                input: count(3)?,
                cache_read: count(4)?,
                cache_write_5m: count(5)?,
                cache_write_1h: count(6)?,
                output: count(7)?,
                reasoning: count(8)?,
            },
            cost_usd: row.get(9)?,
            unpriced: count(10)?,
        };
        let key = match query.by {
            // The local date its period starts.
            By::Day | By::Week | By::Month => {
                let period = match query.by {
                    By::Day => Period::Day,
                    By::Week => Period::Week,
                    _ => Period::Month,
                };
                time::local(time::start_of(period, row.get(0)?))
                    .date()
                    .to_string()
            }
            // One model through several providers: OpenRouter's
            // `google/gemini-3.8-flash` is Google's `gemini-3.8-flash`.
            By::Model => {
                let model: String = row.get(0)?;
                model.rsplit('/').next().unwrap_or(&model).to_owned()
            }
            _ => row.get(0)?,
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
    let time = matches!(query.by, By::Day | By::Week | By::Month);
    if !time {
        rows.sort_by(|a, b| {
            b.cost_usd
                .total_cmp(&a.cost_usd)
                .then(b.responses.cmp(&a.responses))
        });
    }
    // Time keeps its latest rows, anything else its largest.
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
pub fn cost(usd: f64, unpriced: u64) -> String {
    match unpriced {
        0 => dollars(usd),
        more => format!("{} (+{} unpriced)", dollars(usd), count(more)),
    }
}

/// Dollars to the cent, with the thousands marked: `$31,704.31`.
pub fn dollars(amount: f64) -> String {
    let cents = (amount.max(0.0) * 100.0).round() as u64;
    format!("${}.{:02}", count(cents / 100), cents % 100)
}

/// A row's share of a limit, to the point, as shares are approximate:
/// `13%`, and under half a point, `<1%`.
pub fn share(points: f64) -> String {
    if points < 0.5 {
        "<1%".to_owned()
    } else {
        format!("{points:.0}%")
    }
}

/// Usage as a table: its span and total, then a row each. With `shares`, a
/// limit's name and each row's share of it, a column more.
pub fn text(usage: &Usage, shares: Option<(&str, &[Option<f64>])>) -> String {
    let unpriced = |row: &Row| match row.unpriced {
        0 => String::new(),
        more => format!(" (+{} unpriced)", count(more)),
    };
    let date = |at: i64| {
        let minute = (at + time::MINUTE / 2) / time::MINUTE * time::MINUTE;
        time::local(minute).strftime("%a %b %-d %H:%M").to_string()
    };
    let mut out = format!(
        "{} to {}: {} responses, {}{}\n",
        date(usage.since),
        date(usage.until),
        count(usage.total.responses),
        dollars(usage.total.cost_usd),
        unpriced(&usage.total),
    );
    // Time's earlier rows, added up, come before it; anything else's rest
    // after.
    let time = matches!(usage.by, By::Day | By::Week | By::Month);
    let mut rows: Vec<(&Row, Option<f64>)> = usage
        .rows
        .iter()
        .enumerate()
        .map(|(at, row)| {
            (
                row,
                shares.and_then(|(_, shares)| shares.get(at).copied().flatten()),
            )
        })
        .collect();
    if let Some(rest) = &usage.rest {
        rows.insert(if time { 0 } else { rows.len() }, (rest, None));
    }
    if rows.is_empty() {
        return out + "Nothing was used.";
    }
    let what = match usage.by {
        By::Day => "Day",
        By::Week => "Week of",
        By::Month => "Month of",
        By::Model => "Model",
        By::Project => "Project",
        By::Agent => "Agent",
        By::Account => "Account",
        By::Session => "Session",
    };
    let labels: Vec<String> = rows
        .iter()
        .map(|(row, _)| {
            // A session by the start of its agent's own id, which names it
            // as well, and its title.
            let label = match (&row.name, row.key.split_once(':')) {
                (name, Some((_, native))) if usage.by == By::Session => {
                    let start: String = native.chars().take(8).collect();
                    name.as_ref()
                        .map_or(start.clone(), |name| format!("{start} {name}"))
                }
                (Some(name), _) => name.clone(),
                (None, _) if row.key.is_empty() => "(none)".to_owned(),
                (None, _) => row.key.clone(),
            };
            crate::sessions::short(&label, 55)
        })
        .collect();
    let wide = labels
        .iter()
        .map(|label| label.chars().count())
        .chain([what.len()])
        .max()
        .unwrap_or_default();
    let costs: Vec<String> = rows.iter().map(|(row, _)| dollars(row.cost_usd)).collect();
    let cost_wide = costs.iter().map(String::len).max().unwrap_or(4).max(4);
    let share_head = shares.map(|(limit, _)| format!("% of {limit}"));
    out += &format!(
        "\n  {what:<wide$}  {:>9}  {:>cost_wide$}{}\n",
        "Responses",
        "Cost",
        share_head
            .as_ref()
            .map_or(String::new(), |head| format!("  {head}"))
    );
    for (((row, points), label), cost) in rows.iter().zip(&labels).zip(&costs) {
        let points = match (&share_head, points) {
            (Some(head), Some(points)) => {
                format!("  {:>width$}", share(*points), width = head.len())
            }
            _ => String::new(),
        };
        out += &format!(
            "  {label:<wide$}  {:>9}  {cost:>cost_wide$}{points}{}\n",
            count(row.responses),
            unpriced(row)
        );
    }
    out + "\nCosts are at list prices: what the same tokens cost on each provider's API."
}

/// What used a limit: `account`'s usage in `limit`'s window, or since
/// `since`, by `by`, each row with the points of the limit it took (its share
/// of the cost, of what's used), approximate, as providers say how much of a
/// limit is used but not by what.
pub fn breakdown(
    db: &Connection,
    account: &crate::status::Account,
    limit: &crate::status::Limit,
    since: Option<i64>,
    by: By,
    top: usize,
    now: i64,
) -> Result<(Usage, Vec<Option<f64>>)> {
    let window = limit.window.and_then(|window| window.starts_at);
    let usage = usage(
        db,
        &Query {
            // A key's spend counts by the month; a limit with no window
            // start otherwise over the last week.
            since: since.or(window).unwrap_or_else(|| match account.kind {
                crate::providers::Kind::ApiKey => time::start_of(time::Period::Month, now),
                crate::providers::Kind::Subscription => now - 7 * time::DAY,
            }),
            until: now,
            by,
            agent: None,
            account: Some(account.id.clone()),
            top,
        },
    )?;
    // Points, for the limit's own window alone.
    let used = f64::from(100 - limit.left_percent);
    let points = usage
        .rows
        .iter()
        .map(|row| {
            (since.is_none() && window.is_some() && usage.total.cost_usd > 0.0)
                .then(|| row.cost_usd / usage.total.cost_usd * used)
        })
        .collect();
    Ok((usage, points))
}
