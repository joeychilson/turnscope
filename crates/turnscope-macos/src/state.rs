//! What the app shows and where the panel is: the status the engine last gave, what's in
//! progress (agents being connected, what went wrong), and the panel's page and what's open
//! on it. Views read it; clicks change it and ask the engine through `engine`.

use std::collections::HashMap;

use crate::preferences::{MenuShows, Preferences};
use crate::standing::*;
use crate::words::{self, Words};
use turnscope::engine::{Shown, Stopped, UsedMost};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Main,
    Settings,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    General,
    Agents,
    Accounts,
}

impl Tab {
    pub const ALL: [(Tab, &'static str); 3] = [
        (Tab::General, "General"),
        (Tab::Agents, "Agents"),
        (Tab::Accounts, "Accounts"),
    ];
}

pub struct State {
    pub status: Option<Status>,
    pub agents: Vec<Agent>,
    pub problem: Option<Stopped>,
    pub changing_agents: HashMap<String, Connection>,
    pub agent_errors: HashMap<String, String>,
    pub used: HashMap<String, (String, Vec<UsedMost>)>,
    /// Fixed status timestamp (`--fixture` / `--snapshot`) so wording stays stable.
    pub frozen: Option<i64>,
    pub from_disk_image: bool,
    pub live: bool,
    pub preferences: Preferences,
    pub notifications_denied: bool,
    pub update_available: bool,
    pub checks_automatically: bool,
    pub login: crate::login::Login,
    pub opening: Option<crate::launch::Opening>,
    pub clicked: Option<String>,
    /// Held until the first status arrives so notifications can name accounts and agents.
    pub alerts_waiting: Vec<turnscope::alerts::Alert>,

    pub page: Page,
    pub tab: Tab,
    pub open: Option<String>,
    pub unused_open: bool,
    pub hidden_shown: bool,
    pub now: i64,
    pub panel_open: bool,
    pub tallest: Option<f32>,

    pub grown: bool,
    pub grow_due: Option<f64>,
    /// Row just closed by click; delays re-enabling its press shade (`poll`).
    pub closed: Option<(String, f64)>,
}

impl Default for State {
    fn default() -> Self {
        State::new(Preferences::default())
    }
}

impl State {
    pub fn new(preferences: Preferences) -> State {
        State {
            status: None,
            agents: Vec::new(),
            problem: None,
            changing_agents: HashMap::new(),
            agent_errors: HashMap::new(),
            used: HashMap::new(),
            frozen: None,
            from_disk_image: false,
            live: false,
            preferences,
            notifications_denied: false,
            update_available: false,
            checks_automatically: false,
            login: crate::login::Login::default(),
            opening: None,
            clicked: None,
            alerts_waiting: Vec::new(),
            page: Page::default(),
            tab: Tab::default(),
            open: None,
            unused_open: false,
            hidden_shown: false,
            now: turnscope::time::minute(turnscope::now()),
            panel_open: false,
            tallest: None,
            grown: true,
            grow_due: None,
            closed: None,
        }
    }

