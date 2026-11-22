//! Reading accounts' limits from providers and in-band agent signals.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::agents::{self, Agent, Credential};
use crate::providers::{self, Kind, Limits, Problem, Provider, Reading};
use crate::{Result, now};

/// Active polling and sign-in discovery cadence.
pub const DUE: i64 = 5 * crate::time::MINUTE;

// 25 min — 5 min before `status::STALE` (30 min), keeping healthy idle accounts
// live with 5x fewer API requests.
const IDLE_DUE: i64 = 25 * crate::time::MINUTE;
const ACTIVE_WINDOW: i64 = 15 * crate::time::MINUTE;
const IN_BAND_FRESH: i64 = 2 * crate::time::MINUTE;
const CLAIM_GAP: i64 = 15 * 1000;
// Brief grace past window reset so the provider's backend has rolled over.
const RESET_GRACE: i64 = 5 * 1000;

/// Longest wait between reads while a provider keeps failing.
pub const BACKED_OFF: i64 = 30 * crate::time::MINUTE;

// Only a provider that answered is backed off. With no answer, as when the
// Mac is offline, the next read is in five minutes: backed off too, limits
// stayed unread for up to half an hour after the Mac was back online. With
// a stand-in provider out of reach for 36 minutes, the first read that
// worked was at minute 65 (measured 2026-11-18).
//
// Chosen over asking every five minutes regardless of answer. Capped at 30
// minutes (including `Retry-After`) so limits recover within half an hour.
// Refused logins and partial API-key reads aren't backed off. Not measured:
// no provider's 429 or `Retry-After` has been seen here.
fn backoff(reads: i64, retry_after: Option<i64>) -> i64 {
    let doubled = DUE.saturating_mul(1_i64 << (reads - 1).clamp(0, 8));
    doubled.max(retry_after.unwrap_or(0)).min(BACKED_OFF)
}

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

// Shorter than models.dev's 60 s: `guard` runs in an agent hook and blocks the
// agent until this returns.
const WAIT: Duration = Duration::from_secs(15);

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

struct Held {
    provider: &'static dyn Provider,
    account: providers::Account,
    credentials: Vec<Credential>,
}

