//! Which account each response drew on: the one signed in where it was made,
//! when it was made.
//!
//! Nothing an agent writes of a response says which account it used. What
//! can be known is where the agent was signed in: each look for sign-ins
//! records, for every place an agent keeps one, what that place held. A
//! place is one of the agent's folders ([`crate::Folder`]) and the provider
//! its usage names when it uses what is kept there: Codex's `~/.codex` for
//! `openai`, Pi's `~/.pi/agent` for `openai-codex`, its ChatGPT sign-in.
//! What a place held is kept in the ledger as spans, each what it held and
//! the first and last looks that found it so, a look that finds the same
//! stretching the latest span, and one that finds another beginning a span
//! of its own. A second Claude account kept in `~/.claude-work` is a place of
//! its own, so what was used there is that account's, and never the other's.
//!
//! **The rule.** A response of an agent, made in one of its folders, of a
//! provider, at a time, drew on what the place held then: the span that
//! holds its time, of several the one that began latest; and, between
//! spans or beyond them, the nearest, the later of two as near. So history
//! from before the first look goes to what the first look found there, and
//! use in a stretch no look covered, as while the app was closed, to the
//! account found nearer to it in time: which of the two it was is not
//! known. Usage whose time isn't known goes to what was found first. The
//! rule rests on the spans alone, whatever order they were recorded in.
//!
//! What a place held counts only for responses of its own provider: an
//! agent can use several providers at once from one folder, each with a
//! sign-in of its own. A response of no provider, as usage outside the
//! conversation that its agent's totals give for no model, is no account's.
//!
//! **API keys.** A place can hold an API key rather than a sign-in to a
//! subscription, or a sign-in to something Turnscope doesn't read, as
//! GitHub Copilot's through OpenCode; `super::api` says how each is told.
//! Use of a key, and use from a place that held nothing, as an agent reading
//! its key from the environment does, drew on the provider's API-key
//! account, `api:` and the provider ([`api_account`]); so did use from a
//! folder looked in whose place for the provider was never found holding
//! anything. Use from a place that held a sign-in to something unread drew
//! on no account known. So does use from a folder never looked in, as when
//! no look has been made, whose account can't be told; and use of a
//! provider only a subscription reaches ([`SUBSCRIBED`]) from a place that
//! held nothing.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::sign_in::SignIn;
use super::{Reader, Subscription};
use crate::agent::Agent;
use crate::error::Result;
use crate::folders::Folder;
use crate::ledger::Ledger;

/// One of the places an agent keeps a sign-in: a folder of its, and the
/// provider its usage names when it uses what is kept there.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Place {
    /// The agent.
    pub agent: Agent,
    /// The folder of its.
    pub folder: PathBuf,
    /// The provider, as the agent's usage names it.
    pub provider: String,
}

/// What a place held when it was looked at.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Held {
    /// A sign-in to the account with this id.
    Account(String),
    /// An API key.
    Key,
    /// A sign-in to something Turnscope doesn't read, as a subscription
    /// whose limits it has no reader for.
    Other,
    /// Nothing.
    Nothing,
}

/// Providers only a subscription reaches, whose use from a place that held
/// nothing was of no API key: Pi's ChatGPT sign-in, and OpenCode Go, whose
/// key is the plan's.
const SUBSCRIBED: &[&str] = &["openai-codex", "opencode-go"];

/// The id of `provider`'s API-key account.
pub(super) fn api_account(provider: &str) -> String {
    Subscription::ApiKey.account_id(provider)
}

/// What one look found a place held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    /// The place.
    pub place: Place,
    /// What it held.
    pub held: Held,
}

/// A place holding the same from one look to another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    /// What it held.
    pub held: Held,
    /// When the first look that found it so was, in milliseconds.
    pub first: i64,
    /// When the last was, in milliseconds.
    pub last: i64,
}

/// What every place held over time, as the ledger keeps it.
#[derive(Debug, Default)]
pub(crate) struct Timeline {
    spans: HashMap<Place, Vec<Span>>,
    /// Each agent's folders a look found anything in, for any provider.
    looked: HashSet<(Agent, PathBuf)>,
}

