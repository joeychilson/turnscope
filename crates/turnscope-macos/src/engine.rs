//! The engine runs in the app's own process (`turnscope::engine`). This passes what it tells on
//! to the app's state, and the app's requests on to it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rune::{AppHandle, Tx};
use turnscope::engine::{Engine, Event, Shown};
use turnscope::status::{Account, Connection};

use crate::state::{Page, State};

static ENGINE: OnceLock<Engine> = OnceLock::new();

/// Resolves the bundled `Helpers/turnscope` (or sibling binary under `target/`)
/// rather than searching `PATH`, because `connect` writes this path into agent configs.
pub fn binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let folder = exe.parent().unwrap_or(Path::new("/"));
    if folder.file_name().is_some_and(|name| name == "MacOS") {
        folder.with_file_name("Helpers").join("turnscope")
    } else {
        folder.join("turnscope")
    }
}

pub fn start(home: PathBuf, data: PathBuf, handle: AppHandle<State>) {
    let engine = Engine::start(data, home, binary(), move |event| {
        handle.update_if(move |s, tx| took(s, event, tx));
    });
    let _ = ENGINE.set(engine);
}

fn took(s: &mut State, event: Event, tx: &mut Tx) -> bool {
    match event {
        Event::Shown(shown, alerts) => {
            // Rune shows a change no input caused at once: animated, the open panel's changes move.
            if s.panel_open {
                tx.animated();
            }
            let taken = take(s, *shown);
            crate::drew(s);
            crate::notifier::renewed(&taken.readable);
            for (account, limit) in taken.asks {
                ask(|e| e.used_most(account, limit));
            }
            let mut told = std::mem::take(&mut s.alerts_waiting);
            told.extend(alerts);
            crate::notifier::tell(s, told);
            s.panel_open
        }
        Event::Pending(alerts) if s.status.is_none() => {
            s.alerts_waiting.extend(alerts);
            false
        }
        Event::Pending(alerts) => {
            crate::notifier::tell(s, alerts);
            false
        }
        Event::Kept(stopped) => {
            s.problem = stopped;
            crate::menu_bar::draw(s);
            s.panel_open
        }
        Event::UsedMost {
            account,
            limit,
            sessions,
        } => {
            // Shown only under the account open in the panel, where they move in.
            let shown = s.panel_open && s.open.as_ref() == Some(&account);
            if shown {
                tx.animated();
            }
            s.used.insert(account, (limit, sessions));
            shown
        }
        Event::Connected {
            agent,
            connecting,
            error,
        } => {
            s.changing_agents.remove(&agent);
            match error {
                Some(error) => {
                    s.agent_errors.insert(agent, error);
                }
                // Shown now, or the old switch came back until the next status.
                None => {
                    if let Some(shown) = s.agents.iter_mut().find(|a| a.id == agent) {
                        shown.connection = if connecting {
                            Connection::Connected
                        } else {
                            Connection::Available
                        };
                    }
                }
            }
            s.panel_open
        }
    }
}

struct Taken {
    /// Accounts to ask what used them most, each with its deciding limit.
    asks: Vec<(String, String)>,
    /// Accounts whose sign-in notification can go.
    readable: Vec<String>,
}

fn take(s: &mut State, shown: Shown) -> Taken {
    let first = s.status.is_none();
    let refused: Vec<String> = s
        .all()
        .iter()
        .filter(|a| a.refused())
        .map(|a| a.id.clone())
        .collect();
    s.take(shown);
    if let Some(opening) = s.opening.take() {
        crate::open_with(opening);
    }
    if let Some(clicked) = s.clicked.take()
        && s.panel_open
        && s.page == Page::Main
    {
        s.open_in_place(&clicked);
    }
    let readable = s
        .all()
        .iter()
        .filter(|a| !a.refused() && (first || refused.contains(&a.id)))
        .map(|a| a.id.clone())
        .collect();
    let asks = if s.panel_open {
        asks(s, |a| s.open.as_deref() == Some(a.id.as_str()))
    } else {
        Vec::new()
    };
    Taken { asks, readable }
}

/// The shown accounts to ask what used them most, each with its deciding limit: those `again`
/// picks, and those with no answer for that limit yet.
pub fn asks(s: &State, again: impl Fn(&Account) -> bool) -> Vec<(String, String)> {
    s.all()
        .iter()
        .filter(|a| !a.hidden)
        .filter_map(|a| {
            let limit = a.deciding.as_ref()?;
            let have = s.used.get(&a.id).is_some_and(|(of, _)| of == limit);
            (again(a) || !have).then(|| (a.id.clone(), limit.clone()))
        })
        .collect()
}

pub fn ask_used(s: &State, id: &str) {
    if let Some(limit) = s
        .all()
        .iter()
        .find(|a| a.id == id)
        .and_then(|a| a.deciding.clone())
    {
        ask(|e| e.used_most(id.to_owned(), limit));
    }
}

pub fn hide(s: &mut State, id: &str, hidden: bool) {
    if s.stopped() {
        return;
    }
    s.mark_hidden(id, hidden);
    crate::drew(s);
    ask(|e| e.hide(id.to_owned(), hidden));
}

pub fn connect(s: &mut State, id: &str, connecting: bool) {
    if !running() || s.stopped() || s.from_disk_image || s.changing_agents.contains_key(id) {
        return;
    }
    let to = if connecting {
        Connection::Connected
    } else {
        Connection::Available
    };
    s.changing_agents.insert(id.to_owned(), to);
    s.agent_errors.remove(id);
    ask(|e| e.connect(id.to_owned(), connecting));
}

