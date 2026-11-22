//! Reading accounts' limits: every login the agents hold, grouped by the
//! account it's to, each account asked of its provider once.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::agents::{self, Agent, Credential};
use crate::providers::{self, Kind, Limits, Problem, Provider, Reading};
use crate::{Result, now};

/// How long an active account's limits stand before any process reads them
/// again, and how often login discovery runs to spot sign-ins and sign-outs.
pub const DUE: i64 = 5 * crate::time::MINUTE;

/// How long an idle account's limits stand before its provider is asked
/// again: 25 minutes, 5 minutes before `status::STALE` (30 minutes), so
/// healthy idle accounts stay live with 5x fewer API requests.
pub const IDLE_DUE: i64 = 25 * crate::time::MINUTE;

/// How recently a response must have drawn on an account for it to count as
/// active when scheduling limit reads.
const ACTIVE_WINDOW: i64 = 15 * crate::time::MINUTE;

/// How recently an in-band reading suppresses the active 5-minute API poll
/// while a full poll has still run within [`IDLE_DUE`].
const IN_BAND_FRESH: i64 = 2 * crate::time::MINUTE;

/// Short concurrency claim window when a specific account becomes due between
/// 5-minute discovery ticks (such as after a window reset or resumed work).
const CLAIM_GAP: i64 = 15 * 1000;

/// Delay past a window's reset time before polling for the fresh counter,
/// giving the provider's backend a moment to roll the window over.
const RESET_GRACE: i64 = 5 * 1000;

/// The longest an account waits between reads while its provider keeps
/// failing (`backoff`).
pub const BACKED_OFF: i64 = 30 * crate::time::MINUTE;

/// How long an account waits for its next read after `reads` reads in a
/// row found its provider busy or not understood: [`DUE`], doubled with
/// each read after the first, up to [`BACKED_OFF`], and as long as the
/// provider asked, up to that too. A read that works ends it.
///
/// Only a provider that answered is backed off. With no answer, as when the
/// Mac is offline, the next read is in five minutes: backed off too, limits
/// stayed unread for up to half an hour after the Mac was back online. With
/// a stand-in provider out of reach for 36 minutes, the first read that
/// worked was at minute 65 (measured 2026-11-18). The cost: a provider
/// that takes the read's 15 s without answering is asked every five
/// minutes, as before.
///
/// Chosen over asking again in five minutes whatever the answer, as before:
/// a provider down, or answering 429, was asked every five minutes for as
/// long as it lasted. At most 30 minutes, so limits are back within half an
/// hour of the provider, and a `Retry-After` beyond that is cut to it, over
/// leaving limits unread for hours on one header. A refused login isn't
/// backed off: signing in again in the agent gives a new login, which the
/// next read, in five minutes, shows. Nor is a read of some of an API-key
/// account's keys, as the provider answered. Not measured: no provider's 429 or
/// `Retry-After` has been seen here.
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

/// How long an account's read is waited for, all its requests together.
/// Where models.dev's catalog gets 60 s: `guard` reads limits inside an
/// agent's hook, which holds the agent until it's done, and an answer of
/// limits is one small JSON object, not a download.
const WAIT: Duration = Duration::from_secs(15);

/// What a read of an account gave: its limits, if any were read, and what
/// went wrong, if anything did. Both, when some of an API-key account's
/// keys were read and others weren't.
struct Read {
    limits: Option<Limits>,
    problem: Option<Problem>,
}

