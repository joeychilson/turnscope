//! The engine runs in the app's own process (`turnscope::engine`). This
//! passes what it tells on to the app's state, and the app's requests on to
//! it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rune::{AppHandle, Tx};
use turnscope::alerts::Settings;
use turnscope::engine::{Engine, Event};
use turnscope::status::Connection;

use crate::state::State;

static ENGINE: OnceLock<Engine> = OnceLock::new();

/// Resolves the bundled `Helpers/turnscope` (or sibling binary under `target/`)
/// rather than searching `PATH`, because `connect` writes this path into agent configs.
pub fn binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let folder = exe.parent().unwrap_or(Path::new("/"));
    match folder.file_name().is_some_and(|name| name == "MacOS") {
        true => folder.with_file_name("Helpers").join("turnscope"),
        false => folder.join("turnscope"),
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
        Event::Shown(shown, mut alerts) => {
            if !s.alerts_waiting.is_empty() {
                let mut combined = std::mem::take(&mut s.alerts_waiting);
                combined.append(&mut alerts);
                alerts = combined;
            }
            crate::took(s, *shown, alerts, tx);
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
            if s.problem == stopped {
                return false;
            }
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
                // Shown now, so the old switch doesn't come back until the
                // next status says the same.
                None => {
                    if let Some(shown) = s.agents.iter_mut().find(|a| a.id == agent) {
                        shown.connection = match connecting {
                            true => Connection::Connected,
                            false => Connection::Available,
                        };
                    }
                }
            }
            s.panel_open
        }
    }
}

pub fn running() -> bool {
    ENGINE.get().is_some()
}

fn ask(request: impl FnOnce(&Engine)) {
    if let Some(engine) = ENGINE.get() {
        request(engine);
    }
}

pub fn hide(account: String, hidden: bool) {
    ask(|engine| engine.hide(account, hidden));
}

pub fn save_settings(settings: Settings) {
    ask(|engine| engine.save_settings(settings));
}

pub fn acknowledge(ids: Vec<i64>) {
    ask(|engine| engine.acknowledge(ids));
}

pub fn used_most(account: String, limit: String) {
    ask(|engine| engine.used_most(account, limit));
}

pub fn connect(agent: String, connecting: bool) {
    ask(|engine| engine.connect(agent, connecting));
}

pub fn ask_limits() {
    ask(|engine| engine.ask_limits());
}

pub fn log(line: &str) {
    if let Some(data) = DATA.get() {
        turnscope::engine::log(data, line);
    }
}

pub static DATA: OnceLock<PathBuf> = OnceLock::new();

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::example;
    use turnscope::alerts::{Alert, Kind};
    use turnscope::engine::Stopped;

    fn sign_in(account: &str) -> Alert {
        Alert {
            id: 1,
            kind: Kind::SignIn,
            account: account.into(),
            account_title: "OpenCode Go".into(),
            limit: None,
            limit_name: None,
            replaces: format!("account:{account}"),
            resets: None,
            runs_out: None,
        }
    }

    #[test]
    fn alerts_told_before_the_first_status_wait_for_it() {
        let mut s = State::default();
        let mut tx = Tx::default();
        let alert = sign_in("opencode-go:key-0123456789abcdef");
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
    fn why_the_data_isnt_kept_current_shows_as_it_changes() {
        let mut s = State {
            panel_open: true,
            ..State::default()
        };
        let mut tx = Tx::default();
        let stopped = Stopped {
            why: "The disk is full.".into(),
            trying_again: true,
        };
        assert!(took(&mut s, Event::Kept(Some(stopped.clone())), &mut tx));
        assert_eq!(s.problem.as_ref(), Some(&stopped));
        assert!(!s.stopped());
        // Told again the same, nothing changes on screen.
        assert!(!took(&mut s, Event::Kept(Some(stopped)), &mut tx));
        // A newer Turnscope wrote to the data: this one stops trying.
        let newer = Stopped {
            why: "A newer Turnscope wrote to the data.".into(),
            trying_again: false,
        };
        assert!(took(&mut s, Event::Kept(Some(newer)), &mut tx));
        assert!(s.stopped());
        assert!(took(&mut s, Event::Kept(None), &mut tx));
        assert_eq!(s.problem, None);
        assert!(!s.stopped());
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
}
