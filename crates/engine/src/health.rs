//! How well history reads: what each agent's artifacts held, what could not
//! be made sense of, and how the transcripts compare with the agents' own
//! totals.
//!
//! `turnscope doctor` prints this, and is run after any change to a reader,
//! merging or pricing, to compare with the run before. On 2026-09-26 it
//! read 599 Claude Code files, 902 Codex rollouts, OpenCode's database, 8 Pi
//! sessions and 98 Grok Build files with nothing unrecognized, in 1.1 s with
//! its rescan; Grok Build's own totals of 5 sessions matched its transcripts
//! exactly, and the prices agreed with the agents' own costs as
//! [`crate::price`] records. A negative difference, where the transcripts
//! show more than an agent's own totals, counts as nothing outside the
//! conversation and is listed here, since reports can lag while a session
//! runs.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use crate::agent::{Agent, DiagnosticKind};
use crate::error::Result;
use crate::ledger::Ledger;
use crate::model::ModelKey;
use crate::outside::{self, Counts, Transcripts};
use crate::price::{self, Basis, PriceBook, Priceable};
use crate::runtime::CatalogCheck;
use crate::session::{SessionKey, Tree};
use crate::time::Instant;
use crate::usage::{Response, Tokens, Usd};

/// How close a cost must come to an agent's own to count as agreeing: one
/// part in a hundred.
const AGREEING: f64 = 0.01;

/// How close an agent's own token counts must come to what the transcripts
/// show for its cost to test the prices alone: half a part in a hundred.
const MATCHING: f64 = 0.005;

/// How complete the history answers come from is: what an answer may be
/// missing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Health {
    /// When every agent's history was last looked through in full: `None`
    /// until a first look has finished, when answers may be missing any of
    /// it.
    pub looked: Option<Instant>,
    /// What that look could not read, and why: artifacts, and folders it
    /// could not see into. What was read from them before stands.
    pub failed: Vec<(PathBuf, String)>,
}

/// A report on the ledger, for the `turnscope doctor` command.
#[derive(Clone, Debug)]
pub struct Doctor {
    /// Each agent with any artifacts.
    pub agents: Vec<AgentHealth>,
    /// Each session and model an agent reported its own totals for, beside
    /// what the transcripts show.
    pub checks: Vec<Check>,
    /// What each agent's usage cost, and how that compares with what the
    /// agent itself says it cost.
    pub pricing: Vec<Pricing>,
    /// Where the prices come from.
    pub prices: PriceSource,
}

/// Where the prices come from.
#[derive(Clone, Debug)]
pub struct PriceSource {
    /// When the catalog prices are taken from was read from models.dev.
    pub as_of: Option<Instant>,
    /// Where it came from: `bundled` with the app, or `models.dev`.
    pub source: Option<String>,
    /// The latest check of models.dev.
    pub checked: Option<CatalogCheck>,
}

/// What one agent's usage cost.
#[derive(Clone, Debug)]
pub struct Pricing {
    /// The agent.
    pub agent: Agent,
    /// What the provider charged, where the agent recorded it.
    pub charged: Usd,
    /// What the catalog's rates come to.
    pub catalog: Usd,
    /// Of that, what rests on a rate that stood in for one the catalog lacks.
    pub approximate: Usd,
    /// The agent's own estimates, for models the catalog does not price.
    pub estimated: Usd,
    /// Models with usage and no price at all, most used first.
    pub unpriced: Vec<Unpriced>,
    /// The catalog's cost beside the agent's own, where both are known.
    pub comparison: Option<Comparison>,
}

/// A model whose usage has no price.
#[derive(Clone, Debug)]
pub struct Unpriced {
    /// The provider.
    pub provider: String,
    /// The model.
    pub model: String,
    /// Its responses.
    pub responses: u64,
    /// Their tokens.
    pub tokens: u64,
}

