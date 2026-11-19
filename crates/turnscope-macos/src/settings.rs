//! Settings, a page of the panel with three tabs (docs/app.md#settings). A tab taller than the
//! screen allows scrolls under its title.

use rune::{El, Tx, col, row, spacer, text};

use crate::login;
use crate::panel::{account_name, agent_name, go, waiting};
use crate::preferences::MenuShows;
use crate::standing::*;
use crate::state::{Page, State, Tab};
use crate::style::*;
use crate::updater;
use crate::words::Words;

pub fn page(s: &State, ink: &Ink) -> El {
    // The page's title with the Back button beside it, and the tabs. They line up with the
    // cards' edges.
    let tabs: Vec<(Tab, &str)> = Tab::ALL.iter().map(|t| (*t, t.title())).collect();
    let header = row()
        .key("settings-header")
        .gap(4.0)
        .center_y()
        .w_full()
        .px(8.0)
        .pt(8.0)
        .pb(10.0)
        .child(
            icon_button("back", "chevron.left", 28.0, 28.0, true, ink)
                .label("Back")
                .help("Back")
                .on_press(|s: &mut State| go(s, Page::Main)),
        )
        .child(
            text("Settings")
                .font(TITLE)
                .color(ink.primary)
                .role("heading"),
        )
        .child(spacer())
        .child(choice(
            "tabs",
            "Settings",
            &tabs,
            s.tab,
            |s, tab, _| s.tab = tab,
            ink,
        ));
    let w = s.words();
    let tab = match s.tab {
        Tab::General => general(s, ink),
        // The other tabs list what the status holds. Before the first one, they say what the
        // panel says.
        _ if s.status.is_none() => waiting(s, ink),
        Tab::Agents => agents(s, &w, ink),
        Tab::Accounts => accounts(s, &w, ink),
    };
    // A tab that scrolls fades at the edge that has more under it.
    let mut scrolls = col()
        .key("tab")
        .w_full()
        .scroll(rune::ScrollSpec {
            stick_bottom: false,
        })
        .scroll_edge_fade(16.0)
        .scroll_margin(16.0)
        .child(tab.px(8.0).pb(8.0).w_full());
    if let Some(tallest) = s.tallest {
        scrolls = scrolls.max_h(tallest);
    }
    col().key("settings").w_full().child(header).child(scrolls)
}

/// The most height the screen allows a tab, below the menu bar and inside the panel's edges.
/// AppKit is asked, so it's worked out as the panel opens (`State::reset`), not as a view runs.
pub fn tallest() -> f32 {
    crate::menu_bar::screen_height().map_or(800.0, |h| h as f32) - 140.0
}

/// A titled group of rows in a card. Where a row's name alone doesn't say what its switch
/// does, the title says it, as "Notify me when" does.
fn group(key: &str, title: &str, rows: Vec<El>, ink: &Ink) -> El {
    let mut card = col()
        .key("card")
        .w_full()
        .py(3.0)
        .radius(row_size::RADIUS)
        .continuous_corners()
        .fill(ink.wash(0.05));
    for row in rows {
        card = card.child(row);
    }
    col()
        .key(key)
        .w_full()
        .child(section_title(title, ink))
        .child(card)
}

/// A setting: its name and its control on one line. More about it, `help`, shows on hover.
fn setting(key: &str, title: &str, help: Option<&str>, control: El, ink: &Ink) -> El {
    let mut line = row()
        .key(key)
        .center_y()
        .gap(row_size::GAP)
        .w_full()
        .min_h(row_size::SETTING)
        .py(row_size::PAD_Y)
        .px(row_size::PAD_X)
        .child(text(title).font(NAME).color(ink.primary))
        .child(spacer().min_w(8.0))
        .child(control);
    if let Some(help) = help {
        line = line.help(help);
    }
    line
}

// General

fn general(s: &State, ink: &Ink) -> El {
    col()
        .key("general")
        .gap(14.0)
        .w_full()
        .child(menu_bar(s, ink))
        .child(notifications(s, ink))
        .child(updates(s, ink))
}

