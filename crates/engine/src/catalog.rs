//! The model catalog: what each provider charges for each model, and what the
//! model is, from [models.dev](https://models.dev).
//!
//! The catalog is read from models.dev's `api.json` and kept in a form of its
//! own, which is what ships inside the app and what later snapshots are stored
//! as: `turnscope catalog` trims `api.json` to the prices, dates and name of
//! each listing, and the build embeds the result, which stands until a newer
//! catalog is fetched from models.dev.
//! `scripts/catalog.sh` refreshes it. Prices are in dollars per million
//! tokens, as models.dev gives them: `input`, `output`, `cache_read`,
//! `cache_write`, and sometimes `reasoning`.
//!
//! On 2026-09-23 `api.json` was 4.9 MB, covering 223 providers and 8,126
//! models. It comes with an `ETag` and `must-revalidate`, so a check when
//! nothing has changed costs one small request.
//!
//! **Names.** Each model's name is kept, to show in place of its id:
//! "Claude Opus 5.5" for `claude-opus-5-5`. Providers that resell a model
//! mostly spell it as its maker does, and a few don't: of the 29 listings of
//! `glm-5.3` on 2026-09-25, 27 said "GLM-5.3", one "GLM5.3" and one
//! "GLM 5.3". So a model is called what most of its listings call it, the
//! first of equal spellings in order, and one the catalog doesn't list, such
//! as `codex-auto-review`, is known by its key.
//!
//! **Long contexts.** Some providers charge more once a prompt passes a size.
//! models.dev lists those rates twice: in `tiers`, each with the size it starts
//! above, 272K for GPT models and 200K for others, and in the older
//! `context_over_200k`, whose name implies 200K even where the provider's
//! threshold is 272K. Tiers are taken when a model lists them;
//! `context_over_200k` stands only for a model that lists no tiers.
//!
//! **Priority.** A provider's faster tier is listed under
//! `experimental.modes`, as the mode whose request body asks for
//! `service_tier: "priority"` (OpenAI) or `speed: "fast"` (Anthropic), with
//! prices of its own: `gpt-6-astra` costs $20 input and $100 output a million
//! at priority, against $10 and $50.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::time::Instant;

/// The most any rate may be, in dollars per million tokens. A catalog listing
/// more is broken, not expensive.
const HIGHEST_RATE: f64 = 10_000.0;

/// The longest name kept, in characters. A longer one is not a name.
const LONGEST_NAME: usize = 120;

/// Rates for one kind of token each, in dollars per million tokens.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Rates {
    /// Input neither read from nor written to the cache.
    pub input: f64,
    /// Output.
    pub output: f64,
    /// Input read from the cache, when the provider charges it apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    /// Input written to the cache, when the provider charges it apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
    /// Reasoning, when the provider charges it apart from output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<f64>,
}

impl Rates {
    /// The rates in `object`, when it lists input and output rates and every
    /// rate is a sensible price.
    fn read(object: &Map<String, Value>) -> Option<Rates> {
        let rate = |field: &str| -> Option<Option<f64>> {
            match object.get(field) {
                None | Some(Value::Null) => Some(None),
                Some(value) => {
                    let rate = value.as_f64()?;
                    (rate.is_finite() && (0.0..=HIGHEST_RATE).contains(&rate)).then_some(Some(rate))
                }
            }
        };
        Some(Rates {
            input: rate("input")??,
            output: rate("output")??,
            cache_read: rate("cache_read")?,
            cache_write: rate("cache_write")?,
            reasoning: rate("reasoning")?,
        })
    }
}

/// Rates that apply once a prompt is longer than `above` tokens.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Tier {
    /// The prompt size, in tokens, past which these rates apply.
    pub above: u64,
    /// The rates.
    pub rates: Rates,
}

/// What a model costs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Prices {
    /// The rates for a prompt of any size, unless a tier applies.
    pub base: Rates,
    /// Rates for long prompts, from the shortest threshold up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<Tier>,
    /// The rates for priority processing, when the provider offers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Rates>,
}