/// The catalog's cost for some usage beside what the agent says it cost.
#[derive(Clone, Debug, Default)]
pub struct Comparison {
    /// How many costs were compared: responses, or an agent's session totals.
    pub compared: u64,
    /// Of those, how many agree within one part in a hundred.
    pub agreeing: u64,
    /// What the agent says they cost.
    pub recorded: Usd,
    /// What the catalog's rates come to.
    pub ours: Usd,
    /// The furthest apart, furthest first.
    pub furthest: Vec<Difference>,
}

/// One cost that does not agree with the agent's own.
#[derive(Clone, Debug)]
pub struct Difference {
    /// What was compared: a response or a session, and its model.
    pub what: String,
    /// What the agent says it cost.
    pub recorded: Usd,
    /// What the catalog's rates come to.
    pub ours: Usd,
}

impl Comparison {
    fn add(&mut self, what: impl FnOnce() -> String, recorded: Usd, ours: Usd) {
        self.compared += 1;
        self.recorded = self.recorded.saturating_add(recorded);
        self.ours = self.ours.saturating_add(ours);
        let apart = (recorded.nanos() - ours.nanos()).unsigned_abs();
        if apart as f64 <= AGREEING * recorded.nanos().max(ours.nanos()) as f64 {
            self.agreeing += 1;
        } else {
            self.furthest.push(Difference {
                what: what(),
                recorded,
                ours,
            });
        }
    }

    fn finish(mut self) -> Option<Comparison> {
        self.furthest.sort_by_key(|difference| {
            std::cmp::Reverse(
                (difference.recorded.nanos() - difference.ours.nanos()).unsigned_abs(),
            )
        });
        self.furthest.truncate(5);
        (self.compared > 0).then_some(self)
    }
}

/// What one agent's artifacts held.
#[derive(Clone, Debug)]
pub struct AgentHealth {
    /// The agent.
    pub agent: Agent,
    /// Artifacts read.
    pub artifacts: u64,
    /// Of those, how many have since disappeared.
    pub absent: u64,
    /// Sessions.
    pub sessions: u64,
    /// Responses, each counted once however many artifacts report it.
    pub responses: u64,
    /// Their usage.
    pub tokens: Tokens,
    /// Web searches.
    pub web_searches: u64,
    /// Trouble found, most frequent first.
    pub diagnostics: Vec<Diagnostic>,
}

/// One kind of trouble, across every artifact it came up in.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    /// What kind.
    pub kind: DiagnosticKind,
    /// What exactly.
    pub detail: String,
    /// How many times.
    pub count: u64,
    /// In how many artifacts.
    pub artifacts: u64,
    /// One artifact it came up in.
    pub example: PathBuf,
    /// The byte offset of the line it first came up on there.
    pub offset: u64,
}

/// An agent's own totals for one model in one session, beside what the
/// transcripts of the sessions those totals cover show, as the cache compares
/// them to find usage outside the conversation.
#[derive(Clone, Debug)]
pub struct Check {
    /// The session reported on.
    pub session: SessionKey,
    /// The model, or `None` for totals that cover every model the session
    /// used.
    pub model: Option<ModelKey>,
    /// The sessions the totals cover: the session and, when the agent's
    /// totals include them, its subagents.
    pub sessions: Vec<SessionKey>,
    /// What the agent counted, under every name it gave the model.
    pub reported: Counts,
    /// What the agent says it cost; `None` when it doesn't say, or says it
    /// cost nothing, as for usage a subscription covered, which says nothing
    /// of its price.
    pub(crate) reported_cost: Option<Usd>,
    /// What the transcripts show.
    pub observed: Counts,
}

