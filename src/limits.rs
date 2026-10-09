//! Reading accounts' limits: every login the agents hold, grouped by the
//! account it's to, each account asked of its provider once, all at once.
//!
//! A subscription is read with its freshest login that works; an API-key
//! account with every key, since each key can carry a limit of its own. A
//! login known to have expired is never sent: agents renew their own logins,
//! and Turnscope never does. What each agent folder holds is recorded as a
//! sign-in, which says whose usage is whose.
//!
//! Where a provider can't say which plan a key is to, accounts it reads as
//! one plan (`Provider::plan_mark`) are made one, and a key keeps the account
//! it was last signed in to, so they stay one.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::agents::{self, Credential};
use crate::providers::{self, Kind, Limits, Problem, Provider, Reading};
use crate::{Result, ingest, now};

/// How long limits stand before any process reads them again.
const DUE: i64 = 5 * crate::time::MINUTE;

/// An account, and the logins it's read with.
struct Held {
    provider: &'static dyn Provider,
    account: providers::Account,
    credentials: Vec<Credential>,
}

/// What an agent folder holds for a provider: the account its login is to,
/// if any, and an API key's fingerprint.
struct SignedIn {
    agent: &'static str,
    folder: String,
    /// The provider as the agent's responses name it.
    provider: String,
    account: Option<String>,
    key: Option<String>,
}

/// Read every account's limits, when it's five minutes since any process
/// last did.
pub fn refresh(db: &mut Connection, home: &Path) -> Result<()> {
    let now = now();
    if !claim(db, now)? {
        return Ok(());
    }
    let (signed_in, accounts) = gather(db, home)?;
    // Every account at once, so the slowest provider sets how long it takes.
    let answers: Vec<(Held, std::result::Result<Limits, Problem>)> = std::thread::scope(|scope| {
        let reading: Vec<_> = accounts
            .into_values()
            .map(|mut held| {
                scope.spawn(move || {
                    let answer = read(&mut held, now);
                    (held, answer)
                })
            })
            .collect();
        reading
            .into_iter()
            .filter_map(|thread| thread.join().ok())
            .collect()
    });
    let changed = record(db, signed_in, answers, now)?;
    // Whose usage is whose changed.
    ingest::derive(db, ingest::Of::SignIns(changed))
}

/// Claim reading the limits if they're due, in one statement, so no two
/// processes read them at once.
fn claim(db: &Connection, now: i64) -> Result<bool> {
    // Looked at first: claiming waits for the write lock, which a hook
    // shouldn't when nothing is due.
    let last: Option<i64> = db.query_row("SELECT limits_at FROM state", [], |row| row.get(0))?;
    if last.is_some_and(|last| now - DUE < last && last <= now + DUE) {
        return Ok(false);
    }
    // Claimed more than `DUE` ahead of now is a clock set back since.
    let claimed = db.execute(
        "UPDATE state SET limits_at = ?1
         WHERE limits_at IS NULL OR limits_at <= ?1 - ?2 OR limits_at > ?1 + ?2",
        params![now, DUE],
    )?;
    Ok(claimed > 0)
}

