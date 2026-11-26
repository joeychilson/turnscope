//! Domain state, engine event handling, and side-effect queuing for the app.
//! Ephemeral UI chrome state (which row is open, folds, active page/tab) stays in the shell
//! so UI interactions never round-trip through FFI or JSON.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};
use turnscope::alerts::{Notify, Settings, replaces};
use turnscope::engine::{Event, Shown, Stopped, UsedMost};
use turnscope::status::{Account, Agent, Connection, Standing, Status};

use crate::view::{self, AppSync};
use crate::words::{self, AlertNote, Words};

/// What the menu bar shows: every account in use, or only the most urgent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MenuShows {
    #[default]
    All,
    One,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Preferences {
    pub menu_shows: MenuShows,
    pub dismissed_connect: BTreeSet<String>,
}

/// A `UsedMost` session with project existence resolved off the UI thread.
#[derive(Clone, Debug, PartialEq)]
pub struct UsedSession {
    pub used: UsedMost,
    pub exists: bool,
}

pub fn resolve_used(sessions: Vec<UsedMost>) -> Vec<UsedSession> {
    sessions
        .into_iter()
        .map(|used| {
            let exists = used
                .project_path
                .as_deref()
                .is_some_and(|path| Path::new(path).exists());
            UsedSession { used, exists }
        })
        .collect()
}

