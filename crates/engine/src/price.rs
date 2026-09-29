//! What usage cost.
//!
//! A response's cost rests on one of three things, in this order:
//!
//! 1. **What the provider charged**, where the agent records it. Grok Build
//!    records xAI's charge for every turn, and that is the bill.
//! 2. **The catalog's rates** for the model, in the long-context tier its
//!    prompt reached, and at priority rates when it was served at priority.
//! 3. **The agent's own estimate**, for a model the catalog does not price,
//!    where the agent records one (OpenCode and Pi do).
//!
//! Otherwise the cost is unknown, and is shown as unknown, never as zero. A
//! recorded cost of zero is usage a subscription covered, not a price, so it
//! is neither a charge nor an estimate.
//!
//! What a cost rests on travels with it: the cache keeps each response's
//! [`Basis`], and totals say how much of a cost is what providers charged
//! and how much agents' own estimates, the rest being at list prices. Usage
//! outside the conversation that an agent's own totals cost is its estimate
//! too. On 2026-09-27, $7.50 of Grok Build's usage here was charged, and
//! $0.07 of OpenCode's its own estimate, so a total holding them is not all
//! at list prices, and an answer can say so.
//!
//! **Checked against the agents.** Where an agent records its own cost, the
//! rules are checked against it, and doctor lists what disagrees by more
//! than 1%. On 2026-09-26, catalog prices agreed with Claude Code's own
//! costs in all 40 sessions compared, OpenCode's in 2,079 of 2,084
//! responses, and Pi's in all 60; each reader says what the exceptions were.
//!
//! **Over time.** Prices change. Each response is charged at the prices in
//! effect when it was made, from the [`PriceBook`] the ledger keeps; usage
//! from before the catalog first listed a model is charged at the first prices
//! it listed.
//!
//! The catalog cannot say everything. Anthropic charges a cache write kept for
//! an hour at twice the input rate, and a web search at $10 a thousand; both
//! are rules here, since models.dev lists one cache-write price and Claude
//! Code mostly writes hour-long ones. Reasoning is part of the output and is
//! charged at the output rate, unless the catalog lists a rate of its own for
//! it. Priority processing and fast mode are charged at the catalog's
//! priority mode, so no multiplier is kept here. Where the catalog lacks a
//! rate the usage needs, such as a priority rate or a cache read rate, the
//! nearest listed rate stands in and the cost is marked approximate.
//!
//! The rules are versioned by the cache's schema: a change to one raises it,
//! and the cache is built again, every response priced anew. Each rule is
//! tested against a published price.
//!
//! **Context tier.** A provider's long-context rates are set by the size of
//! the prompt, its input however it was cached, measured per response. This
//! is why prices are applied to each response, never to totals.
//!
//! **Several calls.** A response's tier is set by one prompt: its own, or,
//! for usage the agent counts over several model calls together, the largest
//! call's for Claude Code's iterations, the average call's for Grok Build's
//! turns, and the latest request's for growth in an older Codex running
//! total. The counts don't say how much of the usage each call was, so all
//! of it is charged at that call's rates. Where those are a long-context
//! tier's, calls with shorter prompts may have been charged less, and the
//! cost is marked approximate; below every threshold it is exact. Such usage
//! is told by its prompt being less than the prompt its counts add up to.

use std::collections::HashMap;

use crate::agent::Agent;
use crate::catalog::{Prices, Rates};
use crate::time::Instant;
use crate::usage::{Response, Tokens, Usd};

/// What Anthropic charges for a web search, in dollars.
const ANTHROPIC_WEB_SEARCH: f64 = 10.0 / 1_000.0;

/// What a response's cost rests on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Basis {
    /// What the provider charged, as the agent recorded it.
    Charged,
    /// The catalog's rates.
    Catalog,
    /// The agent's own estimate, for a model the catalog does not price.
    Agent,
}

impl Basis {
    /// The basis as the cache stores it.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Basis::Charged => "charged",
            Basis::Catalog => "catalog",
            Basis::Agent => "agent",
        }
    }
}

/// What a response cost, and what that rests on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cost {
    /// The amount.
    pub usd: Usd,
    /// What it rests on.
    pub basis: Basis,
    /// Whether a rate the usage needed was missing, so a nearby one stood in.
    pub approximate: bool,
}

/// Every model's prices over time.
#[derive(Clone, Debug, Default)]
pub(crate) struct PriceBook {
    /// Each model's prices, earliest first, by provider and then model, so
    /// a lookup borrows both rather than making a key of them.
    history: HashMap<String, HashMap<String, Vec<Priced>>>,
}

