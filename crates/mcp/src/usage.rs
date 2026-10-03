//! `get_usage`: tokens and cost over a period, split as asked.
//!
//! **Accounts.** Usage is told apart by the account each response drew on,
//! the one signed in where and when it was made, as the engine keeps it
//! ([`turnscope_engine::Dimension::Account`]), so two accounts of one
//! subscription each have what was made under them, and a session made
//! under one and then another after a switch counts in each for its part.
//! Use of an API key is its provider's API-key account's. Usage no account
//! is known to have drawn on, as from a folder no look for sign-ins was
//! made in, is a row of its own, after the accounts'. An account the person
//! hid is given by its id and said to be hidden, not named.

use serde::Deserialize;
use serde_json::{Value, json};
use turnscope_engine::{
    Agent, Bucket, Dimension, Filter, ModelKey, Tokens, Totals, UsageQuery, UsageRow, UsageTable,
    Usd,
};

use crate::accounts;
use crate::prose;
use crate::tools::{
    self, Answer, Failure, Reply, Server, account_schema, agent_schema, folder_schema, object,
    rounded, since_schema, until_schema,
};

/// The most rows an answer holds, as the tool's description says: at about
/// 200 characters a row, within the 25,000 tokens Claude Code takes from a
/// tool by default. A question that makes more is asked to narrow. A split
/// by time gives a row only for each time with usage, so a period of any
/// length can be split as finely as its usage allows.
const MOST_ROWS: usize = 200;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetUsage {
    since: Option<String>,
    until: Option<String>,
    folder: Option<String>,
    account: Option<String>,
    agent: Option<String>,
    model: Option<String>,
    by: Option<Split>,
}

pub(crate) fn schema() -> Value {
    object(
        json!({
            "since": since_schema("From then on, or all of history"),
            "until": until_schema("Until then, or up to now"),
            "folder": folder_schema(),
            "account": account_schema("Only the usage that draws on it, or on any account it names."),
            "agent": agent_schema(),
            "model": {
                "type": "string",
                "description": "Only this model, as agents name it, such as claude-opus-5. One no usage is of is refused.",
            },
            "by": {
                "type": "string",
                "enum": ["day", "week", "month", "project", "model", "agent", "account", "session"],
                "description": "Split the total by local days, weeks from Monday or months, a row for each with usage; or by project, model, agent, account or session. A null group is usage that names none, as usage outside the conversation may name no model; by account, usage no account is known to have drawn on, after the accounts.",
            },
        }),
        &[],
    )
}

/// How a total is split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum By {
    Time(Bucket),
    Dimension(Dimension),
}

/// A split as get_usage's `by` names it.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Split {
    Day,
    Week,
    Month,
    Project,
    Model,
    Agent,
    Account,
    Session,
}

impl Split {
    fn by(self) -> By {
        match self {
            Split::Day => By::Time(Bucket::Day),
            Split::Week => By::Time(Bucket::Week),
            Split::Month => By::Time(Bucket::Month),
            Split::Project => By::Dimension(Dimension::Project),
            Split::Model => By::Dimension(Dimension::Model),
            Split::Agent => By::Dimension(Dimension::Agent),
            Split::Account => By::Dimension(Dimension::Account),
            Split::Session => By::Dimension(Dimension::Session),
        }
    }
}

