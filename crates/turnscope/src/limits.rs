//! Reading accounts' limits: every login the agents hold, grouped by the
//! account it's to, each account asked of its provider once.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::agents::{self, Agent, Credential};
use crate::providers::{self, Kind, Limits, Problem, Provider, Reading};
use crate::{Result, now};

/// How long an account's limits stand while it's in use, and how often the
/// agents' logins are gathered for sign-ins and sign-outs.
pub const DUE: i64 = 5 * crate::time::MINUTE;

/// How long an account's limits stand while it isn't in use, and the longest
/// between two reads of all its limits, whatever an agent writes of some
/// (`record_in_band`). Under `status::STALE` (30 minutes) by more than `DUE`,
/// so a failed read is tried again before the account shows as stale.
const IDLE: i64 = 20 * crate::time::MINUTE;

/// How long a claim to read the accounts due holds off other processes:
/// longer than a read takes ([`WAIT`]), so no two ask a provider at once.
const CLAIM: i64 = 30 * 1000;

/// The longest an account waits between reads while its provider keeps
/// failing (`backoff`).
pub const BACKED_OFF: i64 = 30 * crate::time::MINUTE;

/// How long an account waits after `reads` reads in a row its provider
/// answered busy or not understood: [`DUE`], doubled with each, or as long as
/// the provider asked, up to [`BACKED_OFF`]. Only an answer is backed off:
/// backed off too, a provider out of reach for 36 minutes was first read
/// again at minute 65 (measured 2026-11-18).
fn backoff(reads: i64, retry_after: Option<i64>) -> i64 {
    let doubled = DUE.saturating_mul(1_i64 << (reads - 1).clamp(0, 8));
    doubled.max(retry_after.unwrap_or(0)).min(BACKED_OFF)
}

/// Whether `problem` came with an answer, and how long it asked to be left:
/// of two, the one kept (`keys`).
fn asked_for(problem: &Problem) -> (bool, Option<i64>) {
    match problem {
        Problem::Unavailable {
            answered,
            retry_after,
            ..
        } => (*answered, *retry_after),
        Problem::SignIn | Problem::Expired => (false, None),
    }
}

/// How long an account's read is waited for, all its requests together:
/// `guard` reads limits inside an agent's hook, which holds the agent.
const WAIT: Duration = Duration::from_secs(15);

/// What a read of an account gave: its limits, if any were read, and what
/// went wrong, if anything did. Both, when some of an API-key account's
/// keys were read and others weren't.
struct Read {
    limits: Option<Limits>,
    problem: Option<Problem>,
}

impl Read {
    fn of(limits: Limits) -> Read {
        Read {
            limits: Some(limits),
            problem: None,
        }
    }

    fn failed(problem: Problem) -> Read {
        Read {
            limits: None,
            problem: Some(problem),
        }
    }

    /// Nothing was read, and no provider answered, as while the Mac is
    /// offline.
    fn unanswered(&self) -> bool {
        self.limits.is_none()
            && matches!(
                self.problem,
                Some(Problem::Unavailable {
                    answered: false,
                    ..
                })
            )
    }
}

/// An account, and the logins it's read with.
struct Held {
    provider: &'static dyn Provider,
    account: providers::Account,
    credentials: Vec<Credential>,
}

impl Held {
    /// Read the account with `credential` too, unless it already is: a login
    /// held in two agent folders is sent once.
    fn hold(&mut self, credential: Credential) {
        let fingerprint = credential.secret.fingerprint();
        if !self
            .credentials
            .iter()
            .any(|known| known.secret.fingerprint() == fingerprint)
        {
            self.credentials.push(credential);
        }
    }
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

/// What every folder of `agents` holds: each folder's sign-in, each
/// account to read with its logins, and the read of each account whose
/// logins couldn't be read, unless another folder's login to it was
/// (`failed_reads`).
struct Gathered {
    signed_in: Vec<SignedIn>,
    accounts: BTreeMap<String, Held>,
    unreadable: Vec<(Read, Held)>,
}

/// Why limits are read. Whatever the ask, a provider that asked to be left
/// is left (`backoff`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// As they come due: every [`DUE`] in use, every `IDLE` not.
    Scheduled,
    /// The panel opened, or an agent wrote a login: those read [`DUE`] ago
    /// or more, and those whose read failed.
    Opened,
    /// Refresh Limits: every one not asked of its provider in the last
    /// `CLAIM`.
    Force,
}

/// Read the limits of each account that's due (`Schedule::due`), and the
/// agents' logins, when no other process is reading them (one asked for
/// waits for it) and none claimed doing so in the last `CLAIM`, or the last
/// [`DUE`] for a scheduled read with no signed-in account due; whether it
/// did. A read opened, or forced with a signed-in account due or after
/// waiting, isn't held off by a claim.
///
/// A read asked for that no provider answered leaves the account as it
/// was: asked for as the Mac woke, before its network was back, it showed
/// as unread for five minutes.
pub fn refresh(db: &mut Connection, home: &Path, ask: Ask) -> Result<bool> {
    refresh_with(db, home, agents::ALL, providers::ALL, ask)
}

fn refresh_with(
    db: &mut Connection,
    home: &Path,
    agents: &[&dyn Agent],
    providers: &[&'static dyn Provider],
    ask: Ask,
) -> Result<bool> {
    // One asked for waits out another process's read, then skips what that
    // just asked: asked again at once, a login refused once would count as
    // refused twice.
    let lock = crate::db::dir(db).join("limits.lock");
    let (_reading, waited) = match crate::db::lock_within(&lock, Duration::ZERO)? {
        Some(reading) => (reading, false),
        None if ask == Ask::Scheduled => return Ok(false),
        None => match crate::db::lock_within(&lock, Duration::from_millis(CLAIM as u64))? {
            Some(reading) => (reading, true),
            None => return Ok(false),
        },
    };
    let now = now();
    let schedules = schedules(db, now)?;
    let due: HashSet<&str> = schedules
        .iter()
        .filter(|(_, schedule)| {
            schedule.due(now, ask) && (!waited || schedule.due(now, Ask::Force))
        })
        .map(|(id, _)| id.as_str())
        .collect();
    let signed_in_due = schedules
        .iter()
        .any(|(id, schedule)| schedule.signed_in && due.contains(id.as_str()));
    // One that waited isn't held off by the claim of the read it waited for.
    let gap = match (ask, signed_in_due) {
        (Ask::Opened, _) | (Ask::Force, true) => 0,
        (Ask::Force, false) if waited => 0,
        (Ask::Force, false) | (Ask::Scheduled, true) => CLAIM,
        (Ask::Scheduled, false) => DUE,
    };
    if !crate::db::claim(db, crate::db::Claim::Limits, now, gap, now)? {
        return Ok(false);
    }
    let mut gathered = gather(db, home, agents, providers)?;
    // An account not known before is read at once.
    let read_now = |id: &str| due.contains(id) || !schedules.contains_key(id);
    gathered.accounts.retain(|id, _| read_now(id));
    gathered
        .unreadable
        .retain(|(_, held)| read_now(&held.account.id));
    // Every account at once, so the slowest provider sets how long it takes.
    let mut answers: Vec<(Read, Held)> = std::thread::scope(|scope| {
        let reading: Vec<_> = gathered
            .accounts
            .into_values()
            .map(|mut held| {
                scope.spawn(move || {
                    let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        read(&mut held, now)
                    }))
                    .unwrap_or_else(|_| Read::failed(Problem::unavailable(PANICKED)));
                    (answer, held)
                })
            })
            .collect();
        reading
            .into_iter()
            .filter_map(|thread| thread.join().ok())
            .collect()
    });
    answers.extend(gathered.unreadable);
    let answers = answers
        .into_iter()
        .filter(|(read, _)| !(ask != Ask::Scheduled && read.unanswered()))
        .collect();
    record(db, gathered.signed_in, answers, now)?;
    Ok(true)
}

/// What decides when an account's limits are read (`Schedule::due`).
struct Schedule {
    /// An agent folder is signed in to it now.
    signed_in: bool,
    /// The last reading: a read's, or one an agent wrote (`record_in_band`).
    read_at: Option<i64>,
    /// The last read of all its limits that worked.
    full_read_at: Option<i64>,
    /// When its provider was last asked, whatever came of it.
    checked_at: Option<i64>,
    /// Its last read failed (`failing`).
    failing: bool,
    /// When its provider asked to be asked again (`backoff`).
    retry_at: Option<i64>,
    /// In use, as `status::Account::in_use` has it.
    in_use: bool,
    /// A limit's window reset after the provider was last asked.
    reset_passed: bool,
}

impl Schedule {
    fn due(&self, now: i64, ask: Ask) -> bool {
        // A wait longer than any is a clock set back since.
        if self
            .retry_at
            .is_some_and(|at| now < at && at <= now + BACKED_OFF)
        {
            return false;
        }
        // Long ago for a time ahead of now by more than `DUE`: the clock was
        // set back since.
        let ago = |at: Option<i64>| match at {
            Some(at) if at <= now + DUE => now - at,
            _ => i64::MAX,
        };
        if self.checked_at.is_none() || self.reset_passed {
            return true;
        }
        if ask == Ask::Force {
            return ago(self.checked_at) >= CLAIM;
        }
        let asked = ask == Ask::Opened;
        if self.failing {
            return asked || ago(self.checked_at) >= DUE;
        }
        let stands = if self.in_use || asked { DUE } else { IDLE };
        ago(self.read_at) >= stands || ago(self.full_read_at) >= IDLE
    }
}

