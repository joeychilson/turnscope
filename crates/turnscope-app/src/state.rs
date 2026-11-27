//! Domain state, engine event handling, and side-effect queuing for the app.
//! Ephemeral UI chrome state (which row is open, folds, active page/tab) stays in the shell
//! so UI interactions never round-trip through FFI or JSON.

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use turnscope::alerts::{Notify, Settings, replaces};
use turnscope::engine::{Event, Shown, Stopped, UsedMost};
use turnscope::status::{Account, Agent, Connection, Standing, Status};

use crate::view::{self, AppSync};
use crate::words::{self, AlertNote, Words};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preferences {
    pub dismissed_connect: BTreeSet<String>,
}

/// What the shell does once, after it next syncs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Effect {
    PostAlerts { alerts: Vec<AlertNote> },
    RemoveNotifications { identifiers: Vec<String> },
    SavePreferences { dismissed_connect: Vec<String> },
}

/// What the shell asks. Variants with no fields are written `{}`, as Swift encodes them.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Action {
    PanelOpened {},
    PanelClosed {},
    SetHidden {
        id: String,
        hidden: bool,
    },
    SetConnected {
        id: String,
        connecting: bool,
    },
    SetNotify {
        notify: Notify,
    },
    DismissConnect {
        id: String,
    },
    AcknowledgeAlert {
        id: i64,
        sign_in_account: Option<String>,
    },
    RefreshNow {},
    Tick {},
}

/// A request to the engine, sent once the core's lock is let go.
#[derive(Debug, PartialEq)]
pub enum Call {
    UsedMost { account: String, limit: String },
    AskLimits,
    RefreshNow,
    Hide { account: String, hidden: bool },
    Connect { agent: String, connecting: bool },
    SaveSettings(Settings),
    Acknowledge(i64),
}

#[derive(Default)]
pub struct State {
    pub status: Option<Status>,
    pub agents: Vec<Agent>,
    pub problem: Option<Stopped>,
    pub refreshing: bool,
    pub changing_agents: HashMap<String, Connection>,
    /// Why a connect failed, with the agent's connection then: it's said until that changes.
    pub agent_errors: HashMap<String, (Connection, String)>,
    pub used: HashMap<String, (String, Vec<UsedMost>)>,
    /// A fixed status's time (`--fixture`, `--snapshot`), which its words are as of. With
    /// one, there's no engine, and nothing changed is kept.
    pub frozen: Option<i64>,
    pub from_disk_image: bool,
    pub preferences: Preferences,
    /// The minute the open panel's words are as of: set as it opens, and each minute after.
    pub now: i64,
    pub panel_open: bool,
    // Taken by the shell's next `sync`.
    pub effects: Vec<Effect>,
    pub animated: bool,
}

impl State {
    pub fn fixed(shown: Shown) -> State {
        let mut state = State {
            frozen: Some(shown.status.at),
            ..State::default()
        };
        state.take(shown);
        state
    }

    pub fn take(&mut self, shown: Shown) {
        self.agent_errors.retain(|id, (then, _)| {
            shown
                .agents
                .iter()
                .any(|a| &a.id == id && a.connection == *then)
        });
        self.status = Some(shown.status);
        self.agents = shown.agents;
    }

    fn live(&self) -> bool {
        self.frozen.is_none()
    }

    pub fn all(&self) -> &[Account] {
        self.status.as_ref().map_or(&[], |status| &status.accounts)
    }

    pub fn reading(&self) -> bool {
        self.status
            .as_ref()
            .is_some_and(|status| !status.history_read)
    }

    /// Until the history is read, every account counts as in use, so the panel doesn't say
    /// "Nothing in use" while it reads.
    pub fn counts(&self, account: &Account) -> bool {
        self.reading() || account.in_use
    }

    pub fn in_use(&self) -> Vec<&Account> {
        self.all()
            .iter()
            .filter(|a| !a.hidden && self.counts(a))
            .collect()
    }

