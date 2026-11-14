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

impl By {
    /// The period, for a grouping by time.
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
    /// Responses with no known price.
    pub unpriced: u64,
}

impl Row {
    /// What it's called: its name, or else its key, or `(none)` for usage
    /// with neither, as on no account or in no project.
    pub fn label(&self) -> &str {
        match &self.name {
            Some(name) => name,
            None if self.key.is_empty() => "(none)",
            None => &self.key,
        }
    }

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
            crate::status::Account::FULL_TITLE,
            "LEFT JOIN account a ON a.id = r.account",
        ),
        By::Project => (
            "coalesce(s.project, '')",
            "NULL",
            "LEFT JOIN session s ON s.id = r.session",
        ),
        // A subagent's usage counts toward the session at the top of its
        // tree, as finding sessions counts it. A fork's or a continuation's
        // is its own.
        By::Session => (
            "coalesce(t.root, r.session)",
            "s.title",
            "LEFT JOIN tree t ON t.id = r.session LEFT JOIN session s ON s.id = coalesce(t.root, r.session)",
        ),
    };
    let trees = match query.by {
        By::Session => crate::sessions::trees("1"),
        _ => String::new(),
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
    if !query.models.is_empty() {
        let any = vec!["instr(lower(r.model), ?) > 0"; query.models.len()];
        filters.push(format!("({})", any.join(" OR ")));
        values.extend(query.models.iter().map(|model| model.clone().into()));
    }
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
            // The local date its period starts.
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
pub fn cost(usd: f64, unpriced_count: u64) -> String {
    dollars(usd) + &unpriced(unpriced_count)
}

/// ` (+3 unpriced)`, and with none, nothing.
fn unpriced(n: u64) -> String {
    match n {
        0 => String::new(),
        _ => format!(" (+{} unpriced)", count(n)),
    }
}

/// Dollars to the cent, with the thousands marked: `$31,704.31`.
pub fn dollars(amount: f64) -> String {
    let cents = (amount.max(0.0) * 100.0).round() as u64;
    format!("${}.{:02}", count(cents / 100), cents % 100)
}

/// A row's share of a limit, to the point, as shares are approximate:
/// `13%`, and under half a point, `<1%`.
fn share(points: f64) -> String {
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

/// Usage as a table: its span and total, then a row each. With `shares`, a
/// limit's name and each row's share of it, a column more.
pub fn text(usage: &Usage, shares: Option<(&str, &Shares)>) -> String {
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
    // Time's earlier rows, added up, come before it; anything else's rest
    // after.
    let time = usage.by.period().is_some();
    let mut rows: Vec<(&Row, Option<f64>)> = usage
        .rows
        .iter()
        .enumerate()
        .map(|(at, row)| (row, shares.map(|(_, shares)| shares.rows[at])))
        .collect();
    if let Some(rest) = &usage.rest {
        let points = shares.and_then(|(_, shares)| shares.rest);
        rows.insert(if time { 0 } else { rows.len() }, (rest, points));
    }
    if rows.is_empty() {
        return out + "Nothing was used.";
    }
    let what = usage.by.title();
    let labels: Vec<String> = rows
        .iter()
        .map(|(row, _)| {
            // A session by the start of its agent's own id, which names it
            // as well, and its title.
            let label = match row.key.split_once(':') {
                Some((_, native)) if usage.by == By::Session => {
                    let start: String = native.chars().take(8).collect();
                    row.name
                        .as_ref()
                        .map_or(start.clone(), |name| format!("{start} {name}"))
                }
                _ => row.label().to_owned(),
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
            unpriced(row.unpriced)
        );
    }
    out + "\nCosts are at list prices: what the same tokens cost on each provider's API."
}

/// Usage by anything but time as a Markdown table, its rest added up in a
/// last row. With `shares`, a limit and each row's points of it, a column
/// more. The asking session, `yours`, is marked.
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
    for (at, row) in usage.rows.iter().enumerate() {
        let name = match usage.by {
            // Its name is its title, which a session may not have yet.
            By::Session => format!(
                "`{}` {}{}",
                row.key,
                crate::sessions::title(row.name.as_deref()),
                if Some(row.key.as_str()) == yours {
                    " (yours)"
                } else {
                    ""
                }
            ),
            _ => row.label().to_owned(),
        };
        let points = cell(shares.map(|(_, shares)| shares.rows[at]));
        out += &format!(
            "| {name} | {} | {} |{points}\n",
            cost(row.cost_usd, row.unpriced),
            count(row.responses)
        );
    }
    if let Some(rest) = &usage.rest {
        let points = cell(shares.and_then(|(_, shares)| shares.rest));
        // Its key, made in `usage`, counts the rows it adds up: `3 more`.
        let rows = match rest.key.as_str() {
            "1 more" => format!("1 more {}", what.to_lowercase()),
            more => format!("{more} {}s", what.to_lowercase()),
        };
        out += &format!(
            "| {rows} | {} | {} |{points}\n",
            cost(rest.cost_usd, rest.unpriced),
            count(rest.responses)
        );
    }
    out
}

/// What used a limit: `account`'s usage in `limit`'s window, or since
/// `since`, by `by`, each row with the points of the limit it took (its share
/// of the cost, of what's used), approximate, as providers say how much of a
/// limit is used but not by what. With no limit, the account's usage.
///
/// A limit whose window reset since it was read is in a new window, which
/// began at the reset or after, and how much of that one is used isn't
/// known. So its usage counts from the reset, with no shares. Counted from
/// the start it was read with, it listed the sessions of the window before,
/// each at `<1%` of a limit shown as 100% left.
///
/// Where the window's length is known, it counts from no earlier than one
/// length back: a reset longer ago than that began a window that has ended
/// too, as when reads failed for longer than a window while its agent
/// worked. Chosen over the reset alone, which counted every window since:
/// with Claude's 5 hours read 30 hours before, reset 28 hours before, `usage
/// --limit "5 hours"` listed a session from 27 hours before. A window that
/// rolls has no length known, and counts from the reset.
pub fn breakdown(
    db: &Connection,
    account: &crate::status::Account,
    limit: Option<&crate::status::Limit>,
    since: Option<i64>,
    by: By,
    top: usize,
    now: i64,
) -> Result<(Usage, Option<Shares>)> {
    let window = limit.and_then(crate::status::Limit::started);
    let start = window.or_else(|| {
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
            since: since.or(start).unwrap_or_else(|| match account.kind {
                crate::providers::Kind::ApiKey => time::start_of(time::Period::Month, now),
                crate::providers::Kind::Subscription => now - 7 * time::DAY,
            }),
            until: now,
            by,
            agent: None,
            account: Some(account.id.clone()),
            // A limit on some models is used by their responses alone, whose
            // ids name a model as its scope does: "Opus" in `claude-opus-5`.
            // A scope that names several joins them with "and", as Google's
            // "Claude and GPT" does, and any of them counts.
            models: limit
                .and_then(|limit| limit.scope.as_ref())
                .into_iter()
                .flat_map(|scope| scope.split(" and "))
                .map(|model| model.trim().to_lowercase().replace(' ', "-"))
                .collect(),
            top,
        },
    )?;
    // Points, for the limit's own window alone.
    let used = limit
        .filter(|_| since.is_none() && window.is_some() && usage.total.cost_usd > 0.0)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::Account;

    /// The accounts of the contract's example status, and when it's as of.
    fn example() -> (Vec<Account>, i64) {
        let example: serde_json::Value =
            serde_json::from_str(include_str!("../contract/status.notification.json")).unwrap();
        let accounts = serde_json::from_value(example["params"]["accounts"].clone()).unwrap();
        (accounts, example["params"]["at"].as_i64().unwrap())
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
        let text = text(&usage, Some(("Weekly", &shares)));
        assert!(
            text.contains("  2 more                 2  $1.00          10%\n"),
            "{text}"
        );
        let markdown = markdown(&usage, Some((weekly, &shares)), None);
        assert!(
            markdown.contains("| 2 more models | $1.00 | 2 | 10% |\n"),
            "{markdown}"
        );
    }
}
