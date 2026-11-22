//! What the app shows and where the panel is: the status the engine last gave, what's in
//! progress (agents being connected, what went wrong), and the panel's page and what's open
//! on it. Views read it; clicks change it and ask the engine through `engine`.

use std::collections::HashMap;

use turnscope::engine::{Shown, Stopped, UsedMost};
use turnscope::status::{Account, Agent, Connection, Status};

use crate::preferences::{MenuShows, Preferences};
use crate::standing::{AccountStanding, Standing};
use crate::words::{self, Words};

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

#[derive(Default)]
pub struct State {
    pub status: Option<Status>,
    pub agents: Vec<Agent>,
    pub problem: Option<Stopped>,
    pub changing_agents: HashMap<String, Connection>,
    pub agent_errors: HashMap<String, String>,
    pub used: HashMap<String, (String, Vec<UsedMost>)>,
    /// A fixed status's time (`--fixture`, `--snapshot`), which its words are as of.
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
    /// The minute the open panel's words are as of: set as it opens, and each minute after.
    pub now: i64,
    pub panel_open: bool,
    pub tallest: Option<f32>,
    /// When the open row's bars grow, once its lines have faded in (`poll`).
    pub grow_due: Option<f64>,
    /// Row just closed by click; delays re-enabling its press shade (`poll`).
    pub closed: Option<(String, f64)>,
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

    pub fn close_open(&mut self) {
        self.open = None;
        self.grow_due = None;
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
        let state = State::fixed(example);
        // The words are as of the status's time, whenever it's drawn.
        assert_eq!(state.words().now, at);
        assert_eq!(state.words_now().now, at);
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
}