pub(crate) fn usage(server: &Server, arguments: GetUsage) -> Answer {
    let by = arguments.by.map(Split::by);
    let span = server.span(arguments.since.as_deref(), arguments.until.as_deref())?;
    let accounts = server.engine.limits()?;
    let model = arguments.model.as_deref().map(ModelKey::of);
    let mut filter = Filter {
        projects: tools::folder(server, arguments.folder.as_deref())?,
        models: model.iter().cloned().collect(),
        ..Filter::default()
    };
    if let Some(asked) = arguments.account.as_deref() {
        filter.accounts = accounts::ids(asked, &accounts)?;
    }
    if let Some(agent) = tools::agent(arguments.agent.as_deref())? {
        filter.agents = vec![agent];
    }
    let UsageTable {
        mut rows, total, ..
    } = server.engine.usage(
        &UsageQuery {
            span,
            filter,
            by: match by {
                Some(By::Dimension(dimension)) => Some(dimension),
                _ => None,
            },
            every: match by {
                Some(By::Time(bucket)) => Some(bucket),
                _ => None,
            },
        },
        &server.local(),
    )?;
    if let Some(model) = &model
        && total.responses == 0
        && total.tokens.total() == 0
    {
        known(server, model)?;
    }
    if by == Some(By::Dimension(Dimension::Account)) {
        // Each account by the name it goes by, and the usage no account is
        // known to have drawn on after them.
        for row in &mut rows {
            row.label = match group(row) {
                Some(id) => accounts::called(id, &accounts).map(|said| prose::capitalized(&said)),
                None => Some("No account known".to_owned()),
            };
        }
        rows.sort_by_key(|row| group(row).is_none());
    }
    if by == Some(By::Dimension(Dimension::Model)) {
        // Each model by what the catalog calls it, as explain_limit names
        // them; the group stays the key the model argument takes.
        let names = server.model_names()?;
        for row in &mut rows {
            if let Some(name) = group(row).and_then(|key| names.get(key)) {
                row.label = Some(name.clone());
            }
        }
    }
    if by == Some(By::Dimension(Dimension::Agent)) {
        // Each agent by its name; the group stays the key the agent
        // argument takes.
        for row in &mut rows {
            if let Some(agent) = group(row).and_then(Agent::from_key) {
                row.label = Some(agent.name().to_owned());
            }
        }
    }
    if rows.len() > MOST_ROWS {
        return Err(Failure(format!(
            "That splits into {} rows, more than {MOST_ROWS}; narrow the period, or split by a \
             longer interval.",
            rows.len()
        )));
    }
    let mut answer = json!({
        "since": span.from.map(|at| server.time(at)),
        "until": span.until.map(|at| server.time(at)),
        "total": totals(&total),
    });
    let mut said = vec![format!(
        "{}: {}.",
        period(server, span.from, span.until),
        amount(&total, true)
    )];
    if by.is_some() {
        answer["rows"] = rows
            .iter()
            .map(|row| {
                let mut value = json!({"usage": totals(&row.totals)});
                match by {
                    Some(By::Time(_)) => {
                        value["start"] = json!(row.start.map(|start| server.time(start)));
                    }
                    _ => {
                        value["group"] = json!(group(row));
                        value["label"] = json!(row.label);
                    }
                }
                value
            })
            .collect();
        // Most used first, for the sentences; by time, in time's order.
        let mut ordered: Vec<&UsageRow> = rows.iter().collect();
        if !matches!(by, Some(By::Time(_))) {
            ordered.sort_by_key(|row| std::cmp::Reverse(row.totals.tokens.total()));
        }
        let lines: Vec<String> = ordered
            .iter()
            .map(|row| {
                let name = match (by, row.start) {
                    (Some(By::Time(_)), Some(start)) => {
                        prose::day(start, turnscope_engine::Instant::now(), &server.zone)
                    }
                    (Some(By::Time(_)), None) => "at no time known".to_owned(),
                    (Some(By::Dimension(dimension)), _) => named(row, dimension),
                    (None, _) => "none named".to_owned(),
                };
                format!("- {name}: {}", amount(&row.totals, false))
            })
            .collect();
        if !lines.is_empty() {
            said.push(lines.join("\n"));
        }
    }
    Ok(Reply::Answer {
        said: said.join("\n\n"),
        data: answer,
    })
}

/// Refuse `model` when no usage in history is of it, as of a misspelt one,
/// whose total would read as none used. Asked only of an empty total, so
/// that an answer with usage costs no question of every model used.
fn known(server: &Server, model: &ModelKey) -> Result<(), Failure> {
    if server
        .engine
        .models()?
        .iter()
        .any(|used| used.key == *model)
    {
        Ok(())
    } else {
        Err(Failure(format!(
            "No usage in history is of the model {:?}; get_usage split by model lists those \
             that are.",
            model.as_str()
        )))
    }
}