impl Prices {
    /// The rates for a response with a prompt of `prompt` tokens, served at
    /// priority or not. The second value says whether priority rates were
    /// wanted but not listed, so the standard rates stand in for them.
    pub(crate) fn rates(&self, prompt: u64, priority: bool) -> (&Rates, bool) {
        if priority {
            return match &self.priority {
                Some(rates) => (rates, false),
                None => (self.standard(prompt), true),
            };
        }
        (self.standard(prompt), false)
    }

    /// Whether the rates for a response with a prompt of `prompt` tokens,
    /// served at priority or not, are a long-context tier's rather than
    /// those for a prompt of any size.
    pub(crate) fn tiered(&self, prompt: u64, priority: bool) -> bool {
        !(priority && self.priority.is_some()) && self.tiers.iter().any(|tier| prompt > tier.above)
    }

    /// The standard rates for a prompt of `prompt` tokens.
    fn standard(&self, prompt: u64) -> &Rates {
        self.tiers
            .iter()
            .rev()
            .find(|tier| prompt > tier.above)
            .map_or(&self.base, |tier| &tier.rates)
    }
}

/// One model as a provider offers it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Model {
    /// What the provider calls it, such as `Claude Opus 5.5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// When it was released, as a date such as `2026-07-24`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<String>,
    /// When its listing last changed, as a date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    /// What it costs, when the catalog says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prices: Option<Prices>,
}

/// Every model the catalog lists, by provider and then by the provider's own
/// id for it, as of one moment.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    /// When the catalog was read from models.dev, in milliseconds since the
    /// epoch; [`Catalog::bundled_as_of`] reads it alone.
    as_of: i64,
    providers: BTreeMap<String, BTreeMap<String, Model>>,
}

/// The catalog that ships with the app, as `turnscope catalog` wrote it.
const BUNDLED: &[u8] = include_bytes!("../data/catalog.json");

/// Why a catalog could not be read.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// The document is not the catalog's JSON.
    #[error("the catalog is not the expected JSON: {0}")]
    Shape(String),
    /// The document parsed but lists too few priced models to be whole.
    #[error("the catalog lists only {0} priced models")]
    Sparse(usize),
}

/// The fewest priced models a whole catalog lists. models.dev lists thousands;
/// fewer than this is a partial or broken download.
const FEWEST_PRICED: usize = 1_000;

impl Catalog {
    /// The catalog that ships inside the app, as of its build.
    pub(crate) fn bundled() -> Catalog {
        serde_json::from_slice(BUNDLED)
            .expect("the bundled catalog is written by `turnscope catalog`, which only writes catalogs it read")
    }

    /// When the catalog that ships with the app was read from models.dev,
    /// read without the rest of it: a tenth of what reading it takes, which
    /// every opening of the engine would otherwise spend to find that the
    /// ledger's catalog is as new.
    pub(crate) fn bundled_as_of() -> i64 {
        #[derive(Deserialize)]
        struct Dated {
            as_of: i64,
        }
        serde_json::from_slice::<Dated>(BUNDLED)
            .expect("the bundled catalog is written by `turnscope catalog`, which only writes catalogs it read")
            .as_of
    }

    /// Read models.dev's `api.json`.
    ///
    /// A model whose prices are not sensible (missing input or output rates,
    /// or a rate that is negative, not finite, or above ten thousand dollars
    /// a million) is kept without prices, so its usage reads as unpriced
    /// rather than priced wrongly.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::Shape`] when the document is not the catalog's
    /// JSON, and [`CatalogError::Sparse`] when it lists fewer than a thousand
    /// priced models, which only a partial or broken download does.
    pub fn from_models_dev(json: &[u8], as_of: Instant) -> Result<Catalog, CatalogError> {
        let mut catalog = Catalog::parse(json)?;
        catalog.as_of = as_of.millis();
        let priced = catalog.priced();
        if priced < FEWEST_PRICED {
            return Err(CatalogError::Sparse(priced));
        }
        Ok(catalog)
    }