impl Timeline {
    /// The timeline the ledger keeps.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Ledger`] when the ledger cannot be read.
    pub(crate) fn read(ledger: &Ledger) -> Result<Timeline> {
        Ok(Timeline::of(ledger.sign_ins()?))
    }

    /// The timeline of `spans`, each of a place.
    fn of(spans: impl IntoIterator<Item = (Place, Span)>) -> Timeline {
        let mut timeline = Timeline::default();
        for (place, span) in spans {
            timeline.looked.insert((place.agent, place.folder.clone()));
            timeline.spans.entry(place).or_default().push(span);
        }
        timeline
    }

    /// The account a response of `agent`, made in its `folder`, of
    /// `provider`, at `at` in milliseconds, drew on, by the rules above;
    /// `None` when none is known.
    pub(crate) fn account(
        &self,
        agent: Agent,
        folder: &Path,
        provider: &str,
        at: Option<i64>,
    ) -> Option<Cow<'_, str>> {
        if provider.is_empty() {
            return None;
        }
        let place = Place {
            agent,
            folder: folder.to_path_buf(),
            provider: provider.to_owned(),
        };
        let held = match self.spans.get(&place) {
            Some(spans) => held_at(spans, at)?,
            // A folder looked in that held nothing of this provider's.
            None if self.looked.contains(&(agent, place.folder)) => &Held::Nothing,
            None => return None,
        };
        match held {
            Held::Account(account) => Some(Cow::Borrowed(account)),
            Held::Nothing if SUBSCRIBED.contains(&provider) => None,
            Held::Key | Held::Nothing => Some(Cow::Owned(api_account(provider))),
            Held::Other => None,
        }
    }
}

/// Each place's latest span of `spans`: the one it was last seen in, the
/// latest begun of two that ended together, as the ledger orders them.
pub(super) fn latest(spans: &[(Place, Span)]) -> HashMap<&Place, &Span> {
    let mut latest: HashMap<&Place, &Span> = HashMap::new();
    for (place, span) in spans {
        if latest
            .get(place)
            .is_none_or(|known| (span.last, span.first) > (known.last, known.first))
        {
            latest.insert(place, span);
        }
    }
    latest
}

/// Where each account is signed in, from `spans` of every place: each agent
/// and folder whose place last held a sign-in to it; for an account no
/// place holds now, those whose place held it last. By agent and then
/// folder, each once.
pub(super) fn folders(spans: &[(Place, Span)]) -> HashMap<String, Vec<(Agent, PathBuf)>> {
    let latest = latest(spans);
    // For each account, the latest span of any place that held it.
    let mut last_held: HashMap<&str, (i64, Vec<&Place>)> = HashMap::new();
    for (place, span) in spans {
        if let Held::Account(account) = &span.held {
            let entry = last_held.entry(account).or_insert((span.last, Vec::new()));
            if span.last > entry.0 {
                *entry = (span.last, vec![place]);
            } else if span.last == entry.0 {
                entry.1.push(place);
            }
        }
    }
    let mut folders: HashMap<String, Vec<(Agent, PathBuf)>> = HashMap::new();
    for (place, span) in &latest {
        if let Held::Account(account) = &span.held {
            folders
                .entry(account.clone())
                .or_default()
                .push((place.agent, place.folder.clone()));
        }
    }
    for (account, (_, places)) in last_held {
        folders.entry(account.to_owned()).or_insert_with(|| {
            places
                .iter()
                .map(|place| (place.agent, place.folder.clone()))
                .collect()
        });
    }
    for held in folders.values_mut() {
        held.sort();
        held.dedup();
    }
    folders
}

/// What `spans` of one place say it held at `at`, by the rule above.
fn held_at(spans: &[Span], at: Option<i64>) -> Option<&Held> {
    let Some(at) = at else {
        return spans
            .iter()
            .min_by(|a, b| (a.first, a.last, &a.held).cmp(&(b.first, b.last, &b.held)))
            .map(|span| &span.held);
    };
    let holding = spans
        .iter()
        .filter(|span| span.first <= at && at <= span.last)
        .max_by(|a, b| (a.first, Reverse(&a.held)).cmp(&(b.first, Reverse(&b.held))));
    if let Some(span) = holding {
        return Some(&span.held);
    }
    // Neither end is at it, so how far it is is how far the nearer end is.
    let distance = |span: &Span| {
        if at < span.first {
            span.first.saturating_sub(at)
        } else {
            at.saturating_sub(span.last)
        }
    };
    spans
        .iter()
        .min_by(|a, b| {
            (distance(a), Reverse(a.first), &a.held).cmp(&(distance(b), Reverse(b.first), &b.held))
        })
        .map(|span| &span.held)
}

