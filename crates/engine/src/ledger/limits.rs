//! Subscription accounts, their limits' readings, the alerts sent of them,
//! and what each place an agent keeps a sign-in held over time.

use std::path::PathBuf;

use rusqlite::{OptionalExtension, params};

use super::changes::{Touch, next_revision, touch};
use super::{Ledger, instant, optional_instant};
use crate::agent::Agent;
use crate::error::{Error, Result};
use crate::limits::{
    AlertKind, Held, LimitProblem, Place, Read as LimitRead, SAME_WINDOW, Seen, Span, Subscription,
};
use crate::time::Instant;

/// How long readings of a limit are kept, all but those its account's
/// latest successful read found: ninety days.
const READINGS_KEPT: i64 = 90 * 24 * 60 * 60 * 1000;

/// An account as last read.
#[derive(Clone, Debug)]
pub(crate) struct StoredAccount {
    pub id: String,
    pub subscription: Subscription,
    pub label: Option<String>,
    pub plan: Option<String>,
    /// The agents signed into it when it was last found signed in.
    pub agents: Vec<Agent>,
    /// Whether it was found signed in when last looked for.
    pub signed_in: bool,
    pub read_at: Option<Instant>,
    /// When its subscription was last read, whether or not its own read
    /// worked.
    pub checked_at: Instant,
    pub problem: Option<LimitProblem>,
}

/// One reading of one limit.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Reading {
    pub key: String,
    pub name: String,
    pub scope: Option<String>,
    pub at: Instant,
    pub used: f64,
    pub starts: Option<Instant>,
    pub resets: Option<Instant>,
}

