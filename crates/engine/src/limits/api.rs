//! API keys: use no subscription covers, kept as an account of each
//! provider's.
//!
//! **A key or a sign-in.** Which a response was made with is told by what
//! the place it was made from held ([`super::attribution`]), as each agent
//! keeps it:
//!
//! - Claude Code keeps a sign-in to a Claude plan in the Keychain; without
//!   one, its use of Anthropic was of an API key, whether from the
//!   environment or kept by Claude Code. A key in the environment is taken
//!   over a sign-in, and can't be seen, so use made so while signed in is
//!   put down to the plan.
//! - Codex keeps a ChatGPT sign-in, or with `auth_mode` `apikey` a key, in
//!   `auth.json`: anything but a sign-in is a key.
//! - OpenCode keeps a row for each provider, its `type` `key` (or `api` in
//!   the `auth.json` it kept before) for a key, and `oauth` for a sign-in:
//!   to ChatGPT, SuperGrok or OpenCode Go a plan's, and to anything else, as
//!   GitHub Copilot or a Claude plan, one Turnscope doesn't read. A provider
//!   with no row is reached with a key from the environment.
//! - Pi keeps an entry for each provider in `auth.json`, its `type`
//!   `api_key` for a key and `oauth` for a sign-in, but for OpenRouter,
//!   whose sign-in gives Pi a key (`sk-or-`) it keeps as `access`.
//! - Grok Build keeps a sign-in to SuperGrok; without one, it used a key.
//!
//! A kind of entry not listed here is taken for a sign-in Turnscope doesn't
//! read, so its use is no account's rather than a guess.
//!
//! **Accounts.** The use of a provider's keys is one account, `api:` and
//! the provider as its agents' usage names it, as `api:anthropic`, found
//! once any usage drew on it or a key to it carries a limit. Two keys to
//! one provider are one account: which a response used isn't recorded.
//!
//! **A key's own limit.** An OpenRouter key can carry a limit of its own,
//! read from OpenRouter with each key an agent keeps ([`super::openrouter`])
//! and kept as any limit's readings are; every key's is a limit of the
//! account. A key no agent keeps any longer carries none.

use std::collections::{BTreeSet, HashSet};

use rusqlite::Connection;
use serde_json::Value;

use super::attribution::{self, Held, Place, Seen, Span, api_account};
use super::sign_in::{credentials, json_file};
use super::{
    AccountLimits, AccountRead, LimitProblem, PlanLimits, Reader, SignIn, Subscription, openrouter,
};
use crate::agent::{self, Agent};
use crate::error::Result;
use crate::folders::Folder;
use crate::time::Instant;

/// Where keys that carry limits of their own are kept, and how those are
/// read: OpenRouter's, the only provider whose keys do.
pub(super) const READER: Reader = Reader {
    sources: openrouter::SOURCES,
    identity: super::one_account,
    fetch: openrouter::fetch,
};

/// Read each key among `sign_ins` that is not known to have expired for its
/// own limit, with `fetch`, and give each provider's as its API-key
/// account's: the limits of all its keys, each key asked once however many
/// agents keep it. Read whole or not at all: a key that gets no answer is
/// the account's answer, as its other keys' limits would look like all of
/// them.
pub(super) fn read(
    sign_ins: &[SignIn],
    now: Instant,
    fetch: impl Fn(&str, Instant) -> Result<PlanLimits, LimitProblem>,
) -> Vec<AccountRead> {
    let mut providers: Vec<&str> = sign_ins.iter().map(|sign_in| sign_in.provider).collect();
    providers.sort_unstable();
    providers.dedup();
    providers
        .into_iter()
        .map(|provider| {
            let held: Vec<&SignIn> = sign_ins
                .iter()
                .filter(|sign_in| sign_in.provider == provider)
                .collect();
            let mut agents: Vec<Agent> = held.iter().map(|sign_in| sign_in.agent).collect();
            agents.sort();
            agents.dedup();
            let mut keys: Vec<&str> = held
                .iter()
                .filter(|sign_in| sign_in.expires.is_none_or(|at| at > now))
                .map(|sign_in| sign_in.token.as_str())
                .collect();
            keys.sort_unstable();
            keys.dedup();
            let mut limits = Ok(Vec::new());
            for key in keys {
                match (fetch(key, now), &mut limits) {
                    (Ok(answer), Ok(limits)) => limits.extend(answer.limits),
                    (Err(problem), Ok(_)) => limits = Err(problem),
                    (_, Err(_)) => {}
                }
            }
            AccountRead {
                id: api_account(provider),
                label: None,
                plan: None,
                agents,
                limits,
            }
        })
        .collect()
}