    pub fn recent(&self) -> Vec<&Account> {
        self.all()
            .iter()
            .filter(|a| !a.hidden && !self.counts(a) && a.recent)
            .collect()
    }

    pub fn unused(&self) -> Vec<&Account> {
        self.all()
            .iter()
            .filter(|a| !a.hidden && !self.counts(a) && !a.recent)
            .collect()
    }

    pub fn hidden(&self) -> Vec<&Account> {
        self.all().iter().filter(|a| a.hidden).collect()
    }

    pub fn hidden_in_use(&self) -> bool {
        self.all().iter().any(|a| a.hidden && self.counts(a))
    }

    pub fn installed(&self) -> Vec<&Agent> {
        self.agents.iter().filter(|a| a.installed).collect()
    }

    pub fn agent_names(&self) -> Vec<&str> {
        self.agents.iter().map(|a| a.name.as_str()).collect()
    }

    pub fn words(&self) -> Words {
        Words::new(self.frozen.unwrap_or(self.now))
    }

    pub fn words_now(&self) -> Words {
        Words::new(self.frozen.unwrap_or_else(turnscope::now))
    }

    /// Whether a newer Turnscope wrote to the data, so nothing can be changed.
    pub fn stopped(&self) -> bool {
        matches!(self.problem, Some(Stopped::Newer { .. }))
    }

    /// Whether `agent`'s connection can be changed: not while it's changing, from a disk image,
    /// or once a newer Turnscope wrote to the data.
    pub fn can_change(&self, agent: &str) -> bool {
        !self.changing_agents.contains_key(agent) && !self.from_disk_image && !self.stopped()
    }

    /// Whether Refresh Limits can be asked for: without a status, there's nothing to refresh.
    pub fn can_refresh(&self) -> bool {
        self.live() && self.status.is_some() && !self.stopped()
    }

    pub fn verdict(&self, words: &Words) -> (String, String) {
        if self.all().is_empty() {
            words::welcome(&self.installed(), &self.agent_names(), self.reading())
        } else {
            words.verdict(
                self.in_use().first().copied(),
                self.hidden_in_use(),
                self.all(),
            )
        }
    }

    pub fn to_connect(&self) -> Option<&Agent> {
        if self.from_disk_image {
            return None;
        }
        self.agents.iter().find(|a| {
            a.installed
                && a.in_use
                && a.connection != Connection::Connected
                && !self.preferences.dismissed_connect.contains(&a.id)
        })
    }

    pub fn used_most(&self, account: &Account) -> &[UsedMost] {
        match self.used.get(&account.id) {
            Some((limit, sessions)) if Some(limit) == account.deciding.as_ref() => sessions,
            _ => &[],
        }
    }

    /// The accounts in use, the most urgent first, as the item drops those that don't fit from
    /// the end: running out, while there's time to change course, then used up, which drops
    /// out once its agent stops drawing on it, then the rest. The sort is stable, so each
    /// keeps `status::rank`'s order: the soonest to run out, then the least left, first.
    /// Until the history is read, every account counts as in use, so only the first shows.
    pub fn in_menu_bar(&self) -> Vec<&Account> {
        let mut shown = self.in_use();
        shown.sort_by_key(|a| match a.standing() {
            Standing::RunningOut => 0,
            Standing::UsedUp => 1,
            Standing::Lasts => 2,
        });
        if self.reading() {
            shown.truncate(1);
        }
        shown
    }

    pub fn urgent(&self) -> bool {
        self.in_menu_bar()
            .iter()
            .any(|a| a.standing() != Standing::Lasts)
    }

    pub fn counting(&self) -> bool {
        self.live() && (self.panel_open || self.urgent())
    }

    pub fn mark_hidden(&mut self, account: &str, hidden: bool) {
        if let Some(status) = &mut self.status
            && let Some(found) = status.accounts.iter_mut().find(|a| a.id == account)
        {
            found.hidden = hidden;
        }
    }