    pub fn fixed(shown: Shown, preferences: Preferences) -> State {
        let mut state = State::new(preferences);
        state.frozen = Some(shown.status.at);
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

    /// Before initial history ingest completes, treat every account as in use so
    /// the panel doesn't show "Nothing in use" while reading history.
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

    pub fn stopped(&self) -> bool {
        self.problem.as_ref().is_some_and(|p| !p.trying_again)
    }

    pub fn verdict(&self, words: &Words) -> (String, String, Standing) {
        if self.all().is_empty() {
            words::welcome(&self.installed(), &self.agent_names(), self.reading())
        } else {
            words.verdict(&self.in_use(), self.hidden_in_use())
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
        let iter = self.all().iter().filter(|a| !a.hidden && self.counts(a));
        match self.preferences.menu_shows {
            MenuShows::All => iter.collect(),
            MenuShows::One => iter.take(1).collect(),
        }
    }

    pub fn urgent(&self) -> bool {
        let mut iter = self.all().iter().filter(|a| !a.hidden && self.counts(a));
        match self.preferences.menu_shows {
            MenuShows::All => iter.any(|a| a.standing() != Standing::Lasts),
            MenuShows::One => iter.next().is_some_and(|a| a.standing() != Standing::Lasts),
        }
    }

    pub fn close_open(&mut self) {
        self.open = None;
        self.grow_due = None;
        self.grown = true;
    }

    pub fn reset(&mut self) {
        self.page = Page::Main;
        self.tab = Tab::General;
        self.close_open();
        self.closed = None;
        self.unused_open = false;
        self.hidden_shown = false;
    }

    pub fn mark_hidden(&mut self, account: &str, hidden: bool) {
        if let Some(status) = &mut self.status
            && let Some(found) = status.accounts.iter_mut().find(|a| a.id == account)
        {
            found.hidden = hidden;
        }
        if hidden && self.open.as_deref() == Some(account) {
            self.close_open();
        }
    }

    pub fn open_in_place(&mut self, account: &str) {
        let Some(found) = self.all().iter().find(|a| a.id == account && !a.hidden) else {
            return;
        };
        self.unused_open = !self.counts(found) && !found.recent;
        self.page = Page::Main;
        self.open = Some(account.to_owned());
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The example status in tests/fixtures/status.json, as `--fixture` shows it.
    pub(crate) fn example() -> Shown {
        serde_json::from_str(include_str!("../../turnscope/tests/fixtures/status.json")).unwrap()
    }

    fn shown(shown: Shown) -> State {
        let mut state = State::default();
        state.take(shown);
        state
    }

    /// The example as on a first launch, once the limits are read and before any history is:
    /// no account is known to be in use, or used this week.
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
        let state = State::fixed(example, Preferences::default());
        // The words are as of the status's time, whenever it's drawn, and name agents as the
        // status does.
        assert_eq!(state.words().now, at);
        assert_eq!(state.words_now().now, at);
        assert_eq!(
            words::via(&state.all()[0]).as_deref(),
            Some("via Claude Code")
        );
    }

    #[test]
    fn the_agent_to_offer_connecting_is_one_in_use_that_cant_ask() {
        // Claude Code, the agent in use, is connected, so there's none to offer.
        assert!(shown(example()).to_connect().is_none());
        let mut using = example();
        using.agents[1].in_use = true;
        let mut state = shown(using);
        assert_eq!(state.to_connect().map(|a| a.id.as_str()), Some("codex"));
        state.preferences.dismissed_connect.insert("codex".into());
        assert!(state.to_connect().is_none());
        // Run from its disk image, the app offers none.
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
        // None is folded away, and the headline is about the first, the most urgent, as the
        // engine lists them until then.
        assert_eq!(ids(reading.in_use()), visible);
        assert!(reading.recent().is_empty() && reading.unused().is_empty());
        let words = reading.words();
        let verdict = reading.verdict(&words);
        assert_ne!(verdict.0, "Nothing in use");
        assert_eq!(verdict, words.verdict(&reading.in_use()[..1], false));
        // Once it's read, the engine's word holds: nothing in use, the rest folded.
        let mut done = first;
        done.status.history_read = true;
        let read = shown(done);
        assert!(read.in_use().is_empty());
        assert_eq!(ids(read.unused()), visible);
        assert_eq!(read.verdict(&read.words()).0, "Nothing in use");
    }

    #[test]
    fn with_no_account_the_headline_says_why_as_settings_does() {
        // As on a first launch, before the limits are read.
        let mut first = first_launch();
        first.status.accounts.clear();
        let reading = shown(first.clone());
        assert_eq!(reading.verdict(&reading.words()).0, "Getting your limits");
        first.status.history_read = true;
        let read = shown(first);
        assert_eq!(read.verdict(&read.words()).0, "No accounts found yet");
    }
}