/// Put together a report on the ledger, pricing usage from `book`.
pub(crate) fn doctor(ledger: &Ledger, book: &PriceBook) -> Result<Doctor> {
    let mut agents: BTreeMap<Agent, AgentHealth> = BTreeMap::new();
    let health = |agent: Agent| -> AgentHealth {
        AgentHealth {
            agent,
            artifacts: 0,
            absent: 0,
            sessions: 0,
            responses: 0,
            tokens: Tokens::default(),
            web_searches: 0,
            diagnostics: Vec::new(),
        }
    };

    for (agent, artifacts, absent) in ledger.artifact_counts()? {
        let entry = agents.entry(agent).or_insert_with(|| health(agent));
        entry.artifacts = artifacts;
        entry.absent = absent;
    }
    for (agent, sessions) in ledger.session_counts()? {
        agents
            .entry(agent)
            .or_insert_with(|| health(agent))
            .sessions = sessions;
    }

    // Each response once, its reports combined as everything else combines
    // them.
    let responses = ledger.responses()?;
    for response in &responses {
        let entry = agents
            .entry(response.agent)
            .or_insert_with(|| health(response.agent));
        entry.responses += 1;
        entry.tokens.add(&response.tokens);
        entry.web_searches = entry.web_searches.saturating_add(response.web_searches);
    }

    let mut diagnostics: BTreeMap<(Agent, DiagnosticKind, String), Diagnostic> = BTreeMap::new();
    for noted in ledger.noted()? {
        diagnostics
            .entry((noted.agent, noted.kind, noted.detail.clone()))
            .and_modify(|seen| {
                seen.count = seen.count.saturating_add(noted.count);
                seen.artifacts += 1;
            })
            .or_insert(Diagnostic {
                kind: noted.kind,
                detail: noted.detail,
                count: noted.count,
                artifacts: 1,
                example: noted.path,
                offset: noted.first,
            });
    }
    for ((agent, _, _), diagnostic) in diagnostics {
        agents
            .entry(agent)
            .or_insert_with(|| health(agent))
            .diagnostics
            .push(diagnostic);
    }
    for entry in agents.values_mut() {
        entry
            .diagnostics
            .sort_by(|a, b| b.count.cmp(&a.count).then(a.detail.cmp(&b.detail)));
    }

    let checks = checks(ledger, &responses)?;
    let pricing = pricing(&responses, &checks, book);
    let stored = ledger.catalog()?;
    let prices = PriceSource {
        as_of: stored.as_ref().and_then(|stored| stored.catalog.as_of()),
        source: stored.map(|stored| stored.source),
        checked: ledger.last_check()?,
    };
    Ok(Doctor {
        agents: agents.into_values().collect(),
        checks,
        pricing,
        prices,
    })
}

