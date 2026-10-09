//! What a response cost: the provider's charge where the agent records it
//! (Grok Build's), otherwise models.dev's list prices for the model, otherwise
//! the agent's own estimate (OpenCode's and Pi's). Unknown is never zero.
//!
//! A response costs what the model's price was when it was made: each price
//! is kept from when it was first read, so a price that changes later leaves
//! what was used before as it was. Usage from before a model's first price
//! was read takes that first price, the nearest known.
//!
//! Prices are dollars a million tokens, as models.dev's `api.json` gives
//! them. Some models cost more once a prompt passes a size (`tiers`, or the
//! older `context_over_200k` where a model lists none); a faster tier is the
//! mode whose request asks for `speed: fast` or `service_tier: priority`.
//! models.dev lists one cache-write price, so a cache write kept an hour costs
//! twice the input rate and a web search $10 a thousand, as Anthropic, the
//! only provider whose usage reports either, charges. Reasoning is output,
//! unless a rate of its own is listed.

use std::collections::HashMap;

use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::Value;

use crate::agents::{self, Response};
use crate::{Error, Result, providers};

#[derive(Deserialize, Clone, Copy)]
struct Rates {
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
    reasoning: Option<f64>,
}

struct Model {
    base: Rates,
    /// Each tier's rates, from the prompt size it starts above, smallest first.
    tiers: Vec<(u64, Rates)>,
    priority: Option<Rates>,
}

/// Each model's prices, by provider and model, from when each was in force,
/// oldest first.
pub struct Prices(HashMap<(String, String), Vec<(i64, Model)>>);

impl Prices {
    pub fn load(db: &Connection) -> Result<Prices> {
        #[derive(Deserialize)]
        struct Cost {
            #[serde(flatten)]
            base: Rates,
            #[serde(default)]
            tiers: Vec<Tiered>,
            context_over_200k: Option<Rates>,
        }
        #[derive(Deserialize)]
        struct Tiered {
            #[serde(flatten)]
            rates: Rates,
            tier: Threshold,
        }
        #[derive(Deserialize)]
        struct Threshold {
            size: u64,
        }
        let mut models: HashMap<(String, String), Vec<(i64, Model)>> = HashMap::new();
        let mut rows =
            db.prepare("SELECT provider, model, since, cost, fast FROM price ORDER BY since")?;
        let mut rows = rows.query([])?;
        while let Some(row) = rows.next()? {
            let Ok(cost) = serde_json::from_str::<Cost>(&row.get::<_, String>(3)?) else {
                continue;
            };
            let mut tiers: Vec<(u64, Rates)> = cost
                .tiers
                .iter()
                .map(|tier| (tier.tier.size, tier.rates))
                .collect();
            if tiers.is_empty()
                && let Some(long) = cost.context_over_200k
            {
                tiers.push((200_000, long));
            }
            tiers.sort_by_key(|(above, _)| *above);
            let priority = row
                .get::<_, Option<String>>(4)?
                .and_then(|fast| serde_json::from_str(&fast).ok());
            models.entry((row.get(0)?, row.get(1)?)).or_default().push((
                row.get(2)?,
                Model {
                    base: cost.base,
                    tiers,
                    priority,
                },
            ));
        }
        Ok(Prices(models))
    }

    /// What `agent`'s `response` cost.
    pub fn cost(&self, agent: &str, response: &Response) -> Option<f64> {
        // A recorded cost of zero is usage a subscription covered: it says
        // nothing of what it would have cost.
        let recorded = response.cost.filter(|cost| *cost > 0.0);
        let charges = agents::by_id(agent).is_some_and(|agent| agent.info().charges);
        if charges && recorded.is_some() {
            return recorded;
        }
        let Some(model) = self.at(&response.provider, &response.model, response.at) else {
            return recorded;
        };
        let rates = match model.priority {
            Some(priority) if response.priority => priority,
            _ => model
                .tiers
                .iter()
                .rev()
                .find(|(above, _)| response.prompt > *above)
                .map_or(model.base, |(_, rates)| *rates),
        };
        let tokens = &response.tokens;
        let dollars = tokens.input as f64 * rates.input
            + tokens.cache_read as f64 * rates.cache_read.unwrap_or(rates.input)
            + tokens.cache_write_5m as f64 * rates.cache_write.unwrap_or(rates.input)
            + tokens.cache_write_1h as f64 * 2.0 * rates.input
            + tokens.output.saturating_sub(tokens.reasoning) as f64 * rates.output
            + tokens.reasoning as f64 * rates.reasoning.unwrap_or(rates.output);
        Some(dollars / 1e6 + response.web_searches as f64 * 0.01)
    }