fn schedules(db: &Connection, now: i64) -> Result<BTreeMap<String, Schedule>> {
    // In SQL, over working out the status: this is asked at every catch-up.
    let mut rows = db.prepare_cached(&format!(
        "SELECT a.id, a.read_at, a.full_read_at, a.checked_at, f.account IS NOT NULL, f.retry_at,
             EXISTS (SELECT 1 FROM sign_in s WHERE s.account = a.id AND {SIGNED_IN_NOW}),
             {},
             EXISTS (SELECT 1 FROM reading r WHERE r.account = a.id AND r.at >= a.full_read_at
                 AND {LATEST_READING} AND r.resets > a.checked_at AND r.resets <= ?1)
         FROM account a LEFT JOIN failing f ON f.account = a.id",
        crate::status::Account::in_use_sql()
    ))?;
    let schedules = rows
        .query_map([now], |row| {
            Ok((
                row.get(0)?,
                Schedule {
                    read_at: row.get(1)?,
                    full_read_at: row.get(2)?,
                    checked_at: row.get(3)?,
                    failing: row.get(4)?,
                    retry_at: row.get(5)?,
                    signed_in: row.get(6)?,
                    in_use: row.get(7)?,
                    reset_passed: row.get(8)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(schedules)
}

/// What every folder of `agents` signs in to, the accounts those logins are
/// to, each with its logins, read by one of `providers`, and the accounts
/// whose logins couldn't be read.
fn gather(
    db: &Connection,
    home: &Path,
    agents: &[&dyn Agent],
    providers: &[&'static dyn Provider],
) -> Result<Gathered> {
    let mut accounts: BTreeMap<String, Held> = BTreeMap::new();
    let mut signed_in = Vec::new();
    let mut unreadable: BTreeMap<String, String> = BTreeMap::new();
    // A key is to the account it was last signed in to, which may be
    // another key's it was made one with, at the same provider alone.
    let mut kept = db.prepare(
        "SELECT account FROM sign_in WHERE key = ?1 AND provider = ?2 AND account IS NOT NULL
         ORDER BY since DESC LIMIT 1",
    )?;
    let listable = std::fs::read_dir(home).is_ok();
    let mut latest = db.prepare(&format!(
        "SELECT folder, provider, account FROM sign_in s
         WHERE agent = ?1 AND account IS NOT NULL AND {SIGNED_IN_NOW}"
    ))?;
    for &agent in agents {
        let folders = agent.installed_folders(home);
        // Each folder whose logins were read, with the providers they name.
        let mut read: Vec<(String, Vec<String>)> = Vec::new();
        // Each folder whose logins couldn't be read, with why.
        let mut failed: BTreeMap<String, String> = BTreeMap::new();
        for folder in &folders {
            // A folder whose logins can't be read keeps what it held, and
            // its account says why (`record`).
            let logins = match agent.logins(folder, home) {
                Ok(logins) => logins,
                Err(error) => {
                    failed.insert(folder.to_string_lossy().into_owned(), error.to_string());
                    continue;
                }
            };
            let folder = folder.to_string_lossy().into_owned();
            let mut named = Vec::new();
            for login in logins {
                let mut held = login.credential.and_then(|credential| {
                    let (provider, account) = account_of(&credential, providers)?;
                    Some((provider, account, credential))
                });
                let key = held
                    .as_ref()
                    .filter(|(_, _, credential)| credential.key)
                    .map(|(_, _, credential)| credential.secret.fingerprint());
                if let (Some((_, account, _)), Some(key)) = (held.as_mut(), &key)
                    && let Some(id) = kept
                        .query_row([key, &login.provider], |row| row.get::<_, String>(0))
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
                        .hold(credential);
                }
            }
            read.push((folder, named));
        }
        // A login gone from a folder read is signed out of, and so is every
        // login of a folder no longer listed. A home that can't be listed,
        // as one on a volume that's away, lists none: each keeps what it held.
        for row in latest.query_map([agent.info().id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })? {
            let (folder, provider, account): (String, String, String) = row?;
            if let Some(why) = failed.get(&folder) {
                unreadable.entry(account).or_insert_with(|| why.clone());
                continue;
            }
            let gone = match read.iter().find(|(read, _)| *read == folder) {
                Some((_, named)) => !named.contains(&provider),
                None => {
                    listable
                        && !folders
                            .iter()
                            .any(|listed| listed.to_string_lossy() == folder)
                }
            };
            if gone {
                signed_in.push(SignedIn {
                    agent: agent.info().id,
                    folder,
                    provider,
                    account: None,
                    key: None,
                });
            }
        }
    }
    unreadable.retain(|id, _| !accounts.contains_key(id));
    Ok(Gathered {
        unreadable: failed_reads(db, unreadable, providers)?,
        signed_in,
        accounts,
    })
}

/// The reads of `unreadable` accounts, each failed with why its logins
/// couldn't be read: such an account isn't signed out.
fn failed_reads(
    db: &Connection,
    unreadable: BTreeMap<String, String>,
    providers: &[&'static dyn Provider],
) -> Result<Vec<(Read, Held)>> {
    let mut stored = db.prepare("SELECT provider, kind, label FROM account WHERE id = ?1")?;
    let mut failed = Vec::new();
    for (id, why) in unreadable {
        let row: Option<(String, Kind, Option<String>)> = stored
            .query_row([&id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .optional()?;
        let Some((provider, kind, label)) = row else {
            continue;
        };
        let Some(&provider) = providers.iter().find(|read| read.info().id == provider) else {
            continue;
        };
        let held = Held {
            provider,
            account: providers::Account { id, kind, label },
            credentials: Vec::new(),
        };
        failed.push((Read::failed(Problem::unanswered(why)), held));
    }
    Ok(failed)
}

/// Write what was read, in one write: accounts read as one plan made one,
/// each folder's sign-ins and whose usage is whose since, then each account
/// and its readings. A read that failed differently from the one before is
/// added to `app.log` in the data directory, for `turnscope doctor`.
fn record(
    db: &mut Connection,
    mut signed_in: Vec<SignedIn>,
    answers: Vec<(Read, Held)>,
    now: i64,
) -> Result<()> {
    let tx = crate::db::write(db)?;
    let one = same_plan(&tx, &answers)?;
    for (other, first) in &one {
        merge(&tx, other, first)?;
    }
    for signed in &mut signed_in {
        if let Some(first) = signed.account.as_ref().and_then(|id| one.get(id)) {
            signed.account = Some(first.clone());
        }
    }
    sign_ins(&tx, &signed_in, now)?;
    // Of one plan, the account that stands for it is written once: by its
    // own read that worked, else a merged key's that worked, else the first
    // read that failed.
    let mut chosen: BTreeMap<String, (u8, Read, Held)> = BTreeMap::new();
    for (read, held) in answers {
        let id = one.get(&held.account.id).unwrap_or(&held.account.id);
        let rank = match (read.limits.is_some(), *id == held.account.id) {
            (true, true) => 0,
            (true, false) => 1,
            (false, _) => 2,
        };
        if chosen.get(id).is_none_or(|(had, ..)| rank < *had) {
            chosen.insert(id.clone(), (rank, read, held));
        }
    }
    let mut logged = Vec::new();
    for (id, (_, read, held)) in chosen {
        let title = title(&held, &read);
        // Named in `app.log` by its title alone: a label can be an email,
        // and `doctor` prints the log for a report someone may post.
        logged.extend(failing(&tx, &id, &read, &title, now)?);
        // A read that failed may not know the plan, so the title stays. One
        // of some limits only, as an API key's keys without its credits,
        // leaves the others as last read, and the account as of them unless
        // that's ahead of now (the clock set back since); one of none isn't
        // a read.
        let full = read.limits.is_some() && read.problem.is_none();
        let some = read
            .limits
            .as_ref()
            .is_some_and(|limits| full || !limits.readings.is_empty());
        tx.prepare_cached(
            "INSERT INTO account (id, provider, kind, title, label, read_at, full_read_at, checked_at, problem)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?9, ?7, ?8)
             ON CONFLICT (id) DO UPDATE SET
                 title = iif(excluded.read_at IS NULL, title, excluded.title),
                 label = coalesce(excluded.label, label),
                 read_at = iif(excluded.full_read_at IS NULL,
                     coalesce(min(read_at, excluded.read_at), read_at, excluded.read_at),
                     excluded.read_at),
                 full_read_at = coalesce(excluded.full_read_at, full_read_at),
                 checked_at = excluded.checked_at, problem = excluded.problem",
        )?
        .execute(params![
            id,
            held.provider.info().id,
            held.account.kind,
            title,
            held.account.label,
            some.then_some(now),
            now,
            read.problem.as_ref().map(problem_name),
            full.then_some(now),
        ])?;
        if let Some(limits) = &read.limits {
            // Readings stamped after this one were stamped while the clock
            // was ahead. Kept, they'd stand for the limits, and keep out what
            // Codex writes, until the clock caught up with them; left out of
            // the status alone, they'd stand again once it had.
            tx.prepare_cached("DELETE FROM reading WHERE account = ?1 AND at > ?2")?
                .execute(params![id, now])?;
            write_readings(&tx, &id, now, &limits.readings)?;
        }
    }
    tx.commit()?;
    for line in logged {
        crate::engine::log(crate::db::dir(db), &line);
    }
    Ok(())
}

/// The provider of `credential` among `providers`, and the account it's a
/// login to, if Turnscope reads that provider and the login names one.
fn account_of(
    credential: &Credential,
    providers: &[&'static dyn Provider],
) -> Option<(&'static dyn Provider, providers::Account)> {
    let provider = *providers
        .iter()
        .find(|provider| provider.info().id == credential.provider)?;
    Some((provider, provider.account(credential)?))
}

/// The accounts `agent`'s `folder` holds a login to now, each with the
/// provider as the agent names it, as a read of the limits finds them.
pub fn held_in(agent: &dyn Agent, folder: &Path, home: &Path) -> Result<Vec<(String, String)>> {
    Ok(agent
        .logins(folder, home)?
        .into_iter()
        .filter_map(|login| {
            let (_, account) = account_of(login.credential.as_ref()?, providers::ALL)?;
            Some((login.provider, account.id))
        })
        .collect())
}

fn write_readings<'a>(
    tx: &Transaction,
    account: &str,
    at: i64,
    readings: impl IntoIterator<Item = &'a Reading>,
) -> Result<()> {
    let mut insert = tx.prepare_cached(
        "INSERT OR REPLACE INTO reading (account, limit_key, at, name, scope, used, size, starts, resets)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for reading in readings {
        insert.execute(params![
            account,
            reading.key,
            at,
            reading.name,
            reading.scope,
            reading.used.max(0.0),
            reading.size,
            reading.starts,
            reading.resets,
        ])?;
    }
    Ok(())
}

/// Take in the limits an agent wrote in its history for `account`, as Codex
/// does with each response: each reading newer than the last of its limit
/// and unlike it. They move `read_at` alone, so a limit the agent doesn't
/// write, as one on some models, stands as the last full read gave it, and
/// the account is still read in full within `IDLE`. Whether any was taken in.
pub fn record_in_band(
    tx: &Transaction,
    account: &str,
    snapshot: &agents::InBand,
    now: i64,
) -> Result<bool> {
    let full_read_at: Option<Option<i64>> = tx
        .prepare_cached("SELECT full_read_at FROM account WHERE id = ?1 AND kind = 'subscription'")?
        .query_row([account], |row| row.get(0))
        .optional()?;
    // Older than a read of all the limits, it says less; stamped ahead of
    // now, the clock was set back since.
    let Some(full_read_at) = full_read_at else {
        return Ok(false);
    };
    if full_read_at.is_some_and(|full| snapshot.at <= full) || snapshot.at > now {
        return Ok(false);
    }
    let mut last = tx.prepare_cached(
        "SELECT at, used, resets FROM reading WHERE account = ?1 AND limit_key = ?2
         ORDER BY at DESC LIMIT 1",
    )?;
    let mut new = Vec::new();
    for reading in &snapshot.readings {
        let before: Option<(i64, f64, Option<i64>)> = last
            .query_row(params![account, reading.key], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .optional()?;
        // Codex writes its limits with every response, most often unchanged:
        // as used, of the same window.
        if !before.is_some_and(|(at, used, resets)| {
            at >= snapshot.at
                || (used == reading.used && crate::forecast::same_reset(resets, reading.resets))
        }) {
            new.push(reading);
        }
    }
    if new.is_empty() {
        return Ok(false);
    }
    write_readings(tx, account, snapshot.at, new)?;
    tx.prepare_cached("UPDATE account SET read_at = max(coalesce(read_at, ?2), ?2) WHERE id = ?1")?
        .execute(params![account, snapshot.at])?;
    Ok(true)
}

fn problem_name(problem: &Problem) -> &'static str {
    match problem {
        Problem::SignIn => "signIn",
        Problem::Expired => "expired",
        Problem::Unavailable { .. } => "unavailable",
    }
}

/// Note in `failing` how `account`'s read went, before its row says: how
/// many reads in a row failed as this one did, what it met, and when it's
/// read next. What's new of it, if anything, as the log says it, `named`
/// naming the account.
fn failing(
    tx: &Transaction,
    account: &str,
    read: &Read,
    named: &str,
    now: i64,
) -> Result<Option<String>> {
    let Some(problem) = &read.problem else {
        tx.execute("DELETE FROM failing WHERE account = ?1", [account])?;
        return Ok(None);
    };
    let before: Option<(Option<String>, Option<i64>, Option<String>)> = tx
        .query_row(
            "SELECT a.problem, f.reads, f.why FROM account a LEFT JOIN failing f ON f.account = a.id
             WHERE a.id = ?1",
            [account],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (was, reads, said) = before.unwrap_or_default();
    let again = was.as_deref() == Some(problem_name(problem));
    let reads = if again { reads.unwrap_or(0) + 1 } else { 1 };
    // Some keys read, the provider answered: it isn't backed off. Nor is
    // one that didn't answer. Neither is counted, so the first read backed
    // off after them waits five minutes: counted, one after an hour offline
    // waited thirty.
    let (why, retry_at, reads) = match problem {
        Problem::Unavailable {
            why,
            answered: false,
            ..
        } => (Some(why.as_str()), None, 0),
        Problem::Unavailable { why, .. } if read.limits.is_some() => (Some(why.as_str()), None, 0),
        Problem::Unavailable {
            why, retry_after, ..
        } => (
            Some(why.as_str()),
            Some(now + backoff(reads, *retry_after)),
            reads,
        ),
        _ => (None, None, reads),
    };
    tx.execute(
        "INSERT OR REPLACE INTO failing (account, reads, why, retry_at) VALUES (?1, ?2, ?3, ?4)",
        params![account, reads, why, retry_at],
    )?;
    Ok(match problem {
        _ if again && said.as_deref() == why => None,
        Problem::Unavailable { why, .. } => Some(format!("{named}: limits not read: {why}")),
        Problem::SignIn => Some(format!("{named}: its login was refused")),
        // The status says so, and what to do.
        Problem::Expired => None,
    })
}

/// The sign-in in force at response `r`, or before the folder's first, the
/// first, as `ingest::Derived::account` finds it.
pub const SIGN_IN_AT: &str =
    "FROM sign_in held WHERE held.folder = r.folder AND held.provider = r.provider
    ORDER BY held.since > r.at, iif(held.since > r.at, held.since, -held.since) LIMIT 1";

/// Whether sign-in `s` is the one its folder holds for its provider now.
pub const SIGNED_IN_NOW: &str =
    "s.since = (SELECT max(since) FROM sign_in WHERE folder = s.folder AND provider = s.provider)";

/// Whether reading `r` is the latest of its limit.
pub const LATEST_READING: &str = "NOT EXISTS (SELECT 1 FROM reading n
    WHERE n.account = r.account AND n.limit_key = r.limit_key AND n.at > r.at)";

/// Write each folder's sign-in that changed, and the account of the
/// responses made in that folder since, in the same write: after it, a
/// process that ended between the two left the responses on the account
/// before for good. Without `ingest.lock`, as a read of history takes in the
/// sign-ins at each of its writes (`ingest::Derived`).
fn sign_ins(tx: &Transaction, signed_in: &[SignedIn], now: i64) -> Result<()> {
    let mut update = tx.prepare(&format!(
        "UPDATE response AS r SET account = (SELECT account {SIGN_IN_AT})
         WHERE folder = ?1 AND provider = ?2 AND at >= ?3"
    ))?;
    for signed in signed_in {
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
            update.execute(params![signed.folder, signed.provider, from])?;
        }
    }
    Ok(())
}

/// What to call `held`: a subscription by its plan, as `read` or else one
/// of its logins names it; anything else as its provider's API key.
fn title(held: &Held, read: &Read) -> String {
    let plan = read
        .limits
        .as_ref()
        .and_then(|limits| limits.plan.as_deref())
        .or_else(|| {
            held.credentials
                .iter()
                .find_map(|credential| credential.plan.as_deref())
        })
        .filter(|plan| !plan.is_empty());
    let info = held.provider.info();
    match (held.account.kind, info.subscription) {
        (Kind::Subscription, Some(subscription)) => match plan {
            Some(plan) => format!("{subscription} {}", providers::words(plan)),
            None => subscription.to_owned(),
        },
        _ => format!("{} API key", info.name),
    }
}

/// Each account read as the same plan as another, by its provider's
/// `plan_mark`, with the account that stands for them: one already known,
/// so its history is kept, or else the first.
///
/// An account not read now, or whose login was refused, counts by the mark
/// of its last read, so a key swapped for a new one to the same plan keeps
/// its account. Other failures don't merge: `merge` can't be undone.
fn same_plan(tx: &Transaction, answers: &[(Read, Held)]) -> Result<BTreeMap<String, String>> {
    let mut plans: BTreeMap<(&str, i64), Vec<String>> = BTreeMap::new();
    let mut marked: BTreeMap<&str, &'static dyn Provider> = BTreeMap::new();
    for (read, held) in answers {
        if let Some(mark) = read
            .limits
            .as_ref()
            .and_then(|limits| held.provider.plan_mark(limits))
        {
            let provider = held.provider.info().id;
            marked.insert(provider, held.provider);
            plans
                .entry((provider, mark))
                .or_default()
                .push(held.account.id.clone());
        }
    }
    let skip_stored: HashSet<&str> = answers
        .iter()
        .filter(|(read, _)| read.limits.is_some() || read.problem != Some(Problem::SignIn))
        .map(|(_, held)| held.account.id.as_str())
        .collect();
    let mut by_provider = tx.prepare("SELECT id FROM account WHERE provider = ?1")?;
    for (&provider, &impl_) in &marked {
        let ids: Vec<String> = by_provider
            .query_map([provider], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in ids {
            if skip_stored.contains(id.as_str()) {
                continue;
            }
            if let Some(mark) = stored_limits(tx, &id)?.and_then(|limits| impl_.plan_mark(&limits))
                && let Some(group) = plans.get_mut(&(provider, mark))
            {
                group.push(id);
            }
        }
    }
    let existing: HashSet<String> = tx
        .prepare("SELECT id FROM account")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut one = BTreeMap::new();
    for ids in plans.values().filter(|ids| ids.len() > 1) {
        let first = ids
            .iter()
            .find(|id| existing.contains(*id))
            .unwrap_or(&ids[0]);
        for id in ids.iter().filter(|id| *id != first) {
            one.insert(id.clone(), first.clone());
        }
    }
    Ok(one)
}

/// What `account` was last read with, as a provider gave it: at its last
/// read of all its limits, not counting what an agent wrote of some since.
fn stored_limits(tx: &Transaction, account: &str) -> Result<Option<Limits>> {
    let readings: Vec<Reading> = tx
        .prepare(
            "SELECT limit_key, name, scope, used, size, starts, resets FROM reading
             WHERE account = ?1 AND at = (SELECT full_read_at FROM account WHERE id = ?1)",
        )?
        .query_map([account], |row| {
            Ok(Reading {
                key: row.get(0)?,
                name: row.get(1)?,
                scope: row.get(2)?,
                used: row.get(3)?,
                size: row.get(4)?,
                starts: row.get(5)?,
                resets: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok((!readings.is_empty()).then(|| readings.into()))
}

/// Make `other` part of `first`. The plan stays shown unless both were
/// hidden, over hidden if either was: of two accounts to one plan, the one
/// hidden is most likely the one someone took for a copy, as a refused
/// key's was. An `other` new this read leaves `first` as it was.
fn merge(tx: &Transaction, other: &str, first: &str) -> Result<()> {
    for moved in [
        "UPDATE sign_in SET account = ?2 WHERE account = ?1",
        "UPDATE response SET account = ?2 WHERE account = ?1",
        "UPDATE OR IGNORE reading SET account = ?2 WHERE account = ?1",
        "UPDATE OR IGNORE alert SET account = ?2 WHERE account = ?1",
        "UPDATE account SET hidden = hidden AND coalesce((SELECT hidden FROM account WHERE id = ?1), hidden)
         WHERE id = ?2",
    ] {
        tx.execute(moved, params![other, first])?;
    }
    // Readings and alerts left over duplicate the first's, and how its
    // reads went is the first's to say.
    for left in [
        "DELETE FROM reading WHERE account = ?1",
        "DELETE FROM alert WHERE account = ?1",
        "DELETE FROM failing WHERE account = ?1",
        "DELETE FROM account WHERE id = ?1",
    ] {
        tx.execute(left, [other])?;
    }
    Ok(())
}

/// An account's limits with its logins, in [`WAIT`] all told. A login
/// within a minute of expiring is sent only with what renews it
/// (`Credential::refresh`): agents renew their own logins, and Turnscope
/// renews one only where that takes nothing from the agent.
fn read(held: &mut Held, now: i64) -> Read {
    let by = Instant::now() + WAIT;
    held.credentials
        .sort_by_key(|credential| std::cmp::Reverse(credential.expires));
    let fresh: Vec<&Credential> = held
        .credentials
        .iter()
        .filter(|credential| credential.fresh(now) || credential.refresh.is_some())
        .collect();
    let provider = held.provider;
    if fresh.is_empty() {
        Read::failed(Problem::Expired)
    } else if !provider.info().key_limits && fresh.iter().all(|credential| credential.key) {
        Read::of(Limits::default())
    } else if held.account.kind == Kind::Subscription {
        plan(provider, &fresh, now, by)
    } else {
        keys(provider, &fresh, now, by)
    }
}

/// A subscription's limits, read with its freshest login, or the next when
/// one is refused, as the agent may have renewed it since. One out of reach
/// or not understood ends the read: the next would most likely fare the
/// same.
fn plan(provider: &dyn Provider, logins: &[&Credential], now: i64, by: Instant) -> Read {
    for login in logins {
        match provider.limits(login, now, by) {
            Ok(limits) => return Read::of(limits),
            Err(Problem::SignIn) => {}
            Err(other) => return Read::failed(other),
        }
    }
    Read::failed(Problem::SignIn)
}

/// Why a read that panicked failed.
const PANICKED: &str = "reading it failed unexpectedly";

/// An API-key account's limits: each key's own and the account's, all at
/// once: one after another, three keys to a provider out of reach held
/// `guard` 45 s. A key that can't be read is the account's problem, and of a
/// refusal and another problem, the refusal is said, as the one to act on.
fn keys(provider: &dyn Provider, keys: &[&Credential], now: i64, by: Instant) -> Read {
    let (own, shared) = std::thread::scope(|scope| {
        let own: Vec<_> = keys
            .iter()
            .map(|key| scope.spawn(move || provider.limits(key, now, by)))
            .collect();
        let shared = scope.spawn(|| {
            for key in keys {
                match provider.account_limits(key, now, by) {
                    Err(Problem::SignIn) => {}
                    read => return read,
                }
            }
            Ok(Vec::new())
        });
        let own: Vec<_> = own
            .into_iter()
            .map(|key| {
                key.join()
                    .unwrap_or_else(|_| Err(Problem::unavailable(PANICKED)))
            })
            .collect();
        let shared = shared
            .join()
            .unwrap_or_else(|_| Err(Problem::unavailable(PANICKED)));
        (own, shared)
    });
    let mut limits: Option<Limits> = None;
    let mut problem: Option<Problem> = None;
    let mut add = |read: std::result::Result<Limits, Problem>| match read {
        Ok(read) => {
            let limits = limits.get_or_insert_with(Limits::default);
            limits.plan = limits.plan.take().or(read.plan);
            limits.readings.extend(read.readings);
        }
        // Of two others, an answer over none, then the longest `Retry-After`,
        // so the log says why it waits.
        Err(error) => {
            problem = Some(match (problem.take(), error) {
                (Some(Problem::SignIn), _) | (_, Problem::SignIn) => Problem::SignIn,
                (Some(before), error) if asked_for(&before) > asked_for(&error) => before,
                (_, error) => error,
            })
        }
    };
    for read in own {
        add(read);
    }
    match shared {
        // None asked, or no key had access to them.
        Ok(readings) if readings.is_empty() => {}
        shared => add(shared.map(Limits::from)),
    }
    Read { limits, problem }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::db::test::{one, response, rows};

    /// A provider that answers each login by its secret: one with `refused`
    /// in it is refused, one with `down` out of reach, one with `offline`
    /// unanswered, and any other reads a limit keyed by it, after 300 ms for
    /// one with `slow`. The account's credits refuse a key with `nocredits`.
    /// It notes what it was asked.
    /// Every key is one account, and accounts whose `monthly` limit resets
    /// at one time are one plan.
    struct Fake {
        info: &'static providers::Info,
        asked: std::sync::Mutex<Vec<String>>,
    }

    impl Default for Fake {
        fn default() -> Fake {
            Fake {
                info: &FAKE,
                asked: Default::default(),
            }
        }
    }

    static FAKE: providers::Info = providers::Info {
        id: "fake",
        name: "Fake",
        subscription: Some("Fake"),
        key_limits: true,
    };

    /// Another, which the same key may be kept for.
    static FAKE_TOO: providers::Info = providers::Info {
        id: "fake-too",
        ..FAKE
    };

    fn limit(key: &str) -> Reading {
        Reading {
            key: key.to_owned(),
            name: key.to_owned(),
            scope: None,
            used: 10.0,
            size: None,
            starts: None,
            resets: None,
        }
    }

    impl Provider for Fake {
        fn info(&self) -> &'static providers::Info {
            self.info
        }

        fn account(&self, credential: &Credential) -> Option<providers::Account> {
            let (account, kind) = match credential.key {
                true => ("api", Kind::ApiKey),
                false => ("account", Kind::Subscription),
            };
            Some(providers::Account {
                id: format!("{}:{account}", self.info.id),
                kind,
                label: None,
            })
        }

        fn plan_mark(&self, limits: &Limits) -> Option<i64> {
            limits
                .readings
                .iter()
                .find(|reading| reading.key == "monthly")?
                .resets
        }

        fn limits(
            &self,
            credential: &Credential,
            _now: i64,
            _by: Instant,
        ) -> std::result::Result<Limits, Problem> {
            let secret = credential.secret.expose();
            self.asked.lock().unwrap().push(secret.to_owned());
            if secret.contains("slow") {
                std::thread::sleep(Duration::from_millis(300));
            }
            match secret {
                refused if refused.contains("refused") => Err(Problem::SignIn),
                down if down.contains("down") => Err(Problem::unavailable("down")),
                offline if offline.contains("offline") => {
                    Err(Problem::unanswered("no route to host"))
                }
                busy if busy.contains("busy") => Err(Problem::Unavailable {
                    why: "busy".to_owned(),
                    retry_after: Some(600_000),
                    answered: true,
                }),
                key => Ok(vec![limit(&format!("key-{key}"))].into()),
            }
        }

        fn account_limits(
            &self,
            credential: &Credential,
            _now: i64,
            _by: Instant,
        ) -> std::result::Result<Vec<Reading>, Problem> {
            self.asked.lock().unwrap().push("credits".to_owned());
            match credential.secret.expose().contains("nocredits") {
                true => Err(Problem::SignIn),
                false => Ok(vec![limit("credits")]),
            }
        }
    }

    /// An account of a fake provider, read with `secrets`, each a login
    /// expiring before the one before it.
    fn fake(kind: Kind, secrets: &[&str]) -> (&'static Fake, Held) {
        let fake: &'static Fake = Box::leak(Box::default());
        let credentials = secrets
            .iter()
            .enumerate()
            .map(|(at, secret)| {
                credential(
                    secret,
                    kind == Kind::ApiKey,
                    Some(now() + crate::time::DAY - at as i64),
                )
            })
            .collect();
        let held = Held {
            provider: fake,
            account: providers::Account {
                id: "fake:account".to_owned(),
                kind,
                label: None,
            },
            credentials,
        };
        (fake, held)
    }

    /// A login to the fake provider.
    fn credential(secret: &str, key: bool, expires: Option<i64>) -> Credential {
        Credential {
            provider: "fake",
            secret: agents::Secret::new(secret),
            key,
            expires,
            identity: None,
            plan: None,
            refresh: None,
        }
    }

    fn keys(read: &Read) -> Vec<&str> {
        let mut keys: Vec<&str> = read
            .limits
            .iter()
            .flat_map(|limits| &limits.readings)
            .map(|reading| reading.key.as_str())
            .collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn a_subscription_is_read_with_its_freshest_login_past_any_refused() {
        let (provider, mut held) = fake(Kind::Subscription, &["refused", "old", "older"]);
        let limits = read(&mut held, now());
        assert_eq!((keys(&limits), &limits.problem), (vec!["key-old"], &None));
        assert_eq!(*provider.asked.lock().unwrap(), ["refused", "old"]);
        // One out of reach ends the read: the next would most likely fare
        // the same.
        let (provider, mut held) = fake(Kind::Subscription, &["down", "old"]);
        assert_eq!(
            read(&mut held, now()).problem,
            Some(Problem::unavailable("down"))
        );
        assert_eq!(*provider.asked.lock().unwrap(), ["down"]);
        let (_, mut held) = fake(Kind::Subscription, &["refused", "refused too"]);
        assert_eq!(read(&mut held, now()).problem, Some(Problem::SignIn));
    }

    /// `guard` waits on the read in an agent's hook, so three keys take as
    /// long as one, and the account's own limits are asked once.
    #[test]
    fn every_key_of_an_api_key_account_is_read_at_once() {
        let (provider, mut held) = fake(Kind::ApiKey, &["slow a", "slow b", "slow c"]);
        let began = Instant::now();
        let limits = read(&mut held, now());
        // One after another, they'd take 900 ms.
        assert!(began.elapsed() < Duration::from_millis(600));
        assert_eq!(
            keys(&limits),
            ["credits", "key-slow a", "key-slow b", "key-slow c"]
        );
        let asked = provider.asked.lock().unwrap();
        let credits = asked.iter().filter(|asked| *asked == "credits").count();
        assert_eq!(credits, 1);
        // A key without access to them gives way to the next.
        let (_, mut held) = fake(Kind::ApiKey, &["a nocredits", "b"]);
        assert_eq!(
            keys(&read(&mut held, now())),
            ["credits", "key-a nocredits", "key-b"]
        );
    }

    /// A key that can't be read leaves its limit out, so the account isn't
    /// shown as read in full; what was read is kept. A refusal is said over
    /// a provider out of reach, and the longest wait a provider asked for
    /// over none, with its reason, whichever key it was.
    #[test]
    fn a_key_that_cant_be_read_is_said() {
        let (_, mut held) = fake(Kind::ApiKey, &["a", "down b"]);
        let limits = read(&mut held, now());
        assert_eq!(keys(&limits), ["credits", "key-a"]);
        assert_eq!(limits.problem, Some(Problem::unavailable("down")));
        let (_, mut held) = fake(Kind::ApiKey, &["down a", "refused b", "down c"]);
        assert_eq!(read(&mut held, now()).problem, Some(Problem::SignIn));
        let busy = Some(Problem::Unavailable {
            why: "busy".to_owned(),
            retry_after: Some(600_000),
            answered: true,
        });
        for keys in [["busy a", "down b"], ["down a", "busy b"]] {
            let (_, mut held) = fake(Kind::ApiKey, &keys);
            assert_eq!(read(&mut held, now()).problem, busy);
        }
    }

    /// `folder` signed in to `account`, by its key when it's `fake:key-` one.
    fn signed(folder: &str, account: &str) -> SignedIn {
        SignedIn {
            agent: "fake",
            folder: folder.to_owned(),
            provider: "fake".to_owned(),
            account: Some(account.to_owned()),
            key: account.strip_prefix("fake:key-").map(str::to_owned),
        }
    }

    /// The fake provider's account, as signed in to in one folder.
    fn signed_in() -> Vec<SignedIn> {
        vec![signed("/f", "fake:account")]
    }

    /// Record a read of the fake provider's account that ended in `problem`
    /// at `at`, and give how the status then says it stands.
    fn failed(db: &mut Connection, problem: Problem, at: i64) -> Option<crate::status::Stale> {
        let (_, held) = fake(Kind::Subscription, &[]);
        record(db, signed_in(), vec![(Read::failed(problem), held)], at).unwrap();
        let status = crate::status::status(db, at).unwrap();
        status.accounts[0].state.why()
    }

    /// An agent may replace its token while a read sends it: refused once,
    /// it's read at the next try, so it isn't said to need signing in.
    #[test]
    fn a_login_refused_twice_in_a_row_needs_signing_in_again() {
        use crate::status::Stale;
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let minutes = |count: i64| count * crate::time::MINUTE;
        assert_eq!(
            failed(&mut db, Problem::SignIn, minutes(5)),
            Some(Stale::ReadFailed)
        );
        // Asked for as the agent writes the login it renewed, it's read at
        // once.
        assert!(due(&db, minutes(5) + 1_000, Ask::Opened));
        assert_eq!(
            failed(&mut db, Problem::SignIn, minutes(10)),
            Some(Stale::SignIn)
        );
        // Out of reach between two refusals: they aren't in a row.
        failed(&mut db, Problem::unavailable("down"), minutes(15));
        assert_eq!(
            failed(&mut db, Problem::SignIn, minutes(20)),
            Some(Stale::ReadFailed)
        );
    }

    /// When the fake provider's account is read next, as `failing` says.
    fn retry_at(db: &Connection) -> Option<i64> {
        one(db, "SELECT max(retry_at) FROM failing")
    }

    #[test]
    fn a_provider_that_keeps_failing_is_asked_less_often() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let minute = crate::time::MINUTE;
        let mut at = 0;
        for waits in [5, 10, 20, 30, 30] {
            at += 60 * minute;
            failed(&mut db, Problem::unavailable("down"), at);
            assert_eq!(retry_at(&db), Some(at + waits * minute));
        }
        // As long as it asks, up to as long as any wait.
        let asks = |seconds: i64| Problem::Unavailable {
            why: "busy".to_owned(),
            retry_after: Some(seconds * 1000),
            answered: true,
        };
        let (_, held) = fake(Kind::Subscription, &[]);
        record(
            &mut db,
            signed_in(),
            vec![(Read::failed(asks(0)), held)],
            at,
        )
        .unwrap();
        assert_eq!(retry_at(&db), Some(at + 30 * minute));
        // A read that works ends it.
        let (_, held) = fake(Kind::Subscription, &[]);
        record(&mut db, signed_in(), vec![(monthly(1), held)], at).unwrap();
        assert_eq!(retry_at(&db), None);
        // Nor is a read of some keys, as the provider answered.
        let some = Read {
            problem: Some(Problem::unavailable("down")),
            ..monthly(1)
        };
        let (_, held) = fake(Kind::ApiKey, &[]);
        record(&mut db, signed_in(), vec![(some, held)], at).unwrap();
        assert_eq!(retry_at(&db), None);
        // Nor is a provider that didn't answer, as when the Mac is offline,
        // however long it lasts.
        for _ in 0..6 {
            let offline = Problem::Unavailable {
                why: "host not found".to_owned(),
                retry_after: None,
                answered: false,
            };
            failed(&mut db, offline, at);
            assert_eq!(retry_at(&db), None);
        }
        // And the first that answers after them waits five minutes.
        failed(&mut db, Problem::unavailable("down"), at);
        assert_eq!(retry_at(&db), Some(at + 5 * minute));
        // A refused login isn't backed off.
        failed(&mut db, Problem::SignIn, at);
        failed(&mut db, Problem::SignIn, at + 5 * minute);
        assert_eq!(retry_at(&db), None);
        // Waiting, it isn't read, even when opened or forced: until its wait is
        // out, or unless the wait is longer than any (clock set back).
        failed(&mut db, asks(3600), at);
        assert!(!due(&db, at + 5 * minute, Ask::Opened));
        assert!(!due(&db, at + 5 * minute, Ask::Force));
        assert!(due(&db, at + 30 * minute, Ask::Scheduled));
        assert!(due(&db, at - 10 * minute, Ask::Scheduled));
    }

    /// Whether the fake provider's account is read at `now`.
    fn due(db: &Connection, now: i64, ask: Ask) -> bool {
        schedules(db, now).unwrap()["fake:account"].due(now, ask)
    }

    /// The fake provider's account read at `at`, its limit `used` and
    /// resetting at `resets`.
    fn read_at(db: &mut Connection, at: i64, used: f64, resets: i64) {
        let mut read = monthly(resets);
        read.limits.as_mut().unwrap().readings[0].used = used;
        let (_, held) = fake(Kind::Subscription, &[]);
        record(db, signed_in(), vec![(read, held)], at).unwrap();
    }

    #[test]
    fn an_account_is_read_as_often_as_its_use_needs() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let minute = crate::time::MINUTE;
        let at = 1_000 * crate::time::DAY;
        read_at(&mut db, at, 10.0, at + 300 * minute);
        // Not in use, its limits stand 20 minutes, 5 when opened, or CLAIM when forced.
        assert!(!due(&db, at + 10 * minute, Ask::Scheduled));
        assert!(due(&db, at + IDLE, Ask::Scheduled));
        assert!(!due(&db, at + 2 * minute, Ask::Opened));
        assert!(!due(&db, at + 10_000, Ask::Force));
        assert!(due(&db, at + 2 * minute, Ask::Force));
        assert!(due(&db, at + DUE, Ask::Opened));
        // Drawn on, it's in use: they stand 5.
        let fake = ("fake", "/f", "fake", Some("fake:account"));
        response(&db, fake, "r1", at + minute, "m", None);
        assert!(!due(&db, at + 4 * minute, Ask::Scheduled));
        assert!(due(&db, at + DUE, Ask::Scheduled));

        // Once a window resets, it's read, in use or not; and once read
        // after, not again, though the provider still gives the old window.
        let at = at + 60 * minute;
        read_at(&mut db, at, 10.0, at + 8 * minute);
        assert!(!due(&db, at + 7 * minute, Ask::Scheduled));
        assert!(due(&db, at + 8 * minute, Ask::Scheduled));
        read_at(&mut db, at + 9 * minute, 10.0, at + 8 * minute);
        assert!(!due(&db, at + 10 * minute, Ask::Scheduled));

        // A limit that rose at its last read, as use elsewhere makes it, puts
        // it in use; not one that's of a new window.
        let in_use =
            |db: &Connection, now| crate::status::status(db, now).unwrap().accounts[0].in_use;
        read_at(&mut db, at + 20 * minute, 10.0, at + 300 * minute);
        assert!(!due(&db, at + 25 * minute, Ask::Scheduled));
        read_at(&mut db, at + 30 * minute, 20.0, at + 300 * minute);
        assert!(due(&db, at + 35 * minute, Ask::Scheduled));
        assert!(in_use(&db, at + 35 * minute));

        // Nor one with no start that rose from a reading over a day before,
        // as after a day asleep.
        let (at, resets) = (at + 40 * minute, at + 30 * crate::time::DAY);
        read_at(&mut db, at, 30.0, resets);
        let at = at + crate::time::DAY + minute;
        read_at(&mut db, at, 40.0, resets);
        assert!(!in_use(&db, at + DUE));
        assert!(!due(&db, at + DUE, Ask::Scheduled));
    }

    /// Codex's limits for the fake provider's account, as its history gave
    /// them at `at`.
    fn in_band(at: i64, used: f64) -> agents::InBand {
        agents::InBand {
            provider: "fake".to_owned(),
            at,
            readings: vec![Reading {
                used,
                resets: Some(1_001 * crate::time::DAY),
                ..limit("five")
            }],
        }
    }

    /// Take in `snapshot` at `now`, and give whether it was.
    fn take(db: &mut Connection, snapshot: &agents::InBand, now: i64) -> bool {
        let tx = crate::db::write(db).unwrap();
        let took = record_in_band(&tx, "fake:account", snapshot, now).unwrap();
        tx.commit().unwrap();
        took
    }

    /// What the status shows left of each limit of the first account at
    /// `now`.
    fn left(db: &Connection, now: i64) -> Vec<(String, u8)> {
        let status = crate::status::status(db, now).unwrap();
        let limits = &status.accounts[0].limits;
        let left = limits
            .iter()
            .map(|limit| (limit.key.clone(), limit.left_percent));
        left.collect()
    }

    /// As OpenRouter's key limit read without its credits.
    #[test]
    fn a_read_of_some_limits_leaves_the_others_as_last_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let minute = crate::time::MINUTE;
        let (_, held) = fake(Kind::ApiKey, &[]);
        let both = Read::of(vec![limit("credits"), limit("key")].into());
        record(&mut db, signed_in(), vec![(both, held)], minute).unwrap();
        let some = Read {
            problem: Some(Problem::unavailable("down")),
            ..Read::of(vec![limit("key")].into())
        };
        let (_, held) = fake(Kind::ApiKey, &[]);
        record(&mut db, signed_in(), vec![(some, held)], 5 * minute).unwrap();
        let shown: Vec<_> = left(&db, 5 * minute)
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(shown, ["credits", "key"]);
        // The account is as of the read of them all; and a first read of
        // none isn't a read.
        let state = |db: &Connection| {
            let status = crate::status::status(db, 5 * minute).unwrap();
            status.accounts[0].state.clone()
        };
        assert!(
            matches!(state(&db), crate::status::State::AsOf { read_at, .. } if read_at == minute)
        );
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let none = Read {
            problem: Some(Problem::unavailable("down")),
            ..Read::of(Vec::new().into())
        };
        let (_, held) = fake(Kind::ApiKey, &[]);
        record(&mut db, signed_in(), vec![(none, held)], 5 * minute).unwrap();
        assert!(matches!(state(&db), crate::status::State::Unread { .. }));
    }

    #[test]
    fn limits_an_agent_writes_move_read_at_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let minute = crate::time::MINUTE;
        let at = 1_000 * crate::time::DAY;
        let (_, held) = fake(Kind::Subscription, &[]);
        let read = Read::of(vec![limit("five"), limit("seven:Opus")].into());
        record(&mut db, signed_in(), vec![(read, held)], at).unwrap();
        failed(&mut db, Problem::unavailable("down"), at + 5 * minute);
        let now = at + 10 * minute;
        // Older than the last read of all of them, or stamped ahead of now,
        // it's left.
        assert!(!take(&mut db, &in_band(at - minute, 30.0), now));
        assert!(!take(&mut db, &in_band(at + 11 * minute, 30.0), now));
        assert!(take(&mut db, &in_band(at + 6 * minute, 30.0), now));
        // The same again adds nothing.
        assert!(!take(&mut db, &in_band(at + 7 * minute, 30.0), now));

        let read = format!("{}|{at}|unavailable", at + 6 * minute);
        let sql = "SELECT read_at, full_read_at, problem FROM account";
        assert_eq!(rows(&db, sql), [read]);
        // Newer than the read that failed, the status has it current, the
        // limit it didn't write as the last read gave it.
        let status = crate::status::status(&db, at + 7 * minute).unwrap();
        assert!(matches!(
            status.accounts[0].state,
            crate::status::State::Live { .. }
        ));
        let shown = left(&db, at + 7 * minute);
        assert_eq!(
            shown,
            [("five".to_owned(), 70), ("seven:Opus".to_owned(), 90)]
        );
        // A read that fails after it is said again.
        failed(&mut db, Problem::unavailable("down"), at + 8 * minute);
        let status = crate::status::status(&db, at + 8 * minute).unwrap();
        assert!(matches!(
            status.accounts[0].state,
            crate::status::State::AsOf { .. }
        ));
    }

    #[test]
    fn a_read_once_the_clock_is_set_back_stands() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let minute = crate::time::MINUTE;
        let at = 1_000 * crate::time::DAY;
        let read = |db: &mut Connection, at: i64, used: f64| {
            let (_, held) = fake(Kind::Subscription, &[]);
            let read = Read::of(in_band(at, used).readings.into());
            record(db, signed_in(), vec![(read, held)], at).unwrap();
        };
        // Read with the clock an hour ahead, then set right.
        read(&mut db, at + 60 * minute, 10.0);
        read(&mut db, at + 6 * minute, 50.0);
        assert_eq!(left(&db, at + 7 * minute), [("five".to_owned(), 50)]);
        assert!(take(
            &mut db,
            &in_band(at + 8 * minute, 70.0),
            at + 9 * minute
        ));
        assert_eq!(left(&db, at + 9 * minute), [("five".to_owned(), 30)]);
        // So does a read of some limits only.
        read(&mut db, at + 70 * minute, 10.0);
        let some = Read {
            problem: Some(Problem::unavailable("down")),
            ..Read::of(in_band(at, 60.0).readings.into())
        };
        let (_, held) = fake(Kind::Subscription, &[]);
        record(&mut db, signed_in(), vec![(some, held)], at + 10 * minute).unwrap();
        assert_eq!(left(&db, at + 11 * minute), [("five".to_owned(), 40)]);
    }

    #[test]
    fn an_account_never_read_in_full_shows_every_limit_an_agent_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let minute = crate::time::MINUTE;
        let at = 1_000 * crate::time::DAY;
        failed(&mut db, Problem::unavailable("down"), at);
        for (at, five) in [(at + minute, 30.0), (at + 2 * minute, 40.0)] {
            let mut snapshot = in_band(at, five);
            snapshot.readings.push(Reading {
                resets: Some(1_007 * crate::time::DAY),
                ..limit("seven")
            });
            assert!(take(&mut db, &snapshot, at));
        }
        // The second wrote the limit that moved alone: the other still shows.
        let shown = left(&db, at + 3 * minute);
        assert_eq!(shown, [("five".to_owned(), 60), ("seven".to_owned(), 90)]);
    }

    #[test]
    fn limits_asked_for_that_no_provider_answers_leave_the_account_as_it_was() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut db = crate::db::open(data.path()).unwrap();
        let fake: &'static Fake = Box::leak(Box::default());
        let folder = home.path().join(".fake");
        std::fs::create_dir(&folder).unwrap();
        let login = |key: &str| {
            std::fs::write(folder.join("login"), format!(r#"{{"fake": "{key}"}}"#)).unwrap();
        };
        let refresh = |db: &mut Connection, asked: bool| {
            // As if no process had claimed reading them.
            db.execute("UPDATE state SET limits_at = NULL", []).unwrap();
            refresh_with(
                db,
                home.path(),
                &[&FakeAgent::default()],
                &[fake],
                if asked { Ask::Opened } else { Ask::Scheduled },
            )
            .unwrap()
        };
        let account = |db: &Connection| -> (i64, Option<String>) {
            db.query_row("SELECT checked_at, problem FROM account", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap()
        };
        login("key-nocredits");
        assert!(refresh(&mut db, false));
        // Due whether asked for or not.
        let ago = now() - IDLE - crate::time::MINUTE;
        db.execute(
            "UPDATE account SET read_at = ?1, full_read_at = ?1, checked_at = ?1",
            [ago],
        )
        .unwrap();

        // Offline, as before the network is back once the Mac wakes.
        login("offline-nocredits");
        assert!(refresh(&mut db, true));
        assert_eq!(account(&db), (ago, None));
        // A read it wasn't asked for says so.
        assert!(refresh(&mut db, false));
        let (checked, problem) = account(&db);
        assert!(checked > ago);
        assert_eq!(problem.as_deref(), Some("unavailable"));
    }

    /// A read asked for takes no notice of another process's claim, but
    /// waits out its read, then asks only what that didn't: both refused,
    /// they'd count as two refusals in a row, and ask to sign in again. One
    /// as limits come due gives way at once.
    #[test]
    fn a_read_asked_for_waits_out_another_processs() {
        // Refresh Limits beside a read, with a login to fake-too written
        // during it; and an agent writing a login beside a read refused, with
        // fake-too read a minute ago, too recently for it.
        for (key, ask) in [("slow key", Ask::Force), ("slow refused key", Ask::Opened)] {
            let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let fake: &'static Fake = Box::leak(Box::default());
            let too: &'static Fake = Box::leak(Box::new(Fake {
                info: &FAKE_TOO,
                ..Fake::default()
            }));
            let folder = home.path().join(".fake");
            std::fs::create_dir(&folder).unwrap();
            let login = |logins: &str| std::fs::write(folder.join("login"), logins).unwrap();
            let logins = format!(r#"{{"fake": "{key}", "fake-too": "key"}}"#);
            login(&match ask {
                Ask::Opened => logins.clone(),
                _ => format!(r#"{{"fake": "{key}"}}"#),
            });
            let refresh = |db: &mut Connection, ask| {
                refresh_with(db, home.path(), &[&FakeAgent::default()], &[fake, too], ask).unwrap()
            };
            let open = || crate::db::open(data.path()).unwrap();
            let (mut other, mut db) = (open(), open());
            // Fake read an hour ago, so both reads find it due.
            assert!(refresh(&mut db, Ask::Opened));
            db.execute_batch(
                "UPDATE account SET read_at = read_at - 3600000,
                     full_read_at = full_read_at - 3600000, checked_at = checked_at - 3600000
                     WHERE id LIKE 'fake:%';
                 UPDATE account SET read_at = read_at - 60000,
                     full_read_at = full_read_at - 60000, checked_at = checked_at - 60000
                     WHERE id LIKE 'fake-too:%';
                 UPDATE state SET limits_at = limits_at - 3600000",
            )
            .unwrap();
            std::thread::scope(|scope| {
                scope.spawn(|| refresh(&mut other, Ask::Scheduled));
                std::thread::sleep(Duration::from_millis(100));
                login(&logins);
                let asked = Instant::now();
                assert!(!refresh(&mut open(), Ask::Scheduled));
                assert!(asked.elapsed() < Duration::from_millis(100));
                assert!(refresh(&mut db, ask));
            });
            // Fake by the other read; fake-too once, by this one or before.
            for (provider, key, reads) in [(fake, key, 2), (too, "key", 1)] {
                let asked = provider.asked.lock().unwrap();
                let count = asked.iter().filter(|asked| *asked == key).count();
                assert_eq!(count, reads, "{asked:?}");
            }
        }
    }

    #[test]
    fn a_read_that_fails_differently_from_the_last_is_logged() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let minute = crate::time::MINUTE;
        let busy = || Problem::unavailable("https://x: answered 503");
        failed(&mut db, busy(), 5 * minute);
        failed(&mut db, busy(), 10 * minute);
        failed(
            &mut db,
            Problem::unavailable("https://x: no answer in time"),
            15 * minute,
        );
        failed(&mut db, Problem::SignIn, 20 * minute);
        failed(&mut db, Problem::SignIn, 25 * minute);
        failed(&mut db, busy(), 30 * minute);
        let log = std::fs::read_to_string(dir.path().join("app.log")).unwrap();
        let lines: Vec<&str> = log
            .lines()
            .map(|line| line.split_once(' ').unwrap().1)
            .collect();
        assert_eq!(
            lines,
            [
                "Fake: limits not read: https://x: answered 503",
                "Fake: limits not read: https://x: no answer in time",
                "Fake: its login was refused",
                "Fake: limits not read: https://x: answered 503",
            ]
        );
    }

    fn held(id: &str) -> Held {
        Held {
            provider: Box::leak(Box::<Fake>::default()),
            account: providers::Account {
                id: id.to_owned(),
                kind: Kind::Subscription,
                label: Some(format!("key {id}")),
            },
            credentials: Vec::new(),
        }
    }

    fn monthly(resets: i64) -> Read {
        Read::of(
            vec![Reading {
                resets: Some(resets),
                ..limit("monthly")
            }]
            .into(),
        )
    }

    #[test]
    fn a_login_held_in_two_folders_is_sent_once() {
        let mut held = held("fake:key-a");
        for secret in ["sk-go-a", "sk-go-a", "sk-go-b"] {
            held.hold(credential(secret, true, None));
        }
        assert_eq!(held.credentials.len(), 2);
    }

    #[test]
    fn an_expired_login_is_sent_with_what_renews_it() {
        let (_, mut held) = fake(Kind::Subscription, &[]);
        let renews = agents::Refresh {
            token: agents::Secret::new("refresh-token"),
            programs: Vec::new(),
        };
        held.credentials.push(Credential {
            refresh: Some(renews),
            ..credential("expired-token", false, Some(1_000_000 - 60_000))
        });
        let limits = read(&mut held, 1_000_000);
        assert_eq!(
            (keys(&limits), &limits.problem),
            (vec!["key-expired-token"], &None)
        );
        // Without it, one with under a minute left isn't sent either.
        held.credentials[0] = credential("sk-a", false, Some(1_000_000 + 30_000));
        assert_eq!(read(&mut held, 1_000_000).problem, Some(Problem::Expired));
    }

    /// An agent whose own folder is `.fake`, installed once it holds a
    /// `login`, and a second account's `.fake-` and a name, once signed in
    /// there. A folder's `login` names the key it holds for each provider.
    /// It notes whether it was asked for logins.
    #[derive(Default)]
    struct FakeAgent {
        asked: AtomicBool,
    }

    static FAKE_AGENT: agents::Info = agents::Info {
        id: "fake",
        name: "Fake",
        command: "fake",
        app: None,
        mcp_add: &[],
        mcp_remove: None,
        folder_var: None,
        charges: false,
        session_var: None,
        login_file: None,
    };

    impl Agent for FakeAgent {
        fn info(&self) -> &'static agents::Info {
            &FAKE_AGENT
        }

        fn folders(&self, home: &Path) -> Vec<std::path::PathBuf> {
            agents::own_and_found(home, ".fake", ".fake-", "login")
        }

        fn installed(&self, folder: &Path) -> bool {
            folder.join("login").exists()
        }

        fn files(&self, _folder: &Path) -> Vec<std::path::PathBuf> {
            Vec::new()
        }

        fn read(&self, _file: &Path, _cursor: &str) -> Result<agents::Read> {
            Ok(agents::Read::default())
        }

        fn logins(&self, folder: &Path, _home: &Path) -> Result<Vec<agents::Login>> {
            self.asked.store(true, Ordering::Relaxed);
            let Some(kept) = agents::json_file(&folder.join("login"))? else {
                return Ok(Vec::new());
            };
            let kept = kept.as_object().into_iter().flatten();
            Ok(kept
                .map(|(provider, key)| agents::Login {
                    provider: provider.clone(),
                    credential: [FAKE.id, FAKE_TOO.id]
                        .into_iter()
                        .find(|id| id == provider)
                        .map(|reads| Credential {
                            provider: reads,
                            ..credential(key.as_str().unwrap_or_default(), true, None)
                        }),
                })
                .collect())
        }

        fn mcp_command(&self, _folder: &Path, _home: &Path) -> Option<String> {
            None
        }
    }

    /// `agent`'s folders under `home`, read by the two fake providers.
    fn gathered(db: &Connection, home: &Path, agent: &FakeAgent) -> Gathered {
        let fake: &'static Fake = Box::leak(Box::default());
        let too: &'static Fake = Box::leak(Box::new(Fake {
            info: &FAKE_TOO,
            ..Fake::default()
        }));
        gather(db, home, &[agent], &[fake, too]).unwrap()
    }

    #[test]
    fn a_key_keeps_only_an_account_of_the_same_provider() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let db = crate::db::open(data.path()).unwrap();
        // One key kept for two providers, as one pasted in the wrong place.
        let key = "made-up";
        let folder = home.path().join(".fake");
        std::fs::create_dir(&folder).unwrap();
        let kept = serde_json::json!({"fake": key, "fake-too": key});
        std::fs::write(folder.join("login"), kept.to_string()).unwrap();
        // Its account at the first was made one with another key's. Its
        // sign-in to the second is the later.
        for (provider, since, account) in [
            ("fake", 1000, "fake:another"),
            ("fake-too", 2000, "fake-too:api"),
        ] {
            db.execute(
                "INSERT INTO sign_in (agent, folder, provider, since, account, key)
                 VALUES ('fake', ?1, ?2, ?3, ?4, ?5)",
                params![
                    folder.to_string_lossy(),
                    provider,
                    since,
                    account,
                    agents::Secret::new(key).fingerprint()
                ],
            )
            .unwrap();
        }
        let Gathered {
            signed_in,
            accounts,
            ..
        } = gathered(&db, home.path(), &FakeAgent::default());
        let signed: Vec<_> = signed_in
            .iter()
            .map(|signed| (signed.provider.as_str(), signed.account.as_deref()))
            .collect();
        assert_eq!(
            signed,
            [
                ("fake", Some("fake:another")),
                ("fake-too", Some("fake-too:api")),
            ]
        );
        // Each is read by its own provider.
        let read: Vec<_> = accounts
            .iter()
            .map(|(id, held)| (id.as_str(), held.provider.info().id))
            .collect();
        assert_eq!(
            read,
            [("fake-too:api", "fake-too"), ("fake:another", "fake")]
        );
    }

    #[test]
    fn what_is_read_is_recorded_and_one_plan_keeps_its_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        // A response in each folder before any sign-in, and one later.
        for (id, folder, at) in [("r1", "/f1", 500), ("r2", "/f2", 500), ("r3", "/f1", 3_100)] {
            response(&db, ("fake", folder, "fake", None), id, at, "m", None);
        }
        // Key b is read alone, then key a, to the same plan, joins it; key
        // c, to another plan, and key d, not read, stay their own.
        record(
            &mut db,
            vec![signed("/f1", "fake:key-b")],
            vec![(monthly(1), held("fake:key-b"))],
            1_000,
        )
        .unwrap();
        db.execute("UPDATE account SET hidden = 1 WHERE id = 'fake:key-b'", [])
            .unwrap();
        record(
            &mut db,
            vec![signed("/f1", "fake:key-b"), signed("/f2", "fake:key-a")],
            vec![
                (monthly(1), held("fake:key-a")),
                (monthly(1), held("fake:key-b")),
                (monthly(2), held("fake:key-c")),
                (
                    Read::failed(Problem::unavailable("down")),
                    held("fake:key-d"),
                ),
            ],
            2_000,
        )
        .unwrap();
        assert_eq!(
            rows(&db, "SELECT id, hidden FROM account ORDER BY id"),
            ["fake:key-b|1", "fake:key-c|0", "fake:key-d|0"]
        );
        let readings = "SELECT count(*) FROM reading WHERE account = 'fake:key-b'";
        assert_eq!(one::<i64>(&db, readings), 2);

        // A read that fails keeps the title, and says why. The first folder
        // now holds key c.
        db.execute(
            "UPDATE account SET title = 'Fake Max' WHERE id = 'fake:key-b'",
            [],
        )
        .unwrap();
        record(
            &mut db,
            vec![signed("/f1", "fake:key-c")],
            vec![(Read::failed(Problem::Expired), held("fake:key-b"))],
            3_000,
        )
        .unwrap();
        // A folder's first sign-in is in force from the start, as the
        // plan's account it joined, and a later one from when it was read.
        assert_eq!(
            rows(&db, "SELECT id, account FROM response ORDER BY id"),
            ["r1|fake:key-b", "r2|fake:key-b", "r3|fake:key-c"]
        );
        let sql = "SELECT title, problem FROM account WHERE id = 'fake:key-b'";
        assert_eq!(rows(&db, sql), ["Fake Max|expired"]);
    }

    #[test]
    fn limits_read_once_a_newer_turnscope_has_written_are_not_recorded() {
        // As when it wrote while the providers answered.
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        db.execute("UPDATE state SET version = ?1", [i64::MAX])
            .unwrap();
        let error = record(
            &mut db,
            Vec::new(),
            vec![(monthly(1), held("fake:key-b"))],
            1_000,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), crate::db::newer().to_string());
        assert_eq!(one::<i64>(&db, "SELECT count(*) FROM account"), 0);
    }

    #[test]
    fn a_key_swapped_for_another_to_the_same_plan_keeps_its_account() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let (a, b) = ("fake:key-a", "fake:key-b");
        record(
            &mut db,
            vec![signed("/f1", a)],
            vec![(monthly(1), held(a))],
            1_000,
        )
        .unwrap();
        record(
            &mut db,
            vec![signed("/f1", b)],
            vec![(monthly(1), held(b))],
            2_000,
        )
        .unwrap();
        let accounts = rows(&db, "SELECT id, read_at, label FROM account");
        assert_eq!(accounts, ["fake:key-a|2000|key fake:key-b"]);
        let signed_in = "SELECT account FROM sign_in ORDER BY since DESC LIMIT 1";
        assert_eq!(one::<String>(&db, signed_in), a);
    }

    #[test]
    fn a_working_key_for_the_same_plan_replaces_a_failing_ones_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let accounts = "SELECT id, read_at, label, problem FROM account ORDER BY id";
        record(
            &mut db,
            vec![signed("/f1", "fake:key-a")],
            vec![(monthly(1), held("fake:key-a"))],
            1_000,
        )
        .unwrap();
        // Key a is refused, as when revoked, while key b in another folder
        // reads the same plan: b's read is a's account's.
        record(
            &mut db,
            vec![signed("/f1", "fake:key-a"), signed("/f2", "fake:key-b")],
            vec![
                (Read::failed(Problem::SignIn), held("fake:key-a")),
                (monthly(1), held("fake:key-b")),
            ],
            2_000,
        )
        .unwrap();
        assert_eq!(rows(&db, accounts), ["fake:key-a|2000|key fake:key-b|NULL"]);
        let readings = "SELECT count(*) FROM reading WHERE account = 'fake:key-a'";
        assert_eq!(one::<i64>(&db, readings), 2);

        // Failing otherwise, key a may still be read: key c stays its own.
        let folders = || vec![signed("/f1", "fake:key-a"), signed("/f3", "fake:key-c")];
        let down = Read::failed(Problem::unavailable("down"));
        let answers = vec![(down, held("fake:key-a")), (monthly(1), held("fake:key-c"))];
        record(&mut db, folders(), answers, 3_000).unwrap();
        assert_eq!(
            rows(&db, accounts),
            [
                "fake:key-a|2000|key fake:key-a|unavailable",
                "fake:key-c|3000|key fake:key-c|NULL"
            ]
        );
        // Made one once a is refused, as 0.3.0 did, with a hidden as the
        // copy: the plan still shows.
        db.execute("UPDATE account SET hidden = 1 WHERE id = 'fake:key-a'", [])
            .unwrap();
        let refused = Read::failed(Problem::SignIn);
        let answers = vec![
            (refused, held("fake:key-a")),
            (monthly(1), held("fake:key-c")),
        ];
        record(&mut db, folders(), answers, 4_000).unwrap();
        let sql = "SELECT id, hidden FROM account";
        assert_eq!(rows(&db, sql), ["fake:key-c|0"]);
    }

    #[test]
    fn a_login_gone_is_signed_out_of_and_an_uninstalled_agent_isnt_asked() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let db = crate::db::open(data.path()).unwrap();
        let agent = FakeAgent::default();
        let folder = |name: &str| home.path().join(name).to_string_lossy().into_owned();
        let signed = |home: &Path| -> Vec<(String, Option<String>)> {
            let mut signed: Vec<_> = gathered(&db, home, &agent)
                .signed_in
                .into_iter()
                .map(|signed| (signed.folder, signed.account))
                .collect();
            signed.sort();
            signed
        };
        // Neither missing nor a folder made by another tool (as Gemini CLI's
        // `~/.gemini` for Antigravity) counts as installed.
        assert!(signed(home.path()).is_empty());
        std::fs::create_dir(folder(".fake")).unwrap();
        assert!(signed(home.path()).is_empty());
        // Signed in when last read: the own folder, a second account's
        // deleted since, and one whose sign-in is half written.
        for name in [".fake", ".fake-gone", ".fake-written"] {
            db.execute(
                "INSERT INTO sign_in (agent, folder, provider, since, account)
                 VALUES ('fake', ?1, 'fake', 1000, 'fake:account')",
                [folder(name)],
            )
            .unwrap();
        }
        let all = [".fake", ".fake-gone", ".fake-written"].map(|name| (folder(name), None));
        assert_eq!(signed(home.path()), all);
        assert!(!agent.asked.load(Ordering::Relaxed), "logins was asked");
        // The one that can't be read keeps what it held.
        std::fs::create_dir(folder(".fake-written")).unwrap();
        std::fs::write(home.path().join(".fake-written/login"), "{").unwrap();
        assert_eq!(signed(home.path()), all[..2]);
        // A home that can't be listed, as one on a volume that's away, says
        // nothing of its folders.
        assert!(signed(&home.path().join("away")).is_empty());
    }

    #[test]
    fn a_login_that_cant_be_read_fails_its_accounts_read_without_signing_it_out() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut db = crate::db::open(data.path()).unwrap();
        let folder = home.path().join(".fake");
        std::fs::create_dir(&folder).unwrap();
        // A login half written, to an account whose login had expired.
        std::fs::write(folder.join("login"), "{").unwrap();
        db.execute(
            "INSERT INTO account (id, provider, kind, title, read_at, problem)
             VALUES ('fake:api', 'fake', 'subscription', 'Fake Plan', 1000, 'expired')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO sign_in (agent, folder, provider, since, account)
             VALUES ('fake', ?1, 'fake', 1000, 'fake:api')",
            [folder.to_string_lossy().as_ref()],
        )
        .unwrap();
        let first = gathered(&db, home.path(), &FakeAgent::default());
        assert!(first.signed_in.is_empty(), "signed out");
        assert!(first.accounts.is_empty());
        record(&mut db, first.signed_in, first.unreadable, 2_000).unwrap();

        // It couldn't be read, which isn't backed off, and the log says why.
        let problem = "SELECT problem FROM account WHERE id = 'fake:api'";
        assert_eq!(one::<String>(&db, problem), "unavailable");
        assert_eq!(retry_at(&db), None);
        let log = std::fs::read_to_string(data.path().join("app.log")).unwrap();
        assert!(log.contains(": limits not read: reading"), "{log}");

        // Another folder's login to it is read instead.
        let second = home.path().join(".fake-work");
        std::fs::create_dir(&second).unwrap();
        std::fs::write(second.join("login"), r#"{"fake":"good-key"}"#).unwrap();
        let covered = gathered(&db, home.path(), &FakeAgent::default());
        assert!(covered.accounts.contains_key("fake:api"));
        assert!(covered.unreadable.is_empty());
    }
}
