//! Usage outside the conversation: what an agent's own totals for a session
//! count beyond what its transcripts show.
//!
//! Claude Code's `cost-state` and OpenCode's session totals count usage the
//! transcripts never recorded, such as a title written by another model.
//! Doctor shows the two side by side, and the cache keeps the difference as
//! usage of kind `outside`; both take the comparison from here, so they
//! cannot disagree.
//!
//! Totals are compared model by model, each under its [`ModelKey`], so the
//! `claude-opus-5[1m]` that `cost-state` names apart is counted with the
//! `claude-opus-5` the transcripts record. The difference is taken count by
//! count, and a count the transcripts show more of than the agent does, as
//! they can while a session runs, adds nothing and takes nothing away.

use std::collections::BTreeMap;

use crate::agent::{Agent, ReportScope, SessionReport};
use crate::model::ModelKey;
use crate::session::{SessionKey, Tree};
use crate::time::Instant;
use crate::usage::{Tokens, Usd};

/// Token counts as an agent's own totals give them, with cache writes as one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Input neither read from nor written to the cache.
    pub input: u64,
    /// Input read from the cache.
    pub cache_read: u64,
    /// Input written to the cache.
    pub cache_write: u64,
    /// Output, reasoning included.
    pub output: u64,
}

impl Counts {
    /// The counts of `tokens`.
    pub(crate) fn of(tokens: &Tokens) -> Counts {
        Counts {
            input: tokens.input,
            cache_read: tokens.cache_read,
            cache_write: tokens.cache_write(),
            output: tokens.output,
        }
    }

    /// Add `other` to these counts.
    pub fn add(&mut self, other: Counts) {
        self.input = self.input.saturating_add(other.input);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.output = self.output.saturating_add(other.output);
    }
}

/// What the transcripts of some sessions show of one model, or of them all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shown {
    /// What the responses used.
    pub tokens: Tokens,
    /// Their tokens, by the quarter hour they were used in.
    pub quarters: BTreeMap<Instant, u64>,
    /// The provider and the model's id as recorded, the least of those
    /// recorded so that it doesn't depend on the order responses come in.
    pub served: Option<(String, String)>,
    /// What the agent recorded the responses cost, when it recorded what
    /// each cost.
    pub recorded: Option<Usd>,
}

impl Default for Shown {
    fn default() -> Shown {
        Shown {
            tokens: Tokens::default(),
            quarters: BTreeMap::new(),
            served: None,
            recorded: Some(Usd::default()),
        }
    }
}

impl Shown {
    /// Add a response the transcripts show.
    fn add(&mut self, served: (&str, &str), at: Instant, tokens: &Tokens, recorded: Option<Usd>) {
        self.tokens.add(tokens);
        let quarter = self.quarters.entry(at.quarter()).or_default();
        *quarter = quarter.saturating_add(tokens.total());
        if self
            .served
            .as_ref()
            .is_none_or(|(provider, model)| served < (provider.as_str(), model.as_str()))
        {
            self.served = Some((served.0.to_owned(), served.1.to_owned()));
        }
        self.recorded = self
            .recorded
            .zip(recorded)
            .and_then(|(sum, cost)| sum.checked_add(cost));
    }
}

/// What the transcripts of some sessions show, model by model.
#[derive(Debug, Default)]
pub(crate) struct Transcripts {
    models: BTreeMap<ModelKey, Shown>,
    all: Shown,
}

impl Transcripts {
    /// Add a response of `model`, as recorded, which `provider` served at
    /// `at`, using `tokens`, and which the agent recorded cost `recorded`.
    pub(crate) fn add(
        &mut self,
        provider: &str,
        model: &str,
        at: Instant,
        tokens: &Tokens,
        recorded: Option<Usd>,
    ) {
        self.models.entry(ModelKey::of(model)).or_default().add(
            (provider, model),
            at,
            tokens,
            recorded,
        );
        self.all.add((provider, model), at, tokens, recorded);
    }
}

/// An agent's own totals for one model, or for every model together, beside
/// what the transcripts show of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Compared {
    /// The model; `None` for totals of every model together.
    pub model: Option<ModelKey>,
    /// What the agent counted.
    pub reported: Counts,
    /// What the agent says it cost. `None` when it doesn't say for every
    /// total, or says nothing was charged: a recorded cost of nothing means
    /// a subscription covered the usage, and says nothing of its price.
    pub cost: Option<Usd>,
    /// What the transcripts show.
    pub shown: Shown,
}

