//! What the app shows and where the panel is: the status the engine last gave, what's in
//! progress (agents being connected, what went wrong), and the panel's page and what's open
//! on it. Views read it; clicks change it and ask the engine through `engine`.

use std::collections::HashMap;

use crate::preferences::{MenuShows, Preferences};
use crate::standing::*;
use crate::words::Words;
use turnscope::engine::{Shown, UsedMost};

/// The panel's pages, left to right. Going deeper, the new page comes in from the right;
/// going back, from the left.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Page {
    Main,
    Settings,
}

/// The tabs of Settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    General,
    Agents,
    Accounts,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::General, Tab::Agents, Tab::Accounts];

    pub fn title(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Agents => "Agents",
            Tab::Accounts => "Accounts",
        }
    }
}

pub struct State {
    /// The last status, kept until a newer one arrives, even while the data can't be kept
    /// current, so the panel never goes empty. None before the first.
    pub status: Option<Status>,
    /// Every agent Turnscope reads, installed or not, with whether it runs Turnscope.
    pub agents: Vec<Agent>,
    /// Why the data isn't kept current, or None while it is.
    pub failure: Option<String>,
    /// The agents being connected or disconnected right now, by id, with the connection each
    /// will have. A status given meanwhile can say either.
    pub changing_agents: HashMap<String, Connection>,
    /// Why connecting or disconnecting an agent failed, by agent id, until it's tried again.
    pub agent_errors: HashMap<String, String>,
    /// What used each account's deciding limit, by account id, with the key of the limit
    /// it's for.
    pub used: HashMap<String, (String, Vec<UsedMost>)>,
    /// For a fixed status, as `--fixture` shows, the time its words are as of, whatever the
    /// clock says: its own time, so it reads the same whenever it's drawn.
    pub frozen: Option<i64>,
    /// Whether the app runs from its disk image, where no agent is offered to connect.
    pub from_disk_image: bool,
    /// Whether the panel is drawn on the system's glass, as the menu bar app does, or on the
    /// window's background, as a snapshot is.
    pub live: bool,
    pub preferences: Preferences,
    /// Whether the engine tries again after `failure`. It doesn't once a newer Turnscope has
    /// written to the data.
    pub trying_again: bool,
    /// Whether macOS lets the app post notifications: None until asked.
    pub notifications_allowed: Option<bool>,
    /// Whether an update a scheduled check found is waiting to be shown.
    pub update_available: bool,
    /// Whether Sparkle checks for updates on its own.
    pub checks_automatically: bool,
    /// Whether the app opens at login.
    pub login: crate::login::Login,
    /// What to open once the first status arrives, as `--open` says. None once it has.
    pub opening: Option<crate::launch::Opening>,
    /// The account of a notification clicked before the first status, as when the click
    /// launched the app, to open in place once the status arrives. None once it has, or once
    /// the panel closes.
    pub clicked: Option<String>,
    /// Alerts the engine told before its first status, kept for it: a note names the account
    /// and agent from the status, so without it they were missing.
    pub alerts_waiting: Vec<turnscope::alerts::Alert>,

    // Where the panel is.
    pub page: Page,
    /// Whether the last move between pages went deeper.
    pub forward: bool,
    pub tab: Tab,
    /// The id of the account open in place. Only one is open at a time, wherever it's listed.
    pub open: Option<String>,
    pub unused_open: bool,
    pub hidden_shown: bool,
    /// The time the panel's countdowns run from, in milliseconds: set when the panel opens
    /// and each minute while it's open.
    pub now: i64,
    /// Whether the panel is open or opening.
    pub panel_open: bool,
    /// The most height a Settings tab takes before it scrolls, from the screen the panel opens
    /// on (`settings::tallest`). None until it opens, as in a snapshot, which draws all of it.
    pub tallest: Option<f32>,

    /// Whether the open row's bars have grown to their level, and when they start to: as what
    /// opened with them fades in.
    pub grown: bool,
    pub grow_due: Option<f64>,
    /// The row a click just closed, and when its header takes a press's shade again (`poll`).
    pub closed: Option<(String, f64)>,
}

