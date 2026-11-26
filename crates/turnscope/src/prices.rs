//! What a response cost, in the order docs/architecture.md#prices gives.
//!
//! Prices are dollars a million tokens, as models.dev's `api.json` gives
//! them. models.dev lists one cache-write price and none for a web search, so
//! a cache write kept an hour costs twice the input rate and a web search $10
//! a thousand, as Anthropic, the only provider whose usage reports either,
//! charges.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

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

#[derive(Deserialize)]
struct Cost {
    #[serde(flatten)]
    base: Rates,
    #[serde(default)]
    tiers: Vec<Tiered>,
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

struct Model {
    base: Rates,
    tiers: Vec<(u64, Rates)>,
    priority: Option<Rates>,
}

pub struct Prices(HashMap<String, HashMap<String, History<Model>>>);

impl Prices {
    pub fn load(db: &Connection) -> Result<Prices> {
        let mut models: HashMap<String, HashMap<String, History<Model>>> = HashMap::new();
        let mut rows =
            db.prepare("SELECT provider, model, since, cost, fast FROM price ORDER BY since")?;
        let mut rows = rows.query([])?;
        while let Some(row) = rows.next()? {
            let Ok(cost) = serde_json::from_str::<Cost>(row.get_ref(3)?.as_str()?) else {
                continue;
            };
            let mut tiers: Vec<(u64, Rates)> = cost
                .tiers
                .iter()
                .map(|tier| (tier.tier.size, tier.rates))
                .collect();
            tiers.sort_by_key(|(above, _)| *above);
            let priority = row
                .get_ref(4)?
                .as_str_or_null()?
                .and_then(|fast| serde_json::from_str(fast).ok());
            models
                .entry(row.get(0)?)
                .or_default()
                .entry(row.get(1)?)
                .or_default()
                .push((
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

    /// The price of `model` from `provider` in force `at`, or where
    /// `provider` serves a model it doesn't list, its maker's: Antigravity
    /// records the Claude models Google serves it as `google`'s, and
    /// models.dev lists them under Anthropic alone.
    fn at(&self, provider: &str, model: &str, at: i64) -> Option<&Model> {
        let listed = |provider: &str| self.0.get(provider)?.get(model);
        let history = listed(provider).or_else(|| listed(providers::maker(model)?.info().id))?;
        in_force(history, at)
    }
}

pub type History<T> = Vec<(i64, T)>;

pub fn in_force<T>(history: &[(i64, T)], at: i64) -> Option<&T> {
    history
        .iter()
        .rfind(|(since, _)| *since <= at)
        .or(history.first())
        .map(|(_, value)| value)
}

/// Take in models.dev's catalog when it's six hours since any process last
/// did: a price that changed is kept from now, beside the one before it, a
/// model listed for the first time prices what was used of it before, and
/// what had no price is priced again. Whether it was taken in.
pub fn refresh(db: &mut Connection) -> Result<bool> {
    let data = crate::db::dir(db).to_path_buf();
    refresh_from(db, || catalog(&data))
}

fn refresh_from(db: &mut Connection, ask: impl FnOnce() -> Result<Catalog>) -> Result<bool> {
    let now = crate::now();
    if !claim(db, now)? {
        return Ok(false);
    }
    match ask() {
        Ok(catalog) => take(db, &catalog, now).map(|()| true),
        Err(error) => {
            // Asked again in 10 minutes, not 6 hours, so a first launch
            // offline gets prices soon after.
            let tx = crate::db::write(db)?;
            tx.execute(
                "UPDATE state SET prices_at = ?1 WHERE prices_at <= ?2",
                params![now - DUE + 10 * crate::time::MINUTE, now - DUE + CLAIMED],
            )?;
            tx.commit()?;
            Err(error)
        }
    }
}

const DUE: i64 = 6 * crate::time::HOUR;
const CLAIMED: i64 = 2 * crate::time::MINUTE;

/// Claim the refresh for this process: for 2 minutes, until `take` records it
/// for 6 hours, so a process that dies meanwhile holds it up 2 minutes.
fn claim(db: &mut Connection, now: i64) -> Result<bool> {
    crate::db::claim(db, "prices_at", now, DUE, now - DUE + CLAIMED)
}

type Catalog = Vec<((String, String), (String, Option<String>))>;

/// models.dev's catalog.
fn catalog(data: &Path) -> Result<Catalog> {
    let (status, json) = providers::get("https://models.dev/api.json", 60)?;
    let checked = check(status, &json);
    tell(data, checked.as_ref().err().map(ToString::to_string));
    checked
}

/// A catalog refused goes in `app.log` in `data`, which `doctor` shows, as
/// prices then go stale: once while it's refused alike, though it's asked
/// again every 10 minutes.
fn tell(data: &Path, refused: Option<String>) {
    static TOLD: Mutex<Option<String>> = Mutex::new(None);
    let mut told = TOLD.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(why) = refused.as_ref().filter(|&why| told.as_ref() != Some(why)) {
        crate::engine::log(data, why);
    }
    *told = refused;
}

fn check(status: u16, json: &[u8]) -> Result<Catalog> {
    if status != 200 {
        return Err(Error::Failed(format!("models.dev answered {status}")));
    }
    let catalog = parse_catalog(json)?;
    // A catalog cut short would leave most usage unpriced.
    if catalog.len() < 1000 {
        return Err(Error::Failed(format!(
            "models.dev's catalog listed only {} priced models",
            catalog.len()
        )));
    }
    Ok(catalog)
}

/// Priced models from models.dev's JSON. One whose `cost` doesn't read as
/// rates is left out: stored, it would stop `take` pricing what was used of
/// it once its rates are listed.
fn parse_catalog(json: &[u8]) -> Result<Catalog> {
    let providers: HashMap<String, Value> = serde_json::from_slice(json)
        .map_err(|error| Error::Failed(format!("models.dev's catalog didn't read: {error}")))?;
    let mut catalog = Catalog::new();
    for (provider, mut listing) in providers {
        let Some(models) = listing.get_mut("models").and_then(Value::as_object_mut) else {
            continue;
        };
        for (id, model) in std::mem::take(models) {
            let Some(cost) = model
                .get("cost")
                .filter(|cost| Cost::deserialize(*cost).is_ok())
            else {
                continue;
            };
            let fast = model["experimental"]["modes"]
                .as_object()
                .and_then(|modes| {
                    modes.values().find_map(|mode| {
                        let body = &mode["provider"]["body"];
                        let cost = mode
                            .get("cost")
                            .filter(|cost| Rates::deserialize(*cost).is_ok())?;
                        (body["speed"] == "fast" || body["service_tier"] == "priority")
                            .then(|| cost.to_string())
                    })
                });
            catalog.push(((provider.clone(), id), (cost.to_string(), fast)));
        }
    }
    Ok(catalog)
}

/// Write what `catalog` prices, as of `now`, and price again what was used of
/// a model it lists for the first time and what had no price, in the same
/// write, so a process that ends between them leaves nothing unpriced.
///
/// Without `ingest.lock`, as a read of history prices what it writes as it
/// writes it. Holding the lock made an agent's tool call that fell due to
/// refresh the prices wait out the app's first read of history, minutes long.
fn take(db: &mut Connection, catalog: &Catalog, now: i64) -> Result<()> {
    let tx = crate::db::write(db)?;
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
    for (model, price) in catalog {
        match current.get(model) {
            Some(was) if was == price => {}
            was => {
                insert.execute(params![model.0, model.1, now, price.0, price.1])?;
                if was.is_none() {
                    first.push(model.clone());
                }
            }
        }
    }
    drop(insert);
    crate::ingest::price_again(&tx, &first)?;
    tx.execute("UPDATE state SET prices_at = ?1", [now])?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::Tokens;
    use crate::db::test::one;

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

    fn response(provider: &str, model: &str, tokens: Tokens) -> Response {
        Response {
            id: "r".to_owned(),
            session: "s".to_owned(),
            copy: false,
            at: 0,
            provider: provider.to_owned(),
            model: model.to_owned(),
            prompt: tokens.prompt(),
            tokens,
            web_searches: 0,
            priority: false,
            cost: None,
        }
    }

    #[test]
    fn anthropic_hour_cache_writes_cost_twice_input_and_searches_a_cent() {
        let prices = priced(&[(
            "anthropic",
            "claude-opus-5",
            0,
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
        let prices = priced(&[(
            "openai",
            "gpt-6-astra",
            0,
            r#"{"input":10,"output":50,"cache_read":1,"tiers":[{"input":20,"output":75,"cache_read":2,"tier":{"type":"context","size":272000}}]}"#,
            Some(r#"{"input":20,"output":100,"cache_read":2}"#),
        )]);
        let tokens = Tokens {
            input: 100_000,
            cache_read: 200_000,
            output: 1_000,
            reasoning: 400,
            ..Tokens::default()
        };
        let mut used = response("openai", "gpt-6-astra", tokens);
        // 300K of prompt is past 272K: 100,000×20 + 200,000×2 + 1,000×75,
        // the 400 of reasoning among the 1,000 of output.
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
        let prices = priced(&[("xai", "grok-4.6", 0, r#"{"input":2,"output":6}"#, None)]);
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

    /// One process asks models.dev at a time: claimed for two minutes, then
    /// six hours once its catalog is taken in, or again by a clock set back.
    /// One that isn't to ask doesn't wait on another's write to be told so.
    #[test]
    fn models_dev_is_asked_by_one_process_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let now = crate::now();
        assert!(claim(&mut db, now).unwrap());
        // A process that read the time 20 ms earlier, and claims after.
        let mut other = crate::db::open(dir.path()).unwrap();
        let writing = other
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        db.busy_timeout(std::time::Duration::ZERO).unwrap();
        assert!(!claim(&mut db, now - 20).unwrap());
        drop(writing);
        assert!(!claim(&mut db, now + CLAIMED - 1).unwrap());
        assert!(claim(&mut db, now + CLAIMED).unwrap());
        assert!(claim(&mut db, now - crate::time::DAY).unwrap(), "set back");

        db.execute("UPDATE state SET prices_at = NULL", []).unwrap();
        let began = crate::now();
        assert!(refresh_from(&mut db, || Ok(listing("zai", "glm-5.3", Value::Null))).unwrap());
        let asked = one::<i64>(&db, "SELECT prices_at FROM state");
        assert!((began..=crate::now()).contains(&asked));
    }

    /// Asked again in ten minutes when models.dev fails, and left as
    /// claimed once a newer Turnscope has written, as one may while
    /// models.dev is asked.
    #[test]
    fn a_failed_refresh_is_tried_again_in_ten_minutes_unless_a_newer_turnscope_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let other = crate::db::open(dir.path()).unwrap();
        let asked = || one::<i64>(&other, "SELECT prices_at FROM state");
        let out_of_reach = || Err(Error::Failed("models.dev is out of reach".to_owned()));
        let began = crate::now();
        refresh_from(&mut db, out_of_reach).unwrap_err();
        let claimed = asked() + DUE - 10 * crate::time::MINUTE;
        assert!((began..=crate::now()).contains(&claimed));

        for answer in [Ok(listing("zai", "glm-5.3", Value::Null)), out_of_reach()] {
            other
                .execute("UPDATE state SET prices_at = NULL, version = NULL", [])
                .unwrap();
            let began = crate::now();
            let error = refresh_from(&mut db, || {
                other
                    .execute("UPDATE state SET version = ?1", [i64::MAX])
                    .unwrap();
                answer
            })
            .unwrap_err();
            assert_eq!(error.to_string(), crate::db::newer().to_string());
            let claimed = asked() + DUE - CLAIMED;
            assert!((began..=crate::now()).contains(&claimed));
        }
    }

    /// A catalog listing `model` of `provider` at `cost`.
    fn listing(provider: &str, model: &str, cost: Value) -> Catalog {
        vec![(
            (provider.to_owned(), model.to_owned()),
            (cost.to_string(), None),
        )]
    }

    /// An OpenCode response of a model no list price was known for, its cost
    /// the agent's own estimate, then models.dev's first price for it.
    #[test]
    fn a_model_listed_for_the_first_time_prices_an_agents_estimate_too() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        db.execute(
            "INSERT INTO response (agent, id, session, copy, file, folder, at, provider,
                 model, input, cache_read, cache_write_5m, cache_write_1h, output,
                 reasoning, prompt, web_searches, priority, recorded, cost)
             VALUES ('opencode', 'm1', 'opencode:s1', 0, 1, '/work', 0, 'zai',
                 'glm-5.3', 1000000, 0, 0, 0, 0, 0, 1000000, 0, 0, 0.001, 0.001)",
            [],
        )
        .unwrap();
        let listed = listing(
            "zai",
            "glm-5.3",
            serde_json::json!({"input": 2, "output": 8}),
        );
        take(&mut db, &listed, 0).unwrap();
        assert_eq!(one::<f64>(&db, "SELECT cost FROM response"), 2.0);
    }

    /// The app reads all of history, minutes the first time, while an
    /// agent's tool call takes the prices in: it waits a second at most for
    /// the read itself (`mcp::PATIENCE`), so it mustn't wait here.
    #[test]
    fn prices_are_taken_in_without_waiting_for_a_read_of_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let listed = listing("zai", "glm-5.3", serde_json::json!({"input": 2}));
        let reading = crate::db::open(dir.path()).unwrap();
        let held = crate::ingest::lock(&reading).unwrap();
        let began = std::time::Instant::now();
        std::thread::scope(|scope| {
            let taking = scope.spawn(|| take(&mut db, &listed, 0));
            let patience = std::time::Duration::from_secs(2);
            while !taking.is_finished() && began.elapsed() < patience {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let waited = began.elapsed();
            drop(held);
            taking.join().unwrap().unwrap();
            assert!(waited < patience, "waited {waited:?}");
        });
    }

    #[test]
    fn a_price_without_its_input_or_output_isnt_taken() {
        let json = serde_json::json!({
            "anthropic": {
                "models": {
                    "incomplete": {"cost": {}},
                    "partial": {"cost": {"input": 2}},
                    "complete": {
                        "cost": {"input": 3, "output": 15},
                        "experimental": {
                            "modes": {
                                "fast": {
                                    "provider": {"body": {"speed": "fast"}},
                                    "cost": {}
                                }
                            }
                        }
                    }
                }
            }
        });
        let parsed = parse_catalog(json.to_string().as_bytes()).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, ("anthropic".to_owned(), "complete".to_owned()));
        assert_eq!(parsed[0].1.1, None);
    }

    #[test]
    fn a_model_served_by_another_provider_takes_its_makers_price() {
        let prices = priced(&[(
            "anthropic",
            "claude-opus-5-5",
            0,
            r#"{"input":5,"output":25}"#,
            None,
        )]);
        let tokens = Tokens {
            input: 1_000_000,
            ..Tokens::default()
        };
        let cost = |provider: &str, model: &str| {
            prices.cost("antigravity", &response(provider, model, tokens))
        };
        assert_eq!(cost("google", "claude-opus-5-5"), Some(5.0));
        // As Antigravity 2.34.0 names its GPT model: no provider makes it.
        assert_eq!(cost("google", "openai/gpt-oss-120b-maas"), None);
        // Where the provider serving it lists it, its own price.
        let listed = priced(&[
            (
                "anthropic",
                "claude-opus-5-5",
                0,
                r#"{"input":5,"output":25}"#,
                None,
            ),
            (
                "google",
                "claude-opus-5-5",
                0,
                r#"{"input":9,"output":30}"#,
                None,
            ),
        ]);
        assert_eq!(
            listed.cost(
                "antigravity",
                &response("google", "claude-opus-5-5", tokens)
            ),
            Some(9.0)
        );
    }

    #[test]
    fn a_catalog_refused_alike_is_logged_once() {
        let data = tempfile::tempdir().unwrap();
        let refused = || Some("models.dev answered 403".to_owned());
        for told in [refused(), refused(), None, refused()] {
            tell(data.path(), told);
        }
        let log = std::fs::read_to_string(data.path().join("app.log")).unwrap();
        assert_eq!(log.matches("models.dev answered 403").count(), 2, "{log}");
    }
}