/// The sessions whose transcripts `report` counts: its session's whole tree,
/// subagents and all, or that session alone, as its scope says.
pub(crate) fn covered(report: &SessionReport, tree: &Tree) -> Vec<SessionKey> {
    if report.scope == ReportScope::Tree {
        tree.below(&report.session)
    } else {
        vec![report.session.clone()]
    }
}

/// `report` beside `transcripts`, the transcripts of the sessions it covers.
///
/// Totals for every model together are compared with everything the
/// transcripts show, and any totals by model beside them are passed over, as
/// they would count the same usage twice. Otherwise each model either names
/// is compared, in order of their keys, the totals the agent gives for one
/// model under several names added together.
pub(crate) fn compare(report: &SessionReport, transcripts: &Transcripts) -> Vec<Compared> {
    let mut totals: BTreeMap<Option<ModelKey>, (Counts, Option<Usd>)> = BTreeMap::new();
    for total in &report.models {
        let (counts, cost) = totals
            .entry(total.model.as_deref().map(ModelKey::of))
            .or_insert((Counts::default(), Some(Usd::default())));
        counts.add(Counts {
            input: total.input,
            cache_read: total.cache_read,
            cache_write: total.cache_write,
            output: total.output,
        });
        let charged = total.cost.filter(|cost| cost.nanos() > 0);
        *cost = cost
            .zip(charged)
            .and_then(|(sum, cost)| sum.checked_add(cost));
    }
    if let Some((reported, cost)) = totals.remove(&None) {
        return vec![Compared {
            model: None,
            reported,
            cost,
            shown: transcripts.all.clone(),
        }];
    }
    let mut models: Vec<&ModelKey> = totals
        .keys()
        .flatten()
        .chain(transcripts.models.keys())
        .collect();
    models.sort();
    models.dedup();
    models
        .into_iter()
        .map(|model| {
            let (reported, cost) = totals
                .get(&Some(model.clone()))
                .copied()
                .unwrap_or((Counts::default(), None));
            Compared {
                model: Some(model.clone()),
                reported,
                cost,
                shown: transcripts.models.get(model).cloned().unwrap_or_default(),
            }
        })
        .collect()
}

impl Compared {
    /// What the agent counted beyond what the transcripts show, count by
    /// count. Cache writes beyond them are kept for an hour in the share the
    /// transcripts' own are, or for five minutes where they show none.
    pub(crate) fn beyond(&self) -> Tokens {
        let shown = &self.shown.tokens;
        let cache_write = self
            .reported
            .cache_write
            .saturating_sub(shown.cache_write());
        let hour = if shown.cache_write() == 0 {
            0
        } else {
            apportion(cache_write, &[shown.cache_write_1h, shown.cache_write_5m])[0]
        };
        Tokens {
            input: self.reported.input.saturating_sub(shown.input),
            cache_read: self.reported.cache_read.saturating_sub(shown.cache_read),
            cache_write_5m: cache_write - hour,
            cache_write_1h: hour,
            output: self.reported.output.saturating_sub(shown.output),
            reasoning: 0,
        }
    }
}

/// A share of the usage outside the conversation, in one quarter hour, or at
/// no time known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Part {
    /// The model it is counted under; `None` for totals of every model.
    pub key: Option<ModelKey>,
    /// The provider it is priced as served by.
    pub provider: String,
    /// The model's id it is priced as.
    pub model: String,
    /// The start of the quarter hour; `None` when no time is known at all,
    /// so it counts in all time and in no stretch of it.
    pub quarter: Option<Instant>,
    /// What it used.
    pub tokens: Tokens,
    /// What the agent's own totals say it cost, where they say: for totals
    /// of every model together, what the agent says the session cost less
    /// what it says the transcripts cost, when that is more than nothing.
    /// Otherwise `None`, to be priced from the catalog.
    pub cost: Option<Usd>,
}