/// A model's prices from one instant on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Priced {
    /// When these prices took effect, in milliseconds since the epoch;
    /// `i64::MIN` for prices in effect since before anything was recorded.
    pub since: i64,
    /// The prices, or `None` for a model the catalog has listed only without
    /// prices.
    pub prices: Option<Prices>,
}

impl PriceBook {
    /// Record that `provider`'s `model` cost `prices` from `since` on.
    pub(crate) fn insert(
        &mut self,
        provider: &str,
        model: &str,
        since: i64,
        prices: Option<Prices>,
    ) {
        let history = self
            .history
            .entry(provider.to_owned())
            .or_default()
            .entry(model.to_owned())
            .or_default();
        let at = history.partition_point(|priced| priced.since < since);
        match history.get_mut(at) {
            Some(existing) if existing.since == since => existing.prices = prices,
            _ => history.insert(at, Priced { since, prices }),
        }
    }

    /// What `provider` charged for `model` at `at`: the prices in effect then,
    /// or, before the model's first prices took effect or at no time known,
    /// those first prices.
    pub(crate) fn prices_at(
        &self,
        provider: &str,
        model: &str,
        at: Option<Instant>,
    ) -> Option<&Prices> {
        let history = self.history.get(provider)?.get(model)?;
        let after = at.map_or(0, |at| {
            history.partition_point(|priced| priced.since <= at.millis())
        });
        history.get(after.saturating_sub(1))?.prices.as_ref()
    }
}

/// A response, as far as pricing it needs.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Priceable<'a> {
    /// The agent that recorded it.
    pub agent: Agent,
    /// When it was made, which decides the prices it was charged at; `None`
    /// when no time is known, charged at the model's first prices.
    pub at: Option<Instant>,
    /// The provider that served it.
    pub provider: &'a str,
    /// The provider's id for the model.
    pub model: &'a str,
    /// What it used.
    pub tokens: &'a Tokens,
    /// The prompt that sets its long-context tier.
    pub prompt: u64,
    /// Web searches the provider ran for it.
    pub web_searches: u64,
    /// Whether it was served at priority.
    pub priority: bool,
    /// What the agent recorded it cost.
    pub recorded: Option<Usd>,
}

impl<'a> Priceable<'a> {
    /// `response`, as far as pricing it needs.
    pub(crate) fn of(response: &'a Response) -> Priceable<'a> {
        Priceable {
            agent: response.agent,
            at: Some(response.at),
            provider: &response.provider,
            model: &response.model,
            tokens: &response.tokens,
            prompt: response.prompt,
            web_searches: response.web_searches,
            priority: response.priority,
            recorded: response.recorded,
        }
    }
}

/// What `response` cost, or `None` when nothing it rests on is known.
pub(crate) fn cost(response: &Priceable, book: &PriceBook) -> Option<Cost> {
    // A recorded cost of zero is usage a subscription covered, which says
    // nothing of what it would have cost.
    let recorded = response.recorded.filter(|usd| usd.nanos() > 0);
    if response.agent == Agent::Grok
        && let Some(usd) = recorded
    {
        return Some(Cost {
            usd,
            basis: Basis::Charged,
            approximate: false,
        });
    }
    if let Some(prices) = book.prices_at(response.provider, response.model, response.at) {
        let (rates, standing_in) = prices.rates(response.prompt, response.priority);
        let (dollars, approximate) = at_rates(response, rates);
        // A response of several calls, whose prompts together come to more
        // than the one that set its tier, is charged at that call's tier
        // throughout, which calls with shorter prompts may not have reached.
        let calls_apart = response.prompt < response.tokens.prompt()
            && prices.tiered(response.prompt, response.priority);
        if let Some(usd) = Usd::from_dollars(dollars) {
            return Some(Cost {
                usd,
                basis: Basis::Catalog,
                approximate: approximate || standing_in || calls_apart,
            });
        }
    }
    recorded.map(|usd| Cost {
        usd,
        basis: Basis::Agent,
        approximate: false,
    })
}

/// What a response cost at `rates`, in dollars, and whether a rate it needed
/// was missing so another stood in.
fn at_rates(response: &Priceable, rates: &Rates) -> (f64, bool) {
    let tokens = response.tokens;
    let anthropic = response.provider == "anthropic";
    let mut approximate = false;
    let mut or_input = |rate: Option<f64>, count: u64| {
        rate.unwrap_or_else(|| {
            approximate |= count > 0;
            rates.input
        })
    };
    let cache_read = or_input(rates.cache_read, tokens.cache_read);
    let cache_write = or_input(rates.cache_write, tokens.cache_write_5m);
    let cache_write_1h = if anthropic {
        2.0 * rates.input
    } else {
        or_input(rates.cache_write, tokens.cache_write_1h)
    };
    let reasoning = rates.reasoning.unwrap_or(rates.output);
    let per_million = |count: u64, rate: f64| {
        // Counts are at most 2^50, which an f64 holds exactly.
        count as f64 * rate / 1e6
    };
    let mut dollars = per_million(tokens.input, rates.input)
        + per_million(tokens.cache_read, cache_read)
        + per_million(tokens.cache_write_5m, cache_write)
        + per_million(tokens.cache_write_1h, cache_write_1h)
        + per_million(tokens.output.saturating_sub(tokens.reasoning), rates.output)
        + per_million(tokens.reasoning, reasoning);
    if response.web_searches > 0 {
        if anthropic {
            dollars += response.web_searches as f64 * ANTHROPIC_WEB_SEARCH;
        } else {
            approximate = true;
        }
    }
    (dollars, approximate)
}

#[cfg(test)]
mod tests {
    use super::{Basis, PriceBook, Priceable, cost};
    use crate::agent::Agent;
    use crate::catalog::{Catalog, Prices, Rates, Tier};
    use crate::time::Instant;
    use crate::usage::{Tokens, Usd};