impl Read {
    fn failed(problem: Problem) -> Read {
        Read {
            limits: None,
            problem: Some(problem),
        }
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
        if let Some(known) = self
            .credentials
            .iter_mut()
            .find(|known| known.secret.fingerprint() == fingerprint)
        {
            if known.refresh.is_none() {
                known.refresh = credential.refresh;
            }
            return;
        }
        self.credentials.push(credential);
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

/// What every folder of `agents` holds: each folder's sign-in, each account to
/// read with its logins, and each account whose login folder couldn't be read,
/// with why.
struct Gathered {
    signed_in: Vec<SignedIn>,
    accounts: BTreeMap<String, Held>,
    unreadable: BTreeMap<String, String>,
}

/// Read due accounts' limits (and discover sign-in changes at least every
/// [`DUE`]); whether a refresh ran.
pub fn refresh(db: &mut Connection, home: &Path) -> Result<bool> {
    refresh_with(db, home, false)
}

/// Check logins now and refresh any account that is stale (`>= DUE`), unread,
/// past a window reset, or failing (for panel open, Mac wake, or auth file
/// changes); whether anything changed.
pub fn refresh_stale(db: &mut Connection, home: &Path) -> Result<bool> {
    refresh_with(db, home, true)
}

fn refresh_with(db: &mut Connection, home: &Path, force: bool) -> Result<bool> {
    let now = now();
    let due = due_accounts(db, now, force)?;
    if !claim(db, now, force, !due.is_empty())? {
        return Ok(false);
    }
    let mut gathered = gather(db, home, agents::ALL, providers::ALL)?;
    let known: HashSet<String> = db
        .prepare("SELECT id FROM account")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let newly_signed = newly_signed_in(db, &gathered.signed_in)?;
    let waiting = waiting(db, now, force)?;
    gathered.accounts.retain(|id, _| {
        newly_signed.contains(id)
            || (!waiting.contains(id) && (due.contains(id) || !known.contains(id)))
    });
    // Every due account at once, so the slowest provider sets how long it takes.
    let answers: Vec<(Read, Held)> = std::thread::scope(|scope| {
        let reading: Vec<_> = gathered
            .accounts
            .into_values()
            .map(|mut held| scope.spawn(move || (read(&mut held, now), held)))
            .collect();
        reading
            .into_iter()
            .filter_map(|thread| thread.join().ok())
            .collect()
    });
    let changed = record(db, gathered.signed_in, answers, gathered.unreadable, now)?;
    // On a normal scheduled claim, return true even when no accounts exist yet
    // so the engine announces the initial limit check before history ingest.
    Ok(if force { changed } else { true })
}

/// Signed-in accounts that need a provider poll at `now`.
fn due_accounts(db: &Connection, now: i64, force: bool) -> Result<HashSet<String>> {
    let mut rows = db.prepare(
        "SELECT a.id, a.read_at, a.full_read_at, a.checked_at, a.next_check_at, a.problem,
                f.retry_at,
                EXISTS (SELECT 1 FROM response r WHERE r.account = a.id AND r.at >= ?1) AS active,
                EXISTS (
                    SELECT 1 FROM reading rd
                    WHERE rd.account = a.id
                      AND rd.at = (
                          SELECT max(at) FROM reading r2
                          WHERE r2.account = a.id
                            AND r2.limit_key = rd.limit_key
                            AND r2.at >= coalesce(a.full_read_at, a.read_at, 0)
                      )
                      AND rd.resets IS NOT NULL
                      AND rd.resets + ?3 <= ?2
                      AND (
                          coalesce(a.checked_at, a.read_at) IS NULL
                          OR coalesce(a.checked_at, a.read_at) < rd.resets
                      )
                ) AS reset_passed
         FROM account a
         LEFT JOIN failing f ON f.account = a.id
         WHERE EXISTS (
             SELECT 1 FROM sign_in s
             WHERE s.account = a.id
               AND s.since = (SELECT max(since) FROM sign_in WHERE folder = s.folder AND provider = s.provider)
         )",
    )?;
    let mut due = HashSet::new();
    let mut rows = rows.query(params![now - ACTIVE_WINDOW, now, RESET_GRACE])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let read_at: Option<i64> = row.get(1)?;
        let full_read_at: Option<i64> = row.get(2)?;
        let checked_at: Option<i64> = row.get(3)?;
        let next_check_at: Option<i64> = row.get(4)?;
        let problem: Option<String> = row.get(5)?;
        let retry_at: Option<i64> = row.get(6)?;
        let active: bool = row.get(7)?;
        let reset_passed: bool = row.get(8)?;

        let backed_off = retry_at.is_some_and(|at| at > now && at <= now + BACKED_OFF);
        let last_check = checked_at.or(read_at);
        let is_due = if force {
            let backoff_blocks =
                backed_off && last_check.is_some_and(|at| now - at < DUE && at <= now + DUE);
            !backoff_blocks
                && (problem.is_some()
                    || read_at.is_none()
                    || reset_passed
                    || read_at.is_some_and(|at| now - at >= DUE || at > now + DUE))
        } else if backed_off {
            false
        } else if reset_passed {
            true
        } else {
            let in_band_fresh = read_at.is_some_and(|at| {
                now - at < IN_BAND_FRESH
                    && at <= now + IN_BAND_FRESH
                    && full_read_at.is_some_and(|full| {
                        at > full && now - full < IDLE_DUE && full <= now + IDLE_DUE
                    })
            });
            if in_band_fresh {
                false
            } else if active && last_check.is_none_or(|at| now - at >= DUE || at > now + DUE) {
                true
            } else if let Some(next) = next_check_at {
                next <= now || next > now + BACKED_OFF.max(IDLE_DUE)
            } else {
                last_check.is_none_or(|at| now - at >= IDLE_DUE || at > now + IDLE_DUE)
            }
        };
        if is_due {
            due.insert(id);
        }
    }
    Ok(due)
}

/// Accounts whose folder sign-in changed since the last refresh (such as a
/// re-login or account switch), so they are polled right away.
fn newly_signed_in(db: &Connection, signed_in: &[SignedIn]) -> Result<HashSet<String>> {
    let mut latest = db.prepare(
        "SELECT account, key FROM sign_in WHERE folder = ?1 AND provider = ?2
         ORDER BY since DESC LIMIT 1",
    )?;
    let mut changed = HashSet::new();
    for signed in signed_in {
        let Some(account) = &signed.account else {
            continue;
        };
        let held: Option<(Option<String>, Option<String>)> = latest
            .query_row(params![signed.folder, signed.provider], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
        if held.as_ref() != Some(&(signed.account.clone(), signed.key.clone())) {
            changed.insert(account.clone());
        }
    }
    Ok(changed)
}

/// The accounts whose provider kept failing that wait out their backoff at
/// `now`. A wait longer than any is a clock set back since. When `force` is
/// set, accounts last checked at least [`DUE`] ago are retried.
fn waiting(db: &Connection, now: i64, force: bool) -> Result<HashSet<String>> {
    if force {
        let mut stmt = db.prepare(
            "SELECT f.account FROM failing f
             LEFT JOIN account a ON a.id = f.account
             WHERE f.retry_at > ?1 AND f.retry_at <= ?1 + ?2
               AND coalesce(a.checked_at, a.read_at) IS NOT NULL
               AND ?1 - coalesce(a.checked_at, a.read_at) < ?3
               AND coalesce(a.checked_at, a.read_at) <= ?1 + ?3",
        )?;
        let rows = stmt.query_map(params![now, BACKED_OFF, DUE], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    } else {
        let mut stmt =
            db.prepare("SELECT account FROM failing WHERE retry_at > ?1 AND retry_at <= ?1 + ?2")?;
        let rows = stmt.query_map(params![now, BACKED_OFF], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// Claim reading the limits if they're due, in one statement, so no two
/// processes read them at once. Not once a newer Turnscope has written.
fn claim(db: &mut Connection, now: i64, force: bool, has_due: bool) -> Result<bool> {
    if force {
        let tx = crate::db::write(db)?;
        tx.execute("UPDATE state SET limits_at = ?1", [now])?;
        tx.commit()?;
        return Ok(true);
    }
    // Looked at first: claiming waits for the write lock, which a hook
    // shouldn't when nothing is due.
    let last: Option<i64> = db.query_row("SELECT limits_at FROM state", [], |row| row.get(0))?;
    let gap = if has_due { CLAIM_GAP } else { DUE };
    if last.is_some_and(|last| now - gap < last && last <= now + DUE) {
        return Ok(false);
    }
    // Claimed more than `DUE` ahead of now is a clock set back since.
    let tx = crate::db::write(db)?;
    let claimed = tx.execute(
        "UPDATE state SET limits_at = ?1
         WHERE limits_at IS NULL OR limits_at <= ?1 - ?2 OR limits_at > ?1 + ?3",
        params![now, gap, DUE],
    )?;
    tx.commit()?;
    Ok(claimed > 0)
}

/// What every folder of `agents` signs in to, the accounts those logins are
/// to, each with its logins, read by one of `providers`, and the accounts
/// whose login folder couldn't be read, with why.
fn gather(
    db: &Connection,
    home: &Path,
    agents: &[&dyn Agent],
    providers: &[&'static dyn Provider],
) -> Result<Gathered> {
    let mut accounts: BTreeMap<String, Held> = BTreeMap::new();
    let mut signed_in = Vec::new();
    let mut unreadable: BTreeMap<String, String> = BTreeMap::new();
    // A key is to one account for good: the one it was last signed in to,
    // which may be another key's it was made one with. Only at the same
    // provider: the same key kept under another is that provider's.
    let mut kept = db.prepare(
        "SELECT account FROM sign_in WHERE key = ?1 AND provider = ?2 AND account IS NOT NULL
         ORDER BY since DESC LIMIT 1",
    )?;
    let listable = std::fs::read_dir(home).is_ok();
    let mut latest = db.prepare(
        "SELECT folder, provider, account FROM sign_in s WHERE agent = ?1 AND account IS NOT NULL
         AND since = (SELECT max(since) FROM sign_in WHERE folder = s.folder AND provider = s.provider)",
    )?;
    for &agent in agents {
        let folders = agent.installed_folders(home);
        // Each folder whose logins were read, with the providers they name.
        let mut read: Vec<(String, Vec<String>)> = Vec::new();
        let mut failed_folders: BTreeMap<String, String> = BTreeMap::new();
        for folder in &folders {
            let folder_name = folder.to_string_lossy().into_owned();
            // A folder whose logins can't be read keeps what it held in
            // `sign_in`, and its known accounts record the failure below.
            let logins = match agent.logins(folder, home) {
                Ok(logins) => logins,
                Err(error) => {
                    failed_folders.insert(folder_name, error.to_string());
                    continue;
                }
            };
            let mut named = Vec::new();
            for login in logins {
                let mut held = login.credential.and_then(|credential| {
                    let provider = *providers
                        .iter()
                        .find(|provider| provider.info().id == credential.provider)?;
                    let account = provider.account(&credential)?;
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
                    folder: folder_name.clone(),
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
            read.push((folder_name, named));
        }
        // A login gone from a folder that was read is signed out of. So is
        // every login of a folder no longer listed: one deleted, or a second
        // account's without the file its agent writes once signed in. A
        // folder is listed by what's under `home` alone, so none drops off
        // the list while its agent is signed in there. A home that can't be
        // listed, as one on a volume that's away, lists no second account's
        // folder: each then keeps what it held.
        for row in latest.query_map([agent.info().id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })? {
            let (folder, provider, account): (String, String, String) = row?;
            if let Some(why) = failed_folders.get(&folder) {
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
    // Another readable folder holding a login for the same account reads it.
    unreadable.retain(|id, _| !accounts.contains_key(id));
    Ok(Gathered {
        signed_in,
        accounts,
        unreadable,
    })
}

/// Write what was read, in one write: accounts read as one plan made one,
/// each folder's sign-ins and whose usage is whose since, then each account
/// and its readings. A read that failed differently from the one before is
/// added to `app.log` in the data directory, for `turnscope doctor`.
/// Returns whether any account, reading, or sign-in changed.
fn record(
    db: &mut Connection,
    mut signed_in: Vec<SignedIn>,
    answers: Vec<(Read, Held)>,
    unreadable: BTreeMap<String, String>,
    now: i64,
) -> Result<bool> {
    let tx = crate::db::write(db)?;
    let existing: HashSet<String> = tx
        .prepare("SELECT id FROM account")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let one = same_plan(&tx, &answers, &existing)?;
    for (other, first) in &one {
        merge(&tx, other, first)?;
    }
    for signed in &mut signed_in {
        if let Some(first) = signed.account.as_ref().and_then(|id| one.get(id)) {
            signed.account = Some(first.clone());
        }
    }
    let sign_in_changed = sign_ins(&tx, &signed_in, now)?;
    let mut own_ok = HashSet::new();
    let mut plan_ok = HashSet::new();
    for (read, held) in &answers {
        if read.limits.is_some() {
            own_ok.insert(held.account.id.clone());
            plan_ok.insert(
                one.get(&held.account.id)
                    .unwrap_or(&held.account.id)
                    .clone(),
            );
        }
    }
    let mut written = HashSet::new();
    let mut logged = Vec::new();
    for (read, held) in answers {
        // Of one plan, the account that stands for it is written once: by its
        // own read when it succeeded, or by a merged key's when its own wasn't
        // read or failed.
        let id = one.get(&held.account.id).unwrap_or(&held.account.id);
        if read.limits.is_none() && plan_ok.contains(id) {
            continue;
        }
        if one.contains_key(&held.account.id) && own_ok.contains(id) {
            continue;
        }
        let id = id.clone();
        if !written.insert(id.clone()) {
            continue;
        }
        let account = &held.account;
        // Named in `app.log` by its title alone: a label can be an email,
        // and `doctor` prints the log for a report someone may post.
        let (log_line, retry_at) = failing(&tx, &id, &read, &title(&held, &read), now)?;
        logged.extend(log_line);
        let next_check = next_check_at(&tx, &id, &read, retry_at, now)?;
        // A read that failed may not know the plan, so the title stays.
        tx.execute(
            "INSERT INTO account (id, provider, kind, title, label, read_at, problem, full_read_at, checked_at, next_check_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (id) DO UPDATE SET
                 title = iif(excluded.read_at IS NULL, title, excluded.title),
                 label = coalesce(excluded.label, label),
                 read_at = coalesce(excluded.read_at, read_at),
                 full_read_at = coalesce(excluded.full_read_at, full_read_at),
                 problem = excluded.problem,
                 checked_at = excluded.checked_at,
                 next_check_at = excluded.next_check_at",
            params![
                id,
                held.provider.info().id,
                match account.kind {
                    Kind::Subscription => "subscription",
                    Kind::ApiKey => "apiKey",
                },
                title(&held, &read),
                account.label,
                read.limits.is_some().then_some(now),
                read.problem.as_ref().map(problem_name),
                read.limits.is_some().then_some(now),
                now,
                next_check,
            ],
        )?;
        for reading in read.limits.iter().flat_map(|limits| &limits.readings) {
            tx.execute(
                "INSERT OR REPLACE INTO reading (account, limit_key, at, name, scope, used, size, starts, resets)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id,
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
    // Accounts whose login folder couldn't be read aren't signed out, but
    // their failure is recorded and logged instead of leaving an older state
    // (such as "expired") frozen in place.
    let mut unreadable_written = false;
    for (id, why) in unreadable {
        let id = one.get(&id).unwrap_or(&id);
        if written.contains(id) {
            continue;
        }
        let Some(named): Option<String> = tx
            .query_row("SELECT title FROM account WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?
        else {
            continue;
        };
        let read = Read::failed(Problem::unanswered(why));
        let (log_line, _) = failing(&tx, id, &read, &named, now)?;
        logged.extend(log_line);
        tx.execute(
            "UPDATE account SET problem = 'unavailable', checked_at = ?2, next_check_at = ?3
             WHERE id = ?1",
            params![id, now, now + DUE],
        )?;
        unreadable_written = true;
    }
    tx.commit()?;
    if let Some(data) = db.path().and_then(|file| Path::new(file).parent()) {
        for line in logged {
            crate::engine::log(data, &line);
        }
    }
    Ok(sign_in_changed || !written.is_empty() || unreadable_written)
}

/// When `account` should next be polled after `read` at `now`: its backoff
/// time when backed off, [`DUE`] while active or recovering from an error,
/// [`IDLE_DUE`] while idle, or [`RESET_GRACE`] past its soonest upcoming
/// window reset if earlier.
fn next_check_at(
    tx: &Transaction,
    account: &str,
    read: &Read,
    retry_at: Option<i64>,
    now: i64,
) -> Result<i64> {
    if let Some(retry_at) = retry_at {
        return Ok(retry_at);
    }
    let Some(limits) = &read.limits else {
        return Ok(now + DUE);
    };
    let mut active: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM response WHERE account = ?1 AND at >= ?2)",
        params![account, now - ACTIVE_WINDOW],
        |row| row.get(0),
    )?;
    if !active && let Some(previous) = stored_limits(tx, account)? {
        active = limits.readings.iter().any(|reading| {
            previous
                .readings
                .iter()
                .find(|before| before.key == reading.key)
                .is_some_and(|before| reading.used > before.used + 0.05)
        });
    }
    let mut next = now + if active { DUE } else { IDLE_DUE };
    if let Some(soonest_reset) = limits
        .readings
        .iter()
        .filter_map(|reading| reading.resets)
        .filter(|&resets| resets > now)
        .min()
    {
        let after_reset = soonest_reset.saturating_add(RESET_GRACE);
        if after_reset < next {
            next = after_reset.max(now + CLAIM_GAP);
        }
    }
    Ok(next)
}

/// Record live limit readings emitted directly by an agent (such as Codex's
/// `token_count` event or Claude Code's `statusline` stdin JSON).
///
/// Advances `account.read_at` without touching `full_read_at`, so scoped
/// limits from the last full API poll stay visible until the next full poll.
/// Returns whether any reading was written.
pub fn record_in_band(
    tx: &Transaction,
    account: &str,
    snapshot: &agents::InBand,
    now: i64,
) -> Result<bool> {
    if snapshot.readings.is_empty()
        || snapshot.at < now - 7 * crate::time::DAY
        || snapshot.at > now + 5 * crate::time::MINUTE
    {
        return Ok(false);
    }
    struct Stored {
        kind: String,
        title: String,
        read_at: Option<i64>,
        full_read_at: Option<i64>,
        problem: Option<String>,
    }
    let Some(stored) = tx
        .query_row(
            "SELECT kind, title, read_at, full_read_at, problem FROM account WHERE id = ?1",
            [account],
            |row| {
                Ok(Stored {
                    kind: row.get(0)?,
                    title: row.get(1)?,
                    read_at: row.get(2)?,
                    full_read_at: row.get(3)?,
                    problem: row.get(4)?,
                })
            },
        )
        .optional()?
    else {
        return Ok(false);
    };
    let Stored {
        kind,
        title: current_title,
        read_at,
        full_read_at,
        problem,
    } = stored;
    if kind != "subscription" {
        return Ok(false);
    }
    if full_read_at.is_some_and(|full| snapshot.at <= full) {
        return Ok(false);
    }

    let mut latest: BTreeMap<String, (i64, f64, Option<i64>)> = BTreeMap::new();
    if let Some(since) = full_read_at.or(read_at) {
        let mut rows = tx.prepare_cached(
            "SELECT limit_key, at, used, resets FROM reading
             WHERE account = ?1 AND at >= ?2 ORDER BY at",
        )?;
        let mut rows = rows.query(params![account, since])?;
        while let Some(row) = rows.next()? {
            latest.insert(row.get(0)?, (row.get(1)?, row.get(2)?, row.get(3)?));
        }
    }

    let fresh: Vec<&Reading> = snapshot
        .readings
        .iter()
        .filter(|reading| {
            reading.used.is_finite()
                && latest
                    .get(&reading.key)
                    .is_none_or(|(at, _, _)| snapshot.at > *at)
        })
        .collect();
    if fresh.is_empty() {
        return Ok(false);
    }

    if problem.is_none()
        && fresh.iter().all(|reading| {
            latest.get(&reading.key).is_some_and(|(at, used, resets)| {
                snapshot.at - *at < crate::time::MINUTE
                    && (reading.used.max(0.0) - *used).abs() < 0.05
                    && reading.resets == *resets
            })
        })
    {
        return Ok(false);
    }

    let mut insert = tx.prepare_cached(
        "INSERT OR REPLACE INTO reading (account, limit_key, at, name, scope, used, size, starts, resets)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for reading in &fresh {
        insert.execute(params![
            account,
            reading.key,
            snapshot.at,
            reading.name,
            reading.scope,
            reading.used.max(0.0),
            reading.size,
            reading.starts,
            reading.resets,
        ])?;
    }
    drop(insert);

    let new_title = snapshot
        .plan
        .as_deref()
        .filter(|plan| !plan.is_empty())
        .and_then(|plan| {
            let subscription = providers::ALL
                .iter()
                .find(|provider| provider.info().id == snapshot.provider)?
                .info()
                .subscription?;
            (current_title == subscription)
                .then(|| format!("{subscription} {}", providers::words(plan)))
        });

    let mut next_check = now + DUE;
    // Never push a full poll past `IDLE_DUE` from the last full poll so
    // scoped limits stay fresh even under continuous in-band updates.
    if let Some(full) = full_read_at.or(read_at) {
        next_check = next_check.min(full.saturating_add(IDLE_DUE));
    }
    if let Some(soonest_reset) = fresh
        .iter()
        .filter_map(|reading| reading.resets)
        .filter(|&resets| resets > now)
        .min()
    {
        let after_reset = soonest_reset.saturating_add(RESET_GRACE);
        if after_reset < next_check {
            next_check = after_reset.max(now + CLAIM_GAP);
        }
    }

    tx.execute(
        "UPDATE account
         SET title = coalesce(?2, title),
             full_read_at = coalesce(full_read_at, read_at),
             read_at = max(coalesce(read_at, ?3), ?3),
             problem = NULL,
             next_check_at = ?4
         WHERE id = ?1",
        params![account, new_title, snapshot.at, next_check],
    )?;
    tx.execute("DELETE FROM failing WHERE account = ?1", [account])?;
    Ok(true)
}

/// Best-effort write of live `statusline` stdin `rate_limits` into an existing
/// database. Waits at most 50 ms for the write lock and swallows errors so a
/// status line stays fast and never fails.
pub fn record_statusline_limits(
    data: &Path,
    account: &str,
    snapshot: &agents::InBand,
    now: i64,
) -> bool {
    let Some(mut db) = crate::db::open_existing_rw(data, Duration::from_millis(50)) else {
        return false;
    };
    let Ok(tx) = crate::db::write(&mut db) else {
        return false;
    };
    match record_in_band(&tx, account, snapshot, now) {
        Ok(true) => tx.commit().is_ok(),
        _ => false,
    }
}

/// How `account.problem` names `problem`.
fn problem_name(problem: &Problem) -> &'static str {
    match problem {
        Problem::SignIn => "signIn",
        Problem::Expired => "expired",
        Problem::Unavailable { .. } => "unavailable",
    }
}

/// Note in `failing` how `account`'s read went, before its row says: how
/// many reads in a row failed as this one did, what it met, and when it's
/// read next. Returns `(log_line, retry_at)`.
fn failing(
    tx: &Transaction,
    account: &str,
    read: &Read,
    named: &str,
    now: i64,
) -> Result<(Option<String>, Option<i64>)> {
    let Some(problem) = &read.problem else {
        tx.execute("DELETE FROM failing WHERE account = ?1", [account])?;
        return Ok((None, None));
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
    let line = match problem {
        _ if again && said.as_deref() == why => None,
        Problem::Unavailable { why, .. } => Some(format!("{named}: limits not read: {why}")),
        Problem::SignIn => Some(format!("{named}: its login was refused")),
        // The status says so, and what to do.
        Problem::Expired => None,
    };
    Ok((line, retry_at))
}

/// Write each folder's sign-in that changed, and the account of the
/// responses made in that folder since. Returns whether any sign-in changed.
///
/// In the write that records the change, over after it: a process that
/// ended between the two left the responses on the account before for
/// good, and the next refresh saw no change to work from. Without
/// `ingest.lock`, so a hook never waits out a read of history: a read
/// takes in the sign-ins at each of its writes (`ingest::Derived`), so
/// it writes nothing with one gone stale.
fn sign_ins(tx: &Transaction, signed_in: &[SignedIn], now: i64) -> Result<bool> {
    // The sign-in in force at each response, or before the folder's first,
    // the first, as `ingest::Derived::of` finds it.
    let mut update = tx.prepare(&format!(
        "UPDATE response AS r SET account = (SELECT account {})
         WHERE folder = ?1 AND provider = ?2 AND at >= ?3",
        crate::prices::SIGN_IN_AT
    ))?;
    let mut changed = false;
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
            changed = true;
        }
    }
    Ok(changed)
}

/// What to call `held`: a subscription by its plan, as `read` or else one
/// of its logins names it; anything else as its provider's API key.
fn title(held: &Held, read: &Read) -> String {
    let plan = read
        .limits
        .as_ref()
        .and_then(|limits| limits.plan.clone())
        .or_else(|| {
            held.credentials
                .iter()
                .find_map(|credential| credential.plan.clone())
        })
        .filter(|plan| !plan.is_empty());
    let info = held.provider.info();
    match (held.account.kind, info.subscription) {
        (Kind::Subscription, Some(subscription)) => match plan {
            Some(plan) => format!("{subscription} {}", providers::words(&plan)),
            None => subscription.to_owned(),
        },
        _ => format!("{} API key", info.name),
    }
}

/// Each account read as the same plan as another, by its provider's
/// `plan_mark`, with the account that stands for them: one already known,
/// so its history is kept, or else the first, accounts being in order.
///
/// An account known from before and not read now, or whose login was
/// refused (`Problem::SignIn`), counts by the mark of its last read: a key
/// swapped for a new one to the same plan made the plan a second account,
/// the first signed out or refused, with no readings behind the new. Other
/// failures don't merge: `merge` can't be undone, and `plan` stops at the
/// first failure that isn't a refused login, so an old key failing another
/// way would stop the working key from being read.
fn same_plan(
    tx: &Transaction,
    answers: &[(Read, Held)],
    existing: &HashSet<String>,
) -> Result<BTreeMap<String, String>> {
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
    for (id, provider) in stored(tx, &marked)? {
        if skip_stored.contains(id.as_str()) {
            continue;
        }
        let mark = stored_limits(tx, &id)?.and_then(|limits| marked[provider].plan_mark(&limits));
        if let Some(ids) = mark.and_then(|mark| plans.get_mut(&(provider, mark))) {
            ids.push(id);
        }
    }
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

/// The accounts known of the providers in `marked`, with each one's
/// provider.
fn stored<'a>(
    tx: &Transaction,
    marked: &BTreeMap<&'a str, &'static dyn Provider>,
) -> Result<Vec<(String, &'a str)>> {
    let mut found = Vec::new();
    let mut accounts = tx.prepare("SELECT id FROM account WHERE provider = ?1")?;
    for provider in marked.keys() {
        for id in accounts.query_map([provider], |row| row.get::<_, String>(0))? {
            found.push((id?, *provider));
        }
    }
    Ok(found)
}

/// What `account` was last read with, as a provider gave it (preserving
/// scoped limits from its last full poll alongside any newer in-band readings).
fn stored_limits(tx: &Transaction, account: &str) -> Result<Option<Limits>> {
    let since: Option<i64> = tx
        .query_row(
            "SELECT min(coalesce(full_read_at, read_at), read_at) FROM account WHERE id = ?1",
            [account],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let Some(since) = since else {
        return Ok(None);
    };
    let mut latest: BTreeMap<String, Reading> = BTreeMap::new();
    let mut rows = tx.prepare_cached(
        "SELECT limit_key, name, scope, used, size, starts, resets FROM reading
         WHERE account = ?1 AND at >= ?2 ORDER BY at, limit_key",
    )?;
    let mut rows = rows.query(params![account, since])?;
    while let Some(row) = rows.next()? {
        let key: String = row.get(0)?;
        latest.insert(
            key.clone(),
            Reading {
                key,
                name: row.get(1)?,
                scope: row.get(2)?,
                used: row.get(3)?,
                size: row.get(4)?,
                starts: row.get(5)?,
                resets: row.get(6)?,
            },
        );
    }
    let readings: Vec<Reading> = latest.into_values().collect();
    Ok((!readings.is_empty()).then_some(Limits {
        plan: None,
        readings,
    }))
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

/// An account's limits with its logins, in [`WAIT`] all told. A login within
/// a minute of expiring isn't sent unless its provider refreshes non-rotating
/// tokens in memory and the login carries a refresh token.
fn read(held: &mut Held, now: i64) -> Read {
    let by = Instant::now() + WAIT;
    let provider = held.provider;
    let is_fresh = |credential: &Credential| {
        credential
            .expires
            .is_none_or(|at| at > now + crate::time::MINUTE)
    };
    held.credentials
        .sort_by_key(|credential| (!is_fresh(credential), std::cmp::Reverse(credential.expires)));
    let fresh: Vec<&Credential> = held
        .credentials
        .iter()
        // One about to expire would be refused on the way, which reads as
        // a login to sign in to again, where it only needs renewing — unless
        // the provider refreshes it in memory.
        .filter(|credential| {
            is_fresh(credential) || (provider.refreshes() && credential.refresh.is_some())
        })
        .collect();
    if fresh.is_empty() {
        Read::failed(Problem::Expired)
    } else if !provider.info().key_limits && fresh.iter().all(|credential| credential.key) {
        Read {
            limits: Some(Limits::default()),
            problem: None,
        }
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
    let mut problem = None;
    for login in logins {
        match provider.limits(login, now, by) {
            Ok(limits) => {
                return Read {
                    limits: Some(limits),
                    problem: None,
                };
            }
            Err(Problem::SignIn) => problem = Some(Problem::SignIn),
            Err(other) => return Read::failed(other),
        }
    }
    Read {
        limits: None,
        problem,
    }
}

/// Why a read that panicked failed.
const PANICKED: &str = "reading it failed unexpectedly";

/// An API-key account's limits: each key's own, as each can carry one, and
/// the account's, asked once. All at once, over one after another: `guard`
/// waits on the read in an agent's hook, and three keys to a provider out
/// of reach held it 45 s, three times [`WAIT`].
///
/// Read when anything is. A key that can't be read is the account's
/// problem, so it isn't shown as read in full with that key's limit gone.
/// Of a refusal and another problem, the refusal is said, as the one
/// someone can act on.
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
        // A refusal is kept over any other problem. Of two others, an answer
        // is kept over none, then the longest `Retry-After` over none or a
        // shorter one, with its reason, so the log says why it waits. Only
        // an answer has a `Retry-After`, so the one kept waits longest.
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
        shared => add(shared.map(|readings| Limits {
            plan: None,
            readings,
        })),
    }
    Read { limits, problem }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Reading;

    /// A provider that answers each login by its secret: one with `refused`
    /// in it is refused, one with `down` out of reach, and any other reads a
    /// limit keyed by it, after 300 ms for one with `slow`. The account's
    /// credits refuse a key with `nocredits`. It notes what it was asked.
    /// Every key is one account, and accounts whose `monthly` limit resets
    /// at one time are one plan.
    struct Fake {
        info: &'static providers::Info,
        asked: std::sync::Mutex<Vec<String>>,
        refreshes: bool,
    }

    impl Default for Fake {
        fn default() -> Fake {
            Fake {
                info: &FAKE,
                asked: Default::default(),
                refreshes: false,
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

        fn refreshes(&self) -> bool {
            self.refreshes
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
                busy if busy.contains("busy") => Err(Problem::Unavailable {
                    why: "busy".to_owned(),
                    retry_after: Some(600_000),
                    answered: true,
                }),
                key => Ok(Limits {
                    plan: None,
                    readings: vec![limit(&format!("key-{key}"))],
                }),
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
            .map(|(at, secret)| Credential {
                provider: "fake",
                secret: agents::Secret::new(secret),
                key: kind == Kind::ApiKey,
                expires: Some(now() + crate::time::DAY - at as i64),
                identity: None,
                plan: None,
                refresh: None,
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

    /// The fake provider's account, as signed in to in one folder.
    fn signed_in() -> Vec<SignedIn> {
        vec![SignedIn {
            agent: "fake",
            folder: "/f".to_owned(),
            provider: "fake".to_owned(),
            account: Some("fake:account".to_owned()),
            key: None,
        }]
    }

    /// Record a read of the fake provider's account that ended in `problem`
    /// at `at`, and give how the status then says it stands.
    fn failed(db: &mut Connection, problem: Problem, at: i64) -> Option<crate::status::Stale> {
        let (_, held) = fake(Kind::Subscription, &[]);
        record(
            db,
            signed_in(),
            vec![(Read::failed(problem), held)],
            BTreeMap::new(),
            at,
        )
        .unwrap();
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
        db.query_row("SELECT retry_at FROM failing", [], |row| row.get(0))
            .optional()
            .unwrap()
            .flatten()
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
            BTreeMap::new(),
            at,
        )
        .unwrap();
        assert_eq!(retry_at(&db), Some(at + 30 * minute));
        // A read that works ends it.
        let (_, held) = fake(Kind::Subscription, &[]);
        record(
            &mut db,
            signed_in(),
            vec![(monthly(1), held)],
            BTreeMap::new(),
            at,
        )
        .unwrap();
        assert_eq!(retry_at(&db), None);
        // Nor is a read of some keys, as the provider answered.
        let some = Read {
            problem: Some(Problem::unavailable("down")),
            ..monthly(1)
        };
        let (_, held) = fake(Kind::ApiKey, &[]);
        record(
            &mut db,
            signed_in(),
            vec![(some, held)],
            BTreeMap::new(),
            at,
        )
        .unwrap();
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
        // Waiting, it isn't read: until its wait is out, or unless the wait
        // is longer than any, as when the clock was set back.
        failed(&mut db, asks(3600), at);
        let waiting = |now| waiting(&db, now, false).unwrap().contains("fake:account");
        assert!(waiting(at + 5 * minute));
        assert!(!waiting(at + 30 * minute));
        assert!(!waiting(at - 5 * minute));
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
        Read {
            limits: Some(Limits {
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
            }),
            problem: None,
        }
    }

    /// Key a read, then swapped in its folder for key b to the same plan:
    /// key b is the plan's account, key a's.
    #[test]
    fn a_key_swapped_for_another_to_the_same_plan_keeps_its_account() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let signed = |account: &str| SignedIn {
            agent: "fake",
            folder: "/f1".to_owned(),
            provider: "fake".to_owned(),
            account: Some(account.to_owned()),
            key: Some(account.trim_start_matches("fake:key-").to_owned()),
        };
        let a = "fake:key-a";
        record(
            &mut db,
            vec![signed(a)],
            vec![(monthly(1), held(a))],
            BTreeMap::new(),
            1_000,
        )
        .unwrap();
        let b = "fake:key-b";
        record(
            &mut db,
            vec![signed(b)],
            vec![(monthly(1), held(b))],
            BTreeMap::new(),
            2_000,
        )
        .unwrap();
        let accounts: Vec<(String, i64, String)> = db
            .prepare("SELECT id, read_at, label FROM account")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(accounts, [(a.to_owned(), 2_000, format!("key {b}"))]);
        let folders: String = db
            .query_row(
                "SELECT account FROM sign_in ORDER BY since DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(folders, a);
        // Another plan's key stays its own.
        let c = "fake:key-c";
        record(
            &mut db,
            Vec::new(),
            vec![(monthly(2), held(c))],
            BTreeMap::new(),
            3_000,
        )
        .unwrap();
        let count: i64 = db
            .query_row("SELECT count(*) FROM account", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn a_login_held_in_two_folders_is_sent_once() {
        let mut held = held("fake:key-a");
        for secret in ["sk-go-a", "sk-go-a", "sk-go-b"] {
            held.hold(Credential {
                provider: "fake",
                secret: agents::Secret::new(secret),
                key: true,
                expires: None,
                identity: None,
                plan: None,
                refresh: None,
            });
        }
        assert_eq!(held.credentials.len(), 2);
    }

    #[test]
    fn a_login_with_under_a_minute_left_isnt_sent() {
        let mut held = held("fake:key-a");
        held.hold(Credential {
            provider: "fake",
            secret: agents::Secret::new("sk-go-a"),
            key: true,
            expires: Some(1_000_000 + 30_000),
            identity: None,
            plan: None,
            refresh: None,
        });
        assert_eq!(read(&mut held, 1_000_000).problem, Some(Problem::Expired));
    }

    #[test]
    fn an_expired_login_is_sent_to_a_provider_that_refreshes_it_in_memory() {
        let provider: &'static Fake = Box::leak(Box::new(Fake {
            refreshes: true,
            ..Fake::default()
        }));
        let mut held = Held {
            provider,
            account: providers::Account {
                id: "fake:account".to_owned(),
                kind: Kind::Subscription,
                label: None,
            },
            credentials: vec![Credential {
                provider: "fake",
                secret: agents::Secret::new("expired-token"),
                key: false,
                expires: Some(1_000_000 - 60_000),
                identity: None,
                plan: None,
                refresh: Some(agents::Refresh {
                    token: agents::Secret::new("refresh-token"),
                    binaries: Vec::new(),
                }),
            }],
        };
        let limits = read(&mut held, 1_000_000);
        assert_eq!(
            (keys(&limits), &limits.problem),
            (vec!["key-expired-token"], &None)
        );
        // Without a refresh token it still isn't sent.
        held.credentials[0].refresh = None;
        assert_eq!(read(&mut held, 1_000_000).problem, Some(Problem::Expired));
    }

    /// An agent whose own folder is `.fake`, and a second account's
    /// `.fake-` and a name, once signed in there. A folder's `login` names
    /// the key it holds for each provider.
    struct FakeAgent;

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
    };

    impl Agent for FakeAgent {
        fn info(&self) -> &'static agents::Info {
            &FAKE_AGENT
        }

        fn folders(&self, home: &Path) -> Vec<std::path::PathBuf> {
            agents::own_and_found(home, ".fake", ".fake-", "login")
        }

        fn files(&self, _folder: &Path) -> Vec<std::path::PathBuf> {
            Vec::new()
        }

        fn read(&self, _file: &Path, _cursor: &str) -> Result<agents::Read> {
            Ok(agents::Read::default())
        }

        fn logins(&self, folder: &Path, _home: &Path) -> Result<Vec<agents::Login>> {
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
                            secret: agents::Secret::new(key.as_str().unwrap_or_default()),
                            key: true,
                            expires: None,
                            identity: None,
                            plan: None,
                            refresh: None,
                        }),
                })
                .collect())
        }

        fn mcp_command(&self, _folder: &Path, _home: &Path) -> Option<String> {
            None
        }
    }

    /// The fake agent's folders under `home`, read by the two fake
    /// providers.
    fn gathered(db: &Connection, home: &Path) -> Gathered {
        let fake: &'static Fake = Box::leak(Box::default());
        let too: &'static Fake = Box::leak(Box::new(Fake {
            info: &FAKE_TOO,
            ..Fake::default()
        }));
        gather(db, home, &[&FakeAgent], &[fake, too]).unwrap()
    }

    #[test]
    fn a_login_gone_is_signed_out_of() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let db = crate::db::open(data.path()).unwrap();
        let folder = |name: &str| home.path().join(name).to_string_lossy().into_owned();
        // Two second accounts, signed in when last read. One's folder was
        // deleted since; the other's sign-in is half written. And the own
        // folder, whose login is gone: it holds none.
        for name in [".fake-gone", ".fake-written", ".fake"] {
            db.execute(
                "INSERT INTO sign_in (agent, folder, provider, since, account)
                 VALUES ('fake', ?1, 'fake', 1000, 'fake:account')",
                [folder(name)],
            )
            .unwrap();
        }
        std::fs::create_dir(folder(".fake-written")).unwrap();
        std::fs::write(home.path().join(".fake-written/login"), "{").unwrap();
        // What each is now recorded as signed in to.
        let signed = |home: &Path| -> Vec<(String, Option<String>)> {
            let mut signed: Vec<_> = gathered(&db, home)
                .signed_in
                .into_iter()
                .map(|signed| (signed.folder, signed.account))
                .collect();
            signed.sort();
            signed
        };
        // The one that can't be read keeps what it held.
        assert_eq!(
            signed(home.path()),
            [(folder(".fake"), None), (folder(".fake-gone"), None)]
        );
        // A home that can't be listed, as one on a volume that's away, says
        // nothing of its folders.
        assert!(signed(&home.path().join("away")).is_empty());
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
        } = gathered(&db, home.path());
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
        let signed = |folder: &str, account: &str| SignedIn {
            agent: "fake",
            folder: folder.to_owned(),
            provider: "fake".to_owned(),
            account: Some(account.to_owned()),
            key: Some(account.trim_start_matches("fake:key-").to_owned()),
        };
        // A response in each folder before any sign-in, and one later.
        for (id, folder, at) in [("r1", "/f1", 500), ("r2", "/f2", 500), ("r3", "/f1", 3_100)] {
            db.execute(
                "INSERT INTO response (agent, id, session, copy, file, folder, at, provider, model,
                     input, cache_read, cache_write_5m, cache_write_1h, output, reasoning, prompt,
                     web_searches, priority)
                 VALUES ('fake', ?1, 's', 0, 1, ?2, ?3, 'fake', 'm', 0, 0, 0, 0, 0, 0, 0, 0, 0)",
                params![id, folder, at],
            )
            .unwrap();
        }
        // Key b is read alone, then key a, to the same plan, joins it; key
        // c, to another plan, and key d, not read, stay their own.
        record(
            &mut db,
            vec![signed("/f1", "fake:key-b")],
            vec![(monthly(1), held("fake:key-b"))],
            BTreeMap::new(),
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
            BTreeMap::new(),
            2_000,
        )
        .unwrap();
        let accounts: Vec<(String, i64)> = db
            .prepare("SELECT id, hidden FROM account ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            accounts,
            [
                ("fake:key-b".to_owned(), 1),
                ("fake:key-c".to_owned(), 0),
                ("fake:key-d".to_owned(), 0),
            ]
        );
        let readings: i64 = db
            .query_row(
                "SELECT count(*) FROM reading WHERE account = 'fake:key-b'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(readings, 2);

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
            BTreeMap::new(),
            3_000,
        )
        .unwrap();
        // A folder's first sign-in is in force from the start, as the
        // plan's account it joined, and a later one from when it was read.
        let whose: Vec<(String, String)> = db
            .prepare("SELECT id, account FROM response ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            whose,
            [
                ("r1".to_owned(), "fake:key-b".to_owned()),
                ("r2".to_owned(), "fake:key-b".to_owned()),
                ("r3".to_owned(), "fake:key-c".to_owned()),
            ]
        );
        let (title, problem): (String, String) = db
            .query_row(
                "SELECT title, problem FROM account WHERE id = 'fake:key-b'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((title.as_str(), problem.as_str()), ("Fake Max", "expired"));
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
            BTreeMap::new(),
            1_000,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), crate::db::newer().to_string());
        let accounts: i64 = db
            .query_row("SELECT count(*) FROM account", [], |row| row.get(0))
            .unwrap();
        assert_eq!(accounts, 0);
    }

    #[test]
    fn a_working_key_for_the_same_plan_replaces_a_failing_ones_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let signed = |folder: &str, account: &str| SignedIn {
            agent: "fake",
            folder: folder.to_owned(),
            provider: "fake".to_owned(),
            account: Some(account.to_owned()),
            key: Some(account.trim_start_matches("fake:key-").to_owned()),
        };
        record(
            &mut db,
            vec![signed("/f1", "fake:key-a")],
            vec![(monthly(1), held("fake:key-a"))],
            BTreeMap::new(),
            1_000,
        )
        .unwrap();
        // Key a fails (e.g. revoked in one agent) while replacement key b in
        // another agent succeeds on the same monthly reset.
        record(
            &mut db,
            vec![signed("/f1", "fake:key-a"), signed("/f2", "fake:key-b")],
            vec![
                (Read::failed(Problem::SignIn), held("fake:key-a")),
                (monthly(1), held("fake:key-b")),
            ],
            BTreeMap::new(),
            2_000,
        )
        .unwrap();
        let accounts: Vec<(String, Option<i64>, Option<String>)> = db
            .prepare("SELECT id, read_at, problem FROM account ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(accounts, [("fake:key-a".to_owned(), Some(2_000), None)]);
        let readings: i64 = db
            .query_row(
                "SELECT count(*) FROM reading WHERE account = 'fake:key-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(readings, 2);

        // Another failure (not `SignIn`) does not merge: `plan` stops at the
        // first non-`SignIn` failure, so merging would block the working key.
        record(
            &mut db,
            vec![signed("/f1", "fake:key-a"), signed("/f3", "fake:key-c")],
            vec![
                (
                    Read::failed(Problem::unavailable("down")),
                    held("fake:key-a"),
                ),
                (monthly(1), held("fake:key-c")),
            ],
            BTreeMap::new(),
            3_000,
        )
        .unwrap();
        let accounts: Vec<(String, Option<i64>, Option<String>)> = db
            .prepare("SELECT id, read_at, problem FROM account ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            accounts,
            [
                (
                    "fake:key-a".to_owned(),
                    Some(2_000),
                    Some("unavailable".to_owned())
                ),
                ("fake:key-c".to_owned(), Some(3_000), None),
            ]
        );
    }

    /// Two accounts to one plan, as 0.3.0 made one when a key was refused,
    /// the refused one hidden: once they're one, the plan still shows.
    #[test]
    fn a_plan_made_one_with_a_hidden_copy_still_shows() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let signed = |folder: &str, account: &str| SignedIn {
            agent: "fake",
            folder: folder.to_owned(),
            provider: "fake".to_owned(),
            account: Some(account.to_owned()),
            key: Some(account.trim_start_matches("fake:key-").to_owned()),
        };
        let both = || vec![signed("/f1", "fake:key-a"), signed("/f2", "fake:key-b")];
        record(
            &mut db,
            vec![signed("/f1", "fake:key-a")],
            vec![(monthly(1), held("fake:key-a"))],
            BTreeMap::new(),
            1_000,
        )
        .unwrap();
        record(
            &mut db,
            both(),
            vec![
                (
                    Read::failed(Problem::unavailable("down")),
                    held("fake:key-a"),
                ),
                (monthly(1), held("fake:key-b")),
            ],
            BTreeMap::new(),
            2_000,
        )
        .unwrap();
        db.execute("UPDATE account SET hidden = 1 WHERE id = 'fake:key-a'", [])
            .unwrap();
        record(
            &mut db,
            both(),
            vec![
                (Read::failed(Problem::SignIn), held("fake:key-a")),
                (monthly(1), held("fake:key-b")),
            ],
            BTreeMap::new(),
            3_000,
        )
        .unwrap();
        let accounts: Vec<(String, i64)> = db
            .prepare("SELECT id, hidden FROM account ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(accounts, [("fake:key-b".to_owned(), 0)]);
    }

    #[test]
    fn an_uninstalled_agent_isnt_asked_for_logins_and_records_no_sign_in() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Counting(AtomicBool);
        impl Agent for Counting {
            fn info(&self) -> &'static agents::Info {
                &FAKE_AGENT
            }
            fn folders(&self, home: &Path) -> Vec<std::path::PathBuf> {
                vec![home.join(".fake")]
            }
            fn installed(&self, folder: &Path) -> bool {
                folder.join("installed").exists()
            }
            fn files(&self, _folder: &Path) -> Vec<std::path::PathBuf> {
                Vec::new()
            }
            fn read(&self, _file: &Path, _cursor: &str) -> Result<agents::Read> {
                Ok(agents::Read::default())
            }
            fn logins(&self, _folder: &Path, _home: &Path) -> Result<Vec<agents::Login>> {
                self.0.store(true, Ordering::Relaxed);
                Ok(vec![agents::Login {
                    provider: "fake".to_owned(),
                    credential: None,
                }])
            }
            fn mcp_command(&self, _folder: &Path, _home: &Path) -> Option<String> {
                None
            }
        }
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let db = crate::db::open(data.path()).unwrap();
        let fake: &'static Fake = Box::leak(Box::default());
        let agent = Counting(AtomicBool::new(false));
        // Neither missing nor a folder made by another tool (as Gemini CLI's
        // `~/.gemini` for Antigravity) counts as installed.
        let gathered = gather(&db, home.path(), &[&agent], &[fake]).unwrap();
        assert!(!agent.0.load(Ordering::Relaxed), "logins was called");
        assert!(gathered.signed_in.is_empty());
        std::fs::create_dir(home.path().join(".fake")).unwrap();
        let gathered = gather(&db, home.path(), &[&agent], &[fake]).unwrap();
        assert!(!agent.0.load(Ordering::Relaxed), "logins was called");
        assert!(gathered.signed_in.is_empty());

        // Once it was signed in and its folder was deleted, it's still signed
        // out without asking `logins`.
        let folder = home.path().join(".fake").to_string_lossy().into_owned();
        db.execute(
            "INSERT INTO sign_in (agent, folder, provider, since, account)
             VALUES ('fake', ?1, 'fake', 1000, 'fake:account')",
            [&folder],
        )
        .unwrap();
        let gathered = gather(&db, home.path(), &[&agent], &[fake]).unwrap();
        assert!(!agent.0.load(Ordering::Relaxed), "logins was called");
        assert_eq!(gathered.signed_in.len(), 1);
        assert_eq!(
            (
                gathered.signed_in[0].folder.as_str(),
                gathered.signed_in[0].account.as_deref()
            ),
            (folder.as_str(), None)
        );
    }

    #[test]
    fn an_unreadable_login_records_unavailable_and_logs_without_signing_out() {
        let (data, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut db = crate::db::open(data.path()).unwrap();
        let folder = home.path().join(".fake");
        std::fs::create_dir(&folder).unwrap();
        // Half-written login file, on an account that previously stood as
        // "expired".
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
        let first = gathered(&db, home.path());
        assert!(first.signed_in.is_empty(), "must not sign the account out");
        assert!(first.accounts.is_empty());
        assert!(first.unreadable.contains_key("fake:api"));
        record(
            &mut db,
            first.signed_in,
            Vec::new(),
            first.unreadable,
            2_000,
        )
        .unwrap();

        // Problem transitions from stale "expired" to "unavailable", with no
        // backoff and one line in app.log.
        let problem: Option<String> = db
            .query_row(
                "SELECT problem FROM account WHERE id = 'fake:api'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(problem.as_deref(), Some("unavailable"));
        assert_eq!(retry_at(&db), None);
        let log = std::fs::read_to_string(data.path().join("app.log")).unwrap();
        assert!(log.contains("Fake Plan: limits not read: reading"), "{log}");

        // If another readable folder holds a valid login for the same account,
        // `gather` drops it from `unreadable` so the readable folder wins.
        let second = home.path().join(".fake-work");
        std::fs::create_dir(&second).unwrap();
        std::fs::write(second.join("login"), r#"{"fake":"good-key"}"#).unwrap();
        let covered = gathered(&db, home.path());
        assert!(covered.accounts.contains_key("fake:api"));
        assert!(!covered.unreadable.contains_key("fake:api"));
    }

    #[test]
    fn scheduling_polls_idle_every_25m_active_every_5m_and_promptly_after_reset() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let minute = crate::time::MINUTE;
        let t0 = 1_790_856_000_000_i64;
        // Idle account with a reset 2 hours away: scheduled 25 minutes out.
        let (_, held_idle) = fake(Kind::Subscription, &[]);
        record(
            &mut db,
            signed_in(),
            vec![(monthly(t0 + 120 * minute), held_idle)],
            BTreeMap::new(),
            t0,
        )
        .unwrap();
        let next: i64 = db
            .query_row(
                "SELECT next_check_at FROM account WHERE id = 'fake:account'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(next, t0 + IDLE_DUE);
        assert!(
            due_accounts(&db, t0 + 10 * minute, false)
                .unwrap()
                .is_empty()
        );
        assert!(
            due_accounts(&db, t0 + IDLE_DUE, false)
                .unwrap()
                .contains("fake:account")
        );
        // `force` (panel open / wake) refreshes an account that is >= 5m old,
        // but skips one read 2 minutes ago.
        assert!(due_accounts(&db, t0 + 2 * minute, true).unwrap().is_empty());
        assert!(
            due_accounts(&db, t0 + DUE, true)
                .unwrap()
                .contains("fake:account")
        );

        // Once a response lands on the idle account, it wakes to the 5m cadence.
        db.execute(
            "INSERT INTO response (agent, id, session, copy, file, folder, at, provider, model,
                 input, cache_read, cache_write_5m, cache_write_1h, output, reasoning, prompt,
                 web_searches, priority, account)
             VALUES ('fake', 'r1', 's1', 0, 1, '/f', ?1, 'fake', 'm', 1, 0, 0, 0, 1, 0, 1,
                 0, 0, 'fake:account')",
            [t0 + 6 * minute],
        )
        .unwrap();
        assert!(
            due_accounts(&db, t0 + 7 * minute, false)
                .unwrap()
                .contains("fake:account")
        );

        // An account with a reset 8 minutes away schedules RESET_GRACE past it,
        // and becomes due right after the reset even while idle.
        let (_, held_reset) = fake(Kind::Subscription, &[]);
        let t1 = t0 + 60 * minute;
        let resets = t1 + 8 * minute;
        record(
            &mut db,
            signed_in(),
            vec![(monthly(resets), held_reset)],
            BTreeMap::new(),
            t1,
        )
        .unwrap();
        let next_reset: i64 = db
            .query_row(
                "SELECT next_check_at FROM account WHERE id = 'fake:account'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(next_reset, resets + RESET_GRACE);
        assert!(due_accounts(&db, resets, false).unwrap().is_empty());
        assert!(
            due_accounts(&db, resets + RESET_GRACE, false)
                .unwrap()
                .contains("fake:account")
        );
    }

    #[test]
    fn in_band_readings_clear_problems_dedup_and_suppress_redundant_polls() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = crate::db::open(dir.path()).unwrap();
        let minute = crate::time::MINUTE;
        let t0 = 1_790_856_000_000_i64;
        db.execute(
            "INSERT INTO account (id, provider, kind, title, read_at, full_read_at, checked_at, next_check_at, problem)
             VALUES ('openai:acct-1', 'openai', 'subscription', 'ChatGPT', ?1, ?1, ?1, ?2, 'expired')",
            params![t0, t0 + DUE],
        )
        .unwrap();
        db.execute(
            "INSERT INTO sign_in (agent, folder, provider, since, account)
             VALUES ('codex', '/f', 'openai', ?1, 'openai:acct-1')",
            [t0],
        )
        .unwrap();
        db.execute(
            "INSERT INTO failing (account, reads) VALUES ('openai:acct-1', 1)",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO reading (account, limit_key, at, name, scope, used, starts, resets)
             VALUES ('openai:acct-1', 'primary_window:Spark', ?1, '5 hours', 'Spark', 10.0, ?1, ?2)",
            params![t0, t0 + 5 * crate::time::HOUR],
        )
        .unwrap();

        let make = |at: i64, used: f64| agents::InBand {
            provider: "openai".to_owned(),
            at,
            plan: Some("plus".to_owned()),
            readings: vec![Reading {
                key: "primary_window".to_owned(),
                name: "5 hours".to_owned(),
                scope: None,
                used,
                size: None,
                starts: Some(t0),
                resets: Some(t0 + 5 * crate::time::HOUR),
            }],
        };

        // Older than `full_read_at` is ignored.
        let tx = crate::db::write(&mut db).unwrap();
        assert!(
            !record_in_band(&tx, "openai:acct-1", &make(t0 - minute, 5.0), t0 + minute).unwrap()
        );
        tx.commit().unwrap();

        // Newer snapshot writes reading, clears problem + failing, refines
        // title, and leaves `full_read_at` untouched.
        let t1 = t0 + 6 * minute;
        let tx = crate::db::write(&mut db).unwrap();
        assert!(record_in_band(&tx, "openai:acct-1", &make(t1, 25.0), t1).unwrap());
        tx.commit().unwrap();
        let (title, read_at, full_read_at, problem): (String, i64, i64, Option<String>) = db
            .query_row(
                "SELECT title, read_at, full_read_at, problem FROM account WHERE id = 'openai:acct-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(title, "ChatGPT Plus");
        assert_eq!((read_at, full_read_at, problem), (t1, t0, None));
        assert_eq!(retry_at(&db), None);

        // Identical snapshot within a minute is deduplicated.
        let tx = crate::db::write(&mut db).unwrap();
        assert!(
            !record_in_band(&tx, "openai:acct-1", &make(t1 + 20_000, 25.0), t1 + 20_000).unwrap()
        );
        // Changed value within a minute IS recorded.
        assert!(
            record_in_band(&tx, "openai:acct-1", &make(t1 + 30_000, 27.0), t1 + 30_000).unwrap()
        );
        tx.commit().unwrap();

        // While active with fresh in-band data and a recent full poll, API
        // polling is suppressed; once `full_read_at` ages to `IDLE_DUE`, a
        // full poll becomes due so scoped limits stay fresh.
        db.execute(
            "INSERT INTO response (agent, id, session, copy, file, folder, at, provider, model,
                 input, cache_read, cache_write_5m, cache_write_1h, output, reasoning, prompt,
                 web_searches, priority, account)
             VALUES ('codex', 'r1', 's1', 0, 1, '/f', ?1, 'openai', 'gpt-5', 1, 0, 0, 0, 1, 0, 1,
                 0, 0, 'openai:acct-1')",
            [t1 + 30_000],
        )
        .unwrap();
        assert!(due_accounts(&db, t1 + minute, false).unwrap().is_empty());
        // Advance to `t0 + IDLE_DUE` with a fresh in-band reading just 30s old.
        let t2 = t0 + IDLE_DUE;
        let tx = crate::db::write(&mut db).unwrap();
        assert!(
            record_in_band(&tx, "openai:acct-1", &make(t2 - 30_000, 35.0), t2 - 30_000).unwrap()
        );
        tx.commit().unwrap();
        assert!(
            due_accounts(&db, t2, false)
                .unwrap()
                .contains("openai:acct-1")
        );
    }
}