/// The usage outside the conversation that `report` shows beside
/// `transcripts`, the transcripts of the sessions it covers, in shares.
///
/// Each model's is spread over the quarter hours the transcripts show it
/// used in, in proportion to its use in each; or, where they show none of
/// it, over those any model was used in; or, where they show nothing, put in
/// the quarter hour of `when`, and with no time known at all, at none, so
/// that it is still counted.
pub(crate) fn outside(
    report: &SessionReport,
    transcripts: &Transcripts,
    when: Option<Instant>,
) -> Vec<Part> {
    let mut parts = Vec::new();
    for compared in compare(report, transcripts) {
        let beyond = compared.beyond();
        if beyond.total() == 0 {
            continue;
        }
        let quarters: Vec<(Option<Instant>, u64)> =
            [&compared.shown.quarters, &transcripts.all.quarters]
                .into_iter()
                .find(|quarters| !quarters.is_empty())
                .map_or_else(
                    || vec![(when.map(Instant::quarter), 1)],
                    |quarters| {
                        quarters
                            .iter()
                            .map(|(at, tokens)| (Some(*at), *tokens))
                            .collect()
                    },
                );
        let weights: Vec<u64> = quarters.iter().map(|(_, weight)| *weight).collect();
        let split = |count: u64| apportion(count, &weights);
        let (input, cache_read, five, hour, output) = (
            split(beyond.input),
            split(beyond.cache_read),
            split(beyond.cache_write_5m),
            split(beyond.cache_write_1h),
            split(beyond.output),
        );
        let shares: Vec<Tokens> = (0..quarters.len())
            .map(|index| Tokens {
                input: input[index],
                cache_read: cache_read[index],
                cache_write_5m: five[index],
                cache_write_1h: hour[index],
                output: output[index],
                reasoning: 0,
            })
            .collect();
        // A total for every model together is priced as the agent priced it,
        // shared out as its tokens are, so that no share without tokens
        // carries any of it.
        let agent_cost = match (&compared.model, compared.cost, compared.shown.recorded) {
            (None, Some(total), Some(recorded)) => u64::try_from(total.nanos() - recorded.nanos())
                .ok()
                .filter(|nanos| *nanos > 0),
            _ => None,
        };
        let costs = agent_cost.map(|nanos| {
            let totals: Vec<u64> = shares.iter().map(Tokens::total).collect();
            apportion(nanos, &totals)
        });
        // A model's is priced as the transcripts recorded it; totals of every
        // model together name none.
        let (provider, model) = match (&compared.model, &compared.shown.served) {
            (Some(_), Some(served)) => served.clone(),
            (key, _) => (
                default_provider(report.session.agent()).to_owned(),
                key.as_ref()
                    .map_or_else(String::new, |key| key.as_str().to_owned()),
            ),
        };
        for (index, ((at, _), tokens)) in quarters.into_iter().zip(shares).enumerate() {
            if tokens.total() == 0 {
                continue;
            }
            parts.push(Part {
                key: compared.model.clone(),
                provider: provider.clone(),
                model: model.clone(),
                quarter: at,
                tokens,
                cost: costs
                    .as_ref()
                    .and_then(|costs| i64::try_from(costs[index]).ok())
                    .and_then(Usd::from_nanos),
            });
        }
    }
    parts
}

/// The provider an agent's usage is counted under when nothing else says.
fn default_provider(agent: Agent) -> &'static str {
    match agent {
        Agent::ClaudeCode => "anthropic",
        Agent::Codex => "openai",
        Agent::Grok => "xai",
        Agent::OpenCode | Agent::Pi => "",
    }
}