/// The period from `from` to `until`, as a sentence starts with it.
fn period(
    server: &Server,
    from: Option<turnscope_engine::Instant>,
    until: Option<turnscope_engine::Instant>,
) -> String {
    match (from, until) {
        (None, None) => "All of history".to_owned(),
        (Some(from), None) => format!("Since {}", server.clock(from)),
        (None, Some(until)) => format!("Until {}", server.clock(until)),
        (Some(from), Some(until)) => {
            format!("From {} to {}", server.clock(from), server.clock(until))
        }
    }
}

/// A row as a sentence names it: what the catalog or its agent calls it,
/// and, where an argument takes something else, that too, so that it can be
/// asked about alone: a session's id, a model's key, an agent's key, a
/// project's folder.
fn named(row: &UsageRow, dimension: Dimension) -> String {
    let label = row.label.as_deref();
    match (label, group(row)) {
        (Some(label), Some(group))
            if label != group
                && matches!(
                    dimension,
                    Dimension::Session | Dimension::Model | Dimension::Agent | Dimension::Project
                ) =>
        {
            format!("{label} ({group})")
        }
        (Some(label), _) => label.to_owned(),
        (None, Some(group)) => group.to_owned(),
        (None, None) => "none named".to_owned(),
    }
}

/// Usage as a sentence says it: its tokens and cost, and, `full`, the kinds
/// of tokens and how many responses.
fn amount(totals: &Totals, full: bool) -> String {
    let tokens = &totals.tokens;
    let mut said = format!("{} tokens", prose::tokens(tokens.total()));
    if full && tokens.total() > 0 {
        let mut kinds = vec![
            format!("{} input", prose::tokens(tokens.input)),
            format!("{} output", prose::tokens(tokens.output)),
        ];
        if tokens.reasoning > 0 {
            kinds[1].push_str(&format!(
                " ({} of it reasoning)",
                prose::tokens(tokens.reasoning)
            ));
        }
        if tokens.cache_read > 0 {
            kinds.push(format!("{} cache reads", prose::tokens(tokens.cache_read)));
        }
        if tokens.cache_write() > 0 {
            kinds.push(format!(
                "{} cache writes",
                prose::tokens(tokens.cache_write())
            ));
        }
        said.push_str(&format!(
            " ({}), {}",
            kinds.join(", "),
            prose::count(totals.responses, "response")
        ));
    }
    said.push_str(", ");
    said.push_str(&cost(totals));
    said
}

/// What usage cost, as a sentence says it: at list prices, but for what
/// providers billed and agents estimated, which are said apart, and what it
/// leaves out.
pub(crate) fn cost(totals: &Totals) -> String {
    let Some(cost) = totals.known_cost() else {
        return "cost unknown".to_owned();
    };
    let (charged, estimated) = (totals.charged.dollars(), totals.estimated.dollars());
    let listed = cost.dollars() - charged - estimated;
    // A part under a tenth of a cent isn't said apart; the figures carry
    // each exactly.
    let parts: Vec<(f64, &str)> = [
        (listed, "at list prices"),
        (charged, "as billed"),
        (estimated, "as agents estimated"),
    ]
    .into_iter()
    .filter(|(dollars, _)| *dollars >= 0.001)
    .collect();
    let whole = prose::money(cost.dollars());
    let mut said = match parts.as_slice() {
        [] => format!("{whole} at list prices"),
        [(_, how)] => format!("{whole} {how}"),
        several => {
            let several: Vec<String> = several
                .iter()
                .map(|(dollars, how)| format!("{} {how}", prose::money(*dollars)))
                .collect();
            format!("{whole}: {}", prose::list(&several))
        }
    };
    if totals.unpriced > 0 {
        said.push_str(", leaving out usage with no known price");
    }
    said
}

/// What `row` is told apart by: `None` for usage that names nothing, such as
/// usage outside the conversation that names no model, which the engine
/// keys as empty.
fn group(row: &UsageRow) -> Option<&str> {
    row.group.as_deref().filter(|group| !group.is_empty())
}