pub fn running() -> bool {
    ENGINE.get().is_some()
}

/// Asks the engine for `request`, if it runs: a fixed status has none.
pub fn ask(request: impl FnOnce(&Engine)) {
    if let Some(engine) = ENGINE.get() {
        request(engine);
    }
}

pub static DATA: OnceLock<PathBuf> = OnceLock::new();

pub fn log(line: &str) {
    if let Some(data) = DATA.get() {
        turnscope::engine::log(data, line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::example;
    use turnscope::alerts::{Alert, Kind};

    const CLAUDE: &str = "anthropic:acct-1:org-1";
    const CODEX: &str = "openai:acct-2";
    const OPENROUTER: &str = "openrouter:api";
    const OPENCODE_GO: &str = "opencode-go:key-0123456789abcdef";

    fn asked(taken: &Taken) -> Vec<&str> {
        taken.asks.iter().map(|(id, _)| id.as_str()).collect()
    }

    #[test]
    fn alerts_told_before_the_first_status_wait_for_it() {
        let mut s = State::default();
        let mut tx = Tx::default();
        let alert = Alert {
            id: 1,
            kind: Kind::SignIn,
            account: OPENCODE_GO.into(),
            account_title: "OpenCode Go".into(),
            limit: None,
            limit_name: None,
            replaces: format!("account:{OPENCODE_GO}"),
            resets: None,
            runs_out: None,
        };
        assert!(!took(&mut s, Event::Pending(vec![alert.clone()]), &mut tx));
        assert_eq!(s.alerts_waiting.len(), 1);
        // Closed, a status updates state without rebuilding the panel.
        assert!(!took(
            &mut s,
            Event::Shown(Box::new(example()), Vec::new()),
            &mut tx,
        ));
        assert_eq!(s.alerts_waiting.len(), 0);
        // Once there's a status, they're posted as they come.
        took(&mut s, Event::Pending(vec![alert]), &mut tx);
        assert_eq!(s.alerts_waiting.len(), 0);
    }

    #[test]
    fn a_connect_that_ends_shows_at_once_and_one_that_fails_says_why() {
        let mut s = State {
            panel_open: true,
            ..State::default()
        };
        let mut tx = Tx::default();
        s.take(example());
        let codex = |s: &State| {
            s.agents
                .iter()
                .find(|a| a.id == "codex")
                .unwrap()
                .connection
        };
        s.changing_agents
            .insert("codex".into(), Connection::Connected);
        let ended = Event::Connected {
            agent: "codex".into(),
            connecting: true,
            error: None,
        };
        assert!(took(&mut s, ended, &mut tx));
        assert!(s.changing_agents.is_empty());
        assert_eq!(codex(&s), Connection::Connected);
        s.changing_agents
            .insert("codex".into(), Connection::Available);
        let failed = Event::Connected {
            agent: "codex".into(),
            connecting: false,
            error: Some("Codex couldn't be run.".into()),
        };
        assert!(took(&mut s, failed, &mut tx));
        assert!(s.changing_agents.is_empty());
        assert_eq!(
            s.agent_errors.get("codex").map(String::as_str),
            Some("Codex couldn't be run.")
        );
        // Its switch stays as it was.
        assert_eq!(codex(&s), Connection::Connected);
    }

    #[test]
    fn a_notification_clicked_before_the_first_status_opens_its_account_with_what_used_it() {
        // As `open_panel` leaves it when the click launched the app.
        let mut s = State {
            panel_open: true,
            clicked: Some(CODEX.into()),
            ..State::default()
        };
        let taken = take(&mut s, example());
        assert_eq!(s.open.as_deref(), Some(CODEX));
        // Every account shown that has a deciding limit, the one clicked among them.
        assert_eq!(asked(&taken), [CLAUDE, CODEX, OPENROUTER]);
    }

    #[test]
    fn while_the_panel_is_open_a_status_asks_what_used_each_account_that_has_none() {
        let mut s = State::default();
        take(&mut s, example());
        s.panel_open = true;
        s.open = Some(CLAUDE.into());
        for (id, limit) in asks(&s, |_| true) {
            s.used.insert(id, (limit, Vec::new()));
        }
        // Only the open account's again, as its sessions move with the status.
        assert_eq!(asked(&take(&mut s, example())), [CLAUDE]);
        // Another limit decides Codex now.
        let mut changed = example();
        changed.status.accounts[1].deciding = Some("primary".into());
        assert_eq!(asked(&take(&mut s, changed)), [CLAUDE, CODEX]);
        // Closed, the panel asks for none.
        s.panel_open = false;
        assert!(take(&mut s, example()).asks.is_empty());
    }

    #[test]
    fn a_sign_in_notification_goes_on_the_first_status_and_once_its_account_is_read_again() {
        let mut s = State::default();
        // Each account that can be read, for a notification left from before the app started.
        let first = take(&mut s, example()).readable;
        assert_eq!(first, [CLAUDE, CODEX, OPENROUTER, "anthropic:api"]);
        // Then none, while no account is read again.
        assert!(take(&mut s, example()).readable.is_empty());
        let mut read = example();
        read.status.accounts[3].state = turnscope::status::State::Live {
            read_at: read.status.at,
        };
        assert_eq!(take(&mut s, read).readable, [OPENCODE_GO]);
    }
}