/// `total` split in proportion to `weights`, in whole parts that add up to
/// `total` exactly: each part rounded down, and what that leaves given one at
/// a time to the parts that lost the most. With no weight at all, the first
/// part is the whole.
fn apportion(total: u64, weights: &[u64]) -> Vec<u64> {
    let sum: u128 = weights.iter().map(|weight| u128::from(*weight)).sum();
    if weights.is_empty() {
        return Vec::new();
    }
    if sum == 0 {
        let mut parts = vec![0; weights.len()];
        parts[0] = total;
        return parts;
    }
    let mut parts: Vec<u64> = Vec::with_capacity(weights.len());
    let mut remainders: Vec<(u128, usize)> = Vec::with_capacity(weights.len());
    for (index, weight) in weights.iter().enumerate() {
        let exact = u128::from(total) * u128::from(*weight);
        // At most `total`, since `weight` is at most `sum`.
        parts.push(u64::try_from(exact / sum).unwrap_or(total));
        remainders.push((exact % sum, index));
    }
    let given: u64 = parts.iter().sum();
    remainders.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (_, index) in remainders
        .into_iter()
        .take(usize::try_from(total - given).unwrap_or(0))
    {
        parts[index] += 1;
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::{Counts, Part, Transcripts, apportion, compare, outside};
    use crate::agent::{Agent, ModelTotal, ReportScope, SessionReport};
    use crate::model::ModelKey;
    use crate::session::SessionKey;
    use crate::time::Instant;
    use crate::usage::{Tokens, Usd};

    fn at(text: &str) -> Instant {
        Instant::parse(text).unwrap()
    }

    fn total(model: Option<&str>, counts: [u64; 4], cost: Option<i64>) -> ModelTotal {
        ModelTotal {
            model: model.map(str::to_owned),
            input: counts[0],
            cache_read: counts[1],
            cache_write: counts[2],
            output: counts[3],
            web_searches: 0,
            cost: cost.and_then(Usd::from_nanos),
        }
    }

    fn report(agent: Agent, models: Vec<ModelTotal>) -> SessionReport {
        SessionReport {
            session: SessionKey::new(agent, "s"),
            scope: ReportScope::Session,
            at: None,
            models,
        }
    }

    fn tokens(input: u64, cache_read: u64, five: u64, hour: u64, output: u64) -> Tokens {
        Tokens {
            input,
            cache_read,
            cache_write_5m: five,
            cache_write_1h: hour,
            output,
            reasoning: 0,
        }
    }

    #[test]
    fn totals_of_one_model_under_two_names_are_compared_together() {
        // Claude Code's cost-state names the one-million-token context apart:
        // 100 + 200 = 300 input, 1,000 + 3,000 = 4,000 cache reads and
        // 50 + 70 = 120 output of claude-opus-5 in all.
        let report = report(
            Agent::ClaudeCode,
            vec![
                total(Some("claude-opus-5"), [100, 1_000, 0, 50], None),
                total(Some("claude-opus-5[1m]"), [200, 3_000, 0, 70], None),
            ],
        );
        let mut transcripts = Transcripts::default();
        let noon = at("2026-09-14T12:00:00Z");
        let shown = tokens(250, 3_500, 0, 0, 100);
        transcripts.add("anthropic", "claude-opus-5", noon, &shown, None);

        let compared = compare(&report, &transcripts);
        assert_eq!(compared.len(), 1);
        assert_eq!(compared[0].model, Some(ModelKey::of("claude-opus-5")));
        assert_eq!(
            compared[0].reported,
            Counts {
                input: 300,
                cache_read: 4_000,
                cache_write: 0,
                output: 120
            }
        );
        // Beyond the transcripts: 300 - 250 = 50 input, 4,000 - 3,500 = 500
        // cache reads and 120 - 100 = 20 output, in one part.
        let parts = outside(&report, &transcripts, Some(noon));
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].tokens, tokens(50, 500, 0, 0, 20));
    }

    #[test]
    fn cache_writes_beyond_the_transcripts_keep_their_share_kept_for_an_hour() {
        let noon = at("2026-09-14T12:00:00Z");
        let mut transcripts = Transcripts::default();
        // 300 of the 400 cache writes shown were kept for an hour.
        transcripts.add(
            "anthropic",
            "claude-opus-5",
            noon,
            &tokens(0, 0, 100, 300, 0),
            None,
        );
        let written = |cache_write: u64| {
            let report = report(
                Agent::ClaudeCode,
                vec![total(Some("claude-opus-5"), [0, 0, cache_write, 0], None)],
            );
            compare(&report, &transcripts)[0].beyond()
        };
        // 1,400 - 400 = 1,000 beyond: 1,000 × 300 / 400 = 750 kept for an
        // hour, and the other 250 for five minutes.
        assert_eq!(written(1_400), tokens(0, 0, 250, 750, 0));
        // Fewer than shown is nothing beyond, not a subtraction.
        assert_eq!(written(300), Tokens::default());

        // Where the transcripts show no cache writes, those beyond them are
        // taken as kept for five minutes.
        let report = report(
            Agent::ClaudeCode,
            vec![total(Some("claude-haiku-4-5"), [0, 0, 1_000, 0], None)],
        );
        let haiku = compare(&report, &transcripts);
        assert_eq!(haiku[0].beyond(), tokens(0, 0, 1_000, 0, 0));
    }

    #[test]
    fn usage_beyond_the_transcripts_is_spread_over_the_quarter_hours_they_show() {
        let (first, second) = (at("2026-09-14T12:05:00Z"), at("2026-09-14T12:40:00Z"));
        let mut transcripts = Transcripts::default();
        // 100 tokens of claude-opus-5 in the quarter hour from 12:00, and 300
        // in the one from 12:30.
        transcripts.add(
            "anthropic",
            "claude-opus-5",
            first,
            &tokens(100, 0, 0, 0, 0),
            None,
        );
        transcripts.add(
            "anthropic",
            "claude-opus-5",
            second,
            &tokens(300, 0, 0, 0, 0),
            None,
        );
        let report = report(
            Agent::ClaudeCode,
            vec![
                total(Some("claude-opus-5"), [1_400, 0, 0, 0], None),
                total(Some("claude-haiku-4-5"), [0, 0, 0, 8], None),
            ],
        );
        let parts: Vec<(String, Option<Instant>, u64)> =
            outside(&report, &transcripts, Some(first))
                .into_iter()
                .map(|part| (part.model, part.quarter, part.tokens.total()))
                .collect();
        // Opus's 1,400 - 400 = 1,000 input beyond, 1:3 as its use: 250 and
        // 750. Haiku, which the transcripts never show, spreads its 8 output
        // over every model's quarter hours alike: 2 and 6.
        let (twelve, half_past) = (
            Some(at("2026-09-14T12:00:00Z")),
            Some(at("2026-09-14T12:30:00Z")),
        );
        let haiku = "claude-haiku-4-5".to_owned();
        let opus = "claude-opus-5".to_owned();
        assert_eq!(
            parts,
            [
                (haiku.clone(), twelve, 2),
                (haiku, half_past, 6),
                (opus.clone(), twelve, 250),
                (opus, half_past, 750),
            ]
        );

        // With no transcripts at all, it is put in the quarter hour given,
        // and with no time given, at none, still counted: Opus's 1,400 and
        // Haiku's 8.
        let nothing = outside(&report, &Transcripts::default(), Some(second));
        assert_eq!(
            nothing.iter().map(|part| part.quarter).collect::<Vec<_>>(),
            [half_past, half_past]
        );
        let timeless: Vec<(Option<Instant>, u64)> = outside(&report, &Transcripts::default(), None)
            .into_iter()
            .map(|part| (part.quarter, part.tokens.total()))
            .collect();
        assert_eq!(timeless, [(None, 8), (None, 1_400)]);
    }

    #[test]
    fn totals_of_every_model_are_priced_as_the_agent_priced_them() {
        let (first, second) = (at("2026-09-14T12:05:00Z"), at("2026-09-14T12:40:00Z"));
        let mut transcripts = Transcripts::default();
        // Two responses OpenCode says cost $0.40 and $0.60, of 100 and 300
        // tokens.
        transcripts.add(
            "opencode-go",
            "glm-5.3",
            first,
            &tokens(100, 0, 0, 0, 0),
            Usd::from_nanos(400_000_000),
        );
        transcripts.add(
            "opencode-go",
            "glm-5.3",
            second,
            &tokens(300, 0, 0, 0, 0),
            Usd::from_nanos(600_000_000),
        );
        let session = |cost: i64| {
            report(
                Agent::OpenCode,
                vec![total(None, [800, 0, 0, 0], Some(cost))],
            )
        };
        // The session cost $1.50, so the 800 - 400 = 400 input beyond cost
        // $1.50 - $1.00 = $0.50, spread 1:3 as the tokens are: 100 input for
        // $0.125 and 300 for $0.375.
        let parts = outside(&session(1_500_000_000), &transcripts, Some(first));
        let priced: Vec<(u64, Option<Usd>)> = parts
            .iter()
            .map(|part| (part.tokens.input, part.cost))
            .collect();
        assert_eq!(
            priced,
            [
                (100, Usd::from_nanos(125_000_000)),
                (300, Usd::from_nanos(375_000_000))
            ]
        );
        // Which model the tokens went through, the totals don't say.
        assert!(
            parts
                .iter()
                .all(|part| part.key.is_none() && part.model.is_empty())
        );

        // A session that cost nothing was covered by a subscription, and
        // says nothing of what the tokens beyond the transcripts cost; nor
        // does one that cost what its transcripts did.
        let unpriced = |parts: Vec<Part>| parts.iter().all(|part| part.cost.is_none());
        assert_eq!(compare(&session(0), &transcripts)[0].cost, None);
        assert!(unpriced(outside(&session(0), &transcripts, Some(first))));
        assert!(unpriced(outside(
            &session(1_000_000_000),
            &transcripts,
            Some(first)
        )));
        // Nor is it known when a transcript's response has no cost recorded.
        transcripts.add(
            "opencode-go",
            "glm-5.3",
            second,
            &tokens(0, 0, 0, 0, 1),
            None,
        );
        assert!(unpriced(outside(
            &session(1_500_000_000),
            &transcripts,
            Some(first)
        )));
    }

    #[test]
    fn apportioning_adds_up_exactly_and_follows_the_weights() {
        assert_eq!(apportion(10, &[1, 1, 1]), [4, 3, 3]);
        assert_eq!(apportion(1_152_626, &[3, 1]), [864_470, 288_156]);
        assert_eq!(apportion(5, &[0, 0]), [5, 0]);
        assert_eq!(apportion(7, &[]), Vec::<u64>::new());
        let parts = apportion(1_000_003, &[7, 11, 13, 17]);
        assert_eq!(parts.iter().sum::<u64>(), 1_000_003);
    }
}