    /// `model`'s price in force `at`: the latest set by then, or before the
    /// first, the first.
    fn at(&self, provider: &str, model: &str, at: i64) -> Option<&Model> {
        let history = self.0.get(&(provider.to_owned(), model.to_owned()))?;
        history
            .iter()
            .rfind(|(since, _)| *since <= at)
            .or(history.first())
            .map(|(_, model)| model)
    }
}

/// Take in models.dev's catalog when it's six hours since any process last
/// did: a price that changed is kept from now, beside the one before it, and
/// a model listed for the first time prices what was used of it before.
pub fn refresh(db: &mut Connection) -> Result<()> {
    let now = crate::now();
    // Claimed in one statement, so two processes never ask at once.
    let claimed = db.execute(
        "UPDATE state SET prices_at = ?1 WHERE prices_at IS NULL OR prices_at <= ?1 - ?2 OR prices_at > ?1",
        params![now, 6 * crate::time::HOUR],
    )?;
    if claimed == 0 {
        return Ok(());
    }
    let taken = take_in(db, now);
    if taken.is_err() {
        // Asked again in ten minutes, not six hours, as a first launch
        // would otherwise price nothing until then.
        db.execute(
            "UPDATE state SET prices_at = ?1",
            [now - 6 * crate::time::HOUR + 10 * crate::time::MINUTE],
        )?;
    }
    taken
}

