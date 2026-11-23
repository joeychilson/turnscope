//! Domain state, engine event handling, and side-effect queuing for the app.
//! Ephemeral UI chrome state (which row is open, folds, active page/tab) stays local to each shell
//! so UI interactions never round-trip through FFI or JSON.

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use turnscope::alerts::{Alert, Kind as AlertKind, Notify, replaces};
use turnscope::engine::{Engine, Event, Shown, Stopped, UsedMost};
use turnscope::status::{Account, Agent, Connection, Status};

use crate::standing::{AccountStanding, Standing};
use crate::view::{AppSync, build_sync};
use crate::words::{self, Words};

/// What the menu bar shows: every account in use, or only the most urgent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MenuShows {
    #[default]
    All,
    One,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preferences {
    pub menu_shows: MenuShows,
    pub dismissed_connect: BTreeSet<String>,
    /// Whether changes persist to the OS defaults: false on `--fixture`.
    pub kept: bool,
}

/// A notification ready to post through the host OS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertNote {
    pub id: i64,
    pub account: String,
    pub replaces: String,
    pub sign_in: bool,
    pub title: String,
    pub subtitle: String,
    pub body: String,
}

/// One-shot side effects the host shell executes after syncing state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Effect {
    PostAlerts {
        alerts: Vec<AlertNote>,
    },
    RemoveNotifications {
        identifiers: Vec<String>,
    },
    SavePreferences {
        menu_shows: MenuShows,
        dismissed_connect: Vec<String>,
    },
}

/// Domain and lifecycle actions sent into `Core::apply`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Action {
    PanelOpened,
    PanelClosed,
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
    SetMenuShows {
        shows: MenuShows,
    },
    DismissConnect {
        id: String,
    },
    AcknowledgeAlert {
        id: i64,
        sign_in_account: Option<String>,
    },
    RefreshNow,
    Tick,
}

/// Engine requests produced while handling an event or action, sent outside the state lock.
#[derive(Default)]
pub struct EngineCalls {
    pub used_most: Vec<(String, String)>,
    pub ask_limits: bool,
    pub refresh_now: bool,
    pub hide: Option<(String, bool)>,
    pub connect: Option<(String, bool)>,
    pub save_settings: Option<turnscope::alerts::Settings>,
    pub acknowledge: Vec<i64>,
}

impl EngineCalls {
    pub fn send(self, engine: Option<&Engine>) {
        let Some(engine) = engine else { return };
        for (account, limit) in self.used_most {
            engine.used_most(account, limit);
        }
        if self.refresh_now {
            engine.refresh_now();
        } else if self.ask_limits {
            engine.ask_limits();
        }
        if let Some((account, hidden)) = self.hide {
            engine.hide(account, hidden);
        }
        if let Some((agent, connecting)) = self.connect {
            engine.connect(agent, connecting);
        }
        if let Some(settings) = self.save_settings {
            engine.save_settings(settings);
        }
        if !self.acknowledge.is_empty() {
            engine.acknowledge(self.acknowledge);
        }
    }
}

#[derive(Default)]
pub struct State {
    pub status: Option<Status>,
    pub agents: Vec<Agent>,
    pub problem: Option<Stopped>,
    pub refreshing: bool,
    pub changing_agents: HashMap<String, Connection>,
    pub agent_errors: HashMap<String, String>,
    pub used: HashMap<String, (String, Vec<UsedMost>)>,
    /// A fixed status's time (`--fixture`, `--snapshot`), which its words are as of.
    pub frozen: Option<i64>,
    pub from_disk_image: bool,
    pub preferences: Preferences,
    /// Held until the first status arrives so notifications can name accounts and agents.
    pub alerts_waiting: Vec<Alert>,
    /// The minute the open panel's words are as of: set as it opens, and each minute after.
    pub now: i64,
    pub panel_open: bool,
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
        self.status = Some(shown.status);
        self.agents = shown.agents;
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