impl State {
    pub fn new(preferences: Preferences) -> State {
        State {
            status: None,
            agents: Vec::new(),
            failure: None,
            changing_agents: HashMap::new(),
            agent_errors: HashMap::new(),
            used: HashMap::new(),
            frozen: None,
            from_disk_image: false,
            live: false,
            preferences,
            trying_again: true,
            notifications_allowed: None,
            update_available: false,
            checks_automatically: false,
            login: crate::login::Login::Off,
            opening: None,
            clicked: None,
            alerts_waiting: Vec::new(),
            page: Page::Main,
            forward: true,
            tab: Tab::General,
            open: None,
            unused_open: false,
            hidden_shown: false,
            now: minute(turnscope::now()),
            panel_open: false,
            tallest: None,
            grown: true,
            grow_due: None,
            closed: None,
        }
    }

    /// A state showing the fixed status `shown`, as `--fixture` and `--snapshot` do.
    pub fn fixed(shown: Shown, preferences: Preferences) -> State {
        let mut state = State::new(preferences);
        state.frozen = Some(shown.status.at);
        state.take(shown);
        state
    }

    /// Takes in a status the engine gave.
    pub fn take(&mut self, shown: Shown) {
        self.status = Some(shown.status);
        self.agents = shown.agents;
    }

    /// Every account, hidden ones too, as Settings lists them.
    pub fn all(&self) -> &[Account] {
        self.status.as_ref().map_or(&[], |status| &status.accounts)
    }

    /// Whether the agents' history is still being read for the first time.
    pub fn reading(&self) -> bool {
        self.status
            .as_ref()
            .is_some_and(|status| !status.history_read)
    }

    /// Whether `account` counts as in use, for every list and headline: as the engine
    /// measured, or, until the first read of the agents' history ends, always. Until then,
    /// which accounts are in use isn't known: counting none, the panel said "Nothing in use"
    /// over a fold of the accounts while the read took minutes, and the menu bar showed no
    /// figure.
    pub fn counts(&self, account: &Account) -> bool {
        self.reading() || account.in_use
    }

    /// The accounts in use, most urgent first, as the engine orders them.
    pub fn in_use(&self) -> Vec<&Account> {
        self.all()
            .iter()
            .filter(|a| !a.hidden && self.counts(a))
            .collect()
    }

    /// The accounts not in use but used this week, most left first.
    pub fn recent(&self) -> Vec<&Account> {
        self.all()
            .iter()
            .filter(|a| !a.hidden && !self.counts(a) && a.recent)
            .collect()
    }

    /// The accounts not used this week, most left first.
    pub fn unused(&self) -> Vec<&Account> {
        self.all()
            .iter()
            .filter(|a| !a.hidden && !self.counts(a) && !a.recent)
            .collect()
    }

    /// The accounts hidden from the menu bar and the panel.
    pub fn hidden(&self) -> Vec<&Account> {
        self.all().iter().filter(|a| a.hidden).collect()
    }

    /// Whether an account hidden from the panel counts as in use.
    pub fn hidden_in_use(&self) -> bool {
        self.all().iter().any(|a| a.hidden && self.counts(a))
    }

    /// The agents installed here, each with whether it runs Turnscope.
    pub fn installed(&self) -> Vec<&Agent> {
        self.agents.iter().filter(|a| a.installed).collect()
    }

    /// The names of every agent Turnscope reads, installed or not.
    pub fn agent_names(&self) -> Vec<&str> {
        self.agents.iter().map(|a| a.name.as_str()).collect()
    }

    /// Words as of the panel's `now`, or a fixed status's time.
    pub fn words(&self) -> Words {
        Words::new(self.frozen.unwrap_or(self.now))
    }

    /// Words as of now, or a fixed status's time, for what's shown whether the panel is open
    /// or not: the menu bar item and notifications.
    pub fn words_now(&self) -> Words {
        Words::new(self.frozen.unwrap_or_else(turnscope::now))
    }