    /// Read models.dev's `api.json` without asking whether it is whole.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::Shape`] when the document is not the catalog's
    /// JSON.
    pub(crate) fn parse(json: &[u8]) -> Result<Catalog, CatalogError> {
        let document: Map<String, Value> =
            serde_json::from_slice(json).map_err(|error| CatalogError::Shape(error.to_string()))?;
        let mut providers = BTreeMap::new();
        for (provider, listing) in document {
            let Some(Value::Object(models)) = listing.get("models") else {
                continue;
            };
            let mut kept = BTreeMap::new();
            for (id, model) in models {
                if let Value::Object(model) = model {
                    kept.insert(id.clone(), read_model(model));
                }
            }
            providers.insert(provider, kept);
        }
        Ok(Catalog {
            as_of: 0,
            providers,
        })
    }

    /// The catalog as though read from models.dev at `as_of`.
    #[cfg(test)]
    pub(crate) fn read_at(mut self, as_of: Instant) -> Catalog {
        self.as_of = as_of.millis();
        self
    }

    /// When the catalog was read from models.dev.
    pub(crate) fn as_of(&self) -> Option<Instant> {
        Instant::from_millis(self.as_of)
    }

    /// Every model, with its provider and id.
    pub(crate) fn models(&self) -> impl Iterator<Item = (&str, &str, &Model)> {
        self.providers.iter().flat_map(|(provider, models)| {
            models
                .iter()
                .map(move |(id, model)| (provider.as_str(), id.as_str(), model))
        })
    }

    /// The catalog as it is stored, and as the app ships it.
    ///
    /// # Errors
    ///
    /// Never, in practice: every value the catalog holds is a string, a whole
    /// number or a finite rate, which JSON writes.
    pub fn to_json(&self) -> serde_json::Result<Vec<u8>> {
        serde_json::to_vec(self)
    }

    /// What models are called, to look up by a model's key
    /// or a provider's id.
    pub(crate) fn names(&self) -> Names {
        let mut spellings: HashMap<String, HashMap<&str, usize>> = HashMap::new();
        for (_, id, model) in self.models() {
            if let Some(name) = &model.name {
                let counts = spellings
                    .entry(crate::model::ModelKey::of(id).as_str().to_owned())
                    .or_default();
                *counts.entry(name).or_default() += 1;
            }
        }
        // The spelling most listings use; of equals, the first in order, so
        // the answer doesn't depend on the order providers are listed in.
        let models = spellings
            .into_iter()
            .filter_map(|(key, counts)| {
                let (name, _) = counts
                    .into_iter()
                    .max_by(|(a, a_count), (b, b_count)| a_count.cmp(b_count).then(b.cmp(a)))?;
                Some((key, name.to_owned()))
            })
            .collect();
        Names { models }
    }

    /// How many models have prices.
    pub fn priced(&self) -> usize {
        self.providers
            .values()
            .flat_map(BTreeMap::values)
            .filter(|model| model.prices.is_some())
            .count()
    }
}

/// What models are called, from a catalog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Names {
    /// By the key a model is counted under.
    models: HashMap<String, String>,
}

impl Names {
    /// What the model counted under `key` is called, when the catalog lists
    /// it.
    pub(crate) fn model(&self, key: &str) -> Option<&str> {
        self.models.get(key).map(String::as_str)
    }
}

/// `text` as a name: trimmed, without control characters, and neither empty
/// nor longer than [`LONGEST_NAME`].
fn name(text: &str) -> Option<String> {
    let text = text.trim();
    let sensible = !text.is_empty()
        && text.chars().count() <= LONGEST_NAME
        && !text.chars().any(char::is_control);
    sensible.then(|| text.to_owned())
}