fn menu_bar(s: &State, ink: &Ink) -> El {
    let shows = choice(
        "menu-shows",
        "Show",
        &[
            (MenuShows::All, "All in use"),
            (MenuShows::One, "Most urgent"),
        ],
        s.preferences.menu_shows,
        |s, shows, _| {
            s.preferences.set_menu_shows(shows);
            crate::menu_bar::draw(s);
        },
        ink,
    );
    let at_login = s.login.at_login();
    let mut rows = vec![
        setting("show", "Show", None, shows, ink),
        setting(
            "login",
            "Open at login",
            None,
            switch("login-switch", "Open at login", at_login, true, ink)
                .on_press(move |s: &mut State| s.login = login::set(!at_login)),
            ink,
        ),
    ];
    // macOS may hold it until it's allowed in System Settings.
    if s.login == login::Login::NeedsApproval {
        rows.push(appears(setting(
            "approve",
            "Allow it in System Settings",
            None,
            pill("open-login-items", "Open", true, true, ink)
                .label("Open Login Items in System Settings")
                .on_press_tx(|_: &mut State, tx: &mut Tx| {
                    tx.close_window();
                    login::open_settings();
                }),
            ink,
        )));
    }
    group("menu-bar", "Menu bar", rows, ink)
}

fn notifications(s: &State, ink: &Ink) -> El {
    let mut rows = Vec::new();
    if s.notifications_allowed == Some(false) {
        rows.push(appears(setting(
            "off",
            "Notifications are off for Turnscope",
            None,
            pill("open-notifications", "Open", true, true, ink)
                .label("Open Notifications in System Settings")
                .on_press_tx(|_: &mut State, tx: &mut Tx| {
                    tx.close_window();
                    crate::notifier::open_settings();
                }),
            ink,
        )));
    }
    // Each finishes the sentence "Notify me when". They come with the status, so until the
    // first one there's nothing to change, nor once the engine has stopped for good.
    let notify = s.status.as_ref().map(|status| status.settings.notify);
    let enabled = notify.is_some() && !s.stopped();
    let on = notify.unwrap_or_default();
    type Pick = fn(&mut turnscope::alerts::Notify) -> &mut bool;
    let kinds: [(&str, &str, Option<&str>, bool, Pick); 4] = [
        (
            "running-out",
            "A limit will run out",
            Some("Before it resets, at your pace"),
            on.running_out,
            |n| &mut n.running_out,
        ),
        ("used-up", "A limit is used up", None, on.used_up, |n| {
            &mut n.used_up
        }),
        (
            "reset",
            "A limit is back",
            Some("After it was used up, once the account can be used again"),
            on.reset,
            |n| &mut n.reset,
        ),
        (
            "sign-in",
            "An account needs signing in",
            Some("Its login was refused, so its limits can't be read"),
            on.sign_in,
            |n| &mut n.sign_in,
        ),
    ];
    for (key, title, help, value, pick) in kinds {
        let control = switch(&format!("{key}-switch"), title, value, enabled, ink).on_press(
            move |s: &mut State| {
                if enabled && let Some(status) = &mut s.status {
                    let flag = pick(&mut status.settings.notify);
                    *flag = !*flag;
                    crate::engine::save_settings(status.settings);
                }
            },
        );
        rows.push(setting(key, title, help, control, ink));
    }
    group("notifications", "Notify me when", rows, ink)
}

/// The version, Check now, and Check automatically, in rows like every other setting.
fn updates(s: &State, ink: &Ink) -> El {
    let version = env!("CARGO_PKG_VERSION");
    let mut rows = Vec::new();
    let check = if updater::available() {
        pill("check-now", "Check now", true, true, ink)
            .label("Check for updates now")
            .on_press_tx(|_: &mut State, tx: &mut Tx| {
                tx.close_window();
                updater::check();
            })
    } else {
        rune::el()
    };
    rows.push(setting(
        "version",
        &format!("Turnscope {version}"),
        None,
        check,
        ink,
    ));
    if updater::available() {
        let automatic = s.checks_automatically;
        rows.push(setting(
            "automatic",
            "Check automatically",
            None,
            switch(
                "automatic-switch",
                "Check automatically",
                automatic,
                true,
                ink,
            )
            .on_press(move |s: &mut State| {
                updater::set_automatic(!automatic);
                s.checks_automatically = !automatic;
            }),
            ink,
        ));
    }
    group("updates", "Updates", rows, ink)
}

// Agents

/// Each installed agent that can use Turnscope's MCP server, with a switch that connects or
/// disconnects it. With none installed, what the panel says then: the agents Turnscope reads.
fn agents(s: &State, w: &Words, ink: &Ink) -> El {
    let installed = s.installed();
    if installed.is_empty() {
        let (headline, detail, _) = w.welcome(&[], &s.agent_names(), false);
        return empty("no-agents", &headline, &detail, ink);
    }
    let title = if s.from_disk_image {
        "Move Turnscope to Applications to connect agents"
    } else {
        "Let these agents check their limits"
    };
    let rows = installed
        .iter()
        .map(|agent| agent_row(s, w, agent, ink))
        .collect();
    group("agents", title, rows, ink)
}