/// What each entry an agent keeps for a provider no subscription's sign-in
/// is read from held, in each of the agents' `folders`: a key, or a sign-in
/// Turnscope doesn't read. A folder that isn't there has none.
///
/// # Errors
///
/// Returns an error when a place that keeps them is there but can't be
/// read, as [`super::sign_in`] says.
pub(super) fn seen(folders: &[Folder]) -> Result<Vec<Seen>> {
    let mut seen = Vec::new();
    for folder in folders.iter().filter(|folder| folder.path.is_dir()) {
        let entries: Vec<(String, Value)> = match folder.agent {
            Agent::OpenCode => {
                let mut entries = credentials(&agent::opencode::database(&folder.path))?;
                // Where OpenCode kept them before its database did.
                entries.extend(entries_of(json_file(&folder.path.join("auth.json"))?));
                entries
            }
            Agent::Pi => entries_of(json_file(&folder.path.join("auth.json"))?),
            Agent::ClaudeCode | Agent::Codex | Agent::Grok => continue,
        };
        for (provider, entry) in entries {
            if subscribed(folder.agent, &provider) {
                continue;
            }
            let place = Place {
                agent: folder.agent,
                folder: folder.path.clone(),
                provider: provider.clone(),
            };
            // Of two entries for one provider, as OpenCode's database and
            // its old file can both hold, a key stands over a sign-in.
            let held = held(&provider, &entry);
            match seen
                .iter_mut()
                .find(|known: &&mut Seen| known.place == place)
            {
                Some(known) if held == Held::Key => known.held = held,
                Some(_) => {}
                None => seen.push(Seen { place, held }),
            }
        }
    }
    Ok(seen)
}

/// The places among `spans` whose latest span held a key or a sign-in
/// Turnscope doesn't read, in a folder of `folders` that is there, that
/// `seen`, what the latest look found, no longer finds anything in: each
/// found now to hold nothing.
pub(crate) fn vanished(seen: &[Seen], spans: &[(Place, Span)], folders: &[Folder]) -> Vec<Seen> {
    let mut gone: Vec<Seen> = attribution::latest(spans)
        .into_iter()
        .filter(|(place, span)| {
            matches!(span.held, Held::Key | Held::Other)
                && folders.iter().any(|folder| {
                    folder.agent == place.agent
                        && folder.path == place.folder
                        && folder.path.is_dir()
                })
                && !seen.iter().any(|seen| seen.place == **place)
        })
        .map(|(place, _)| Seen {
            place: place.clone(),
            held: Held::Nothing,
        })
        .collect();
    gone.sort_by(|a, b| a.place.cmp(&b.place));
    gone
}