impl Held {
    // Deduplicate identical credentials held across multiple agent folders.
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

struct SignedIn {
    agent: &'static str,
    folder: String,
    provider: String,
    account: Option<String>,
    key: Option<String>,
}

struct Gathered {
    signed_in: Vec<SignedIn>,
    accounts: BTreeMap<String, Held>,
    unreadable: BTreeMap<String, String>,
}

/// Read due accounts' limits and discover sign-in changes; whether a refresh ran.
pub fn refresh(db: &mut Connection, home: &Path) -> Result<bool> {
    refresh_with(db, home, false)
}

/// Check logins now and refresh any account that is stale (`>= DUE`), unread,
/// past a reset, or failing; whether anything changed.
pub fn refresh_stale(db: &mut Connection, home: &Path) -> Result<bool> {
    refresh_with(db, home, true)
}

fn refresh_with(db: &mut Connection, home: &Path, force: bool) -> Result<bool> {
    let now = now();
    let last = if force {
        None
    } else {
        let last: Option<i64> = db
            .prepare_cached("SELECT limits_at FROM state")?
            .query_row([], |row| row.get(0))?;
        if last.is_some_and(|last| now - CLAIM_GAP < last && last <= now + DUE) {
            return Ok(false);
        }
        last
    };
    let due = due_accounts(db, now, force)?;
    if !claim(db, now, force, !due.is_empty(), last)? {
        return Ok(false);
    }
    let mut gathered = gather(db, home, agents::ALL, providers::ALL)?;
    let known: HashSet<String> = db
        .prepare("SELECT id FROM account")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let newly_signed = newly_signed_in(db, &gathered.signed_in)?;
    gathered
        .accounts
        .retain(|id, _| newly_signed.contains(id) || due.contains(id) || !known.contains(id));
    let answers: Vec<(Read, Held)> = std::thread::scope(|scope| {
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
    let changed = record(db, gathered.signed_in, answers, gathered.unreadable, now)?;
    // On a scheduled claim, return true even with no accounts yet so the engine
    // announces the initial limit check before history ingest.
    Ok(if force { changed } else { true })
}

/// Signed-in accounts that need a provider poll at `now`.
fn due_accounts(db: &Connection, now: i64, force: bool) -> Result<HashSet<String>> {
    let mut rows = db.prepare_cached(
        "SELECT a.id, a.read_at, a.full_read_at, a.checked_at, a.next_check_at, a.problem,
                f.retry_at,
                EXISTS (SELECT 1 FROM response r WHERE r.account = a.id AND r.at >= ?1) AS active,
                EXISTS (
                    SELECT 1 FROM reading rd
                    WHERE rd.account = a.id
                      AND a.read_at IS NOT NULL
                      AND rd.at >= coalesce(a.full_read_at, a.read_at, 0)
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
                          OR coalesce(a.checked_at, a.read_at) < rd.resets + ?3
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

fn latest_sign_in(
    db: &Connection,
    folder: &str,
    provider: &str,
) -> Result<Option<(Option<String>, Option<String>)>> {
    Ok(db
        .prepare_cached(
            "SELECT account, key FROM sign_in WHERE folder = ?1 AND provider = ?2
             ORDER BY since DESC LIMIT 1",
        )?
        .query_row(params![folder, provider], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?)
}

fn sign_in_matches(held: Option<&(Option<String>, Option<String>)>, signed: &SignedIn) -> bool {
    held.is_some_and(|(account, key)| {
        account.as_deref() == signed.account.as_deref() && key.as_deref() == signed.key.as_deref()
    })
}

fn newly_signed_in(db: &Connection, signed_in: &[SignedIn]) -> Result<HashSet<String>> {
    let mut changed = HashSet::new();
    for signed in signed_in {
        let Some(account) = &signed.account else {
            continue;
        };
        let held = latest_sign_in(db, &signed.folder, &signed.provider)?;
        if !sign_in_matches(held.as_ref(), signed) {
            changed.insert(account.clone());
        }
    }
    Ok(changed)
}

// Atomic claim so two processes don't poll providers at once.
fn claim(
    db: &mut Connection,
    now: i64,
    force: bool,
    has_due: bool,
    last: Option<i64>,
) -> Result<bool> {
    if force {
        let tx = crate::db::write(db)?;
        tx.execute("UPDATE state SET limits_at = ?1", [now])?;
        tx.commit()?;
        return Ok(true);
    }
    // Check before taking the write lock so hooks don't block when nothing is due.
    let gap = if has_due { CLAIM_GAP } else { DUE };
    if last.is_some_and(|last| now - gap < last && last <= now + DUE) {
        return Ok(false);
    }
    // Claimed more than `DUE` ahead of now means the clock was set back.
    let tx = crate::db::write(db)?;
    let claimed = tx.execute(
        "UPDATE state SET limits_at = ?1
         WHERE limits_at IS NULL OR limits_at <= ?1 - ?2 OR limits_at > ?1 + ?3",
        params![now, gap, DUE],
    )?;
    tx.commit()?;
    Ok(claimed > 0)
}

fn gather(
    db: &Connection,
    home: &Path,
    agents: &[&dyn Agent],
    providers: &[&'static dyn Provider],
) -> Result<Gathered> {
    let mut accounts: BTreeMap<String, Held> = BTreeMap::new();
    let mut signed_in = Vec::new();
    let mut unreadable: BTreeMap<String, String> = BTreeMap::new();
    // Keep a key bound to the account it was last merged into for the same provider.
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
        let mut read: Vec<(String, Vec<String>)> = Vec::new();
        let mut failed_folders: BTreeMap<String, String> = BTreeMap::new();
        for folder in &folders {
            let folder_name = folder.to_string_lossy().into_owned();
            // Unreadable folders keep their `sign_in` rows so transient errors
            // don't sign accounts out.
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
        // Only treat missing folders as signed out when `home` itself is listable
        // (an unmounted volume hides folders without signing anything out).
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
    let mut own_ok: HashSet<&str> = HashSet::new();
    let mut plan_ok: HashSet<&str> = HashSet::new();
    for (read, held) in &answers {
        if read.limits.is_some() {
            own_ok.insert(held.account.id.as_str());
            plan_ok.insert(
                one.get(&held.account.id)
                    .unwrap_or(&held.account.id)
                    .as_str(),
            );
        }
    }
    let mut written: HashSet<&str> = HashSet::new();
    let mut logged = Vec::new();
    for (read, held) in &answers {
        // Of one plan, the account that stands for it is written once: by its
        // own read when it succeeded, or by a merged key's when its own wasn't
        // read or failed.
        let id = one
            .get(&held.account.id)
            .unwrap_or(&held.account.id)
            .as_str();
        if read.limits.is_none() && plan_ok.contains(id) {
            continue;
        }
        if one.contains_key(&held.account.id) && own_ok.contains(id) {
            continue;
        }
        if !written.insert(id) {
            continue;
        }
        let account = &held.account;
        let named = title(held, read);
        // Named in `app.log` by its title alone: a label can be an email,
        // and `doctor` prints the log for a report someone may post.
        let (log_line, retry_at) = failing(&tx, id, read, &named, now)?;
        logged.extend(log_line);
        let next_check = next_check_at(&tx, id, read, retry_at, now)?;
        let read_at = read.limits.is_some().then_some(now);
        // A read that failed may not know the plan, so the title stays.
        tx.prepare_cached(
            "INSERT INTO account (id, provider, kind, title, label, read_at, problem, full_read_at, checked_at, next_check_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?6, ?8, ?9)
             ON CONFLICT (id) DO UPDATE SET
                 title = iif(excluded.read_at IS NULL, title, excluded.title),
                 label = coalesce(excluded.label, label),
                 read_at = coalesce(excluded.read_at, read_at),
                 full_read_at = coalesce(excluded.full_read_at, full_read_at),
                 problem = excluded.problem,
                 checked_at = excluded.checked_at,
                 next_check_at = excluded.next_check_at",
        )?
        .execute(params![
            id,
            held.provider.info().id,
            match account.kind {
                Kind::Subscription => "subscription",
                Kind::ApiKey => "apiKey",
            },
            named,
            account.label,
            read_at,
            read.problem.as_ref().map(problem_name),
            now,
            next_check,
        ])?;
        if let Some(limits) = &read.limits {
            write_readings(&tx, id, now, &limits.readings)?;
        }
    }
    // Accounts whose login folder couldn't be read aren't signed out, but
    // their failure is recorded and logged instead of leaving an older state
    // (such as "expired") frozen in place.
    let mut unreadable_written = false;
    for (id, why) in unreadable {
        let id = one.get(&id).unwrap_or(&id).as_str();
        if written.contains(id) {
            continue;
        }
        let Some(named): Option<String> = tx
            .prepare_cached("SELECT title FROM account WHERE id = ?1")?
            .query_row([id], |row| row.get(0))
            .optional()?
        else {
            continue;
        };
        let read = Read::failed(Problem::unanswered(why));
        let (log_line, _) = failing(&tx, id, &read, &named, now)?;
        logged.extend(log_line);
        tx.prepare_cached(
            "UPDATE account SET problem = 'unavailable', checked_at = ?2, next_check_at = ?3
             WHERE id = ?1",
        )?
        .execute(params![id, now, now + DUE])?;
        unreadable_written = true;
    }
    let any_written = !written.is_empty();
    tx.commit()?;
    if let Some(data) = db.path().and_then(|file| Path::new(file).parent()) {
        for line in logged {
            crate::engine::log(data, &line);
        }
    }
    Ok(sign_in_changed || any_written || unreadable_written)
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

/// Pull `next` forward to [`RESET_GRACE`] past the soonest upcoming window
/// reset among `readings` if earlier (at least `now + CLAIM_GAP`).
fn cap_at_reset<'a>(
    mut next: i64,
    readings: impl IntoIterator<Item = &'a Reading>,
    now: i64,
) -> i64 {
    if let Some(soonest_reset) = readings
        .into_iter()
        .filter_map(|reading| reading.resets)
        .filter(|&resets| resets > now)
        .min()
    {
        let after_reset = soonest_reset.saturating_add(RESET_GRACE);
        if after_reset < next {
            next = after_reset.max(now + CLAIM_GAP);
        }
    }
    next
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
    let mut active: bool = tx
        .prepare_cached("SELECT EXISTS (SELECT 1 FROM response WHERE account = ?1 AND at >= ?2)")?
        .query_row(params![account, now - ACTIVE_WINDOW], |row| row.get(0))?;
    if !active && let Some(previous) = stored_limits(tx, account)? {
        active = limits.readings.iter().any(|reading| {
            previous
                .readings
                .iter()
                .find(|before| before.key == reading.key)
                .is_some_and(|before| reading.used > before.used + 0.05)
        });
    }
    let next = now + if active { DUE } else { IDLE_DUE };
    Ok(cap_at_reset(next, &limits.readings, now))
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
    type AccountRow = (
        String,
        String,
        String,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<String>,
    );
    let Some((provider, kind, current_title, read_at, full_read_at, checked_at, problem)) = tx
        .prepare_cached(
            "SELECT provider, kind, title, read_at, full_read_at, checked_at, problem
             FROM account WHERE id = ?1",
        )?
        .query_row([account], |row| {
            Ok::<AccountRow, _>((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .optional()?
    else {
        return Ok(false);
    };
    if kind != "subscription" || provider != snapshot.provider {
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

    let clears_problem = problem.is_some() && snapshot.at >= checked_at.unwrap_or(0);
    if !clears_problem
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

    write_readings(tx, account, snapshot.at, fresh.iter().copied())?;

    let new_title = snapshot
        .plan
        .as_deref()
        .filter(|plan| !plan.is_empty())
        .and_then(|plan| {
            let subscription = providers::by_id(&snapshot.provider)?.info().subscription?;
            (current_title == subscription)
                .then(|| format!("{subscription} {}", providers::words(plan)))
        });

    let mut next_check = now + DUE;
    // Never push a full poll past `IDLE_DUE` from the last full poll so
    // scoped limits stay fresh even under continuous in-band updates.
    if let Some(full) = full_read_at.or(read_at) {
        next_check = next_check.min(full.saturating_add(IDLE_DUE));
    }
    let next_check = cap_at_reset(next_check, fresh.iter().copied(), now);

    tx.prepare_cached(
        "UPDATE account
         SET title = coalesce(?2, title),
             full_read_at = coalesce(full_read_at, read_at),
             read_at = max(coalesce(read_at, ?3), ?3),
             problem = iif(?5, NULL, problem),
             next_check_at = ?4
         WHERE id = ?1",
    )?
    .execute(params![
        account,
        new_title,
        snapshot.at,
        next_check,
        clears_problem
    ])?;
    if clears_problem {
        tx.prepare_cached("DELETE FROM failing WHERE account = ?1")?
            .execute([account])?;
    }
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

fn problem_name(problem: &Problem) -> &'static str {
    match problem {
        Problem::SignIn => "signIn",
        Problem::Expired => "expired",
        Problem::Unavailable { .. } => "unavailable",
    }
}

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
    // Don't back off or increment `reads` for unanswered/partial errors so the
    // first backed-off read after being offline starts at `DUE`, not 30 min.
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
        Problem::Expired => None,
    };
    Ok((line, retry_at))
}

// Reassign affected responses in the same transaction (without `ingest.lock`, so
// hooks don't block on history ingest; ingest re-reads `sign_in` on each write).
fn sign_ins(tx: &Transaction, signed_in: &[SignedIn], now: i64) -> Result<bool> {
    let mut update = tx.prepare(&format!(
        "UPDATE response AS r SET account = (SELECT account {})
         WHERE folder = ?1 AND provider = ?2 AND at >= ?3",
        crate::prices::SIGN_IN_AT
    ))?;
    let mut insert = tx.prepare_cached(
        "INSERT OR REPLACE INTO sign_in (agent, folder, provider, since, account, key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let mut changed = false;
    for signed in signed_in {
        let held = latest_sign_in(tx, &signed.folder, &signed.provider)?;
        if !sign_in_matches(held.as_ref(), signed) {
            insert.execute(params![
                signed.agent,
                signed.folder,
                signed.provider,
                now,
                signed.account,
                signed.key
            ])?;
            // A folder's first sign-in applies retroactively; later ones from `now`.
            let from = if held.is_some() { now } else { i64::MIN };
            update.execute(params![signed.folder, signed.provider, from])?;
            changed = true;
        }
    }
    Ok(changed)
}

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

// Merge accounts that share a provider `plan_mark`, keeping an existing id to
// preserve history. Known unread or `SignIn`-refused accounts match by stored
// mark so key swaps stay one account; other failures don't merge because `plan`
// stops at the first non-`SignIn` failure.
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

// Keep shown unless both were hidden: a hidden copy is usually an old refused key.
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
        // Skip tokens within a minute of expiry so an in-flight expiry isn't
        // misread as a revoked login — unless the provider refreshes in memory.
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

const PANICKED: &str = "reading it failed unexpectedly";

// Poll all keys in parallel so `guard` hooks wait at most one `WAIT`.
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
        // Prefer SignIn over other errors, then answered over unanswered, then longest Retry-After.
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

    fn open_db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::open(dir.path()).unwrap();
        (dir, db)
    }

    fn signed_for(folder: &str, account: &str) -> SignedIn {
        SignedIn {
            agent: "fake",
            folder: folder.to_owned(),
            provider: "fake".to_owned(),
            account: Some(account.to_owned()),
            key: account.strip_prefix("fake:key-").map(str::to_owned),
        }
    }

    fn signed_in() -> Vec<SignedIn> {
        vec![signed_for("/f", "fake:account")]
    }

    fn record_ok(db: &mut Connection, signed: Vec<SignedIn>, answers: Vec<(Read, Held)>, at: i64) {
        record(db, signed, answers, BTreeMap::new(), at).unwrap();
    }

    fn failed(db: &mut Connection, problem: Problem, at: i64) -> Option<crate::status::Stale> {
        let (_, held) = fake(Kind::Subscription, &[]);
        record_ok(db, signed_in(), vec![(Read::failed(problem), held)], at);
        crate::status::status(db, at).unwrap().accounts[0]
            .state
            .why()
    }

    fn retry_at(db: &Connection) -> Option<i64> {
        db.query_row("SELECT retry_at FROM failing", [], |row| row.get(0))
            .optional()
            .unwrap()
            .flatten()
    }

    fn account_hidden(db: &Connection) -> Vec<(String, i64)> {
        db.prepare("SELECT id, hidden FROM account ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn insert_response(db: &Connection, id: &str, folder: &str, at: i64, account: Option<&str>) {
        db.execute(
            "INSERT INTO response (agent, id, session, copy, file, folder, at, provider, model,
                 input, cache_read, cache_write_5m, cache_write_1h, output, reasoning, prompt,
                 web_searches, priority, account)
             VALUES ('fake', ?1, 's1', 0, 1, ?2, ?3, 'fake', 'm', 1, 0, 0, 0, 1, 0, 1, 0, 0, ?4)",
            params![id, folder, at, account],
        )
        .unwrap();
    }

    #[test]
    fn a_login_refused_twice_in_a_row_needs_signing_in_again() {
        use crate::status::Stale;
        let (_dir, mut db) = open_db();
        let minutes = |count: i64| count * crate::time::MINUTE;
        assert_eq!(
            failed(&mut db, Problem::SignIn, minutes(5)),
            Some(Stale::ReadFailed)
        );
        assert_eq!(
            failed(&mut db, Problem::SignIn, minutes(10)),
            Some(Stale::SignIn)
        );
        failed(&mut db, Problem::unavailable("down"), minutes(15));
        assert_eq!(
            failed(&mut db, Problem::SignIn, minutes(20)),
            Some(Stale::ReadFailed)
        );
    }

    #[test]
    fn a_provider_that_keeps_failing_is_asked_less_often() {
        let (_dir, mut db) = open_db();
        let minute = crate::time::MINUTE;
        let mut at = 0;
        for waits in [5, 10, 20, 30, 30] {
            at += 60 * minute;
            failed(&mut db, Problem::unavailable("down"), at);
            assert_eq!(retry_at(&db), Some(at + waits * minute));
        }
        let asks = |seconds: i64| Problem::Unavailable {
            why: "busy".to_owned(),
            retry_after: Some(seconds * 1000),
            answered: true,
        };
        let (_, held) = fake(Kind::Subscription, &[]);
        record_ok(
            &mut db,
            signed_in(),
            vec![(Read::failed(asks(0)), held)],
            at,
        );
        assert_eq!(retry_at(&db), Some(at + 30 * minute));
        // Success resets backoff, as does a partial API-key read or unanswered error.
        let (_, held) = fake(Kind::Subscription, &[]);
        record_ok(&mut db, signed_in(), vec![(monthly(1), held)], at);
        assert_eq!(retry_at(&db), None);
        let some = Read {
            problem: Some(Problem::unavailable("down")),
            ..monthly(1)
        };
        let (_, held) = fake(Kind::ApiKey, &[]);
        record_ok(&mut db, signed_in(), vec![(some, held)], at);
        assert_eq!(retry_at(&db), None);
        for _ in 0..6 {
            failed(&mut db, Problem::unanswered("host not found"), at);
            assert_eq!(retry_at(&db), None);
        }
        failed(&mut db, Problem::unavailable("down"), at);
        assert_eq!(retry_at(&db), Some(at + 5 * minute));
        failed(&mut db, Problem::SignIn, at);
        failed(&mut db, Problem::SignIn, at + 5 * minute);
        assert_eq!(retry_at(&db), None);
        // Backed off until retry_at, or immediately if the clock rolled back.
        failed(&mut db, asks(3600), at);
        let is_due = |now| {
            due_accounts(&db, now, false)
                .unwrap()
                .contains("fake:account")
        };
        assert!(!is_due(at + 5 * minute));
        assert!(is_due(at + 30 * minute));
        assert!(is_due(at - 5 * minute));
    }

    #[test]
    fn a_read_that_fails_differently_from_the_last_is_logged() {
        let (dir, mut db) = open_db();
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

    #[test]
    fn a_key_swapped_for_another_to_the_same_plan_keeps_its_account() {
        let (_dir, mut db) = open_db();
        let (a, b, c) = ("fake:key-a", "fake:key-b", "fake:key-c");
        record_ok(
            &mut db,
            vec![signed_for("/f1", a)],
            vec![(monthly(1), held(a))],
            1_000,
        );
        record_ok(
            &mut db,
            vec![signed_for("/f1", b)],
            vec![(monthly(1), held(b))],
            2_000,
        );
        let accounts: Vec<(String, i64, String)> = db
            .prepare("SELECT id, read_at, label FROM account")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(accounts, [(a.to_owned(), 2_000, format!("key {b}"))]);
        let folder_acct: String = db
            .query_row(
                "SELECT account FROM sign_in ORDER BY since DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(folder_acct, a);
        record_ok(&mut db, Vec::new(), vec![(monthly(2), held(c))], 3_000);
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
    fn an_expired_login_is_sent_only_to_a_provider_that_refreshes_in_memory() {
        let mut plain = held("fake:key-a");
        plain.hold(Credential {
            provider: "fake",
            secret: agents::Secret::new("sk-go-a"),
            key: true,
            expires: Some(1_000_000 + 30_000),
            identity: None,
            plan: None,
            refresh: None,
        });
        assert_eq!(read(&mut plain, 1_000_000).problem, Some(Problem::Expired));

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
        let (_dir, mut db) = open_db();
        for (id, folder, at) in [("r1", "/f1", 500), ("r2", "/f2", 500), ("r3", "/f1", 3_100)] {
            insert_response(&db, id, folder, at, None);
        }
        record_ok(
            &mut db,
            vec![signed_for("/f1", "fake:key-b")],
            vec![(monthly(1), held("fake:key-b"))],
            1_000,
        );
        db.execute("UPDATE account SET hidden = 1 WHERE id = 'fake:key-b'", [])
            .unwrap();
        record_ok(
            &mut db,
            vec![
                signed_for("/f1", "fake:key-b"),
                signed_for("/f2", "fake:key-a"),
            ],
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
        );
        assert_eq!(
            account_hidden(&db),
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

        db.execute(
            "UPDATE account SET title = 'Fake Max' WHERE id = 'fake:key-b'",
            [],
        )
        .unwrap();
        record_ok(
            &mut db,
            vec![signed_for("/f1", "fake:key-c")],
            vec![(Read::failed(Problem::Expired), held("fake:key-b"))],
            3_000,
        );
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
        let (_dir, mut db) = open_db();
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
        let (_dir, mut db) = open_db();
        let status_rows = |db: &Connection| -> Vec<(String, Option<i64>, Option<String>)> {
            db.prepare("SELECT id, read_at, problem FROM account ORDER BY id")
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        record_ok(
            &mut db,
            vec![signed_for("/f1", "fake:key-a")],
            vec![(monthly(1), held("fake:key-a"))],
            1_000,
        );
        record_ok(
            &mut db,
            vec![
                signed_for("/f1", "fake:key-a"),
                signed_for("/f2", "fake:key-b"),
            ],
            vec![
                (Read::failed(Problem::SignIn), held("fake:key-a")),
                (monthly(1), held("fake:key-b")),
            ],
            2_000,
        );
        assert_eq!(
            status_rows(&db),
            [("fake:key-a".to_owned(), Some(2_000), None)]
        );
        // Non-SignIn failure does not merge.
        record_ok(
            &mut db,
            vec![
                signed_for("/f1", "fake:key-a"),
                signed_for("/f3", "fake:key-c"),
            ],
            vec![
                (
                    Read::failed(Problem::unavailable("down")),
                    held("fake:key-a"),
                ),
                (monthly(1), held("fake:key-c")),
            ],
            3_000,
        );
        assert_eq!(
            status_rows(&db),
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

    #[test]
    fn a_plan_made_one_with_a_hidden_copy_still_shows() {
        let (_dir, mut db) = open_db();
        let both = || {
            vec![
                signed_for("/f1", "fake:key-a"),
                signed_for("/f2", "fake:key-b"),
            ]
        };
        record_ok(
            &mut db,
            vec![signed_for("/f1", "fake:key-a")],
            vec![(monthly(1), held("fake:key-a"))],
            1_000,
        );
        record_ok(
            &mut db,
            both(),
            vec![
                (
                    Read::failed(Problem::unavailable("down")),
                    held("fake:key-a"),
                ),
                (monthly(1), held("fake:key-b")),
            ],
            2_000,
        );
        db.execute("UPDATE account SET hidden = 1 WHERE id = 'fake:key-a'", [])
            .unwrap();
        record_ok(
            &mut db,
            both(),
            vec![
                (Read::failed(Problem::SignIn), held("fake:key-a")),
                (monthly(1), held("fake:key-b")),
            ],
            3_000,
        );
        assert_eq!(account_hidden(&db), [("fake:key-b".to_owned(), 0)]);
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
        let (_dir, mut db) = open_db();
        let minute = crate::time::MINUTE;
        let t0 = 1_790_856_000_000_i64;
        let (_, held_idle) = fake(Kind::Subscription, &[]);
        record_ok(
            &mut db,
            signed_in(),
            vec![(monthly(t0 + 120 * minute), held_idle)],
            t0,
        );
        let next = |db: &Connection| -> i64 {
            db.query_row(
                "SELECT next_check_at FROM account WHERE id = 'fake:account'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(next(&db), t0 + IDLE_DUE);
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
        assert!(due_accounts(&db, t0 + 2 * minute, true).unwrap().is_empty());
        assert!(
            due_accounts(&db, t0 + DUE, true)
                .unwrap()
                .contains("fake:account")
        );

        // Activity wakes the account to the 5m cadence.
        insert_response(&db, "r1", "/f", t0 + 6 * minute, Some("fake:account"));
        assert!(
            due_accounts(&db, t0 + 7 * minute, false)
                .unwrap()
                .contains("fake:account")
        );

        // An upcoming reset pulls the next poll to RESET_GRACE past it.
        let (_, held_reset) = fake(Kind::Subscription, &[]);
        let t1 = t0 + 60 * minute;
        let resets = t1 + 8 * minute;
        record_ok(
            &mut db,
            signed_in(),
            vec![(monthly(resets), held_reset)],
            t1,
        );
        assert_eq!(next(&db), resets + RESET_GRACE);
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

        // An in-band snapshot older than a newer failed poll (`checked_at`)
        // must not erase that newer `signIn` problem.
        let t3 = t2 + 5 * minute;
        db.execute(
            "UPDATE account SET problem = 'signIn', checked_at = ?1 WHERE id = 'openai:acct-1'",
            [t3],
        )
        .unwrap();
        let tx = crate::db::write(&mut db).unwrap();
        assert!(record_in_band(&tx, "openai:acct-1", &make(t3 - minute, 40.0), t3).unwrap());
        tx.commit().unwrap();
        let problem: Option<String> = db
            .query_row(
                "SELECT problem FROM account WHERE id = 'openai:acct-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(problem.as_deref(), Some("signIn"));
    }
}