    /// Every model in the test catalog, at its prices, in effect since always.
    fn catalog() -> PriceBook {
        let catalog = Catalog::parse(include_bytes!("../data/test-catalog.json")).unwrap();
        let mut book = PriceBook::default();
        for (provider, id, model) in catalog.models() {
            book.insert(provider, id, i64::MIN, model.prices.clone());
        }
        book
    }

    fn response<'a>(
        agent: Agent,
        provider: &'a str,
        model: &'a str,
        tokens: &'a Tokens,
    ) -> Priceable<'a> {
        Priceable {
            agent,
            at: Instant::from_millis(1_789_000_000_000),
            provider,
            model,
            tokens,
            prompt: tokens.prompt(),
            web_searches: 0,
            priority: false,
            recorded: None,
        }
    }

    #[test]
    fn an_anthropic_hour_long_cache_write_costs_twice_the_input_rate() {
        let tokens = Tokens {
            input: 2,
            cache_read: 30_516,
            cache_write_1h: 6_261,
            output: 87,
            ..Tokens::default()
        };
        let priced = cost(
            &response(Agent::ClaudeCode, "anthropic", "claude-opus-5", &tokens),
            &catalog(),
        )
        .unwrap();
        // At $5 input, $0.50 cache read, $10 (2 × $5) an hour's write, $25 output:
        // 2×5 + 30,516×0.5 + 6,261×10 + 87×25 = 80,053 millionths of a dollar.
        assert_eq!(priced.usd, Usd::from_nanos(80_053_000).unwrap());
        assert_eq!((priced.basis, priced.approximate), (Basis::Catalog, false));
    }

    #[test]
    fn an_anthropic_web_search_costs_ten_dollars_a_thousand() {
        let tokens = Tokens {
            input: 1_000,
            output: 100,
            ..Tokens::default()
        };
        let mut searched = response(Agent::ClaudeCode, "anthropic", "claude-opus-5", &tokens);
        searched.web_searches = 3;
        let priced = cost(&searched, &catalog()).unwrap();
        // At $5 input and $25 output, 1,000×5 + 100×25 = 7,500 millionths of
        // a dollar, and three searches at $0.01 each, 30,000 more.
        assert_eq!(
            (priced.usd, priced.approximate),
            (Usd::from_nanos(37_500_000).unwrap(), false)
        );

        // Another provider's searches have no price here: they are left out,
        // and the cost is marked approximate. At $10 input and $50 output,
        // 1,000×10 + 100×50 = 15,000 millionths.
        let mut elsewhere = response(Agent::Codex, "openai", "gpt-6-astra", &tokens);
        elsewhere.web_searches = 3;
        let priced = cost(&elsewhere, &catalog()).unwrap();
        assert_eq!(
            (priced.usd, priced.approximate),
            (Usd::from_nanos(15_000_000).unwrap(), true)
        );
    }

    #[test]
    fn a_reasoning_rate_of_its_own_charges_reasoning_apart_from_output() {
        let rates = |reasoning: Option<f64>| {
            Some(Prices {
                base: Rates {
                    input: 1.0,
                    output: 4.0,
                    cache_read: None,
                    cache_write: None,
                    reasoning,
                },
                tiers: Vec::new(),
                priority: None,
            })
        };
        let mut book = PriceBook::default();
        book.insert("deepthink", "ponder-2", i64::MIN, rates(Some(10.0)));
        book.insert("deepthink", "ponder-1", i64::MIN, rates(None));
        let tokens = Tokens {
            input: 1_000,
            output: 1_000,
            reasoning: 400,
            ..Tokens::default()
        };
        let priced = |model| {
            cost(
                &response(Agent::OpenCode, "deepthink", model, &tokens),
                &book,
            )
            .unwrap()
            .usd
        };
        // 1,000×$1 in, 600×$4 of output besides reasoning, and 400×$10 of
        // reasoning: 1,000 + 2,400 + 4,000 = 7,400 millionths of a dollar.
        assert_eq!(priced("ponder-2"), Usd::from_nanos(7_400_000).unwrap());
        // Without a rate of its own, reasoning is output: 1,000 + 1,000×4.
        assert_eq!(priced("ponder-1"), Usd::from_nanos(5_000_000).unwrap());
    }

    #[test]
    fn a_long_prompt_is_charged_at_its_tier_and_priority_at_priority_rates() {
        let tokens = Tokens {
            input: 100_000,
            cache_read: 200_000,
            output: 1_000,
            reasoning: 400,
            ..Tokens::default()
        };
        let mut long = response(Agent::Codex, "openai", "gpt-6-astra", &tokens);
        // 300K of prompt is past GPT-6 Astra's 272K threshold: $20 input,
        // $2 cache read, $75 output. 100,000×20 + 200,000×2 + 1,000×75.
        assert_eq!(
            cost(&long, &catalog()).unwrap().usd,
            Usd::from_nanos(2_475_000_000).unwrap()
        );

        long.prompt = 50_000;
        long.priority = true;
        // Priority: $20 input, $2 cache read, $100 output, whatever the prompt.
        // 100,000×20 + 200,000×2 + 1,000×100.
        assert_eq!(
            cost(&long, &catalog()).unwrap().usd,
            Usd::from_nanos(2_500_000_000).unwrap()
        );
    }

    #[test]
    fn several_calls_charged_at_a_long_prompts_tier_are_approximate() {
        let tiered = Some(Prices {
            base: Rates {
                input: 5.0,
                output: 25.0,
                cache_read: Some(0.5),
                cache_write: Some(6.25),
                reasoning: None,
            },
            tiers: vec![Tier {
                above: 200_000,
                rates: Rates {
                    input: 10.0,
                    output: 37.5,
                    cache_read: Some(1.0),
                    cache_write: Some(12.5),
                    reasoning: None,
                },
            }],
            priority: None,
        });
        let mut book = PriceBook::default();
        book.insert("anthropic", "claude-long-1", i64::MIN, tiered);
        // Two calls of 150,000 and 250,000 input, 10 output each.
        let tokens = Tokens {
            input: 400_000,
            output: 20,
            ..Tokens::default()
        };
        let mut calls = response(Agent::ClaudeCode, "anthropic", "claude-long-1", &tokens);
        calls.prompt = 250_000;
        let priced = cost(&calls, &book).unwrap();
        // All of it at the long tier: 400,000×10 + 20×37.5 = 4,000,750
        // millionths of a dollar, where the first call alone, at the base
        // rates, would have cost 150,000×5 + 10×25 = 750,250 rather than
        // 150,000×10 + 10×37.5 = 1,500,375.
        assert_eq!(
            (priced.usd, priced.approximate),
            (Usd::from_nanos(4_000_750_000).unwrap(), true)
        );

        // Both below the threshold, at 150,000 and 190,000: the base rates
        // are each call's, 340,000×5 + 20×25 = 1,700,500 millionths.
        let short = Tokens {
            input: 340_000,
            output: 20,
            ..Tokens::default()
        };
        let mut calls = response(Agent::ClaudeCode, "anthropic", "claude-long-1", &short);
        calls.prompt = 190_000;
        let priced = cost(&calls, &book).unwrap();
        assert_eq!(
            (priced.usd, priced.approximate),
            (Usd::from_nanos(1_700_500_000).unwrap(), false)
        );

        // One call of 250,000 is charged at the long tier exactly:
        // 250,000×10 + 10×37.5 = 2,500,375 millionths.
        let one = Tokens {
            input: 250_000,
            output: 10,
            ..Tokens::default()
        };
        let priced = cost(
            &response(Agent::ClaudeCode, "anthropic", "claude-long-1", &one),
            &book,
        )
        .unwrap();
        assert_eq!(
            (priced.usd, priced.approximate),
            (Usd::from_nanos(2_500_375_000).unwrap(), false)
        );
    }

    #[test]
    fn a_charge_is_the_bill_and_an_agents_estimate_stands_in_for_an_unpriced_model() {
        let tokens = Tokens {
            input: 128_456,
            cache_read: 70_272,
            output: 1_740,
            ..Tokens::default()
        };
        let mut turn = response(Agent::Grok, "xai", "grok-4.6-build", &tokens);
        turn.recorded = Usd::from_nanos(51_422_960);
        assert_eq!(cost(&turn, &catalog()).unwrap().basis, Basis::Charged);

        let mut message = response(Agent::OpenCode, "opencode-go", "omen-alpha", &tokens);
        assert_eq!(cost(&message, &catalog()), None, "unknown is not zero");
        message.recorded = Usd::from_nanos(1_000);
        assert_eq!(cost(&message, &catalog()).unwrap().basis, Basis::Agent);
    }

    #[test]
    fn a_recorded_cost_of_zero_says_nothing_of_the_price() {
        // A subscription covered each of these, so the agent recorded $0.
        let tokens = Tokens {
            input: 1_000,
            output: 100,
            ..Tokens::default()
        };
        let zero = Usd::from_nanos(0);
        // The catalog prices neither grok-4.6-build nor omen-alpha.
        let mut turn = response(Agent::Grok, "xai", "grok-4.6-build", &tokens);
        turn.recorded = zero;
        assert_eq!(cost(&turn, &catalog()), None, "unknown is not zero");
        let mut message = response(Agent::OpenCode, "opencode-go", "omen-alpha", &tokens);
        message.recorded = zero;
        assert_eq!(cost(&message, &catalog()), None, "unknown is not zero");

        // It prices grok-4.6 at $2 input and $6 output: 1,000×2 + 100×6 =
        // 2,600 millionths of a dollar.
        let mut listed = response(Agent::Grok, "xai", "grok-4.6", &tokens);
        listed.recorded = zero;
        let priced = cost(&listed, &catalog()).unwrap();
        assert_eq!(
            (priced.usd, priced.basis),
            (Usd::from_nanos(2_600_000).unwrap(), Basis::Catalog)
        );
    }

    #[test]
    fn a_response_is_charged_at_the_prices_in_effect_when_it_was_made() {
        let mut book = catalog();
        let september = Instant::from_date("2026-09-01").unwrap();
        let opus = book
            .prices_at("anthropic", "claude-opus-5", Some(september))
            .unwrap()
            .clone();
        let mut cut = opus.clone();
        cut.base.output = 20.0;
        book.insert("anthropic", "claude-opus-5", september.millis(), Some(cut));

        let tokens = Tokens {
            output: 1_000_000,
            ..Tokens::default()
        };
        let mut response = response(Agent::ClaudeCode, "anthropic", "claude-opus-5", &tokens);
        response.at = Instant::from_date("2026-08-31");
        assert_eq!(
            cost(&response, &book).unwrap().usd,
            Usd::from_dollars(25.0).unwrap()
        );
        response.at = Some(september);
        assert_eq!(
            cost(&response, &book).unwrap().usd,
            Usd::from_dollars(20.0).unwrap()
        );
        // Before the first recorded prices, and at no time known, the first
        // prices stand.
        response.at = Instant::from_date("2020-01-01");
        assert_eq!(
            cost(&response, &book).unwrap().usd,
            Usd::from_dollars(25.0).unwrap()
        );
        response.at = None;
        assert_eq!(
            cost(&response, &book).unwrap().usd,
            Usd::from_dollars(25.0).unwrap()
        );
    }

    #[test]
    fn a_rate_the_catalog_lacks_stands_in_and_marks_the_cost_approximate() {
        let tokens = Tokens {
            input: 1_000,
            cache_read: 1_000,
            output: 10,
            ..Tokens::default()
        };
        let mut plain = response(Agent::Pi, "opencode-go", "glm-5.3", &tokens);
        assert!(!cost(&plain, &catalog()).unwrap().approximate);
        plain.priority = true;
        assert!(
            cost(&plain, &catalog()).unwrap().approximate,
            "no priority rate is listed"
        );

        // GLM-5.3 lists no cache write rate, so its input rate, $1.40, stands
        // in: 1,000×1.40 + 1,000×0.26 read + 2,000×1.40 written + 10×4.40 out
        // = 1,400 + 260 + 2,800 + 44 = 4,504 millionths of a dollar.
        let written = Tokens {
            cache_write_5m: 2_000,
            ..tokens
        };
        let priced = cost(
            &response(Agent::Pi, "opencode-go", "glm-5.3", &written),
            &catalog(),
        )
        .unwrap();
        assert_eq!(
            (priced.usd, priced.approximate),
            (Usd::from_nanos(4_504_000).unwrap(), true)
        );
    }
}