/// Each place `agent` keeps a sign-in to `subscription` in its `folder`,
/// one for each provider its usage of the subscription names.
#[cfg(feature = "fixture")]
pub(crate) fn places(subscription: Subscription, agent: Agent, folder: &Path) -> Vec<Place> {
    let mut places: Vec<Place> = Vec::new();
    for source in subscription.reader().sources {
        let place = Place {
            agent,
            folder: folder.to_path_buf(),
            provider: source.provider.to_owned(),
        };
        if source.agent == agent && !places.contains(&place) {
            places.push(place);
        }
    }
    places
}

/// What each place `reader`'s sources name held when `sign_ins`, found in
/// `folders`, were found: the account of the first sign-in found there, by
/// the sources' order, or nothing. A folder that isn't there has no place.
pub(super) fn seen(
    subscription: Subscription,
    reader: &Reader,
    folders: &[Folder],
    sign_ins: &[SignIn],
) -> Vec<Seen> {
    let mut seen: Vec<Seen> = Vec::new();
    for source in reader.sources {
        for folder in folders
            .iter()
            .filter(|folder| folder.agent == source.agent && folder.path.is_dir())
        {
            let place = Place {
                agent: source.agent,
                folder: folder.path.clone(),
                provider: source.provider.to_owned(),
            };
            // Sources that name one place, as OpenCode's database and its
            // older file do, find what it holds alike.
            if seen.iter().any(|known| known.place == place) {
                continue;
            }
            let held = sign_ins
                .iter()
                .find(|sign_in| {
                    sign_in.agent == place.agent
                        && sign_in.folder == place.folder
                        && sign_in.provider == place.provider
                })
                .map_or(Held::Nothing, |sign_in| {
                    Held::Account(subscription.account_id(&sign_in.identity(reader).key))
                });
            seen.push(Seen { place, held });
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Held, Place, Span, Timeline, held_at};
    use crate::agent::Agent;

    const MINUTE: i64 = 60_000;

    fn span(held: &str, first: i64, last: i64) -> Span {
        Span {
            held: match held {
                "" => Held::Nothing,
                account => Held::Account(account.to_owned()),
            },
            first: first * MINUTE,
            last: last * MINUTE,
        }
    }

    /// What `spans` say was held at minute `at`, in every order they could
    /// have been recorded in, which must all agree.
    fn at(spans: &[Span], at: Option<i64>) -> Option<Held> {
        let mut orders: Vec<Vec<Span>> = vec![spans.to_vec()];
        for turn in 1..spans.len() {
            let mut turned = spans.to_vec();
            turned.rotate_left(turn);
            orders.push(turned.clone());
            turned.reverse();
            orders.push(turned);
        }
        let answers: Vec<Option<Held>> = orders
            .iter()
            .map(|spans| held_at(spans, at.map(|at| at * MINUTE)).cloned())
            .collect();
        assert!(
            answers.windows(2).all(|pair| pair[0] == pair[1]),
            "{answers:?}"
        );
        answers[0].clone()
    }

    fn account(id: &str) -> Option<Held> {
        Some(Held::Account(id.to_owned()))
    }

    #[test]
    fn a_response_drew_on_what_was_signed_in_where_and_when_it_was_made() {
        // Signed into work from minute 10 to 60, signed out from 70 to 80,
        // and into personal from 100 to 200.
        let spans = [
            span("claude:work", 10, 60),
            span("", 70, 80),
            span("claude:personal", 100, 200),
        ];
        // Within a span, what it held.
        assert_eq!(at(&spans, Some(30)), account("claude:work"));
        assert_eq!(at(&spans, Some(75)), Some(Held::Nothing));
        assert_eq!(at(&spans, Some(150)), account("claude:personal"));
        // Before the first look, what it found; after the last, what that
        // found.
        assert_eq!(at(&spans, Some(0)), account("claude:work"));
        assert_eq!(at(&spans, Some(500)), account("claude:personal"));
        // Between two spans, the nearer: minute 84 is 4 from the sign-out and
        // 16 from personal; 92 is 12 from the one and 8 from the other; 90
        // is 10 from each, and goes to the later.
        assert_eq!(at(&spans, Some(84)), Some(Held::Nothing));
        assert_eq!(at(&spans, Some(92)), account("claude:personal"));
        assert_eq!(at(&spans, Some(90)), account("claude:personal"));
        // At no time known, what was found first.
        assert_eq!(at(&spans, None), account("claude:work"));
    }

    #[test]
    fn of_spans_that_overlap_the_one_begun_latest_holds() {
        // A look recorded late, from another process, found personal at
        // minute 30, inside the span of work.
        let spans = [span("claude:work", 10, 60), span("claude:personal", 30, 30)];
        assert_eq!(at(&spans, Some(30)), account("claude:personal"));
        assert_eq!(at(&spans, Some(20)), account("claude:work"));
        // Two begun at once hold alike in any order.
        let spans = [span("claude:b", 10, 20), span("claude:a", 10, 20)];
        assert_eq!(at(&spans, Some(15)), account("claude:a"));
    }

    #[test]
    fn a_place_counts_only_for_its_own_providers_responses() {
        let work = Place {
            agent: Agent::ClaudeCode,
            folder: "/Users/joey/.claude-work".into(),
            provider: "anthropic".to_owned(),
        };
        let timeline = Timeline::of([(work.clone(), span("claude:work", 10, 60))]);
        let folder = Path::new("/Users/joey/.claude-work");
        let account = |agent: Agent, folder: &Path, provider: &str| {
            timeline
                .account(agent, folder, provider, Some(20 * MINUTE))
                .map(|account| account.into_owned())
        };
        assert_eq!(
            account(Agent::ClaudeCode, folder, "anthropic").as_deref(),
            Some("claude:work")
        );
        // Another provider's usage from the folder looked in, where nothing
        // of it was found, was of an API key, as one read from the
        // environment is.
        assert_eq!(
            account(Agent::ClaudeCode, folder, "openai").as_deref(),
            Some("api:openai")
        );
        // Usage from a folder, or of an agent, never looked in, or of no
        // provider, drew on no account known.
        let own = Path::new("/Users/joey/.claude");
        assert_eq!(account(Agent::ClaudeCode, own, "anthropic"), None);
        assert_eq!(account(Agent::Pi, folder, "anthropic"), None);
        assert_eq!(account(Agent::ClaudeCode, folder, ""), None);
    }

    #[test]
    fn a_key_draws_on_its_providers_api_account_and_an_unread_sign_in_on_none() {
        let place = |agent: Agent, provider: &str| Place {
            agent,
            folder: "/Users/joey/.pi/agent".into(),
            provider: provider.to_owned(),
        };
        let held = |held: Held| Span {
            held,
            first: 0,
            last: 60 * MINUTE,
        };
        let timeline = Timeline::of([
            (place(Agent::Pi, "openrouter"), held(Held::Key)),
            (place(Agent::Pi, "anthropic"), held(Held::Other)),
            (place(Agent::Pi, "openai-codex"), held(Held::Nothing)),
            (place(Agent::Pi, "openai"), held(Held::Nothing)),
        ]);
        let folder = Path::new("/Users/joey/.pi/agent");
        let account = |provider: &str| {
            timeline
                .account(Agent::Pi, folder, provider, Some(MINUTE))
                .map(|account| account.into_owned())
        };
        assert_eq!(account("openrouter").as_deref(), Some("api:openrouter"));
        assert_eq!(account("openai").as_deref(), Some("api:openai"));
        // A Claude plan Pi is signed into, which nothing reads, and a
        // ChatGPT plan it was signed out of: no account known.
        assert_eq!(account("anthropic"), None);
        assert_eq!(account("openai-codex"), None);
    }
}