/// What the shell does once, after it next syncs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Effect {
    PostAlerts { alerts: Vec<AlertNote> },
    RemoveNotifications { identifiers: Vec<String> },
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
    pub used: HashMap<String, (String, Vec<UsedSession>)>,
    /// A fixed status's time (`--fixture`, `--snapshot`), which its words are as of. With
    /// one, there's no engine, and nothing changed is kept.
    pub frozen: Option<i64>,
    /// When set by the shell (e.g. running from a read-only disk image), connecting agents
    /// is disabled and this reason replaces the Settings → Agents heading.
    pub connect_disabled_reason: Option<String>,
    pub preferences: Preferences,
    preferences_dirty: bool,
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

    /// Whether a newer Turnscope wrote to the data, so nothing can be changed.
    pub fn stopped(&self) -> bool {
        matches!(self.problem, Some(Stopped::Newer { .. }))
    }

    /// Whether `agent`'s connection can be changed: not while it's changing, when connecting
    /// is disabled by the shell, or once a newer Turnscope wrote to the data.
    pub fn can_change(&self, agent: &str) -> bool {
        !self.changing_agents.contains_key(agent)
            && self.connect_disabled_reason.is_none()
            && !self.stopped()
    }

    /// Whether Refresh Limits can be asked for: without a status, there's nothing to refresh.
    pub fn can_refresh(&self) -> bool {
        self.live() && self.status.is_some() && !self.stopped()
    }

    pub fn verdict(&self, words: &Words, headlined: Option<&Account>) -> (String, String) {
        if self.all().is_empty() {
            words::welcome(&self.installed(), &self.agent_names(), self.reading())
        } else {
            words.verdict(headlined, self.hidden_in_use(), self.all())
        }
    }

    pub fn to_connect(&self) -> Option<&Agent> {
        if self.connect_disabled_reason.is_some() {
            return None;
        }
        self.agents.iter().find(|a| {
            a.installed
                && a.in_use
                && a.connection != Connection::Connected
                && !self.preferences.dismissed_connect.contains(&a.id)
        })
    }

    pub fn used_most(&self, account: &Account) -> &[UsedSession] {
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

    /// Record a `UsedMost` result whose project paths were already checked off-thread.
    /// Ignores stale answers whose `limit` is no longer `account`'s deciding limit.
    pub fn took_used_most(
        &mut self,
        account: String,
        limit: String,
        sessions: Vec<UsedSession>,
    ) -> bool {
        let current = self
            .all()
            .iter()
            .find(|a| a.id == account)
            .is_some_and(|a| a.deciding.as_deref() == Some(limit.as_str()));
        if !current {
            return false;
        }
        let entry = (limit, sessions);
        let changed = self.used.get(&account) != Some(&entry);
        if changed {
            self.used.insert(account, entry);
        }
        let wake = changed && self.panel_open;
        if wake {
            self.animated = true;
        }
        wake
    }

    /// Returns the calls to send the engine, and whether to wake the shell.
    pub fn took(&mut self, event: Event, now_ms: i64) -> (Vec<Call>, bool) {
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
                    let words = Words::new(self.frozen.unwrap_or(now_ms));
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
                let wake = self.took_used_most(account, limit, resolve_used(sessions));
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
    pub fn apply(&mut self, action: Action) -> Vec<Call> {
        let mut calls = Vec::new();
        match action {
            Action::PanelOpened {} => {
                if !self.panel_open {
                    self.panel_open = true;
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
                if self.can_change(&id) && self.agents.iter().any(|a| a.id == id) {
                    let to = if connecting {
                        Connection::Connected
                    } else {
                        Connection::Available
                    };
                    self.agent_errors.remove(&id);
                    if self.live() {
                        self.changing_agents.insert(id.clone(), to);
                        calls.push(Call::Connect {
                            agent: id,
                            connecting,
                        });
                    } else if let Some(agent) = self.agents.iter_mut().find(|a| a.id == id) {
                        agent.connection = to;
                    }
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
            Action::SetMenuShows { shows } => {
                if self.preferences.menu_shows != shows {
                    self.preferences.menu_shows = shows;
                    if self.live() {
                        self.preferences_dirty = true;
                    }
                }
            }
            Action::DismissConnect { id } => {
                if self.preferences.dismissed_connect.insert(id) && self.live() {
                    self.preferences_dirty = true;
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
            Action::Tick {} => {}
        }
        calls
    }

    pub fn take_preferences_to_save(&mut self) -> Option<Preferences> {
        std::mem::take(&mut self.preferences_dirty).then(|| self.preferences.clone())
    }

    pub fn sync(&mut self, now_ms: i64) -> AppSync {
        let words = Words::new(self.frozen.unwrap_or(now_ms));
        let in_menu_bar = self.in_menu_bar();
        let urgent = in_menu_bar.iter().any(|a| a.standing() != Standing::Lasts);
        let counting = self.live() && (self.panel_open || urgent);
        let has_status = self.status.is_some();
        let can_refresh = self.can_refresh();
        let menu_bar = view::build_menu_bar(self, &words, &in_menu_bar);
        let panel = self.panel_open.then(|| view::build_panel(self, &words));
        AppSync {
            animated: std::mem::take(&mut self.animated),
            counting,
            has_status,
            can_refresh,
            menu_bar,
            panel,
            effects: std::mem::take(&mut self.effects),
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

    fn opened(mut state: State) -> State {
        state.panel_open = true;
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
        let at = example().status.at;
        assert!(shown(example()).to_connect().is_none());
        let mut using = example();
        using.agents[1].in_use = true;
        let mut core = opened(shown(using));
        assert_eq!(core.to_connect().map(|a| a.id.as_str()), Some("codex"));
        core.preferences.dismissed_connect.insert("codex".into());
        assert!(core.to_connect().is_none());
        core.preferences.dismissed_connect.clear();
        let connect = Action::SetConnected {
            id: "codex".into(),
            connecting: true,
        };
        core.apply(connect);
        let offer = core.sync(at).panel.unwrap().connect_offer;
        assert!(offer.is_some_and(|offer| offer.changing));
        core.connect_disabled_reason = Some("Move to Applications".into());
        assert!(core.to_connect().is_none());
    }

    #[test]
    fn until_history_is_read_every_account_shown_counts_as_in_use() {
        let first = first_launch();
        let at = first.status.at;
        let words = Words::new(at);
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
        assert_ne!(
            reading.verdict(&words, reading.in_use().first().copied()).0,
            "Nothing in use"
        );
        let mut done = first;
        done.status.history_read = true;
        let read = shown(done);
        assert!(read.in_use().is_empty());
        assert_eq!(ids(read.unused()), visible);
        assert_eq!(
            read.verdict(&words, read.in_use().first().copied()).0,
            "Nothing in use"
        );
    }

    #[test]
    fn with_no_account_the_headline_says_why_as_settings_does() {
        let mut first = first_launch();
        let words = Words::new(first.status.at);
        first.status.accounts.clear();
        let reading = shown(first.clone());
        assert_eq!(reading.verdict(&words, None).0, "Getting your limits");
        first.status.history_read = true;
        let read = shown(first.clone());
        assert_eq!(read.verdict(&words, None).0, "No accounts found yet");
        for agent in &mut first.agents {
            agent.installed = false;
        }
        let none = shown(first);
        assert_eq!(
            none.verdict(&words, None),
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
        let at = example().status.at;
        let mut core = opened(shown(example()));
        let codex = |core: &mut State| -> (bool, Option<String>) {
            let agents = core.sync(at).panel.unwrap().settings.agents;
            let row = agents.into_iter().find(|a| a.id == "codex").unwrap();
            assert!(row.changing.is_none());
            (row.connected, row.note)
        };
        let connect =
            |core: &mut State, connecting: bool, connection: Connection, error: Option<&str>| {
                let action = Action::SetConnected {
                    id: "codex".into(),
                    connecting,
                };
                let asked = Call::Connect {
                    agent: "codex".into(),
                    connecting,
                };
                assert_eq!(core.apply(action), [asked]);
                let (_, wake) = core.took(
                    Event::Connected {
                        agent: "codex".into(),
                        connection,
                        error: error.map(Into::into),
                    },
                    at,
                );
                assert!(wake);
            };
        let read = |core: &mut State, connection: Connection| {
            let mut shown = example();
            shown.agents[1].connection = connection;
            core.took(Event::Shown(Box::new(shown), Vec::new()), at);
        };
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

        // Unknown agent ids never leak into changing_agents or emit engine calls.
        assert_eq!(
            core.apply(Action::SetConnected {
                id: "nonexistent".into(),
                connecting: true,
            }),
            []
        );
        assert!(!core.changing_agents.contains_key("nonexistent"));

        // Fixture mode updates connection in memory without engine calls.
        let mut fixture = opened(State::fixed(example()));
        assert_eq!(
            fixture.apply(Action::SetConnected {
                id: "codex".into(),
                connecting: true,
            }),
            []
        );
        let row = fixture
            .sync(at)
            .panel
            .unwrap()
            .settings
            .agents
            .into_iter()
            .find(|a| a.id == "codex")
            .unwrap();
        assert!(row.connected);
    }

    #[test]
    fn while_the_panel_is_open_a_status_asks_what_used_in_use_and_missing_accounts() {
        let at = example().status.at;
        let mut core = State::default();
        core.apply(Action::PanelOpened {});
        let (calls, _) = core.took(Event::Shown(Box::new(example()), Vec::new()), at);
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
        let (calls, _) = core.took(Event::Shown(Box::new(example()), Vec::new()), at);
        assert_eq!(asked(&calls), [CLAUDE]);
        let mut changed = example();
        changed.status.accounts[1].deciding = Some("primary".into());
        let (calls, _) = core.took(Event::Shown(Box::new(changed), Vec::new()), at);
        assert_eq!(asked(&calls), [CLAUDE, CODEX]);
        core.apply(Action::PanelClosed {});
        let (calls, _) = core.took(Event::Shown(Box::new(example()), Vec::new()), at);
        assert!(calls.is_empty());
    }

    #[test]
    fn stale_used_most_for_an_earlier_deciding_limit_is_ignored() {
        let at = example().status.at;
        let mut core = opened(shown(example()));
        let fresh = UsedMost {
            session: "claude-code:new".into(),
            title: Some("Current".into()),
            project: None,
            project_path: None,
            agent: "claude-code".into(),
            share_percent: Some(50.0),
        };
        let stale = UsedMost {
            session: "claude-code:old".into(),
            title: Some("Stale".into()),
            project: None,
            project_path: None,
            agent: "claude-code".into(),
            share_percent: Some(10.0),
        };
        let (_, wake) = core.took(
            Event::UsedMost {
                account: CLAUDE.into(),
                limit: "five_hour".into(),
                sessions: vec![fresh],
            },
            at,
        );
        assert!(wake);
        core.sync(at);

        let (_, wake) = core.took(
            Event::UsedMost {
                account: CLAUDE.into(),
                limit: "seven_day".into(),
                sessions: vec![stale],
            },
            at,
        );
        assert!(!wake);
        assert!(!core.animated);
        assert_eq!(core.used.get(CLAUDE).unwrap().0, "five_hour");
    }

    #[test]
    fn a_sign_in_notification_goes_on_the_first_status_and_once_its_account_is_read_again() {
        let at = example().status.at;
        let mut core = State::default();
        core.took(Event::Shown(Box::new(example()), Vec::new()), at);
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
            removed(core.sync(at)),
            [
                format!("account:{CLAUDE}"),
                format!("account:{CODEX}"),
                format!("account:{OPENROUTER}"),
                "account:anthropic:api".to_owned(),
            ]
        );
        core.took(Event::Shown(Box::new(example()), Vec::new()), at);
        assert!(removed(core.sync(at)).is_empty());
        let mut read = example();
        read.status.accounts[3].state = turnscope::status::State::Live {
            read_at: read.status.at,
        };
        core.took(Event::Shown(Box::new(read), Vec::new()), at);
        assert_eq!(removed(core.sync(at)), [format!("account:{OPENCODE_GO}")]);
        // Read again while its notification was posted.
        let acknowledged = Action::AcknowledgeAlert {
            id: 1,
            sign_in_account: Some(OPENCODE_GO.into()),
        };
        assert_eq!(core.apply(acknowledged), [Call::Acknowledge(1)]);
        assert_eq!(removed(core.sync(at)), [format!("account:{OPENCODE_GO}")]);
    }

    #[test]
    fn preferences_are_marked_for_saving_except_on_a_fixture() {
        let changed = |mut core: State| {
            core.apply(Action::SetMenuShows {
                shows: MenuShows::One,
            });
            core.apply(Action::DismissConnect { id: "codex".into() });
            core.take_preferences_to_save()
        };
        assert_eq!(
            changed(shown(example())),
            Some(Preferences {
                menu_shows: MenuShows::One,
                dismissed_connect: BTreeSet::from(["codex".into()]),
            })
        );
        assert_eq!(changed(State::fixed(example())), None);
    }

    #[test]
    fn set_to_most_urgent_the_menu_bar_shows_one_account() {
        let mut two = example();
        let at = two.status.at;
        two.status.accounts[1].in_use = true;
        let mut core = shown(two);
        let slots = |core: &mut State| core.sync(at).menu_bar.slots.len();
        assert_eq!(slots(&mut core), 2);
        core.apply(Action::SetMenuShows {
            shows: MenuShows::One,
        });
        assert_eq!(slots(&mut core), 1);
    }

    #[test]
    fn closed_panel_omits_panel_model_and_open_panel_moves_with_clock() {
        let at = example().status.at;
        let mut core = shown(example());
        let closed = core.sync(at);
        assert!(closed.panel.is_none());
        assert!(closed.has_status);
        assert!(closed.can_refresh);
        assert_eq!(closed.menu_bar.slots[0].figure, "50m");

        let asked = core.apply(Action::PanelOpened {});
        assert!(asked.contains(&Call::AskLimits));
        let headline = |core: &mut State, now: i64| {
            let sync = core.sync(now);
            (
                sync.panel.unwrap().verdict.unwrap().headline,
                sync.menu_bar.slots[0].figure.clone(),
            )
        };
        assert_eq!(
            headline(&mut core, at),
            ("Claude Max runs out in\u{a0}50m".into(), "50m".into())
        );
        // Menu bar and panel share one clock snapshot without drifting across a minute boundary.
        let later = at + 10 * turnscope::time::MINUTE;
        core.apply(Action::Tick {});
        assert_eq!(
            headline(&mut core, later),
            ("Claude Max runs out in\u{a0}40m".into(), "40m".into())
        );
    }

    #[test]
    fn once_a_newer_turnscope_stops_the_engine_nothing_changes_nor_connects_when_disabled() {
        let at = example().status.at;
        let mut stopped = opened(shown(example()));
        stopped.took(Event::Kept(Some(Stopped::Newer { replaced: false })), at);
        let mut disabled = opened(State {
            connect_disabled_reason: Some(
                "Move Turnscope to Applications to connect agents".into(),
            ),
            ..shown(example())
        });
        for core in [&mut stopped, &mut disabled] {
            let connect = Action::SetConnected {
                id: "codex".into(),
                connecting: true,
            };
            assert_eq!(core.apply(connect), []);
            assert!(
                core.sync(at)
                    .panel
                    .unwrap()
                    .settings
                    .agents
                    .iter()
                    .all(|a| !a.enabled)
            );
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
        assert_eq!(stopped.apply(hide), []);
        assert_eq!(stopped.apply(quiet), []);
        assert_eq!(stopped.apply(Action::RefreshNow {}), []);
        let sync = stopped.sync(at);
        let panel = sync.panel.unwrap();
        assert_eq!(panel.in_use[0].id, CLAUDE);
        assert_eq!(panel.settings.notify_options.len(), 4);
        assert!(panel.settings.notify_options.iter().all(|o| o.enabled));
        assert!(panel.stopped && !panel.can_refresh && !sync.can_refresh);
    }

    #[test]
    fn the_minute_is_counted_while_the_panel_is_open_or_an_account_shown_runs_out() {
        let at = example().status.at;
        let mut state = shown(example());
        assert!(state.sync(at).counting);
        state.mark_hidden(CLAUDE, true);
        assert!(!state.sync(at).counting);
        state.panel_open = true;
        assert!(state.sync(at).counting);
    }

    /// A status told meanwhile, as after a hide, isn't the refresh's, nor is
    /// the first catch-up's `Kept`: the refresh ends on `Refreshed` alone,
    /// which the engine tells for every forced read.
    #[test]
    fn refreshing_is_said_until_the_refresh_ends_and_only_once_there_is_a_status() {
        let at = example().status.at;
        let refreshing =
            |core: &mut State| core.sync(at).panel.unwrap().footer_note.map(|n| n.text);
        let mut core = opened(shown(example()));
        assert!(core.sync(at).can_refresh);
        assert_eq!(core.apply(Action::RefreshNow {}), [Call::RefreshNow]);
        assert_eq!(refreshing(&mut core).as_deref(), Some("Refreshing limits…"));
        core.took(Event::Shown(Box::new(example()), Vec::new()), at);
        assert_eq!(refreshing(&mut core).as_deref(), Some("Refreshing limits…"));
        assert_eq!(core.apply(Action::RefreshNow {}), []);
        core.took(Event::Refreshed, at);
        assert_eq!(refreshing(&mut core), None);

        assert_eq!(core.apply(Action::RefreshNow {}), [Call::RefreshNow]);
        core.took(Event::Kept(None), at);
        assert_eq!(refreshing(&mut core).as_deref(), Some("Refreshing limits…"));
        assert_eq!(core.apply(Action::RefreshNow {}), []);

        let mut unopened = opened(State::default());
        assert!(!unopened.sync(at).can_refresh);
        assert_eq!(unopened.apply(Action::RefreshNow {}), []);
        assert_eq!(refreshing(&mut unopened), None);
    }
}