/// Read one model's listing.
fn read_model(model: &Map<String, Value>) -> Model {
    let text = |field: &str| {
        model
            .get(field)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    Model {
        name: model.get("name").and_then(Value::as_str).and_then(name),
        released: text("release_date"),
        updated: text("last_updated"),
        prices: read_prices(model),
    }
}

/// Read what a model costs, when its listing prices it sensibly.
fn read_prices(model: &Map<String, Value>) -> Option<Prices> {
    let Some(Value::Object(cost)) = model.get("cost") else {
        return None;
    };
    let base = Rates::read(cost)?;
    let mut tiers = Vec::new();
    if let Some(Value::Array(listed)) = cost.get("tiers") {
        for tier in listed {
            let Value::Object(tier) = tier else {
                return None;
            };
            let threshold = tier.get("tier")?;
            if threshold.get("type").and_then(Value::as_str) != Some("context") {
                return None;
            }
            tiers.push(Tier {
                above: threshold.get("size").and_then(Value::as_u64)?,
                rates: Rates::read(tier)?,
            });
        }
    } else if let Some(Value::Object(long)) = cost.get("context_over_200k") {
        tiers.push(Tier {
            above: 200_000,
            rates: Rates::read(long)?,
        });
    }
    tiers.sort_by_key(|tier| tier.above);

    let priority = model
        .get("experimental")
        .and_then(|experimental| experimental.get("modes"))
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(Map::values)
        .find(|mode| {
            let body = mode
                .get("provider")
                .and_then(|provider| provider.get("body"));
            let asks = |field: &str, value: &str| {
                body.and_then(|body| body.get(field))
                    .and_then(Value::as_str)
                    == Some(value)
            };
            asks("service_tier", "priority") || asks("speed", "fast")
        })
        .and_then(|mode| mode.get("cost"))
        .and_then(Value::as_object)
        .and_then(Rates::read);
    Some(Prices {
        base,
        tiers,
        priority,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Catalog, CatalogError, Rates, read_prices};
    use crate::model::ModelKey;
    use crate::time::Instant;

    fn listing(cost: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        json!({"id": "gpt-6-astra", "name": "GPT-6 Astra", "cost": cost})
            .as_object()
            .unwrap()
            .clone()
    }

    #[test]
    fn tiers_are_taken_over_the_older_long_context_field_which_stands_without_them() {
        let prices = read_prices(&listing(json!({
            "input": 10, "output": 50, "cache_read": 1, "cache_write": 12.5,
            "tiers": [{"input": 20, "output": 75, "cache_read": 2, "cache_write": 25,
                       "tier": {"type": "context", "size": 272_000}}],
            "context_over_200k": {"input": 20, "output": 75, "cache_read": 2, "cache_write": 25}
        })))
        .unwrap();
        assert_eq!(prices.tiers.len(), 1);
        assert_eq!(prices.tiers[0].above, 272_000);
        // A 250K prompt is under GPT's 272K threshold, though over 200K.
        assert_eq!(prices.rates(250_000, false).0.input, 10.0);
        assert_eq!(prices.rates(272_001, false).0.input, 20.0);

        let older = read_prices(&listing(json!({
            "input": 2, "output": 6, "cache_read": 0.5,
            "context_over_200k": {"input": 4, "output": 12, "cache_read": 1}
        })))
        .unwrap();
        assert_eq!(older.rates(200_000, false).0.input, 2.0);
        assert_eq!(older.rates(200_001, false).0.input, 4.0);
    }

    #[test]
    fn priority_rates_come_from_the_mode_that_asks_for_priority() {
        let mut model = listing(json!({"input": 10, "output": 50, "cache_read": 1}));
        model.insert(
            "experimental".into(),
            json!({"modes": {
                "pro": {"provider": {"body": {"reasoning": {"mode": "pro"}}}},
                "fast": {"cost": {"input": 20, "output": 100, "cache_read": 2, "cache_write": 25},
                         "provider": {"body": {"service_tier": "priority"}}}
            }}),
        );
        let prices = read_prices(&model).unwrap();
        let (rates, standing_in) = prices.rates(10_000, true);
        assert_eq!(
            (rates.input, rates.output, standing_in),
            (20.0, 100.0, false)
        );

        let plain = read_prices(&listing(json!({"input": 10, "output": 50}))).unwrap();
        let (rates, standing_in) = plain.rates(10_000, true);
        assert_eq!((rates.input, standing_in), (10.0, true));
    }

    #[test]
    fn a_model_priced_nonsensically_is_kept_unpriced() {
        assert_eq!(
            read_prices(&listing(json!({"input": -1, "output": 5}))),
            None
        );
        assert_eq!(
            read_prices(&listing(json!({"input": 1e9, "output": 5}))),
            None
        );
        assert_eq!(read_prices(&listing(json!({"output": 5}))), None);
        assert_eq!(
            read_prices(&listing(json!({"input": 1, "output": 5})))
                .unwrap()
                .base,
            Rates {
                input: 1.0,
                output: 5.0,
                cache_read: None,
                cache_write: None,
                reasoning: None
            }
        );
    }

    #[test]
    fn a_partial_download_is_refused() {
        let partial = json!({"openai": {"models": {"gpt-6-astra": {"name": "GPT-6 Astra",
                                                                  "cost": {"input": 10, "output": 50}}}}});
        match Catalog::from_models_dev(partial.to_string().as_bytes(), Instant::now()) {
            Err(CatalogError::Sparse(1)) => {}
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_model_is_called_what_most_of_its_listings_call_it() {
        // Three listings of gpt-6-astra, two spelling it "GPT-6 Astra" and
        // one "GPT 6 Astra"; OpenRouter's id carries its maker's path, which
        // the key drops. Claude Opus 5.5's name is kept trimmed; a name of
        // 121 characters and one with a line break are not names.
        let catalog = Catalog::parse(
            json!({
                "openai": {"name": "OpenAI", "models": {
                    "gpt-6-astra": {"name": "GPT-6 Astra"}}},
                "resold": {"name": "Resold", "models": {
                    "gpt-6-astra": {"name": "GPT 6 Astra"},
                    "long": {"name": "x".repeat(121)},
                    "broken": {"name": "Broken\nModel"}}},
                "openrouter": {"name": "OpenRouter", "models": {
                    "openai/gpt-6-astra": {"name": "GPT-6 Astra"}}},
                "anthropic": {"name": "Anthropic", "models": {
                    "claude-opus-5-5": {"name": " Claude Opus 5.5 "}}}
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        let names = catalog.names();
        assert_eq!(
            names.model(ModelKey::of("openai/gpt-6-astra").as_str()),
            Some("GPT-6 Astra")
        );
        assert_eq!(names.model("claude-opus-5-5"), Some("Claude Opus 5.5"));
        assert_eq!(names.model("long"), None);
        assert_eq!(names.model("broken"), None);
        assert_eq!(names.model("claude-opus-5"), None);
    }

    #[test]
    fn equal_spellings_are_settled_the_same_whatever_the_order() {
        // One listing each of two spellings: the first in order wins, however
        // the providers are listed.
        let listed = |first: &str, second: &str| {
            Catalog::parse(
                json!({
                    first: {"models": {"m": {"name": "Beta"}}},
                    second: {"models": {"m": {"name": "Alpha"}}}
                })
                .to_string()
                .as_bytes(),
            )
            .unwrap()
            .names()
        };
        assert_eq!(listed("a", "b").model("m"), Some("Alpha"));
        assert_eq!(listed("b", "a").model("m"), Some("Alpha"));
    }

    #[test]
    fn the_bundled_catalog_prices_the_models_the_agents_use() {
        let catalog = Catalog::bundled();
        assert!(catalog.priced() >= 1_000);
        for (provider, model) in [
            ("anthropic", "claude-opus-5"),
            ("openai", "gpt-6-astra"),
            ("opencode-go", "glm-5.3"),
        ] {
            assert!(
                catalog.models().any(|(listed, id, listing)| {
                    (listed, id) == (provider, model) && listing.prices.is_some()
                }),
                "{provider} {model}"
            );
        }
    }
}