/// Take in models.dev's catalog as of `now`.
fn take_in(db: &mut Connection, now: i64) -> Result<()> {
    let (status, json) = providers::get("https://models.dev/api.json", &[])?;
    if status != 200 {
        return Err(Error::Failed(format!("models.dev answered {status}")));
    }
    let catalog: HashMap<String, Value> = serde_json::from_slice(&json)
        .map_err(|error| Error::Failed(format!("models.dev's catalog didn't read: {error}")))?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    // Each model's price now: its latest row.
    let current: HashMap<(String, String), (String, Option<String>)> = tx
        .prepare(
            "SELECT provider, model, cost, fast FROM price p
             WHERE since = (SELECT max(since) FROM price WHERE provider = p.provider AND model = p.model)",
        )?
        .query_map([], |row| {
            Ok(((row.get(0)?, row.get(1)?), (row.get(2)?, row.get(3)?)))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut insert = tx.prepare(
        "INSERT OR REPLACE INTO price (provider, model, since, cost, fast) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    let mut first = Vec::new();
    let mut listed = 0;
    for (provider, listing) in &catalog {
        let Some(models) = listing["models"].as_object() else {
            continue;
        };
        for (id, model) in models {
            let Some(cost) = model.get("cost").filter(|cost| cost.is_object()) else {
                continue;
            };
            let fast = model["experimental"]["modes"]
                .as_object()
                .and_then(|modes| {
                    modes.values().find_map(|mode| {
                        let body = &mode["provider"]["body"];
                        (body["speed"] == "fast" || body["service_tier"] == "priority")
                            .then(|| mode["cost"].to_string())
                    })
                });
            listed += 1;
            let price = (cost.to_string(), fast);
            let key = (provider.clone(), id.clone());
            match current.get(&key) {
                Some(was) if *was == price => {}
                was => {
                    insert.execute(params![key.0, key.1, now, price.0, price.1])?;
                    if was.is_none() {
                        first.push(key);
                    }
                }
            }
        }
    }
    drop(insert);
    // A catalog cut short would leave most usage unpriced.
    if listed < 1000 {
        return Err(Error::Failed(format!(
            "models.dev's catalog listed only {listed} priced models"
        )));
    }
    tx.commit()?;
    crate::ingest::derive(db, crate::ingest::Of::Models(first))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::Tokens;

    /// Prices of `rows`, each in force from `since`.
    fn priced(rows: &[(&str, &str, i64, &str, Option<&str>)]) -> Prices {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).unwrap();
        for (provider, model, since, cost, fast) in rows {
            db.execute(
                "INSERT INTO price VALUES (?1, ?2, ?3, ?4, ?5)",
                params![provider, model, since, cost, fast],
            )
            .unwrap();
        }
        Prices::load(&db).unwrap()
    }

    /// Prices of `rows`, each in force from the start.
    fn prices(rows: &[(&str, &str, &str, Option<&str>)]) -> Prices {
        let rows: Vec<_> = rows
            .iter()
            .map(|&(provider, model, cost, fast)| (provider, model, 0, cost, fast))
            .collect();
        priced(&rows)
    }

    fn response(provider: &str, model: &str, tokens: Tokens) -> Response {
        Response {
            id: "r".to_owned(),
            session: "s".to_owned(),
            copy: false,
            at: 0,
            provider: provider.to_owned(),
            model: model.to_owned(),
            prompt: tokens.input
                + tokens.cache_read
                + tokens.cache_write_5m
                + tokens.cache_write_1h,
            tokens,
            web_searches: 0,
            priority: false,
            cost: None,
        }
    }

    #[test]
    fn anthropic_hour_cache_writes_cost_twice_input_and_searches_a_cent() {
        let prices = prices(&[(
            "anthropic",
            "claude-opus-5",
            r#"{"input":5,"output":25,"cache_read":0.5,"cache_write":6.25}"#,
            None,
        )]);
        let mut used = response(
            "anthropic",
            "claude-opus-5",
            Tokens {
                input: 2,
                cache_read: 30_516,
                cache_write_1h: 6_261,
                output: 87,
                ..Tokens::default()
            },
        );
        // 2×5 + 30,516×0.5 + 6,261×10 + 87×25 millionths of a dollar.
        assert!((prices.cost("claude-code", &used).unwrap() - 0.080_053).abs() < 1e-9);
        used.web_searches = 3;
        assert!((prices.cost("claude-code", &used).unwrap() - 0.110_053).abs() < 1e-9);
    }

    #[test]
    fn long_prompts_take_their_tier_and_priority_its_rates() {
        let prices = prices(&[(
            "openai",
            "gpt-6-astra",
            r#"{"input":10,"output":50,"cache_read":1,"tiers":[{"input":20,"output":75,"cache_read":2,"tier":{"type":"context","size":272000}}]}"#,
            Some(r#"{"input":20,"output":100,"cache_read":2}"#),
        )]);
        let tokens = Tokens {
            input: 100_000,
            cache_read: 200_000,
            output: 1_000,
            ..Tokens::default()
        };
        let mut used = response("openai", "gpt-6-astra", tokens);
        // 300K of prompt is past 272K: 100,000×20 + 200,000×2 + 1,000×75.
        assert!((prices.cost("claude-code", &used).unwrap() - 2.475).abs() < 1e-9);
        used.prompt = 50_000;
        used.priority = true;
        assert!((prices.cost("claude-code", &used).unwrap() - 2.5).abs() < 1e-9);
    }

    #[test]
    fn usage_costs_the_price_in_force_when_it_was_made() {
        // $5 input until hour 10, then $3; before the first price was read,
        // that first price.
        let prices = priced(&[
            (
                "anthropic",
                "claude-opus-5",
                5,
                r#"{"input":5,"output":25}"#,
                None,
            ),
            (
                "anthropic",
                "claude-opus-5",
                10,
                r#"{"input":3,"output":15}"#,
                None,
            ),
        ]);
        let tokens = Tokens {
            input: 1_000_000,
            ..Tokens::default()
        };
        let mut used = response("anthropic", "claude-opus-5", tokens);
        for (at, dollars) in [(0, 5.0), (5, 5.0), (9, 5.0), (10, 3.0), (99, 3.0)] {
            used.at = at;
            assert_eq!(prices.cost("claude-code", &used), Some(dollars), "at {at}");
        }
    }

    #[test]
    fn a_charge_is_the_bill_and_an_estimate_stands_in_for_an_unpriced_model() {
        let prices = prices(&[("xai", "grok-4.6", r#"{"input":2,"output":6}"#, None)]);
        let tokens = Tokens {
            input: 1_000,
            output: 100,
            ..Tokens::default()
        };
        let mut turn = response("xai", "grok-4.6", tokens);
        turn.cost = Some(0.5);
        assert_eq!(prices.cost("grok-build", &turn), Some(0.5));
        // A charge of zero was covered by a plan: the list price stands.
        turn.cost = Some(0.0);
        assert!((prices.cost("grok-build", &turn).unwrap() - 0.0026).abs() < 1e-9);
        let mut unlisted = response("opencode-go", "omen-alpha", tokens);
        assert_eq!(
            prices.cost("opencode", &unlisted),
            None,
            "unknown is not zero"
        );
        unlisted.cost = Some(0.001);
        assert_eq!(prices.cost("opencode", &unlisted), Some(0.001));
    }
}
