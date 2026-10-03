//! Reading limits and recording them: each subscription's accounts read
//! from their providers ([`limits::read_subscription`]), what was read and
//! what each place a sign-in is kept held recorded in the ledger, and
//! subscribers told of the limits and, in the process that keeps the data
//! directory, of the alerts and the weekly recap they give rise to. Limits
//! read while this process reads history in full wait for that read, which
//! records them between its groups.

use std::sync::atomic::Ordering;
use std::sync::{Mutex, PoisonError};

use crate::Engine;
use crate::error::Result;
use crate::ledger::Ledger;
use crate::limits::{self, Alert, Subscription, WeekEnded};
use crate::runtime::Change;
use crate::time::{Instant, Zone};

/// Limits waiting for a scan, until dropped ([`Engine::queue_limits`]).
pub(crate) struct Queueing<'a>(&'a Mutex<Option<Vec<LimitsRead>>>);

impl Drop for Queueing<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// What reading one subscription's limits found, to be recorded.
pub(crate) struct LimitsRead {
    subscription: Subscription,
    reads: Vec<limits::AccountRead>,
    seen: Vec<limits::Seen>,
    at: Instant,
}

impl Engine {
    /// Have limits read from now on wait for the scan about to start, until
    /// what this returns is dropped, as it is however the scan ends: one
    /// that panicked would otherwise leave every later read waiting for a
    /// scan that never records it. Those waiting then are read again in
    /// minutes, as any read that failed is.
    pub(crate) fn queue_limits(&self) -> Queueing<'_> {
        *self.waiting.lock().unwrap_or_else(PoisonError::into_inner) = Some(Vec::new());
        Queueing(&self.waiting)
    }

    /// Record the limits waiting for the scan that holds `ledger`, and tell
    /// subscribers of them; with `last`, as the scan ends, none wait after.
    /// The weekly recap waits for the next read, since it draws on the
    /// history the scan is still reading. A read that can't be recorded is
    /// told of, as a read that fails is, and stops neither the rest nor the
    /// scan: limits are read again in minutes, and history is read now.
    pub(crate) fn record_waiting(&self, ledger: &mut Ledger, last: bool) {
        let waited = {
            let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
            if last {
                waiting.take().unwrap_or_default()
            } else {
                waiting.as_mut().map(std::mem::take).unwrap_or_default()
            }
        };
        for read in waited {
            let name = read.subscription.name();
            match self.record_limits_read(ledger, read, false) {
                Ok((alerts, _)) => self.tell_limits(alerts, Vec::new()),
                Err(error) => self.publish(Change::Trouble(format!("{name} limits: {error}"))),
            }
        }
    }

    /// Read every subscription's limits from its provider now, with the
    /// sign-ins on this Mac, and each OpenRouter key's own from OpenRouter,
    /// and tell subscribers what was read. Only while
    /// this process keeps the data directory ([`Engine::run`]) are the alerts
    /// the limits give rise to worked out and sent.
    ///
    /// # Errors
    ///
    /// Returns the first of any subscription's failures, having read every
    /// other: a place that keeps sign-ins to it that is there but can't be
    /// read, as a file is while an agent writes it, which leaves its accounts
    /// as they were until the next read; or a ledger or cache that can't be
    /// read or written. A provider that cannot be reached is no failure but
    /// the account's problem, recorded with it. Called while another thread
    /// of this process reads history in full, what it reads is recorded by
    /// that read, within a tenth of a second, rather than by the time it
    /// returns.
    pub fn read_all_limits(&self) -> Result<()> {
        let mut first = Ok(());
        for subscription in Subscription::ALL {
            // Read whatever became of the one before.
            let read = self.read_limits(subscription);
            first = first.and(read);
        }
        first
    }

    /// Read every account of `subscription` from its provider, record what was
    /// read and what each place a sign-in to it is kept held, and tell
    /// subscribers of the limits, of any sessions whose account that changed,
    /// and, in the process that keeps the data directory, of any alerts they
    /// give rise to and of the weekly recap when it is due. While this
    /// process reads history in full, what was read is left for that read to
    /// record, within a group of it, rather than wait for it to end.
    ///
    /// # Errors
    ///
    /// Returns an error, recording nothing, when a place that keeps sign-ins
    /// to `subscription` can't be read, and when the ledger or the cache
    /// can't be read or written.
    pub(crate) fn read_limits(&self, subscription: Subscription) -> Result<()> {
        let at = Instant::now();
        // The requests are made before the ledger is held, so a slow provider
        // holds up nothing else.
        let folders = self.folders();
        let (reads, seen) = limits::read_subscription(subscription, &folders, &self.home, at)?;
        self.take_limits(LimitsRead {
            subscription,
            reads,
            seen,
            at,
        })
    }

    /// Record `read` and tell subscribers of it, or, while this process
    /// reads history in full, leave it for that read to record.
    ///
    /// # Errors
    ///
    /// Returns an error when the ledger or the cache can't be read or
    /// written.
    fn take_limits(&self, read: LimitsRead) -> Result<()> {
        {
            let mut waiting = self.waiting.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(waiting) = waiting.as_mut() {
                waiting.push(read);
                return Ok(());
            }
        }
        let (alerts, weeks) = {
            let mut ledger = self.writing()?;
            self.record_limits_read(&mut ledger, read, true)?
        };
        self.tell_limits(alerts, weeks);
        Ok(())
    }

    /// Record `read` in `ledger`, held for writing, and work out the alerts
    /// it gives rise to and, with `recap`, the weekly recap when it is due.
    fn record_limits_read(
        &self,
        ledger: &mut Ledger,
        read: LimitsRead,
        recap: bool,
    ) -> Result<(Vec<Alert>, Vec<WeekEnded>)> {
        let LimitsRead {
            subscription,
            reads,
            mut seen,
            at,
        } = read;
        ledger.record_limits(subscription, &reads, at)?;
        // A key or unread sign-in found before and not now is gone.
        if subscription == Subscription::ApiKey {
            let gone = limits::api::vanished(&seen, &ledger.sign_ins()?, &self.folders());
            seen.extend(gone);
        }
        // Which account usage drew on rests on what was signed in where.
        if ledger.record_sign_ins(&seen, at)? {
            self.catch_up(ledger)?;
        }
        // Alerts are the keeper's to send, since it is the app that shows
        // them: another process, such as an MCP server reading limits while
        // no app runs, would record as sent alerts no one sees.
        if !self.keeping.load(Ordering::Acquire) {
            return Ok((Vec::new(), Vec::new()));
        }
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let state = limits::current(ledger, cache.connection(), at)?;
        let alerts = limits::alert::alerts(ledger, &state, at)?;
        let weeks = if recap {
            limits::recap::recap(ledger, cache.connection(), &state, at, &Zone::system())?
        } else {
            Vec::new()
        };
        Ok((alerts, weeks))
    }

    /// Tell subscribers that limits were read, and of the `alerts` and recap
    /// `weeks` they gave rise to.
    fn tell_limits(&self, alerts: Vec<Alert>, weeks: Vec<WeekEnded>) {
        self.publish(Change::Limits);
        for alert in alerts {
            self.publish(Change::Alert(alert));
        }
        if !weeks.is_empty() {
            self.publish(Change::Recap(weeks));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use super::LimitsRead;
    use crate::limits::{AccountRead, Held, Place, Reported, Seen};
    use crate::{Agent, AlertKind, Change, Engine, Instant, Subscription};

    #[test]
    fn limits_read_while_history_is_read_in_full_are_recorded_within_it() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let engine = Arc::new(Engine::open(data.path(), home.path()).unwrap());
        // A scan holds the ledger throughout, as a first read of history
        // does for seconds.
        let mut scanning = engine.writing().unwrap();
        let queueing = engine.queue_limits();
        let read = LimitsRead {
            subscription: Subscription::ChatGpt,
            reads: vec![AccountRead {
                id: "chatgpt:acct-1".into(),
                label: None,
                plan: None,
                agents: vec![Agent::Codex],
                limits: Ok(vec![Reported {
                    key: "primary_window".into(),
                    name: "5 hours".into(),
                    scope: None,
                    used: 40.0,
                    starts: None,
                    resets: None,
                }]),
            }],
            seen: Vec::new(),
            at: Instant::from_millis(1_789_000_000_000).unwrap(),
        };
        // Handed to the scan at once, rather than waiting for it to end.
        let (taken, take) = mpsc::channel();
        let taking = Arc::clone(&engine);
        std::thread::spawn(move || taken.send(taking.take_limits(read).is_ok()));
        assert_eq!(take.recv_timeout(Duration::from_secs(10)), Ok(true));
        // Recorded at the scan's next group, while it still holds the ledger.
        engine.record_waiting(&mut scanning, false);
        drop(queueing);
        drop(scanning);
        let accounts = engine.limits().unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, "chatgpt:acct-1");
        assert_eq!(accounts[0].limits[0].left(), Some(60.0));
    }

    #[test]
    fn a_scan_that_stops_unexpectedly_leaves_no_limits_waiting_for_it() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let engine = Engine::open(data.path(), home.path()).unwrap();
        let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _queueing = engine.queue_limits();
            panic!("the scan stopped");
        }));
        assert!(stopped.is_err());
        // Limits read after are recorded as they are read, not left for a
        // scan that will never record them.
        assert!(engine.waiting.lock().unwrap().is_none());
    }

    #[test]
    fn a_subscription_whose_sign_ins_cant_be_read_is_left_as_it_was() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let engine = Engine::open(data.path(), home.path()).unwrap();
        let read_at = Instant::from_millis(1_789_000_000_000).unwrap();
        let read = AccountRead {
            id: "chatgpt:acct-1".into(),
            label: None,
            plan: None,
            agents: vec![Agent::Codex],
            limits: Ok(vec![Reported {
                key: "primary_window".into(),
                name: "5 hours".into(),
                scope: None,
                used: 40.0,
                starts: None,
                resets: None,
            }]),
        };
        engine
            .writing()
            .unwrap()
            .record_limits(Subscription::ChatGpt, &[read], read_at)
            .unwrap();
        // Codex part way through writing its sign-in.
        let auth = home.path().join(".codex/auth.json");
        std::fs::create_dir_all(auth.parent().unwrap()).unwrap();
        std::fs::write(&auth, r#"{"tokens": {"access_to"#).unwrap();

        assert!(engine.read_limits(Subscription::ChatGpt).is_err());
        let account = &engine.limits().unwrap()[0];
        assert!(account.signed_in, "not signed out");
        assert_eq!(account.checked_at, Some(read_at), "not read at all");
    }

    #[test]
    fn only_the_process_keeping_the_data_directory_sends_alerts() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let engine = Engine::open(data.path(), home.path()).unwrap();
        let changes = engine.subscribe();
        // A limit used up in a window that ended ten minutes ago, said to be
        // reached then. Its account is signed in nowhere here, so reading
        // limits finds it signed out and back.
        let now = Instant::now().millis();
        let minutes = |minutes: i64| Instant::from_millis(now + minutes * 60_000).unwrap();
        let read = AccountRead {
            id: "chatgpt:acct-1".into(),
            label: None,
            plan: None,
            agents: vec![Agent::Codex],
            limits: Ok(vec![Reported {
                key: "primary_window".into(),
                name: "5 hours".into(),
                scope: None,
                used: 100.0,
                starts: None,
                resets: Some(minutes(-10)),
            }]),
        };
        {
            let mut ledger = engine.writing().unwrap();
            ledger
                .record_limits(Subscription::ChatGpt, &[read], minutes(-60))
                .unwrap();
            let window = minutes(-10).millis();
            let reached = AlertKind::Reached;
            ledger
                .send_alert(
                    "chatgpt:acct-1",
                    "primary_window",
                    reached,
                    window,
                    minutes(-60),
                )
                .unwrap();
        }
        let alerts = || -> Vec<AlertKind> {
            std::iter::from_fn(|| changes.try_recv().ok())
                .filter_map(|change| match change {
                    Change::Alert(alert) => Some(alert.kind),
                    _ => None,
                })
                .collect()
        };

        engine.read_limits(Subscription::ChatGpt).unwrap();
        assert!(alerts().is_empty(), "nothing is said, or recorded as said");
        engine.keeping.store(true, Ordering::Release);
        engine.read_limits(Subscription::ChatGpt).unwrap();
        assert_eq!(alerts(), [AlertKind::Available]);
    }

    /// 2026-09-27 at `hour`:`minute` UTC.
    fn at(hour: i64, minute: i64) -> Instant {
        Instant::from_millis(1_790_467_200_000 + hour * 3_600_000 + minute * 60_000).unwrap()
    }

    /// Write a Claude Code session `session`, in the agent's `folder`, of
    /// one response of Claude Opus 5 at each of `times`, each 1,000 tokens
    /// in and 100 out.
    fn claude_session(folder: &Path, session: &str, times: &[Instant]) {
        claude_session_of(folder, session, "claude-opus-5", times);
    }

    /// As [`claude_session`], of `model`.
    fn claude_session_of(folder: &Path, session: &str, model: &str, times: &[Instant]) {
        let log = folder
            .join("projects/-work-ledger")
            .join(format!("{session}.jsonl"));
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        let lines: String = times
            .iter()
            .enumerate()
            .map(|(index, time)| {
                let id = format!("{session}-{index}");
                format!(
                    "{}\n",
                    serde_json::json!({"type": "assistant", "sessionId": session,
                        "timestamp": time.to_string(), "requestId": format!("req_{id}"),
                        "cwd": "/work/ledger",
                        "message": {"id": format!("msg_{id}"), "model": model,
                                    "usage": {"input_tokens": 1_000, "output_tokens": 100}}})
                )
            })
            .collect();
        std::fs::write(log, lines).unwrap();
    }

    /// What Claude Code's `folder` held for Anthropic's usage.
    fn claude_signed(folder: &Path, account: Option<&str>) -> Seen {
        Seen {
            place: Place {
                agent: Agent::ClaudeCode,
                folder: folder.to_path_buf(),
                provider: "anthropic".to_owned(),
            },
            held: account.map_or(Held::Nothing, |account| Held::Account(account.to_owned())),
        }
    }

    /// Each response in the cache, by its key, and the account it drew on.
    fn drew_on(engine: &Engine) -> Vec<(String, Option<String>)> {
        let readers = engine.reading().unwrap();
        let mut statement = readers
            .cache
            .connection()
            .prepare("SELECT response, account FROM usage ORDER BY response")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn usage_draws_on_the_account_signed_in_where_and_when_it_was_made() {
        let home = tempfile::tempdir().unwrap();
        let own = home.path().join(".claude");
        let work = home.path().join(".claude-work");
        // Claude Code in its own folder answered at 10:00 and 14:00; pointed
        // at ~/.claude-work, where it keeps whose sign-in it holds, at 11:00.
        claude_session(&own, "own", &[at(10, 0), at(14, 0)]);
        claude_session(&work, "work", &[at(11, 0)]);
        std::fs::write(work.join(".claude.json"), "{}").unwrap();
        let spans = [
            // Its own folder held personal from 9:00 to 10:30, and, after
            // the person signed into another account, work from 13:00.
            (at(9, 0), claude_signed(&own, Some("claude:personal"))),
            (at(10, 30), claude_signed(&own, Some("claude:personal"))),
            (at(13, 0), claude_signed(&own, Some("claude:work"))),
            (at(15, 0), claude_signed(&own, Some("claude:work"))),
            // ~/.claude-work held a third account throughout.
            (at(9, 0), claude_signed(&work, Some("claude:other"))),
        ];
        let expected = [
            (
                "msg_own-0:req_own-0".to_owned(),
                Some("claude:personal".to_owned()),
            ),
            (
                "msg_own-1:req_own-1".to_owned(),
                Some("claude:work".to_owned()),
            ),
            (
                "msg_work-0:req_work-0".to_owned(),
                Some("claude:other".to_owned()),
            ),
        ];

        // Recorded before history is read, and after, in either order: the
        // same accounts.
        for (history_first, reversed) in [(true, false), (false, true)] {
            let data = tempfile::tempdir().unwrap();
            let engine = Engine::open(data.path(), home.path()).unwrap();
            if history_first {
                engine.scan().unwrap();
            }
            let mut ordered = spans.to_vec();
            if reversed {
                ordered.reverse();
            }
            for (when, seen) in ordered {
                let mut ledger = engine.writing().unwrap();
                if ledger.record_sign_ins(&[seen], when).unwrap() {
                    engine.catch_up(&mut ledger).unwrap();
                }
            }
            if !history_first {
                engine.scan().unwrap();
            }
            assert_eq!(drew_on(&engine), expected, "history first: {history_first}");
            // Of a session that drew on two accounts as much, the first by id.
            let sessions: Vec<(String, Option<String>)> = engine
                .sessions(&crate::SessionQuery {
                    empty: true,
                    ..crate::SessionQuery::default()
                })
                .unwrap()
                .items
                .into_iter()
                .map(|row| (row.key.native().to_owned(), row.account))
                .collect();
            assert!(sessions.contains(&("own".to_owned(), Some("claude:personal".to_owned()))));
            assert!(sessions.contains(&("work".to_owned(), Some("claude:other".to_owned()))));
        }
    }

    #[test]
    fn two_accounts_of_one_subscription_rising_at_once_share_only_their_own_use() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let own = home.path().join(".claude");
        let work = home.path().join(".claude-work");
        // One response in each folder in the same stretch, each costing the
        // same.
        claude_session(&own, "own", &[at(10, 0)]);
        claude_session(&work, "work", &[at(10, 10)]);
        std::fs::write(work.join(".claude.json"), "{}").unwrap();
        let engine = Engine::open(data.path(), home.path()).unwrap();
        engine.scan().unwrap();
        // Each account's five hours began at 9:00 and was read at 11:00:
        // personal's had risen 10 points, work's 20.
        let read = |id: &str, used: f64| AccountRead {
            id: id.to_owned(),
            label: None,
            plan: None,
            agents: vec![Agent::ClaudeCode],
            limits: Ok(vec![Reported {
                key: "five_hour".to_owned(),
                name: "5 hours".to_owned(),
                scope: None,
                used,
                starts: Some(at(9, 0)),
                resets: Some(at(14, 0)),
            }]),
        };
        {
            let mut ledger = engine.writing().unwrap();
            ledger
                .record_limits(
                    Subscription::Claude,
                    &[read("claude:personal", 10.0), read("claude:work", 20.0)],
                    at(11, 0),
                )
                .unwrap();
            ledger
                .record_sign_ins(
                    &[
                        claude_signed(&own, Some("claude:personal")),
                        claude_signed(&work, Some("claude:work")),
                    ],
                    at(11, 0),
                )
                .unwrap();
            engine.catch_up(&mut ledger).unwrap();
        }
        let readers = engine.reading().unwrap();
        let taken = |account: &str| -> Vec<(String, f64)> {
            crate::limits::share::window(
                &readers.ledger,
                readers.cache.connection(),
                account,
                "five_hour",
                at(11, 30),
            )
            .unwrap()
            .unwrap()
            .sessions
            .into_iter()
            .map(|(key, share)| (key.native().to_owned(), share))
            .collect()
        };
        // Each rise is all its own account's one response's: 10 points of
        // personal's, 20 of work's, neither shared with the other.
        assert_eq!(taken("claude:personal"), [("own".to_owned(), 10.0)]);
        assert_eq!(taken("claude:work"), [("work".to_owned(), 20.0)]);
    }

    #[test]
    fn a_rise_only_unpriced_responses_spent_in_is_neither_shared_nor_elsewhere() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let own = home.path().join(".claude");
        // A priced response at 10:00, and one of a model no catalog prices
        // at 12:00, both of the account signed in from 9:00.
        claude_session(&own, "priced", &[at(10, 0)]);
        claude_session_of(&own, "unpriced", "claude-nobody-knows-1", &[at(12, 0)]);
        let engine = Engine::open(data.path(), home.path()).unwrap();
        engine.scan().unwrap();
        let read = |used: f64| AccountRead {
            id: "claude:personal".to_owned(),
            label: None,
            plan: None,
            agents: vec![Agent::ClaudeCode],
            limits: Ok(vec![Reported {
                key: "five_hour".to_owned(),
                name: "5 hours".to_owned(),
                scope: None,
                used,
                starts: Some(at(9, 0)),
                resets: Some(at(14, 0)),
            }]),
        };
        {
            let mut ledger = engine.writing().unwrap();
            ledger
                .record_sign_ins(&[claude_signed(&own, Some("claude:personal"))], at(9, 0))
                .unwrap();
            for (when, used) in [(at(11, 0), 10.0), (at(13, 0), 25.0), (at(13, 30), 30.0)] {
                ledger
                    .record_limits(Subscription::Claude, &[read(used)], when)
                    .unwrap();
            }
            engine.catch_up(&mut ledger).unwrap();
        }
        let readers = engine.reading().unwrap();
        let window = crate::limits::share::window(
            &readers.ledger,
            readers.cache.connection(),
            "claude:personal",
            "five_hour",
            at(13, 45),
        )
        .unwrap()
        .unwrap();
        // 9:00 to 11:00 rose 10, all the priced response's; 11:00 to 13:00
        // rose 15 while only the unpriced one was made here; 13:00 to 13:30
        // rose 5 while nothing was.
        let sessions: Vec<(String, f64)> = window
            .sessions
            .iter()
            .map(|(key, share)| (key.native().to_owned(), *share))
            .collect();
        assert_eq!(sessions, [("priced".to_owned(), 10.0)]);
        assert_eq!(window.unpriced, 15.0);
        assert_eq!(window.elsewhere, 5.0);
    }
}