impl Ledger {
    /// Record what reading `subscription`'s accounts at `at` found. An account
    /// no longer signed in anywhere keeps its last readings and the agents it
    /// was last in, and is marked signed out.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be written.
    pub(crate) fn record_limits(
        &mut self,
        subscription: Subscription,
        reads: &[LimitRead],
        at: Instant,
    ) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "UPDATE account SET signed_in = 0, problem = NULL, checked_at = ?2 WHERE subscription = ?1",
            params![subscription.key(), at.millis()],
        )?;
        for read in reads {
            let agents: Vec<&str> = read.agents.iter().map(|agent| agent.key()).collect();
            let via = serde_json::to_string(&agents)
                .map_err(|error| Error::corrupt("account agents", error.to_string()))?;
            let (read_at, problem) = match &read.limits {
                Ok(_) => (Some(at.millis()), None),
                Err(problem) => (None, Some(problem.key())),
            };
            transaction.execute(
                "INSERT INTO account (id, subscription, label, plan, via, read_at, problem, checked_at, signed_in)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1)
                 ON CONFLICT (id) DO UPDATE SET
                     label = coalesce(excluded.label, label), plan = coalesce(excluded.plan, plan),
                     via = excluded.via, read_at = coalesce(excluded.read_at, read_at),
                     problem = excluded.problem, checked_at = excluded.checked_at, signed_in = 1",
                params![read.id, subscription.key(), read.label, read.plan, via, read_at, problem, at.millis()],
            )?;
            for limit in read.limits.iter().flatten() {
                transaction.execute(
                    "INSERT OR REPLACE INTO limit_reading (account, key, at, name, scope, used, starts, resets)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        read.id,
                        limit.key,
                        at.millis(),
                        limit.name,
                        limit.scope,
                        limit.used,
                        limit.starts.map(Instant::millis),
                        limit.resets.map(Instant::millis),
                    ],
                )?;
            }
        }
        // What an account's latest successful read found stays, however old:
        // it is what the account shows, signed in nowhere or failing to be
        // read. A limit its provider stopped reporting goes with the rest.
        transaction.execute(
            "DELETE FROM limit_reading AS r WHERE at < ?1
             AND at IS NOT (SELECT read_at FROM account WHERE id = r.account)",
            [at.millis().saturating_sub(READINGS_KEPT)],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Every account ever found signed in.
    ///
    /// A newer build sharing the data directory may record subscriptions and
    /// agents this one doesn't know: an account of such a subscription is
    /// passed over, and such an agent left out of an account's.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read, and
    /// [`Error::Corrupt`] when an account's agents are not the list written
    /// or a time of it is out of range.
    pub(crate) fn accounts(&self) -> Result<Vec<StoredAccount>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id, subscription, label, plan, via, read_at, problem, signed_in, checked_at
             FROM account ORDER BY id",
        )?;
        let mut rows = statement.query([])?;
        let mut accounts = Vec::new();
        while let Some(row) = rows.next()? {
            let Some(subscription) = Subscription::from_key(&row.get::<_, String>(1)?) else {
                continue;
            };
            let via: String = row.get(4)?;
            let keys: Vec<String> = serde_json::from_str(&via)
                .map_err(|error| Error::corrupt("account agents", error.to_string()))?;
            let agents = keys.iter().filter_map(|key| Agent::from_key(key)).collect();
            accounts.push(StoredAccount {
                id: row.get(0)?,
                subscription,
                label: row.get(2)?,
                plan: row.get(3)?,
                agents,
                signed_in: row.get(7)?,
                read_at: optional_instant(row.get(5)?, "account read time")?,
                checked_at: instant(row.get(8)?, "account check time")?,
                problem: row
                    .get::<_, Option<String>>(6)?
                    .as_deref()
                    .and_then(LimitProblem::from_key),
            });
        }
        Ok(accounts)
    }

    /// Every reading of `account`'s limits since `since`, by limit, oldest
    /// first.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read, and
    /// [`Error::Corrupt`] when a reading's time is out of range.
    pub(crate) fn readings(&self, account: &str, since: Instant) -> Result<Vec<Reading>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT key, name, scope, at, used, starts, resets FROM limit_reading
             WHERE account = ?1 AND at >= ?2 ORDER BY key, at",
        )?;
        readings(statement.query(params![account, since.millis()])?)
    }

    /// Whether `account`'s limits have been read at one time only, as every
    /// account's are when Turnscope first runs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read.
    pub(crate) fn read_once(&self, account: &str) -> Result<bool> {
        let mut statement = self.connection.prepare_cached(
            "SELECT COUNT(*) FROM (SELECT DISTINCT at FROM limit_reading WHERE account = ?1 LIMIT 2)",
        )?;
        let times: i64 = statement.query_row([account], |row| row.get(0))?;
        Ok(times == 1)
    }

    /// The readings `account`'s latest successful read found: each limit its
    /// provider reported then. A limit it has stopped reporting, as a window
    /// a plan no longer has, is not among them, however recently it was
    /// read before.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read, and
    /// [`Error::Corrupt`] when a reading's time is out of range.
    pub(crate) fn latest_readings(&self, account: &str) -> Result<Vec<Reading>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT r.key, r.name, r.scope, r.at, r.used, r.starts, r.resets
             FROM limit_reading r JOIN account a ON a.id = r.account AND a.read_at = r.at
             WHERE r.account = ?1
             ORDER BY r.key",
        )?;
        readings(statement.query([account])?)
    }

    /// Record what each place in `seen` held at `at`: stretching the latest
    /// span of a place that held the same since that span began, and
    /// beginning a span otherwise. A place whose folder isn't UTF-8, as the
    /// ledger keeps paths as text, is passed over. `true` when a span began,
    /// which can change what account usage drew on, and is recorded as a
    /// change for the cache.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be written.
    pub(crate) fn record_sign_ins(&mut self, seen: &[Seen], at: Instant) -> Result<bool> {
        let transaction = self.connection.transaction()?;
        let mut began = false;
        {
            let mut latest = transaction.prepare_cached(
                "SELECT rowid, held, account, first FROM sign_in
                 WHERE agent = ?1 AND folder = ?2 AND provider = ?3
                 ORDER BY last DESC, first DESC LIMIT 1",
            )?;
            let mut stretch = transaction
                .prepare_cached("UPDATE sign_in SET last = max(last, ?2) WHERE rowid = ?1")?;
            let mut begin = transaction.prepare_cached(
                "INSERT INTO sign_in (agent, folder, provider, held, account, first, last)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            )?;
            for seen in seen {
                let Some(folder) = seen.place.folder.to_str() else {
                    continue;
                };
                let (held, account) = stored_held(&seen.held);
                let place = params![seen.place.agent.key(), folder, seen.place.provider];
                let found: Option<(i64, String, Option<String>, i64)> = latest
                    .query_row(place, |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })
                    .optional()?;
                match found {
                    Some((row, was, was_account, first))
                        if was == held
                            && was_account.as_deref() == account
                            && first <= at.millis() =>
                    {
                        stretch.execute(params![row, at.millis()])?;
                    }
                    _ => {
                        begin.execute(params![
                            seen.place.agent.key(),
                            folder,
                            seen.place.provider,
                            held,
                            account,
                            at.millis()
                        ])?;
                        began = true;
                    }
                }
            }
        }
        if began {
            let revision = next_revision(&transaction)?;
            touch(&transaction, revision, Touch::SignIns, "", "")?;
        }
        transaction.commit()?;
        Ok(began)
    }

    /// Every span of what each place an agent keeps a sign-in held. A span
    /// of an agent this build doesn't know, or of a kind of holding it
    /// doesn't, as a newer build may have recorded, is passed over.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read.
    pub(crate) fn sign_ins(&self) -> Result<Vec<(Place, Span)>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT agent, folder, provider, held, account, first, last FROM sign_in",
        )?;
        let mut rows = statement.query([])?;
        let mut spans = Vec::new();
        while let Some(row) = rows.next()? {
            let Some(agent) = Agent::from_key(&row.get::<_, String>(0)?) else {
                continue;
            };
            let held = match (
                row.get::<_, String>(3)?.as_str(),
                row.get::<_, Option<String>>(4)?,
            ) {
                ("account", Some(account)) => Held::Account(account),
                ("key", _) => Held::Key,
                ("other", _) => Held::Other,
                ("nothing", _) => Held::Nothing,
                _ => continue,
            };
            spans.push((
                Place {
                    agent,
                    folder: PathBuf::from(row.get::<_, String>(1)?),
                    provider: row.get(2)?,
                },
                Span {
                    held,
                    first: row.get(5)?,
                    last: row.get(6)?,
                },
            ));
        }
        Ok(spans)
    }

    /// Record that an alert of `kind` about `account`'s limit `key` was sent for
    /// the window resetting at `window`. `false` when one had been already:
    /// one for a reset within [`SAME_WINDOW`] of it, since a reset given as a
    /// countdown moves by a few seconds from read to read.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be written.
    pub(crate) fn send_alert(
        &mut self,
        account: &str,
        key: &str,
        kind: AlertKind,
        window: i64,
        at: Instant,
    ) -> Result<bool> {
        let added = self.connection.execute(
            "INSERT INTO alert (account, key, kind, window, at)
             SELECT ?1, ?2, ?3, ?4, ?5
             WHERE NOT EXISTS (SELECT 1 FROM alert WHERE account = ?1 AND key = ?2 AND kind = ?3
                                                     AND window BETWEEN ?4 - ?6 AND ?4 + ?6)",
            params![account, key, kind.key(), window, at.millis(), SAME_WINDOW],
        )?;
        Ok(added > 0)
    }

    /// The window, and the kind, of the alert last sent about `account`'s
    /// limit `key` running out, being used up or being back
    /// ([`AlertKind::of_running_out`]), for a window resetting before
    /// `before`. The milestones sent beside those, the weekly recap, and an
    /// alert of a kind this build doesn't know, as a newer one may have sent,
    /// are passed over.
    ///
    /// The last sent, not the latest window: the alerts of one window are
    /// recorded against resets a countdown's drift apart, in any order.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read.
    pub(crate) fn last_alert(
        &self,
        account: &str,
        key: &str,
        before: i64,
    ) -> Result<Option<(i64, AlertKind)>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT window, kind FROM alert WHERE account = ?1 AND key = ?2 AND window < ?3
             ORDER BY at DESC, window DESC",
        )?;
        let mut rows = statement.query(params![account, key, before])?;
        while let Some(row) = rows.next()? {
            let kind = AlertKind::from_key(&row.get::<_, String>(1)?);
            if let Some(kind) = kind.filter(|kind| kind.of_running_out()) {
                return Ok(Some((row.get(0)?, kind)));
            }
        }
        Ok(None)
    }

    /// Record that the weekly recap sent at `at` told of each of `weeks`: an
    /// account, the key of its weekly limit, and the window, named by when it
    /// reset.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be written.
    pub(crate) fn send_recap(&mut self, weeks: &[(&str, &str, i64)], at: Instant) -> Result<()> {
        let transaction = self.connection.transaction()?;
        {
            let mut send = transaction.prepare_cached(
                "INSERT OR IGNORE INTO alert (account, key, kind, window, at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (account, key, window) in weeks {
                send.execute(params![account, key, RECAP, window, at.millis()])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Whether a weekly recap told of `account`'s limit `key` in the window
    /// that reset at `window`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read.
    pub(crate) fn recapped(&self, account: &str, key: &str, window: i64) -> Result<bool> {
        let mut statement = self.connection.prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM alert
                            WHERE account = ?1 AND key = ?2 AND kind = ?3 AND window = ?4)",
        )?;
        Ok(statement.query_row(params![account, key, RECAP, window], |row| row.get(0))?)
    }

    /// Whether a weekly recap was sent at `since` or after.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Ledger`] when the ledger cannot be read.
    pub(crate) fn recapped_since(&self, since: Instant) -> Result<bool> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT EXISTS (SELECT 1 FROM alert WHERE kind = ?1 AND at >= ?2)")?;
        Ok(statement.query_row(params![RECAP, since.millis()], |row| row.get(0))?)
    }
}