    pub fn verdict(&self, words: &Words) -> (String, String, Standing) {
        if self.all().is_empty() {
            words::welcome(&self.installed(), &self.agent_names(), self.reading())
        } else {
            words.verdict(&self.in_use(), self.hidden_in_use(), self.all())
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

    pub fn in_menu_bar(&self) -> Vec<&Account> {
        let mut shown = self.in_use();
        if self.preferences.menu_shows == MenuShows::One {
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
        self.frozen.is_none() && (self.panel_open || self.urgent())
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
    pub fn asks(&self, again: impl Fn(&Account) -> bool) -> Vec<(String, String)> {
        self.all()
            .iter()
            .filter(|a| !a.hidden)
            .filter_map(|a| {
                let limit = a.deciding.as_ref()?;
                let have = self.used.get(&a.id).is_some_and(|(of, _)| of == limit);
                (again(a) || !have).then(|| (a.id.clone(), limit.clone()))
            })
            .collect()
    }

    fn format_alerts(&self, alerts: Vec<Alert>) -> Vec<AlertNote> {
        if alerts.is_empty() {
            return Vec::new();
        }
        let words = self.words_now();
        alerts
            .into_iter()
            .map(|alert| {
                let note = words.note(&alert, self.all());
                AlertNote {
                    id: alert.id,
                    account: alert.account,
                    replaces: alert.replaces,
                    sign_in: alert.kind == AlertKind::SignIn,
                    title: note.title,
                    subtitle: note.subtitle,
                    body: note.body,
                }
            })
            .collect()
    }
}

/// Platform-agnostic coordinator owning `State` and pending OS side effects.
pub struct Core {
    pub state: State,
    pub running_engine: bool,
    effects: Vec<Effect>,
    animated: bool,
}

impl Core {
    pub fn new(state: State, running_engine: bool) -> Core {
        Core {
            state,
            running_engine,
            effects: Vec::new(),
            animated: false,
        }
    }

    /// Handle one `turnscope::engine::Event`. Returns engine calls to fire outside the lock
    /// and whether the UI shell should be woken.
    pub fn took(&mut self, event: Event) -> (EngineCalls, bool) {
        let mut calls = EngineCalls::default();
        match event {
            Event::Shown(shown, alerts) => {
                self.state.refreshing = false;
                if self.state.panel_open {
                    self.animated = true;
                }
                let first = self.state.status.is_none();
                let refused: Vec<String> = self
                    .state
                    .all()
                    .iter()
                    .filter(|a| a.refused())
                    .map(|a| a.id.clone())
                    .collect();
                self.state.take(*shown);
                let identifiers: Vec<String> = self
                    .state
                    .all()
                    .iter()
                    .filter(|a| !a.refused() && (first || refused.contains(&a.id)))
                    .map(|a| replaces(&a.id, None))
                    .collect();
                if !identifiers.is_empty() {
                    self.effects
                        .push(Effect::RemoveNotifications { identifiers });
                }
                if self.state.panel_open {
                    calls.used_most = self.state.asks(|a| self.state.counts(a));
                }
                let mut told = std::mem::take(&mut self.state.alerts_waiting);
                told.extend(alerts);
                let notes = self.state.format_alerts(told);
                if !notes.is_empty() {
                    self.effects.push(Effect::PostAlerts { alerts: notes });
                }
                (calls, true)
            }
            Event::Pending(alerts) if self.state.status.is_none() => {
                self.state.alerts_waiting.extend(alerts);
                (calls, false)
            }
            Event::Pending(alerts) => {
                let notes = self.state.format_alerts(alerts);
                let wake = !notes.is_empty();
                if wake {
                    self.effects.push(Effect::PostAlerts { alerts: notes });
                }
                (calls, wake)
            }
            Event::Kept(stopped) => {
                self.state.refreshing = false;
                self.state.problem = stopped;
                (calls, true)
            }
            Event::UsedMost {
                account,
                limit,
                sessions,
            } => {
                let entry = (limit, sessions);
                let changed = self.state.used.get(&account) != Some(&entry);
                if changed {
                    self.state.used.insert(account, entry);
                }
                let wake = changed && self.state.panel_open;
                if wake {
                    self.animated = true;
                }
                (calls, wake)
            }
            Event::Connected {
                agent,
                connecting,
                error,
            } => {
                self.state.changing_agents.remove(&agent);
                match error {
                    Some(error) => {
                        self.state.agent_errors.insert(agent, error);
                    }
                    None => {
                        if let Some(shown) = self.state.agents.iter_mut().find(|a| a.id == agent) {
                            shown.connection = if connecting {
                                Connection::Connected
                            } else {
                                Connection::Available
                            };
                        }
                    }
                }
                (calls, self.state.panel_open)
            }
        }
    }

    /// Apply a domain or lifecycle action. Returns engine calls to fire outside the lock.
    pub fn apply(&mut self, action: Action, now_ms: i64) -> EngineCalls {
        let mut calls = EngineCalls::default();
        match action {
            Action::PanelOpened => {
                if !self.state.panel_open {
                    self.state.panel_open = true;
                    self.state.now = turnscope::time::minute(now_ms);
                    if self.running_engine {
                        calls.used_most = self.state.asks(|_| true);
                        calls.ask_limits = true;
                    }
                }
            }
            Action::PanelClosed => {
                self.state.panel_open = false;
            }
            Action::SetHidden { id, hidden } => {
                if !self.state.stopped() {
                    self.state.mark_hidden(&id, hidden);
                    if self.running_engine {
                        calls.hide = Some((id, hidden));
                    }
                }
            }
            Action::SetConnected { id, connecting } => {
                if self.running_engine
                    && !self.state.stopped()
                    && !self.state.from_disk_image
                    && !self.state.changing_agents.contains_key(&id)
                {
                    let to = if connecting {
                        Connection::Connected
                    } else {
                        Connection::Available
                    };
                    self.state.changing_agents.insert(id.clone(), to);
                    self.state.agent_errors.remove(&id);
                    calls.connect = Some((id, connecting));
                }
            }
            Action::SetNotify { notify } => {
                if !self.state.stopped()
                    && let Some(status) = &mut self.state.status
                {
                    status.settings.notify = notify;
                    if self.running_engine {
                        calls.save_settings = Some(status.settings);
                    }
                }
            }
            Action::SetMenuShows { shows } => {
                if self.state.preferences.menu_shows != shows {
                    self.state.preferences.menu_shows = shows;
                    self.save_preferences();
                }
            }
            Action::DismissConnect { id } => {
                if self.state.preferences.dismissed_connect.insert(id) {
                    self.save_preferences();
                }
            }
            Action::AcknowledgeAlert {
                id,
                sign_in_account,
            } => {
                if self.running_engine {
                    calls.acknowledge.push(id);
                }
                // The account may have been read again while the notification was posted.
                if let Some(account) = sign_in_account
                    && self
                        .state
                        .all()
                        .iter()
                        .any(|a| a.id == account && !a.refused())
                {
                    self.effects.push(Effect::RemoveNotifications {
                        identifiers: vec![replaces(&account, None)],
                    });
                }
            }
            Action::RefreshNow => {
                if self.running_engine && !self.state.stopped() && !self.state.refreshing {
                    self.state.refreshing = true;
                    calls.refresh_now = true;
                }
            }
            Action::Tick => {
                if self.state.panel_open {
                    self.state.now = turnscope::time::minute(now_ms);
                }
            }
        }
        calls
    }

    fn save_preferences(&mut self) {
        if !self.state.preferences.kept {
            return;
        }
        self.effects.push(Effect::SavePreferences {
            menu_shows: self.state.preferences.menu_shows,
            dismissed_connect: self
                .state
                .preferences
                .dismissed_connect
                .iter()
                .cloned()
                .collect(),
        });
    }

    /// Drain pending effects and produce a complete `AppSync` snapshot for the host shell.
    pub fn sync(&mut self) -> AppSync {
        let animated = std::mem::take(&mut self.animated);
        let effects = std::mem::take(&mut self.effects);
        build_sync(&self.state, animated, effects)
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
    fn a_fixture_is_shown_as_of_its_own_time() {
        let example = example();
        let at = example.status.at;
        let state = State::fixed(example);
        assert_eq!(state.words().now, at);
        assert_eq!(state.words_now().now, at);
        assert!(!state.counting());
    }

    #[test]
    fn the_agent_to_offer_connecting_is_one_in_use_that_cant_ask() {
        assert!(shown(example()).to_connect().is_none());
        let mut using = example();
        using.agents[1].in_use = true;
        let mut state = shown(using);
        assert_eq!(state.to_connect().map(|a| a.id.as_str()), Some("codex"));
        state.preferences.dismissed_connect.insert("codex".into());
        assert!(state.to_connect().is_none());
        state.preferences.dismissed_connect.clear();
        state.from_disk_image = true;
        assert!(state.to_connect().is_none());
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
        let words = reading.words();
        let verdict = reading.verdict(&words);
        assert_ne!(verdict.0, "Nothing in use");
        assert_eq!(
            verdict,
            words.verdict(&reading.in_use()[..1], false, reading.all())
        );
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
                Standing::Lasts
            )
        );
    }

    #[test]
    fn alerts_told_before_the_first_status_wait_for_it() {
        let mut core = Core::new(State::default(), true);
        let alert = Alert {
            id: 1,
            kind: AlertKind::SignIn,
            account: OPENCODE_GO.into(),
            account_title: "OpenCode Go".into(),
            limit: None,
            limit_name: None,
            replaces: format!("account:{OPENCODE_GO}"),
            resets: None,
            runs_out: None,
        };
        let (_, wake) = core.took(Event::Pending(vec![alert.clone()]));
        assert!(!wake);
        assert_eq!(core.state.alerts_waiting.len(), 1);
        let (_, wake) = core.took(Event::Shown(Box::new(example()), Vec::new()));
        assert!(wake);
        assert_eq!(core.state.alerts_waiting.len(), 0);
        let sync = core.sync();
        assert!(sync.effects.iter().any(
            |e| matches!(e, Effect::PostAlerts { alerts } if alerts.len() == 1 && alerts[0].id == 1)
        ));
        let (_, wake) = core.took(Event::Pending(vec![alert]));
        assert!(wake);
        assert_eq!(core.state.alerts_waiting.len(), 0);
    }

    #[test]
    fn a_connect_that_ends_shows_at_once_and_one_that_fails_says_why() {
        let mut state = shown(example());
        state.panel_open = true;
        let mut core = Core::new(state, true);
        let codex = |core: &Core| {
            core.state
                .agents
                .iter()
                .find(|a| a.id == "codex")
                .unwrap()
                .connection
        };
        let calls = core.apply(
            Action::SetConnected {
                id: "codex".into(),
                connecting: true,
            },
            0,
        );
        assert_eq!(calls.connect, Some(("codex".into(), true)));
        let (_, wake) = core.took(Event::Connected {
            agent: "codex".into(),
            connecting: true,
            error: None,
        });
        assert!(wake);
        assert!(core.state.changing_agents.is_empty());
        assert_eq!(codex(&core), Connection::Connected);

        core.apply(
            Action::SetConnected {
                id: "codex".into(),
                connecting: false,
            },
            0,
        );
        let (_, wake) = core.took(Event::Connected {
            agent: "codex".into(),
            connecting: false,
            error: Some("Codex couldn't be run.".into()),
        });
        assert!(wake);
        assert!(core.state.changing_agents.is_empty());
        assert_eq!(
            core.state.agent_errors.get("codex").map(String::as_str),
            Some("Codex couldn't be run.")
        );
        assert_eq!(codex(&core), Connection::Connected);
    }

    #[test]
    fn while_the_panel_is_open_a_status_asks_what_used_in_use_and_missing_accounts() {
        let mut core = Core::new(State::default(), true);
        core.apply(Action::PanelOpened, 0);
        let (calls, _) = core.took(Event::Shown(Box::new(example()), Vec::new()));
        fn asked(calls: &EngineCalls) -> Vec<&str> {
            calls.used_most.iter().map(|(id, _)| id.as_str()).collect()
        }
        assert_eq!(asked(&calls), [CLAUDE, CODEX, OPENROUTER]);
        for (id, limit) in core.state.asks(|_| true) {
            core.state.used.insert(id, (limit, Vec::new()));
        }
        let (calls, _) = core.took(Event::Shown(Box::new(example()), Vec::new()));
        assert_eq!(asked(&calls), [CLAUDE]);
        let mut changed = example();
        changed.status.accounts[1].deciding = Some("primary".into());
        let (calls, _) = core.took(Event::Shown(Box::new(changed), Vec::new()));
        assert_eq!(asked(&calls), [CLAUDE, CODEX]);
        core.apply(Action::PanelClosed, 0);
        let (calls, _) = core.took(Event::Shown(Box::new(example()), Vec::new()));
        assert!(calls.used_most.is_empty());
    }

    #[test]
    fn a_sign_in_notification_goes_on_the_first_status_and_once_its_account_is_read_again() {
        let mut core = Core::new(State::default(), true);
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
    }

    #[test]
    fn fixture_changes_do_not_emit_save_preferences() {
        let mut core = Core::new(State::fixed(example()), false);
        core.apply(
            Action::SetMenuShows {
                shows: MenuShows::One,
            },
            0,
        );
        core.apply(Action::DismissConnect { id: "codex".into() }, 0);
        let sync = core.sync();
        assert!(
            !sync
                .effects
                .iter()
                .any(|e| matches!(e, Effect::SavePreferences { .. }))
        );
    }

    #[test]
    fn refresh_now_sets_refreshing_until_shown_arrives() {
        let mut core = Core::new(shown(example()), true);
        let calls = core.apply(Action::RefreshNow, 0);
        assert!(calls.refresh_now);
        assert!(core.state.refreshing);
        // Second call while already refreshing is coalesced.
        assert!(!core.apply(Action::RefreshNow, 0).refresh_now);
        core.took(Event::Shown(Box::new(example()), Vec::new()));
        assert!(!core.state.refreshing);
    }
}