/// Each provider's entry in a file of them keyed by provider.
fn entries_of(file: Option<Value>) -> Vec<(String, Value)> {
    match file {
        Some(Value::Object(entries)) => entries.into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Whether a subscription's sign-in to `provider` is read from `agent`'s
/// folders, which says what its entry for it held.
fn subscribed(agent: Agent, provider: &str) -> bool {
    Subscription::ALL
        .into_iter()
        .filter(|subscription| *subscription != Subscription::ApiKey)
        .flat_map(|subscription| subscription.reader().sources)
        .any(|source| source.agent == agent && source.provider == provider)
}

/// What an agent's `entry` for `provider` held, by its `type`.
fn held(provider: &str, entry: &Value) -> Held {
    match entry["type"].as_str() {
        Some("key" | "api" | "api_key") => Held::Key,
        // OpenRouter's sign-in gives the agent a key of its own.
        Some("oauth") if provider == openrouter::PROVIDER => Held::Key,
        _ => Held::Other,
    }
}

/// Add to `accounts` every API-key account usage in `cache` drew on, each
/// with the agents its usage came from, whether it is among `recent`, the
/// accounts a response of the last half hour drew on, and, for one whose
/// keys carry no limit read, when keys were last `looked` for. An API-key
/// account already among them, whose keys' limits were read, is given the
/// same.
///
/// # Errors
///
/// Returns [`crate::Error::Ledger`] when the cache cannot be read.
pub(super) fn accounts(
    accounts: &mut Vec<AccountLimits>,
    cache: &Connection,
    recent: &HashSet<String>,
    looked: Option<Instant>,
) -> Result<()> {
    let prefix = format!("{}:", Subscription::ApiKey.key());
    let mut ids: BTreeSet<String> = used(cache)?;
    ids.extend(
        accounts
            .iter()
            .filter(|account| account.subscription == Subscription::ApiKey)
            .map(|account| account.id.clone()),
    );
    for id in ids {
        let Some(provider) = id
            .strip_prefix(&prefix)
            .filter(|provider| !provider.is_empty())
        else {
            continue;
        };
        let index = match accounts.iter().position(|account| account.id == id) {
            Some(index) => index,
            None => {
                accounts.push(AccountLimits {
                    id: id.clone(),
                    subscription: Subscription::ApiKey,
                    label: None,
                    plan: None,
                    agents: Vec::new(),
                    signed_in: true,
                    limits: Vec::new(),
                    read_at: None,
                    // Its keys aren't read, but were looked for.
                    checked_at: looked,
                    problem: None,
                    in_use: false,
                    hidden: false,
                    provider: None,
                    folders: Vec::new(),
                });
                accounts.len() - 1
            }
        };
        let account = &mut accounts[index];
        account.provider = Some(provider.to_owned());
        account.in_use |= recent.contains(&id);
        for agent in agents(cache, &id)? {
            if !account.agents.contains(&agent) {
                account.agents.push(agent);
            }
        }
        account.agents.sort();
        // A key no agent keeps any longer carries no limit of its own; and a
        // key isn't signed out of: one from the environment is never seen.
        if !account.signed_in {
            account.limits.clear();
        }
        account.signed_in = true;
    }
    Ok(())
}

/// Every API-key account usage in `cache` drew on.
fn used(cache: &Connection) -> Result<BTreeSet<String>> {
    let mut statement = cache.prepare_cached(
        "SELECT account FROM usage INDEXED BY usage_account
         WHERE account >= 'api:' AND account < 'api;' GROUP BY account",
    )?;
    let ids = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// The agents whose usage drew on `account`, in their order. An agent this
/// build doesn't know is passed over.
fn agents(cache: &Connection, account: &str) -> Result<Vec<Agent>> {
    let mut statement = cache.prepare_cached(
        "SELECT DISTINCT agent FROM usage INDEXED BY usage_account WHERE account = ?1",
    )?;
    let mut agents: Vec<Agent> = statement
        .query_map([account], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?
        .iter()
        .filter_map(|key| Agent::from_key(key))
        .collect();
    agents.sort();
    Ok(agents)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{held, read, seen, vanished};
    use crate::agent::Agent;
    use crate::folders::{Folder, FolderOrigin};
    use crate::limits::attribution::{Held, Place, Seen, Span};
    use crate::limits::{LimitProblem, PlanLimits, Reported, SignIn};
    use crate::time::Instant;

    #[test]
    fn what_each_entry_holds_is_told_by_its_kind() {
        assert_eq!(
            held("anthropic", &json!({"type": "key", "key": "k"})),
            Held::Key
        );
        assert_eq!(
            held("anthropic", &json!({"type": "api", "key": "k"})),
            Held::Key
        );
        assert_eq!(
            held("zai", &json!({"type": "api_key", "key": "k"})),
            Held::Key
        );
        assert_eq!(
            held(
                "openrouter",
                &json!({"type": "oauth", "access": "sk-or-v1"})
            ),
            Held::Key
        );
        assert_eq!(
            held("github-copilot", &json!({"type": "oauth", "access": "t"})),
            Held::Other
        );
        // A kind not known is no guess at a key.
        assert_eq!(held("anthropic", &json!({"type": "passkey"})), Held::Other);
        assert_eq!(held("anthropic", &json!("key")), Held::Other);
    }

    /// A key to OpenRouter an agent keeps in `folder`.
    fn key(agent: Agent, folder: &str, token: &str) -> SignIn {
        SignIn {
            agent,
            folder: folder.into(),
            provider: "openrouter",
            token: token.to_owned(),
            expires: None,
            plan: None,
            whose: None,
        }
    }

    /// A key's limit, keyed by the key, `used` percent used.
    fn limit(key: &str, used: f64) -> Reported {
        Reported {
            key: format!("key:{key}"),
            name: "Monthly key limit".to_owned(),
            scope: None,
            used,
            starts: None,
            resets: None,
        }
    }

    #[test]
    fn every_key_to_a_provider_is_read_once_as_its_account() {
        let now = Instant::parse("2026-09-29T12:00:00Z").unwrap();
        // OpenCode and Pi keep one key, and Pi a second.
        let keys = [
            key(Agent::OpenCode, "/o", "sk-or-a"),
            key(Agent::Pi, "/p", "sk-or-a"),
            key(Agent::Pi, "/p", "sk-or-b"),
        ];
        let asked = std::cell::RefCell::new(Vec::new());
        let reads = read(&keys, now, |key, _| {
            asked.borrow_mut().push(key.to_owned());
            Ok(PlanLimits {
                plan: None,
                limits: match key {
                    "sk-or-a" => vec![limit("a", 25.0)],
                    // A key with no limit of its own.
                    _ => Vec::new(),
                },
            })
        });
        assert_eq!(asked.into_inner(), ["sk-or-a", "sk-or-b"]);
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].id, "api:openrouter");
        assert_eq!(reads[0].agents, [Agent::OpenCode, Agent::Pi]);
        assert_eq!(reads[0].limits, Ok(vec![limit("a", 25.0)]));

        // One key refused: the account's answer, not the other key's limit
        // alone.
        let reads = read(&keys, now, |key, _| match key {
            "sk-or-a" => Ok(PlanLimits {
                plan: None,
                limits: vec![limit("a", 25.0)],
            }),
            _ => Err(LimitProblem::SignIn),
        });
        assert_eq!(reads[0].limits, Err(LimitProblem::SignIn));
    }

    #[test]
    fn a_key_no_longer_kept_is_found_gone() {
        let home = tempfile::tempdir().unwrap();
        let folder = Folder {
            agent: Agent::OpenCode,
            path: home.path().to_path_buf(),
            origin: FolderOrigin::Own,
        };
        let place = |provider: &str| Place {
            agent: Agent::OpenCode,
            folder: home.path().to_path_buf(),
            provider: provider.to_owned(),
        };
        let span = |held: Held, last: i64| Span {
            held,
            first: 0,
            last,
        };
        // OpenCode kept keys to Anthropic and Z.AI and a Copilot sign-in;
        // the latest look found only the Z.AI key. Mistral's key went long
        // ago, and a place that held nothing holds nothing still.
        let spans = [
            (place("anthropic"), span(Held::Key, 10)),
            (place("zai"), span(Held::Key, 10)),
            (place("github-copilot"), span(Held::Other, 10)),
            (place("mistral"), span(Held::Key, 5)),
            (place("mistral"), span(Held::Nothing, 8)),
            (place("openai"), span(Held::Nothing, 10)),
        ];
        let seen = [Seen {
            place: place("zai"),
            held: Held::Key,
        }];
        let gone: Vec<String> = vanished(&seen, &spans, &[folder])
            .into_iter()
            .inspect(|gone| assert_eq!(gone.held, Held::Nothing))
            .map(|gone| gone.place.provider)
            .collect();
        assert_eq!(gone, ["anthropic", "github-copilot"]);
    }

    #[test]
    fn keys_are_found_where_no_subscriptions_sign_in_is_read() {
        let home = tempfile::tempdir().unwrap();
        let pi = home.path().join(".pi/agent");
        std::fs::create_dir_all(&pi).unwrap();
        // As this Mac's Pi kept them on 2026-09-29, with a Claude sign-in
        // beside them: SuperGrok's and OpenCode Go's are subscriptions'.
        std::fs::write(
            pi.join("auth.json"),
            json!({"xai": {"type": "oauth", "access": "a", "refresh": "r", "expires": 1},
                   "opencode-go": {"type": "api_key", "key": "k"},
                   "openrouter": {"type": "oauth", "access": "sk-or-v1-x", "refresh": "",
                                  "expires": 9_007_199_254_740_991_i64},
                   "anthropic": {"type": "oauth", "access": "a", "refresh": "r", "expires": 1}})
            .to_string(),
        )
        .unwrap();
        let folders = [Folder {
            agent: Agent::Pi,
            path: pi.clone(),
            origin: FolderOrigin::Own,
        }];
        let mut found: Vec<(String, Held)> = seen(&folders)
            .unwrap()
            .into_iter()
            .map(|Seen { place, held }| (place.provider, held))
            .collect();
        found.sort();
        assert_eq!(
            found,
            [
                ("anthropic".to_owned(), Held::Other),
                ("openrouter".to_owned(), Held::Key),
            ]
        );
    }
}