/// The kind the weekly recap's rows in `alert` are kept as. It is no
/// [`AlertKind`], as the recap tells of every account at once.
const RECAP: &str = "recap";

/// What a place held, as the ledger keeps it: its kind, and the account for
/// a sign-in to one.
fn stored_held(held: &Held) -> (&'static str, Option<&str>) {
    match held {
        Held::Account(account) => ("account", Some(account)),
        Held::Key => ("key", None),
        Held::Other => ("other", None),
        Held::Nothing => ("nothing", None),
    }
}

/// The limit readings `rows` hold, as `key, name, scope, at, used, starts,
/// resets`.
fn readings(mut rows: rusqlite::Rows) -> Result<Vec<Reading>> {
    let mut readings = Vec::new();
    while let Some(row) = rows.next()? {
        readings.push(Reading {
            key: row.get(0)?,
            name: row.get(1)?,
            scope: row.get(2)?,
            at: instant(row.get(3)?, "limit reading time")?,
            used: row.get(4)?,
            starts: optional_instant(row.get(5)?, "limit window start")?,
            resets: optional_instant(row.get(6)?, "limit window reset")?,
        });
    }
    Ok(readings)
}

#[cfg(test)]
mod tests {
    use super::Ledger;
    use crate::agent::Agent;
    use crate::limits::{AlertKind, Read, Reported, Subscription};
    use crate::time::Instant;