/// What every agent folder signs in to, and the accounts those logins are
/// to, each with its logins.
fn gather(db: &Connection, home: &Path) -> Result<(Vec<SignedIn>, BTreeMap<String, Held>)> {
    let mut accounts: BTreeMap<String, Held> = BTreeMap::new();
    let mut signed_in = Vec::new();
    // A key is to one account for good: the one it was last signed in to,
    // which may be another key's it was made one with.
    let mut kept = db.prepare(
        "SELECT account FROM sign_in WHERE key = ?1 AND account IS NOT NULL
         ORDER BY since DESC LIMIT 1",
    )?;
    for &agent in agents::ALL {
        for folder in agent.folders(home) {
            // A folder whose logins can't be read keeps what it held.
            let Ok(logins) = agent.logins(&folder, home) else {
                continue;
            };
            let folder = folder.to_string_lossy().into_owned();
            let mut named = Vec::new();
            for login in logins {
                let mut held = login.credential.and_then(|credential| {
                    let provider = providers::by_id(credential.provider)?;
                    let account = provider.account(&credential)?;
                    Some((provider, account, credential))
                });
                let key = held
                    .as_ref()
                    .filter(|(_, _, credential)| credential.key)
                    .map(|(_, _, credential)| credential.secret.fingerprint());
                if let (Some((_, account, _)), Some(key)) = (held.as_mut(), &key)
                    && let Some(id) = kept
                        .query_row([key], |row| row.get::<_, String>(0))
                        .optional()?
                    && id != account.id
                {
                    account.id = id;
                    // Its label named this key, not the account's.
                    account.label = None;
                }
                named.push(login.provider.clone());
                signed_in.push(SignedIn {
                    agent: agent.info().id,
                    folder: folder.clone(),
                    provider: login.provider,
                    account: held.as_ref().map(|(_, account, _)| account.id.clone()),
                    key,
                });
                if let Some((provider, account, credential)) = held {
                    accounts
                        .entry(account.id.clone())
                        .or_insert(Held {
                            provider,
                            account,
                            credentials: Vec::new(),
                        })
                        .credentials
                        .push(credential);
                }
            }
            // A login gone from the folder is signed out of.
            let mut gone = db.prepare_cached(
                "SELECT provider FROM sign_in s WHERE folder = ?1 AND account IS NOT NULL
                 AND since = (SELECT max(since) FROM sign_in WHERE folder = s.folder AND provider = s.provider)",
            )?;
            for provider in gone.query_map([&folder], |row| row.get::<_, String>(0))? {
                let provider = provider?;
                if !named.contains(&provider) {
                    signed_in.push(SignedIn {
                        agent: agent.info().id,
                        folder: folder.clone(),
                        provider,
                        account: None,
                        key: None,
                    });
                }
            }
        }
    }
    Ok((signed_in, accounts))
}

/// Write what was read: accounts read as one plan made one, each folder's
/// sign-ins, then each account and its readings. Gives each folder whose
/// sign-in to a provider changed, by folder and provider, with when its
/// responses may since be another account's.
fn record(
    db: &mut Connection,
    mut signed_in: Vec<SignedIn>,
    answers: Vec<(Held, std::result::Result<Limits, Problem>)>,
    now: i64,
) -> Result<Vec<(String, String, i64)>> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let existing: HashSet<String> = tx
        .prepare("SELECT id FROM account")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let one = same_plan(&answers, &existing);
    for (other, first) in &one {
        merge(&tx, other, first)?;
    }
    for signed in &mut signed_in {
        if let Some(first) = signed.account.as_ref().and_then(|id| one.get(id)) {
            signed.account = Some(first.clone());
        }
    }

    let mut changed = Vec::new();
    for signed in &signed_in {
        let held: Option<(Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT account, key FROM sign_in WHERE folder = ?1 AND provider = ?2
                 ORDER BY since DESC LIMIT 1",
                params![signed.folder, signed.provider],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if held.as_ref() != Some(&(signed.account.clone(), signed.key.clone())) {
            tx.execute(
                "INSERT OR REPLACE INTO sign_in (agent, folder, provider, since, account, key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    signed.agent,
                    signed.folder,
                    signed.provider,
                    now,
                    signed.account,
                    signed.key
                ],
            )?;
            // A folder's first sign-in to a provider is in force before it
            // too; any later one only from now.
            let from = if held.is_some() { now } else { i64::MIN };
            changed.push((signed.folder.clone(), signed.provider.clone(), from));
        }
    }

    for (held, read) in answers {
        let account = &held.account;
        if one.contains_key(&account.id) {
            continue;
        }
        let problem = match &read {
            Ok(_) => None,
            Err(Problem::SignIn) => Some("signIn"),
            Err(Problem::Expired) => Some("expired"),
            Err(Problem::Unavailable) => Some("unavailable"),
        };
        let plan = read
            .as_ref()
            .ok()
            .and_then(|limits| limits.plan.clone())
            .or_else(|| {
                held.credentials
                    .iter()
                    .find_map(|credential| credential.plan.clone())
            })
            .filter(|plan| !plan.is_empty());
        let info = held.provider.info();
        let title = match (account.kind, info.subscription) {
            (Kind::Subscription, Some(subscription)) => match plan {
                Some(plan) => format!("{subscription} {}", providers::words(&plan)),
                None => subscription.to_owned(),
            },
            _ => format!("{} API key", info.name),
        };
        // A read that failed may not know the plan, so the title stays.
        tx.execute(
            "INSERT INTO account (id, provider, kind, title, label, read_at, problem)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (id) DO UPDATE SET
                 title = iif(excluded.read_at IS NULL, title, excluded.title),
                 label = coalesce(excluded.label, label),
                 read_at = coalesce(excluded.read_at, read_at), problem = excluded.problem",
            params![
                account.id,
                info.id,
                if account.kind == Kind::Subscription {
                    "subscription"
                } else {
                    "apiKey"
                },
                title,
                account.label,
                read.is_ok().then_some(now),
                problem,
            ],
        )?;
        for reading in read.iter().flat_map(|limits| &limits.readings) {
            tx.execute(
                "INSERT OR REPLACE INTO reading (account, limit_key, at, name, scope, used, size, starts, resets)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    account.id,
                    reading.key,
                    now,
                    reading.name,
                    reading.scope,
                    reading.used.max(0.0),
                    reading.size,
                    reading.starts,
                    reading.resets,
                ],
            )?;
        }
    }
    tx.commit()?;
    Ok(changed)
}