    /// Shown accounts to ask what used them most, each with its deciding limit:
    /// those `again` picks, and those with no answer for that limit yet.
    pub fn asks(&self, again: impl Fn(&Account) -> bool) -> Vec<Call> {
        self.all()
            .iter()
            .filter(|a| !a.hidden)
            .filter_map(|a| {
                let limit = a.deciding.as_ref()?;
                let have = self.used.get(&a.id).is_some_and(|(of, _)| of == limit);
                (again(a) || !have).then(|| Call::UsedMost {
                    account: a.id.clone(),
                    limit: limit.clone(),
                })
            })
            .collect()
    }

    /// Returns the calls to send the engine, and whether to wake the shell.
    pub fn took(&mut self, event: Event) -> (Vec<Call>, bool) {
        let mut calls = Vec::new();
        match event {
            Event::Shown(shown, alerts) => {
                let before = self.status.take();
                self.take(*shown);
                let was_refused = |id: &str| {
                    before
                        .as_ref()
                        .is_none_or(|b| b.accounts.iter().any(|a| a.id == id && a.refused()))
                };
                let identifiers: Vec<String> = self
                    .all()
                    .iter()
                    .filter(|a| !a.refused() && was_refused(&a.id))
                    .map(|a| replaces(&a.id, None))
                    .collect();
                if !identifiers.is_empty() {
                    self.effects
                        .push(Effect::RemoveNotifications { identifiers });
                }
                if self.panel_open {
                    self.animated = true;
                    calls = self.asks(|a| self.counts(a));
                }
                if !alerts.is_empty() {
                    let words = self.words_now();
                    let alerts = alerts
                        .into_iter()
                        .map(|alert| words.note(alert, self.all()))
                        .collect();
                    self.effects.push(Effect::PostAlerts { alerts });
                }
                (calls, true)
            }
            Event::Kept(stopped) => {
                self.problem = stopped;
                (calls, true)
            }
            Event::UsedMost {
                account,
                limit,
                sessions,
            } => {
                let entry = (limit, sessions);
                let changed = self.used.get(&account) != Some(&entry);
                if changed {
                    self.used.insert(account, entry);
                }
                let wake = changed && self.panel_open;
                if wake {
                    self.animated = true;
                }
                (calls, wake)
            }
            Event::Connected {
                agent,
                connection,
                error,
            } => {
                self.changing_agents.remove(&agent);
                if let Some(shown) = self.agents.iter_mut().find(|a| a.id == agent) {
                    shown.connection = connection;
                    if let Some(error) = error {
                        self.agent_errors.insert(agent, (connection, error));
                    }
                }
                (calls, self.panel_open)
            }
            Event::Refreshed => (calls, std::mem::take(&mut self.refreshing)),
        }
    }