/// What each agent's usage cost, beside the agent's own costs.
fn pricing(responses: &[Response], checks: &[Check], book: &PriceBook) -> Vec<Pricing> {
    let mut pricing: BTreeMap<Agent, Pricing> = BTreeMap::new();
    let mut unpriced: BTreeMap<(Agent, String, String), (u64, u64)> = BTreeMap::new();
    let mut comparisons: BTreeMap<Agent, Comparison> = BTreeMap::new();
    // Catalog costs by session and model, for checking an agent's session
    // totals.
    let mut by_session: HashMap<(SessionKey, ModelKey), Usd> = HashMap::new();

    for response in responses {
        let entry = pricing.entry(response.agent).or_insert_with(|| Pricing {
            agent: response.agent,
            charged: Usd::default(),
            catalog: Usd::default(),
            approximate: Usd::default(),
            estimated: Usd::default(),
            unpriced: Vec::new(),
            comparison: None,
        });
        let Some(cost) = price::cost(&Priceable::of(response), book) else {
            let seen = unpriced
                .entry((
                    response.agent,
                    response.provider.clone(),
                    response.model.clone(),
                ))
                .or_default();
            seen.0 += 1;
            seen.1 = seen.1.saturating_add(response.tokens.total());
            continue;
        };
        let add = |total: &mut Usd| *total = total.saturating_add(cost.usd);
        match cost.basis {
            Basis::Charged => add(&mut entry.charged),
            Basis::Agent => add(&mut entry.estimated),
            Basis::Catalog => {
                add(&mut entry.catalog);
                if cost.approximate {
                    add(&mut entry.approximate);
                }
                let spent = by_session
                    .entry((response.session.clone(), ModelKey::of(&response.model)))
                    .or_default();
                *spent = spent.saturating_add(cost.usd);
                // Where the agent priced the response too, the two are compared.
                // A cost of nothing says only that the agent was not charged by
                // the token, as for usage covered by a subscription, and tests
                // no price.
                if let Some(recorded) = response.recorded.filter(|recorded| recorded.nanos() > 0) {
                    comparisons.entry(response.agent).or_default().add(
                        || format!("{} {}", response.key, response.model),
                        recorded,
                        cost.usd,
                    );
                }
            }
        }
    }

    // An agent's session totals test the prices only where its token counts
    // match what the transcripts show, so no usage outside them is in the cost.
    for check in checks {
        let (Some(model), Some(recorded)) = (&check.model, check.reported_cost) else {
            continue;
        };
        if !matching(check.reported, check.observed) {
            continue;
        }
        // Usage no catalog price was put on, such as Grok Build's, charged as
        // the agent recorded it, tests no price.
        let spent: Vec<Usd> = check
            .sessions
            .iter()
            .filter_map(|session| by_session.get(&(session.clone(), model.clone())).copied())
            .collect();
        if spent.is_empty() {
            continue;
        }
        let ours = spent.into_iter().fold(Usd::default(), Usd::saturating_add);
        comparisons.entry(check.session.agent()).or_default().add(
            || format!("{} {model}", check.session),
            recorded,
            ours,
        );
    }

    for ((agent, provider, model), (responses, tokens)) in unpriced {
        if let Some(entry) = pricing.get_mut(&agent) {
            entry.unpriced.push(Unpriced {
                provider,
                model,
                responses,
                tokens,
            });
        }
    }
    for (agent, comparison) in comparisons {
        if let Some(entry) = pricing.get_mut(&agent) {
            entry.comparison = comparison.finish();
        }
    }
    let mut pricing: Vec<Pricing> = pricing.into_values().collect();
    for entry in &mut pricing {
        entry
            .unpriced
            .sort_by_key(|model| std::cmp::Reverse(model.tokens));
    }
    pricing
}

/// Whether an agent's own counts match what the transcripts show, each within
/// half a part in a hundred.
fn matching(reported: Counts, observed: Counts) -> bool {
    [
        (reported.input, observed.input),
        (reported.cache_read, observed.cache_read),
        (reported.cache_write, observed.cache_write),
        (reported.output, observed.output),
    ]
    .into_iter()
    .all(|(reported, observed)| {
        reported.abs_diff(observed) as f64 <= MATCHING * reported.max(observed) as f64
    })
}

/// Each agent's own totals beside what the transcripts of `responses` show,
/// compared as the cache compares them to find usage outside the
/// conversation.
fn checks(ledger: &Ledger, responses: &[Response]) -> Result<Vec<Check>> {
    let tree = Tree::of(&ledger.parents()?);
    let mut by_session: HashMap<&SessionKey, Vec<&Response>> = HashMap::new();
    for response in responses {
        by_session
            .entry(&response.session)
            .or_default()
            .push(response);
    }

    let mut checks = Vec::new();
    for report in ledger.reports()? {
        let covered = outside::covered(&report, &tree);
        let mut transcripts = Transcripts::default();
        for response in covered
            .iter()
            .filter_map(|session| by_session.get(session))
            .flatten()
        {
            transcripts.add(
                &response.provider,
                &response.model,
                response.at,
                &response.tokens,
                response.recorded,
            );
        }
        for compared in outside::compare(&report, &transcripts) {
            checks.push(Check {
                session: report.session.clone(),
                sessions: covered.clone(),
                model: compared.model,
                reported: compared.reported,
                reported_cost: compared.cost,
                observed: Counts::of(&compared.shown.tokens),
            });
        }
    }
    Ok(checks)
}