/// Each account read as the same plan as another, by its provider's
/// `plan_mark`, with the account that stands for them: one already known,
/// so its history is kept, or else the first, accounts being in order.
fn same_plan(
    answers: &[(Held, std::result::Result<Limits, Problem>)],
    existing: &HashSet<String>,
) -> BTreeMap<String, String> {
    let mut plans: BTreeMap<(&str, i64), Vec<&str>> = BTreeMap::new();
    for (held, read) in answers {
        if let Some(mark) = read
            .as_ref()
            .ok()
            .and_then(|limits| held.provider.plan_mark(limits))
        {
            plans
                .entry((held.provider.info().id, mark))
                .or_default()
                .push(&held.account.id);
        }
    }
    let mut one = BTreeMap::new();
    for ids in plans.values() {
        let first = ids
            .iter()
            .find(|id| existing.contains(**id))
            .unwrap_or(&ids[0]);
        for id in ids.iter().filter(|id| *id != first) {
            one.insert((*id).to_owned(), (*first).to_owned());
        }
    }
    one
}

/// Make `other` part of `first`: the folders signed in to it, what drew on
/// it, what it read and raised, and whether it's hidden.
fn merge(tx: &Transaction, other: &str, first: &str) -> Result<()> {
    for moved in [
        "UPDATE sign_in SET account = ?2 WHERE account = ?1",
        "UPDATE response SET account = ?2 WHERE account = ?1",
        "UPDATE OR IGNORE reading SET account = ?2 WHERE account = ?1",
        "UPDATE OR IGNORE alert SET account = ?2 WHERE account = ?1",
        "UPDATE account SET hidden = hidden OR coalesce((SELECT hidden FROM account WHERE id = ?1), 0)
         WHERE id = ?2",
    ] {
        tx.execute(moved, params![other, first])?;
    }
    // What was the same as the first's at the same time.
    for left in [
        "DELETE FROM reading WHERE account = ?1",
        "DELETE FROM alert WHERE account = ?1",
        "DELETE FROM account WHERE id = ?1",
    ] {
        tx.execute(left, [other])?;
    }
    Ok(())
}

