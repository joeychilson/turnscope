//! Settings, a page of the panel with three tabs (docs/app.md#settings). A tab taller than the
//! screen allows scrolls under its title.

use rune::{El, Tx, col, row, spacer, text};

use crate::login;
use crate::panel::{account_name, agent_name, waiting};
use crate::preferences::MenuShows;
use crate::standing::*;
use crate::state::{Page, State, Tab};
use crate::style::*;
use crate::updater;
use crate::words;

pub fn page(s: &State, ink: &Ink) -> El {
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
                .on_press(|s: &mut State| s.page = Page::Main),
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
            &Tab::ALL,
            s.tab,
            |s, tab, _| s.tab = tab,
            ink,
        ));
    let tab = match s.tab {
        Tab::General => general(s, ink),
        _ if s.status.is_none() => waiting(s, ink),
        Tab::Agents => agents(s, ink),
        Tab::Accounts => accounts(s, ink),
    };
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

/// Computed on panel open rather than inside `view` because it queries AppKit.
pub fn tallest() -> f32 {
    crate::menu_bar::screen_height().map_or(800.0, |h| h as f32) - 140.0
}

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

fn setting(key: &str, title: &str, help: Option<&str>, control: El, ink: &Ink) -> El {
    let mut line = item_row(key)
        .min_h(row_size::SETTING)
        .child(text(title).font(NAME).color(ink.primary))
        .child(spacer().min_w(8.0))
        .child(control);
    if let Some(help) = help {
        line = line.help(help);
    }
    line
}

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
    let mut rows = vec![
        setting("show", "Show", None, shows, ink),
        setting(
            "login",
            "Open at login",
            None,
            switch(
                "login-switch",
                "Open at login",
                s.login.at_login(),
                true,
                ink,
            )
            .on_press(|s: &mut State| s.login = login::set(!s.login.at_login())),
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
    if s.notifications_denied {
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
    let mut on = notify.unwrap_or_default();
    type Pick = fn(&mut turnscope::alerts::Notify) -> &mut bool;
    let kinds: [(&str, &str, Option<&str>, Pick); 4] = [
        (
            "running-out",
            "A limit will run out",
            Some("Before it resets, at your pace"),
            |n| &mut n.running_out,
        ),
        ("used-up", "A limit is used up", None, |n| &mut n.used_up),
        (
            "reset",
            "A limit is back",
            Some("After it was used up, once the account can be used again"),
            |n| &mut n.reset,
        ),
        (
            "sign-in",
            "An account needs signing in",
            Some("Its login was refused, so its limits can't be read"),
            |n| &mut n.sign_in,
        ),
    ];
    for (key, title, help, pick) in kinds {
        let value = *pick(&mut on);
        let control = switch(&format!("{key}-switch"), title, value, enabled, ink).on_press(
            move |s: &mut State| {
                if !s.stopped()
                    && let Some(status) = &mut s.status
                {
                    let flag = pick(&mut status.settings.notify);
                    *flag = !*flag;
                    crate::engine::ask(|e| e.save_settings(status.settings));
                }
            },
        );
        rows.push(setting(key, title, help, control, ink));
    }
    group("notifications", "Notify me when", rows, ink)
}

fn updates(s: &State, ink: &Ink) -> El {
    let version = env!("CARGO_PKG_VERSION");
    let available = updater::available();
    let mut rows = Vec::new();
    let check = if available {
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
    if available {
        rows.push(setting(
            "automatic",
            "Check automatically",
            None,
            switch(
                "automatic-switch",
                "Check automatically",
                s.checks_automatically,
                true,
                ink,
            )
            .on_press(|s: &mut State| {
                let next = !s.checks_automatically;
                updater::set_automatic(next);
                s.checks_automatically = next;
            }),
            ink,
        ));
    }
    group("updates", "Updates", rows, ink)
}

fn agents(s: &State, ink: &Ink) -> El {
    let installed = s.installed();
    if installed.is_empty() {
        let (headline, detail, _) = words::welcome(&[], &s.agent_names(), false);
        return empty("no-agents", &headline, &detail, ink);
    }
    let title = if s.from_disk_image {
        "Move Turnscope to Applications to connect agents"
    } else {
        "Let these agents check their limits"
    };
    let rows = installed
        .iter()
        .map(|agent| agent_row(s, agent, ink))
        .collect();
    group("agents", title, rows, ink)
}

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

fn agent_row(s: &State, agent: &Agent, ink: &Ink) -> El {
    let changing = s.changing_agents.get(&agent.id).copied();
    let note =
        s.agent_errors.get(&agent.id).cloned().or_else(|| {
            words::connection(agent).filter(|_| agent.connection != Connection::Available)
        });
    let mut line = item_row(format!("agent:{}", agent.id))
        .min_h(row_size::SETTING)
        .child(agent_logo(&agent.id, row_size::LOGO, ink.primary, ink))
        .child(agent_name(agent, note, ink))
        .child(spacer().min_w(8.0));
    // Shown beside the switch rather than under the name so the row doesn't resize.
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
    let enabled = changing.is_none() && !s.from_disk_image && !s.stopped();
    let id = agent.id.clone();
    line.child(
        switch(
            "connect-switch",
            &format!("Connect {}", agent.name),
            on,
            enabled,
            ink,
        )
        .on_press(move |s: &mut State| {
            let connected = s
                .agents
                .iter()
                .find(|a| a.id == id)
                .is_some_and(|a| a.connection == Connection::Connected);
            crate::connect(s, &id, !connected);
        }),
    )
}

fn accounts(s: &State, ink: &Ink) -> El {
    if s.all().is_empty() {
        let (headline, detail, _) = words::welcome(&s.installed(), &s.agent_names(), s.reading());
        return empty("no-accounts", &headline, &detail, ink);
    }
    let enabled = !s.stopped();
    let mut tab = col().key("accounts").gap(14.0).w_full();
    for (key, title, api) in [
        ("subscriptions", "Subscriptions", false),
        ("api-keys", "API keys", true),
    ] {
        let rows: Vec<El> = s
            .all()
            .iter()
            .filter(|a| (a.kind == turnscope::providers::Kind::ApiKey) == api)
            .map(|a| account_switch(a, enabled, ink))
            .collect();
        if !rows.is_empty() {
            tab = tab.child(group(key, title, rows, ink));
        }
    }
    tab
}

fn account_switch(a: &Account, enabled: bool, ink: &Ink) -> El {
    let id = a.id.clone();
    item_row(format!("account:{}", a.id))
        .min_h(row_size::SETTING)
        .child(account_logo(&a.provider, row_size::LOGO, ink.primary, ink))
        .child(account_name(a, ink.primary, ink.secondary, ink))
        .child(spacer().min_w(8.0))
        .child(
            switch(
                "show-switch",
                &format!("Show {}", words::called(a, true)),
                !a.hidden,
                enabled,
                ink,
            )
            .on_press(move |s: &mut State| {
                if let Some(account) = s.all().iter().find(|a| a.id == id) {
                    crate::hide(s, &id, !account.hidden);
                }
            }),
        )
}