    /// Returns the calls to send the engine.
    pub fn apply(&mut self, action: Action, now_ms: i64) -> Vec<Call> {
        let mut calls = Vec::new();
        match action {
            Action::PanelOpened {} => {
                if !self.panel_open {
                    self.panel_open = true;
                    self.now = turnscope::time::minute(now_ms);
                    calls = self.asks(|_| true);
                    calls.push(Call::AskLimits);
                }
            }
            Action::PanelClosed {} => {
                self.panel_open = false;
            }
            Action::SetHidden { id, hidden } => {
                if !self.stopped() {
                    self.mark_hidden(&id, hidden);
                    calls.push(Call::Hide {
                        account: id,
                        hidden,
                    });
                }
            }
            Action::SetConnected { id, connecting } => {
                if self.live() && self.can_change(&id) {
                    let to = if connecting {
                        Connection::Connected
                    } else {
                        Connection::Available
                    };
                    self.changing_agents.insert(id.clone(), to);
                    self.agent_errors.remove(&id);
                    calls.push(Call::Connect {
                        agent: id,
                        connecting,
                    });
                }
            }
            Action::SetNotify { notify } => {
                if !self.stopped()
                    && let Some(status) = &mut self.status
                {
                    status.settings.notify = notify;
                    calls.push(Call::SaveSettings(status.settings));
                }
            }
            Action::DismissConnect { id } => {
                if self.preferences.dismissed_connect.insert(id) {
                    self.save_preferences();
                }
            }
            Action::AcknowledgeAlert {
                id,
                sign_in_account,
            } => {
                calls.push(Call::Acknowledge(id));
                // The account may have been read again while the notification was posted.
                if let Some(account) = sign_in_account
                    && self.all().iter().any(|a| a.id == account && !a.refused())
                {
                    self.effects.push(Effect::RemoveNotifications {
                        identifiers: vec![replaces(&account, None)],
                    });
                }
            }
            Action::RefreshNow {} => {
                if self.can_refresh() && !self.refreshing {
                    self.refreshing = true;
                    calls.push(Call::RefreshNow);
                }
            }
            Action::Tick {} => {
                if self.panel_open {
                    self.now = turnscope::time::minute(now_ms);
                }
            }
        }
        calls
    }

    fn save_preferences(&mut self) {
        if !self.live() {
            return;
        }
        self.effects.push(Effect::SavePreferences {
            dismissed_connect: self.preferences.dismissed_connect.iter().cloned().collect(),
        });
    }