    /// Whether the engine stopped for good, as once a newer Turnscope has written to the data.
    /// It then refuses every change, so the controls that ask for one are off.
    pub fn stopped(&self) -> bool {
        self.failure.is_some() && !self.trying_again
    }

    /// What the panel's headline says: before any account is found, why none is; otherwise
    /// whether you can keep going, from the most urgent account in use. Settings → Accounts
    /// says the same while it has no account to list.
    pub fn verdict(&self, words: &Words) -> (String, String, Standing) {
        if self.all().is_empty() {
            words.welcome(&self.installed(), &self.agent_names(), self.reading())
        } else {
            words.verdict(&self.in_use(), self.hidden_in_use())
        }
    }

    /// The agent in use to offer to connect: one that can't ask Turnscope about its limits,
    /// because it isn't connected or runs another copy. Agents whose offer was put away are
    /// skipped, and none is offered while the app runs from its disk image.
    pub fn to_connect(&self) -> Option<&Agent> {
        if self.from_disk_image {
            return None;
        }
        self.installed().into_iter().find(|a| {
            a.in_use
                && a.connection != Connection::Connected
                && !self.preferences.dismissed_connect.contains(&a.id)
        })
    }

    /// The sessions that used `account`'s deciding limit most, as last asked for. None if
    /// they were for a limit that no longer decides it.
    pub fn used_most(&self, account: &Account) -> &[UsedMost] {
        match self.used.get(&account.id) {
            Some((limit, sessions)) if Some(limit) == account.deciding.as_ref() => sessions,
            _ => &[],
        }
    }

    /// The accounts the menu bar item shows: every one in use, or only the most urgent.
    pub fn in_menu_bar(&self) -> Vec<&Account> {
        let in_use = self.in_use();
        let most = match self.preferences.menu_shows {
            MenuShows::All => in_use.len(),
            MenuShows::One => 1,
        };
        in_use.into_iter().take(most).collect()
    }

    /// Whether an account the item shows needs attention because its deciding limit does.
    /// Then the item is in color and counts down with the clock.
    pub fn urgent(&self) -> bool {
        self.in_menu_bar()
            .iter()
            .any(|a| a.standing() != Standing::Lasts)
    }

    /// Go back to where the panel opens, with nothing open, for next time. Run as the panel
    /// opens, it measures the room for a Settings tab on the screen it opens on.
    pub fn reset(&mut self) {
        self.page = Page::Main;
        self.tab = Tab::General;
        self.open = None;
        self.closed = None;
        self.grown = true;
        self.grow_due = None;
        self.unused_open = false;
        self.hidden_shown = false;
        self.tallest = Some(crate::settings::tallest());
    }

    /// Hide `account` or show it again, at once. The engine is asked by the caller, and gives
    /// the status back either way, so a refused change goes back.
    pub fn mark_hidden(&mut self, account: &str, hidden: bool) {
        if let Some(status) = &mut self.status
            && let Some(found) = status.accounts.iter_mut().find(|a| a.id == account)
        {
            found.hidden = hidden;
        }
        if hidden && self.open.as_deref() == Some(account) {
            self.open = None;
        }
    }

    /// Unfolds the place `account` is listed and opens it in place, as a notification about
    /// it asks, if it's shown on the first page.
    pub fn open_in_place(&mut self, account: &str) {
        if !self.all().iter().any(|a| a.id == account && !a.hidden) {
            return;
        }
        self.page = Page::Main;
        self.open = Some(account.to_owned());
        self.unused_open = self.unused().iter().any(|a| a.id == account);
    }
}

/// `at`, in milliseconds, floored to its minute, as the engine counts.
pub fn minute(at: i64) -> i64 {
    at.div_euclid(60_000) * 60_000
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example status in tests/fixtures/status.json, as `--fixture` shows it.
    fn example() -> Shown {
        serde_json::from_str(include_str!("../../turnscope/tests/fixtures/status.json")).unwrap()
    }

    fn shown(shown: Shown) -> State {
        let mut state = State::new(Preferences::default());
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
            state.words().via(&state.all()[0]).as_deref(),
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