/// An account's limits with its logins, the freshest first, and its plan.
/// A subscription is read with the first that works; an API-key account
/// with every key. A login refused gives way to the next. With every login
/// expired, none is sent: the agent renews its login the next time it's
/// used.
fn read(held: &mut Held, now: i64) -> std::result::Result<Limits, Problem> {
    held.credentials
        .sort_by_key(|credential| std::cmp::Reverse(credential.expires));
    let mut readings: Option<Vec<Reading>> = None;
    let mut plan = None;
    let mut problem = None;
    for credential in held
        .credentials
        .iter()
        .filter(|credential| credential.expires.is_none_or(|at| at > now))
    {
        match held.provider.limits(credential, now) {
            Ok(limits) => {
                plan = plan.or(limits.plan);
                let read = readings.get_or_insert_with(Vec::new);
                // What every key reads of the account itself is one limit.
                for reading in limits.readings {
                    if !read.iter().any(|known| known.key == reading.key) {
                        read.push(reading);
                    }
                }
                if held.account.kind == Kind::Subscription {
                    break;
                }
            }
            Err(Problem::SignIn) => {
                problem.get_or_insert(Problem::SignIn);
            }
            Err(other) => {
                problem = Some(other);
                if held.account.kind == Kind::Subscription {
                    break;
                }
            }
        }
    }
    readings
        .map(|readings| Limits { plan, readings })
        .ok_or(problem.unwrap_or(Problem::Expired))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(id: &str) -> Held {
        Held {
            provider: providers::by_id("opencode-go").unwrap(),
            account: providers::Account {
                id: id.to_owned(),
                kind: Kind::Subscription,
                label: Some(format!("key {id}")),
            },
            credentials: Vec::new(),
        }
    }

    fn monthly(resets: i64) -> std::result::Result<Limits, Problem> {
        Ok(Limits {
            plan: None,
            readings: vec![Reading {
                key: "monthly".to_owned(),
                name: "Monthly".to_owned(),
                scope: None,
                used: 10.0,
                size: None,
                starts: None,
                resets: Some(resets),
            }],
        })
    }

    #[test]
    fn keys_read_as_one_plan_are_the_account_already_known() {
        // Two keys to one plan, a key to another, and one not read.
        let answers = vec![
            (held("opencode-go:key-a"), monthly(1_794_088_864_000)),
            (held("opencode-go:key-b"), monthly(1_794_088_864_000)),
            (held("opencode-go:key-c"), monthly(1_794_088_865_000)),
            (held("opencode-go:key-d"), Err(Problem::Unavailable)),
        ];
        let known = HashSet::from(["opencode-go:key-b".to_owned()]);
        assert_eq!(
            same_plan(&answers, &known),
            BTreeMap::from([(
                "opencode-go:key-a".to_owned(),
                "opencode-go:key-b".to_owned()
            )])
        );
    }

    #[test]
    fn what_is_read_is_recorded_and_one_plan_keeps_its_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let signed = |folder: &str, account: &str| SignedIn {
            agent: "opencode",
            folder: folder.to_owned(),
            provider: "opencode-go".to_owned(),
            account: Some(account.to_owned()),
            key: Some(account.trim_start_matches("opencode-go:key-").to_owned()),
        };
        // Key b is read alone, then key a, to the same plan, joins it.
        record(
            &mut db,
            vec![signed("/f1", "opencode-go:key-b")],
            vec![(held("opencode-go:key-b"), monthly(1))],
            1_000,
        )
        .unwrap();
        db.execute(
            "UPDATE account SET hidden = 1, title = 'OpenCode Go' WHERE id = 'opencode-go:key-b'",
            [],
        )
        .unwrap();
        let changed = record(
            &mut db,
            vec![
                signed("/f1", "opencode-go:key-b"),
                signed("/f2", "opencode-go:key-a"),
            ],
            vec![
                (held("opencode-go:key-a"), monthly(1)),
                (held("opencode-go:key-b"), monthly(1)),
            ],
            2_000,
        )
        .unwrap();
        let accounts: Vec<(String, i64)> = db
            .prepare("SELECT id, hidden FROM account")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(accounts, [("opencode-go:key-b".to_owned(), 1)]);
        let readings: i64 = db
            .query_row(
                "SELECT count(*) FROM reading WHERE account = 'opencode-go:key-b'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(readings, 2);
        // The new folder's first sign-in is in force from the start.
        assert_eq!(
            changed,
            [("/f2".to_owned(), "opencode-go".to_owned(), i64::MIN)]
        );

        // A read that fails keeps the title, and says why.
        record(
            &mut db,
            Vec::new(),
            vec![(held("opencode-go:key-b"), Err(Problem::Expired))],
            3_000,
        )
        .unwrap();
        let (title, problem): (String, String) = db
            .query_row(
                "SELECT title, problem FROM account WHERE id = 'opencode-go:key-b'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (title.as_str(), problem.as_str()),
            ("OpenCode Go", "expired")
        );
    }
}