    pub fn sync(&mut self) -> AppSync {
        AppSync {
            animated: std::mem::take(&mut self.animated),
            effects: std::mem::take(&mut self.effects),
            counting: self.counting(),
            menu_bar: view::build_menu_bar(self),
            panel: view::build_panel(self),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const CLAUDE: &str = "anthropic:acct-1:org-1";
    const CODEX: &str = "openai:acct-2";
    const OPENROUTER: &str = "openrouter:api";
    const OPENCODE_GO: &str = "opencode-go:key-0123456789abcdef";

    /// The example status in tests/fixtures/status.json, as `--fixture` shows it.
    pub(crate) fn example() -> Shown {
        serde_json::from_str(include_str!("../../turnscope/tests/fixtures/status.json")).unwrap()
    }

    fn shown(shown: Shown) -> State {
        let mut state = State::default();
        state.take(shown);
        state
    }

    fn first_launch() -> Shown {
        let mut shown = example();
        shown.status.history_read = false;
        for account in &mut shown.status.accounts {
            account.in_use = false;
            account.recent = false;
        }
        shown
    }

    #[test]
    fn the_agent_to_offer_connecting_is_one_in_use_that_cant_ask() {
        assert!(shown(example()).to_connect().is_none());
        let mut using = example();
        using.agents[1].in_use = true;
        let mut core = shown(using);
        assert_eq!(core.to_connect().map(|a| a.id.as_str()), Some("codex"));
        core.preferences.dismissed_connect.insert("codex".into());
        assert!(core.to_connect().is_none());
        core.preferences.dismissed_connect.clear();
        let connect = Action::SetConnected {
            id: "codex".into(),
            connecting: true,
        };
        core.apply(connect, 0);
        let offer = core.sync().panel.connect_offer;
        assert!(offer.is_some_and(|offer| offer.changing));
        core.from_disk_image = true;
        assert!(core.to_connect().is_none());
    }

    #[test]
    fn until_history_is_read_every_account_shown_counts_as_in_use() {
        let first = first_launch();
        let ids =
            |accounts: Vec<&Account>| accounts.iter().map(|a| a.id.clone()).collect::<Vec<_>>();
        let visible: Vec<String> = first
            .status
            .accounts
            .iter()
            .filter(|a| !a.hidden)
            .map(|a| a.id.clone())
            .collect();
        let reading = shown(first.clone());
        assert_eq!(ids(reading.in_use()), visible);
        assert!(reading.recent().is_empty() && reading.unused().is_empty());
        assert_ne!(reading.verdict(&reading.words()).0, "Nothing in use");
        let mut done = first;
        done.status.history_read = true;
        let read = shown(done);
        assert!(read.in_use().is_empty());
        assert_eq!(ids(read.unused()), visible);
        assert_eq!(read.verdict(&read.words()).0, "Nothing in use");
    }

    #[test]
    fn with_no_account_the_headline_says_why_as_settings_does() {
        let mut first = first_launch();
        first.status.accounts.clear();
        let reading = shown(first.clone());
        assert_eq!(reading.verdict(&reading.words()).0, "Getting your limits");
        first.status.history_read = true;
        let read = shown(first.clone());
        assert_eq!(read.verdict(&read.words()).0, "No accounts found yet");
        for agent in &mut first.agents {
            agent.installed = false;
        }
        let none = shown(first);
        assert_eq!(
            none.verdict(&none.words()),
            (
                "No agents found".into(),
                "Turnscope reads Claude Code, Codex, OpenCode, Pi, Grok Build and Antigravity. \
                 Sign in to one, and its limits show up."
                    .into(),
            )
        );
    }

    #[test]
    fn a_connect_that_ends_shows_at_once_and_one_that_fails_says_why_until_it_changes() {
        let mut core = shown(example());
        core.panel_open = true;
        fn codex(core: &mut State) -> (bool, Option<String>) {
            let agents = core.sync().panel.settings.agents;
            let row = agents.into_iter().find(|a| a.id == "codex").unwrap();
            assert!(row.changing.is_none());
            (row.connected, row.note)
        }
        fn connect(
            core: &mut State,
            connecting: bool,
            connection: Connection,
            error: Option<&str>,
        ) {
            let action = Action::SetConnected {
                id: "codex".into(),
                connecting,
            };
            let asked = Call::Connect {
                agent: "codex".into(),
                connecting,
            };
            assert_eq!(core.apply(action, 0), [asked]);
            let (_, wake) = core.took(Event::Connected {
                agent: "codex".into(),
                connection,
                error: error.map(Into::into),
            });
            assert!(wake);
        }
        fn read(core: &mut State, connection: Connection) {
            let mut shown = example();
            shown.agents[1].connection = connection;
            core.took(Event::Shown(Box::new(shown), Vec::new()));
        }
        let failed = Some("Codex couldn't be run.".to_owned());

        // From the example's Outdated.
        connect(&mut core, true, Connection::Outdated, failed.as_deref());
        assert_eq!(codex(&mut core), (false, failed.clone()));
        read(&mut core, Connection::Outdated);
        assert_eq!(codex(&mut core), (false, failed.clone()));
        read(&mut core, Connection::Connected);
        assert_eq!(codex(&mut core), (true, None));
        // A disconnect that failed after removing it from one of its folders.
        connect(&mut core, false, Connection::Available, failed.as_deref());
        read(&mut core, Connection::Available);
        assert_eq!(codex(&mut core), (false, failed));
        read(&mut core, Connection::Connected);
        assert_eq!(codex(&mut core), (true, None));

        connect(&mut core, true, Connection::Connected, None);
        assert_eq!(codex(&mut core), (true, None));
    }

    #[test]
    fn while_the_panel_is_open_a_status_asks_what_used_in_use_and_missing_accounts() {
        let mut core = State::default();
        core.apply(Action::PanelOpened {}, 0);
        let (calls, _) = core.took(Event::Shown(Box::new(example()), Vec::new()));
        fn asked(calls: &[Call]) -> Vec<&str> {
            calls
                .iter()
                .filter_map(|call| match call {
                    Call::UsedMost { account, .. } => Some(account.as_str()),
                    _ => None,
                })
                .collect()
        }
        assert_eq!(asked(&calls), [CLAUDE, CODEX, OPENROUTER]);
        for call in core.asks(|_| true) {
            if let Call::UsedMost { account, limit } = call {
                core.used.insert(account, (limit, Vec::new()));
            }
        }
        let (calls, _) = core.took(Event::Shown(Box::new(example()), Vec::new()));
        assert_eq!(asked(&calls), [CLAUDE]);
        let mut changed = example();
        changed.status.accounts[1].deciding = Some("primary".into());
        let (calls, _) = core.took(Event::Shown(Box::new(changed), Vec::new()));
        assert_eq!(asked(&calls), [CLAUDE, CODEX]);
        core.apply(Action::PanelClosed {}, 0);
        let (calls, _) = core.took(Event::Shown(Box::new(example()), Vec::new()));
        assert!(calls.is_empty());
    }

    #[test]
    fn a_sign_in_notification_goes_on_the_first_status_and_once_its_account_is_read_again() {
        let mut core = State::default();
        core.took(Event::Shown(Box::new(example()), Vec::new()));
        let removed = |sync: AppSync| -> Vec<String> {
            sync.effects
                .into_iter()
                .filter_map(|e| match e {
                    Effect::RemoveNotifications { identifiers } => Some(identifiers),
                    _ => None,
                })
                .flatten()
                .collect()
        };
        assert_eq!(
            removed(core.sync()),
            [
                format!("account:{CLAUDE}"),
                format!("account:{CODEX}"),
                format!("account:{OPENROUTER}"),
                "account:anthropic:api".to_owned(),
            ]
        );
        core.took(Event::Shown(Box::new(example()), Vec::new()));
        assert!(removed(core.sync()).is_empty());
        let mut read = example();
        read.status.accounts[3].state = turnscope::status::State::Live {
            read_at: read.status.at,
        };
        core.took(Event::Shown(Box::new(read), Vec::new()));
        assert_eq!(removed(core.sync()), [format!("account:{OPENCODE_GO}")]);
        // Read again while its notification was posted.
        let acknowledged = Action::AcknowledgeAlert {
            id: 1,
            sign_in_account: Some(OPENCODE_GO.into()),
        };
        assert_eq!(core.apply(acknowledged, 0), [Call::Acknowledge(1)]);
        assert_eq!(removed(core.sync()), [format!("account:{OPENCODE_GO}")]);
    }

    #[test]
    fn preferences_are_saved_except_on_a_fixture() {
        let changed = |mut core: State| {
            core.apply(Action::DismissConnect { id: "codex".into() }, 0);
            core.sync().effects
        };
        assert_eq!(
            changed(shown(example())),
            [Effect::SavePreferences {
                dismissed_connect: vec!["codex".into()],
            }]
        );
        assert_eq!(changed(State::fixed(example())), []);
    }

    /// Accounts come in `status::rank`'s order, which puts used up first.
    #[test]
    fn the_menu_bar_fills_with_running_out_then_used_up_then_the_rest() {
        let mut using = example();
        let accounts = &mut using.status.accounts;
        accounts.swap(0, 1);
        let monthly = &mut accounts[0].limits[1];
        monthly.left_percent = 0;
        monthly.outlook = turnscope::status::Outlook::UsedUp { back: None };
        for account in &mut accounts[..3] {
            account.in_use = true;
        }
        let slots = |using: Shown| State::fixed(using).sync().menu_bar.slots;
        let filled = slots(using.clone());
        let figures: Vec<_> = filled
            .iter()
            .map(|s| (s.figure.as_str(), s.must_show))
            .collect();
        assert_eq!(figures, [("50m", true), ("0%", true), ("$8.70", false)]);
        assert_eq!(
            filled[1].spoken,
            "Claude Max, runs out in 50 minutes; ChatGPT Pro, used up"
        );
        using.status.history_read = false;
        let first: Vec<_> = slots(using).into_iter().map(|s| s.figure).collect();
        assert_eq!(first, ["50m"]);
    }

    #[test]
    fn opening_the_panel_asks_for_limits_and_its_words_move_each_minute() {
        let at = example().status.at;
        let mut core = shown(example());
        let asked = core.apply(Action::PanelOpened {}, at);
        assert!(asked.contains(&Call::AskLimits));
        let headline = |core: &mut State| core.sync().panel.verdict.unwrap().headline;
        assert_eq!(headline(&mut core), "Claude Max runs out in\u{a0}50m");
        core.apply(Action::Tick {}, at + 10 * turnscope::time::MINUTE);
        assert_eq!(headline(&mut core), "Claude Max runs out in\u{a0}40m");
    }

    #[test]
    fn once_a_newer_turnscope_stops_the_engine_nothing_changes_nor_connects_from_a_disk_image() {
        let mut stopped = shown(example());
        stopped.took(Event::Kept(Some(Stopped::Newer { replaced: false })));
        let mut image = State {
            from_disk_image: true,
            ..shown(example())
        };
        for core in [&mut stopped, &mut image] {
            let connect = Action::SetConnected {
                id: "codex".into(),
                connecting: true,
            };
            assert_eq!(core.apply(connect, 0), []);
            assert!(core.sync().panel.settings.agents.iter().all(|a| !a.enabled));
        }
        let notify = example().status.settings.notify;
        let hide = Action::SetHidden {
            id: CLAUDE.into(),
            hidden: true,
        };
        let quiet = Action::SetNotify {
            notify: Notify {
                reset: false,
                ..notify
            },
        };
        assert_eq!(stopped.apply(hide, 0), []);
        assert_eq!(stopped.apply(quiet, 0), []);
        assert_eq!(stopped.apply(Action::RefreshNow {}, 0), []);
        let panel = stopped.sync().panel;
        assert_eq!(panel.in_use[0].id, CLAUDE);
        assert_eq!(panel.settings.notify, Some(notify));
        assert!(panel.stopped && !panel.can_refresh);
    }

    #[test]
    fn the_minute_is_counted_while_the_panel_is_open_or_an_account_shown_runs_out() {
        let mut state = shown(example());
        assert!(state.counting());
        state.mark_hidden(CLAUDE, true);
        assert!(!state.counting());
        state.panel_open = true;
        assert!(state.counting());
    }

    /// A status told meanwhile, as after a hide, isn't the refresh's, nor is
    /// the first catch-up's `Kept`: the refresh ends on `Refreshed` alone,
    /// which the engine tells for every forced read.
    #[test]
    fn refreshing_is_said_until_the_refresh_ends_and_only_once_there_is_a_status() {
        let refreshing = |core: &mut State| core.sync().panel.footer_note.map(|n| n.text);
        let mut core = shown(example());
        assert!(core.sync().panel.can_refresh);
        assert_eq!(core.apply(Action::RefreshNow {}, 0), [Call::RefreshNow]);
        assert_eq!(refreshing(&mut core).as_deref(), Some("Refreshing limits…"));
        core.took(Event::Shown(Box::new(example()), Vec::new()));
        assert_eq!(refreshing(&mut core).as_deref(), Some("Refreshing limits…"));
        assert_eq!(core.apply(Action::RefreshNow {}, 0), []);
        core.took(Event::Refreshed);
        assert_eq!(refreshing(&mut core), None);

        assert_eq!(core.apply(Action::RefreshNow {}, 0), [Call::RefreshNow]);
        core.took(Event::Kept(None));
        assert_eq!(refreshing(&mut core).as_deref(), Some("Refreshing limits…"));
        assert_eq!(core.apply(Action::RefreshNow {}, 0), []);

        let mut unopened = State::default();
        assert!(!unopened.sync().panel.can_refresh);
        assert_eq!(unopened.apply(Action::RefreshNow {}, 0), []);
        assert_eq!(refreshing(&mut unopened), None);
    }
}