/// A tab with nothing to list: what the panel's headline would say, centred.
fn empty(key: &str, headline: &str, detail: &str, ink: &Ink) -> El {
    col()
        .key(key)
        .w_full()
        .min_h(140.0)
        .padding(24.0)
        .center()
        .gap(8.0)
        .child(text(headline).font(STRONG).color(ink.primary))
        .child(
            text(detail)
                .font(LINE)
                .color(ink.secondary)
                .wrap()
                .align(rune::Align::Center),
        )
}

/// An agent, in a row like an account's: its logo, its name, and a switch that connects it. A
/// switch says on or off by itself, so the line under the name shows only what it can't: that
/// the agent uses another copy of Turnscope, which turning it on fixes, or why connecting or
/// disconnecting failed.
fn agent_row(s: &State, w: &Words, agent: &Agent, ink: &Ink) -> El {
    let changing = s.changing_agents.get(&agent.id).copied();
    let note = s.agent_errors.get(&agent.id).cloned().or_else(|| {
        w.connection(agent)
            .filter(|_| agent.connection != Connection::Available)
    });
    let mut line = row()
        .key(format!("agent:{}", agent.id))
        .center_y()
        .gap(row_size::GAP)
        .w_full()
        .min_h(row_size::SETTING)
        .py(row_size::PAD_Y)
        .px(row_size::PAD_X)
        .child(agent_logo(&agent.id, row_size::LOGO, ink.primary, ink))
        .child(agent_name(agent, note, ink))
        .child(spacer().min_w(8.0));
    // Connecting runs the agent's own command, which can take a while. Meanwhile the switch
    // shows where it's going, and what's happening is said beside it rather than under the
    // name, so the row doesn't grow and shrink.
    if let Some(changing) = changing {
        let said = if changing == Connection::Connected {
            "Connecting…"
        } else {
            "Disconnecting…"
        };
        line = line.child(appears(
            text(said).key("changing").font(SMALL).color(ink.secondary),
        ));
    }
    let on = changing.unwrap_or(agent.connection) == Connection::Connected;
    let enabled = changing.is_none() && !s.from_disk_image;
    let id = agent.id.clone();
    line.child(
        switch(
            "connect-switch",
            &format!("Connect {}", agent.name),
            on,
            enabled,
            ink,
        )
        .on_press_tx(move |s: &mut State, _: &mut Tx| {
            if enabled {
                crate::connect(s, &id, !on);
            }
        }),
    )
}

// Accounts

/// Every account found, with subscriptions and API keys in separate groups. Each has a switch
/// to leave it out of the menu bar and the panel.
fn accounts(s: &State, w: &Words, ink: &Ink) -> El {
    if s.all().is_empty() {
        // With no account to list, what the panel's headline says, as "Getting your limits"
        // on a first launch.
        let (headline, detail, _) = s.verdict(w);
        return empty("no-accounts", &headline, &detail, ink);
    }
    let mut tab = col().key("accounts").gap(14.0).w_full();
    for (key, title, api) in [
        ("subscriptions", "Subscriptions", false),
        ("api-keys", "API keys", true),
    ] {
        let accounts: Vec<&Account> = s
            .all()
            .iter()
            .filter(|a| (a.kind == turnscope::providers::Kind::ApiKey) == api)
            .collect();
        if accounts.is_empty() {
            continue;
        }
        let rows = accounts
            .iter()
            .map(|a| account_switch(w, a, !s.stopped(), ink))
            .collect();
        tab = tab.child(group(key, title, rows, ink));
    }
    tab
}

/// An account with a switch to show it, which is off once the engine has stopped for good
/// (`enabled`).
fn account_switch(w: &Words, a: &Account, enabled: bool, ink: &Ink) -> El {
    let id = a.id.clone();
    let shown = !a.hidden;
    row()
        .key(format!("account:{}", a.id))
        .center_y()
        .gap(row_size::GAP)
        .w_full()
        .min_h(row_size::SETTING)
        .py(row_size::PAD_Y)
        .px(row_size::PAD_X)
        .child(account_logo(&a.provider, row_size::LOGO, ink.primary, ink))
        .child(account_name(w, a, ink.primary, ink.secondary, ink))
        .child(spacer().min_w(8.0))
        .child(
            switch(
                "show-switch",
                &format!("Show {}", w.called(a, true)),
                shown,
                enabled,
                ink,
            )
            .on_press_tx(move |s: &mut State, _: &mut Tx| {
                if enabled {
                    crate::hide(s, &id, shown)
                }
            }),
        )
}