/// Usage, as answers give it.
///
/// `cost_usd` is null rather than zero when some of the usage has no price
/// and what was priced came to nothing, as [`Totals::known_cost`] decides;
/// `unpriced_usage` marks a cost that leaves some usage out, and
/// `cost_approximate` one that is approximate, as [`Totals::approximate`]
/// says.
pub(crate) fn totals(totals: &Totals) -> Value {
    let cost = match totals.known_cost() {
        Some(cost) => dollars(cost),
        None => Value::Null,
    };
    let mut value = json!({
        "tokens": tokens(&totals.tokens),
        "responses": totals.responses,
        "cost_usd": cost,
    });
    if totals.unpriced > 0 {
        value["unpriced_usage"] = json!(true);
    }
    if totals.approximate {
        value["cost_approximate"] = json!(true);
    }
    // The parts of the cost that rest on something other than list prices.
    if totals.charged > Usd::default() {
        value["cost_charged_usd"] = dollars(totals.charged);
    }
    if totals.estimated > Usd::default() {
        value["cost_agent_estimate_usd"] = dollars(totals.estimated);
    }
    if totals.outside > 0 {
        value["outside_conversation_tokens"] = json!(totals.outside);
    }
    value
}

/// Money, as answers give it: to a millionth of a dollar, finer than any
/// price.
fn dollars(usd: Usd) -> Value {
    json!(rounded(usd.dollars(), 6))
}

fn tokens(tokens: &Tokens) -> Value {
    json!({
        "input": tokens.input,
        "cache_read": tokens.cache_read,
        "cache_write": tokens.cache_write(),
        "output": tokens.output,
        "reasoning": tokens.reasoning,
        "total": tokens.total(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use turnscope_engine::{Totals, Usd};

    use super::{cost, totals};

    #[test]
    fn a_cost_is_given_to_a_millionth_of_a_dollar_with_what_isnt_at_list_prices() {
        let approximate = Totals {
            cost: Usd::from_nanos(1_234_567_891).unwrap(),
            approximate: true,
            ..Totals::default()
        };
        let answer = totals(&approximate);
        assert_eq!(answer["cost_usd"], json!(1.234568));
        assert_eq!(answer["cost_approximate"], json!(true));
        // $25 at list prices, $7.50 charged and $0.06 estimated: $32.56.
        let mixed = Totals {
            cost: Usd::from_nanos(32_560_000_000).unwrap(),
            charged: Usd::from_nanos(7_500_000_000).unwrap(),
            estimated: Usd::from_nanos(60_000_000).unwrap(),
            ..Totals::default()
        };
        let answer = totals(&mixed);
        assert_eq!(answer["cost_usd"], json!(32.56));
        assert_eq!(answer["cost_charged_usd"], json!(7.5));
        assert_eq!(answer["cost_agent_estimate_usd"], json!(0.06));
        assert_eq!(
            cost(&mixed),
            "$32.56: $25.00 at list prices, $7.50 as billed and $0.06 as agents estimated"
        );
        // An estimate under a tenth of a cent isn't said apart.
        let slight = Totals {
            cost: Usd::from_nanos(25_000_400_000).unwrap(),
            estimated: Usd::from_nanos(400_000).unwrap(),
            ..Totals::default()
        };
        assert_eq!(cost(&slight), "$25.00 at list prices");
        // All at list prices, neither part is said.
        let all_listed = Totals {
            cost: Usd::from_nanos(25_000_000_000).unwrap(),
            ..Totals::default()
        };
        let listed = totals(&all_listed);
        assert!(listed.get("cost_charged_usd").is_none());
        assert!(listed.get("cost_agent_estimate_usd").is_none());
        assert!(listed.get("cost_approximate").is_none());
        assert_eq!(cost(&all_listed), "$25.00 at list prices");
        // All billed, with a model no catalog prices besides: none of it is
        // at list prices.
        let billed = Totals {
            cost: Usd::from_nanos(7_500_000_000).unwrap(),
            charged: Usd::from_nanos(7_500_000_000).unwrap(),
            unpriced: 1,
            ..Totals::default()
        };
        assert_eq!(
            cost(&billed),
            "$7.50 as billed, leaving out usage with no known price"
        );
    }
}