    fn day(days: i64) -> Instant {
        Instant::from_millis(1_789_000_000_000 + days * 86_400_000).unwrap()
    }

    fn ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("ledger.sqlite")).unwrap();
        (dir, ledger)
    }

    /// A read of one Claude account whose limits `keys` are each 40% used.
    fn read(keys: &[&str]) -> Read {
        Read {
            id: "claude:".into(),
            label: None,
            plan: None,
            agents: vec![Agent::ClaudeCode],
            limits: Ok(keys
                .iter()
                .map(|key| Reported {
                    key: (*key).to_owned(),
                    name: "Weekly".into(),
                    scope: None,
                    used: 40.0,
                    starts: None,
                    resets: None,
                })
                .collect()),
        }
    }

    #[test]
    fn readings_go_after_ninety_days_but_what_the_latest_read_found() {
        let (_dir, mut ledger) = ledger();
        ledger
            .record_limits(
                Subscription::Claude,
                &[read(&["five_hour", "seven_day"])],
                day(0),
            )
            .unwrap();
        // The weekly window is no longer reported, as when the plan changed.
        ledger
            .record_limits(Subscription::Claude, &[read(&["five_hour"])], day(1))
            .unwrap();
        let latest = |ledger: &Ledger| -> Vec<(String, Instant)> {
            ledger
                .latest_readings("claude:")
                .unwrap()
                .into_iter()
                .map(|reading| (reading.key, reading.at))
                .collect()
        };
        assert_eq!(latest(&ledger), [("five_hour".to_owned(), day(1))]);
        // Signed out of since, and a hundred days on another read.
        ledger
            .record_limits(Subscription::Claude, &[], day(100))
            .unwrap();
        let kept: Vec<(String, Instant)> = ledger
            .readings("claude:", day(0))
            .unwrap()
            .into_iter()
            .map(|reading| (reading.key, reading.at))
            .collect();
        assert_eq!(kept, [("five_hour".to_owned(), day(1))]);
        assert_eq!(latest(&ledger), kept);
    }

    #[test]
    fn a_stored_time_out_of_range_is_refused_as_corrupt() {
        let (_dir, mut ledger) = ledger();
        ledger
            .record_limits(Subscription::Claude, &[read(&["five_hour"])], day(0))
            .unwrap();
        // No instant is i64::MAX milliseconds from the epoch, some 292
        // million years.
        ledger
            .connection
            .execute("UPDATE limit_reading SET resets = 9223372036854775807", [])
            .unwrap();
        assert!(matches!(
            ledger.latest_readings("claude:"),
            Err(crate::Error::Corrupt { .. })
        ));
        ledger
            .connection
            .execute("UPDATE account SET checked_at = 9223372036854775807", [])
            .unwrap();
        assert!(matches!(
            ledger.accounts(),
            Err(crate::Error::Corrupt { .. })
        ));
    }

    #[test]
    fn an_alert_of_a_kind_a_newer_build_sent_is_passed_over() {
        let (_dir, mut ledger) = ledger();
        let window = day(1).millis();
        ledger
            .send_alert("claude:", "five_hour", AlertKind::Reached, window, day(0))
            .unwrap();
        ledger
            .connection
            .execute(
                "INSERT INTO alert (account, key, kind, window, at)
                 VALUES ('claude:', 'five_hour', 'halfway', ?1, ?2)",
                [window, day(0).millis() + 1],
            )
            .unwrap();
        assert_eq!(
            ledger
                .last_alert("claude:", "five_hour", day(2).millis())
                .unwrap(),
            Some((window, AlertKind::Reached))
        );
    }

    #[test]
    fn what_a_newer_build_recorded_of_accounts_is_passed_over() {
        let (_dir, mut ledger) = ledger();
        ledger
            .record_limits(Subscription::Claude, &[read(&["five_hour"])], day(0))
            .unwrap();
        // A newer build knows a subscription and an agent this one doesn't.
        ledger
            .connection
            .execute_batch(
                r#"INSERT INTO account (id, subscription, via, checked_at, signed_in)
                   VALUES ('gemini:1', 'gemini', '["gemini-cli"]', 0, 1);
                   UPDATE account SET via = '["claude-code","jules"]' WHERE id = 'claude:';"#,
            )
            .unwrap();
        let accounts: Vec<(String, Vec<Agent>)> = ledger
            .accounts()
            .unwrap()
            .into_iter()
            .map(|account| (account.id, account.agents))
            .collect();
        assert_eq!(accounts, [("claude:".to_owned(), vec![Agent::ClaudeCode])]);
    }
}
