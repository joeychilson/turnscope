//! Serializable view models rendered by every native platform shell.
//! Every word, ranking, standing, and accessibility label is computed here once.

use serde::Serialize;
use turnscope::alerts::Notify;
use turnscope::engine::{Stopped, UsedMost};
use turnscope::status::{Account, Agent, Connection, Limit};

use crate::standing::{AccountStanding, LimitStanding, Standing};
use crate::state::{Effect, MenuShows, Page, State, Tab};
use crate::words::{self, Words};

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSync {
    pub animated: bool,
    pub counting: bool,
    pub menu_bar: MenuBarModel,
    pub panel: PanelModel,
    pub effects: Vec<Effect>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MenuBarModel {
    pub slots: Vec<MenuBarSlot>,
    pub spoken: String,
    pub out_of_date: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MenuBarSlot {
    pub provider: String,
    pub standing: Standing,
    pub figure: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelModel {
    pub version: String,
    pub page: Page,
    pub tab: Tab,
    pub open_account: Option<String>,
    pub unused_open: bool,
    pub hidden_shown: bool,
    pub has_status: bool,
    pub stopped: bool,
    pub from_disk_image: bool,
    pub waiting: Option<WaitingModel>,
    pub verdict: Option<VerdictModel>,
    pub in_use: Vec<AccountRowModel>,
    pub recent: Vec<AccountRowModel>,
    pub unused: Vec<AccountRowModel>,
    pub hidden: Vec<HiddenAccountModel>,
    pub unused_fold_label: Option<String>,
    pub hidden_fold_label: Option<String>,
    pub connect_offer: Option<ConnectOfferModel>,
    pub footer_note: Option<FooterNoteModel>,
    pub settings: SettingsModel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitingModel {
    pub problem: Option<String>,
    pub retries: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerdictModel {
    pub headline: String,
    pub detail: String,
    pub standing: Standing,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRowModel {
    pub id: String,
    pub provider: String,
    pub title: String,
    pub label: Option<String>,
    pub called: String,
    pub standing: Standing,
    pub status: String,
    pub figure: String,
    pub figure_then: Option<String>,
    pub figure_spoken: String,
    pub header_spoken: String,
    pub openable: bool,
    pub limits: Vec<LimitLineModel>,
    pub note: Option<String>,
    pub used_most_heading: Option<String>,
    pub used_most: Vec<UsedMostModel>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitLineModel {
    pub key: String,
    pub name: String,
    pub ahead: Option<String>,
    pub window: Option<String>,
    pub left: String,
    pub size: Option<String>,
    pub spoken: String,
    pub left_percent: u8,
    pub standing: Standing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsedMostModel {
    pub session: String,
    pub title: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HiddenAccountModel {
    pub id: String,
    pub provider: String,
    pub title: String,
    pub label: Option<String>,
    pub via: Option<String>,
    pub called: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectOfferModel {
    pub id: String,
    pub name: String,
    pub note: Option<String>,
    pub changing: bool,
    pub outdated: bool,
    pub verb: String,
    pub help: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum FooterNoteModel {
    Stale {
        text: String,
        help: String,
        spoken: String,
    },
    Reading {
        text: String,
        help: String,
        spoken: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmptyMessage {
    pub headline: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRowModel {
    pub id: String,
    pub name: String,
    pub note: Option<String>,
    pub changing: Option<String>,
    pub connected: bool,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSwitchModel {
    pub id: String,
    pub provider: String,
    pub title: String,
    pub label: Option<String>,
    pub via: Option<String>,
    pub called: String,
    pub shown: bool,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsModel {
    pub menu_shows: MenuShows,
    pub notify: Option<Notify>,
    pub can_edit_notify: bool,
    pub agents_empty: Option<EmptyMessage>,
    pub agents_title: String,
    pub agents: Vec<AgentRowModel>,
    pub accounts_empty: Option<EmptyMessage>,
    pub subscriptions: Vec<AccountSwitchModel>,
    pub api_keys: Vec<AccountSwitchModel>,
}

pub fn build_sync(s: &State, animated: bool, effects: Vec<Effect>) -> AppSync {
    AppSync {
        animated,
        counting: s.counting(),
        menu_bar: build_menu_bar(s),
        panel: build_panel(s),
        effects,
    }
}

pub fn build_menu_bar(s: &State) -> MenuBarModel {
    let words = s.words_now();
    let shown = s.in_menu_bar();
    let out_of_date = s.problem.is_some();
    MenuBarModel {
        slots: shown
            .iter()
            .map(|a| MenuBarSlot {
                provider: a.provider.clone(),
                standing: a.standing(),
                figure: words.figure(a),
            })
            .collect(),
        spoken: words.spoken_item(&shown, out_of_date),
        out_of_date,
    }
}

pub fn build_panel(s: &State) -> PanelModel {
    let has_status = s.status.is_some();
    let w = s.words();
    let in_use_accounts = s.in_use();
    let recent_accounts = s.recent();
    let unused_accounts = s.unused();
    let hidden_accounts = s.hidden();

    let waiting = (!has_status).then(|| WaitingModel {
        problem: s.problem.as_ref().map(ToString::to_string),
        retries: matches!(s.problem, Some(Stopped::Failing(_))),
    });

    let verdict = has_status.then(|| {
        let (headline, detail, standing) = s.verdict(&w);
        VerdictModel {
            headline,
            detail,
            standing,
        }
    });

    let in_use: Vec<AccountRowModel> = in_use_accounts
        .iter()
        .enumerate()
        .map(|(i, a)| build_account_row(s, &w, a, i == 0))
        .collect();
    let recent: Vec<AccountRowModel> = recent_accounts
        .iter()
        .map(|a| build_account_row(s, &w, a, false))
        .collect();
    let unused: Vec<AccountRowModel> = unused_accounts
        .iter()
        .map(|a| build_account_row(s, &w, a, false))
        .collect();
    let hidden: Vec<HiddenAccountModel> = hidden_accounts
        .iter()
        .map(|a| HiddenAccountModel {
            id: a.id.clone(),
            provider: a.provider.clone(),
            title: a.title.clone(),
            label: a.label.clone(),
            via: words::via(a),
            called: words::called(a, true),
        })
        .collect();

    let beside = !in_use.is_empty() || !recent.is_empty();
    let unused_fold_label = if unused.is_empty() && hidden.is_empty() {
        None
    } else if unused.is_empty() {
        Some(words::hidden(hidden.len()))
    } else {
        Some(words::unused(unused.len(), beside))
    };
    let hidden_fold_label =
        (!hidden.is_empty() && !unused.is_empty()).then(|| words::hidden(hidden.len()));

    let connect_offer = s.to_connect().map(|agent| {
        let note = s
            .agent_errors
            .get(&agent.id)
            .cloned()
            .or_else(|| words::connection(agent));
        let outdated = agent.connection == Connection::Outdated;
        let verb = if outdated { "Reconnect" } else { "Connect" };
        let help = if outdated {
            format!("Points {} at this copy of Turnscope", agent.name)
        } else {
            format!(
                "Adds Turnscope to {}'s MCP servers, so it can pace itself",
                agent.name
            )
        };
        ConnectOfferModel {
            id: agent.id.clone(),
            name: agent.name.clone(),
            note,
            changing: s.changing_agents.contains_key(&agent.id),
            outdated,
            verb: verb.to_owned(),
            help,
        }
    });

    PanelModel {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        page: s.page,
        tab: s.tab,
        open_account: s.open.clone(),
        unused_open: s.unused_open,
        hidden_shown: s.hidden_shown,
        has_status,
        stopped: s.stopped(),
        from_disk_image: s.from_disk_image,
        waiting,
        verdict,
        in_use,
        recent,
        unused,
        hidden,
        unused_fold_label,
        hidden_fold_label,
        connect_offer,
        footer_note: build_footer_note(s),
        settings: build_settings(s),
    }
}

fn build_account_row(s: &State, w: &Words, a: &Account, headlined: bool) -> AccountRowModel {
    let note = w.opened(a, headlined);
    let openable = !a.limits.is_empty() || note.is_some();
    let open = openable && s.open.as_deref() == Some(a.id.as_str());
    let lines = open && !a.limits.is_empty();
    let labelled = words::twinned(a, s.all());
    let called = words::called(a, labelled);
    let status = w.status(a);
    let (figure, figure_then) = words::row_figure(a);
    let then_text = figure_then
        .as_deref()
        .map(|t| format!(" {t}"))
        .unwrap_or_default();
    let left = if a.deciding_limit().is_some() {
        " left"
    } else {
        ""
    };
    let figure_spoken = format!("{figure}{then_text}{left}");
    let header_spoken = if lines {
        called.clone()
    } else {
        format!("{called}, {status}, {figure_spoken}")
    };

    let limits = a.limits.iter().map(|l| build_limit_line(w, l)).collect();
    let sessions = s.used_most(a);
    let used_most = build_used_most(sessions);
    let used_most_heading = a
        .deciding_limit()
        .map(|l| format!("Used most {}", words::of(l)));

    AccountRowModel {
        id: a.id.clone(),
        provider: a.provider.clone(),
        title: a.title.clone(),
        label: labelled.then(|| a.label.clone()).flatten(),
        called,
        standing: a.standing(),
        status,
        figure,
        figure_then,
        figure_spoken,
        header_spoken,
        openable,
        limits,
        note,
        used_most_heading,
        used_most,
    }
}

fn build_limit_line(w: &Words, limit: &Limit) -> LimitLineModel {
    LimitLineModel {
        key: limit.key.clone(),
        name: words::name(limit),
        ahead: w.ahead(limit),
        window: w.window(limit),
        left: words::left(limit),
        size: words::size(limit),
        spoken: format!("{} left", words::left_of(limit)),
        left_percent: limit.left_percent,
        standing: limit.standing(),
    }
}

fn build_used_most(sessions: &[UsedMost]) -> Vec<UsedMostModel> {
    let first = sessions.first().map(|u| u.agent.as_str());
    let agents = sessions.iter().any(|u| Some(u.agent.as_str()) != first);
    sessions
        .iter()
        .map(|used| UsedMostModel {
            session: used.session.clone(),
            title: used
                .title
                .clone()
                .unwrap_or_else(|| "Untitled session".to_owned()),
            detail: words::used(used, agents),
        })
        .collect()
}

fn build_footer_note(s: &State) -> Option<FooterNoteModel> {
    if s.status.is_some()
        && let Some(problem) = &s.problem
    {
        let (said, then) = match problem {
            Stopped::Failing(_) => ("Not up to date · trying again", " Trying again."),
            Stopped::Newer { replaced: true } => ("Not up to date · start Turnscope again", ""),
            Stopped::Newer { replaced: false } => ("Not up to date · update Turnscope", ""),
        };
        return Some(FooterNoteModel::Stale {
            text: said.to_owned(),
            help: problem.to_string(),
            spoken: format!("Not up to date: {problem}.{then}"),
        });
    }
    (s.reading() && !s.all().is_empty()).then(|| FooterNoteModel::Reading {
        text: "Reading your agents' history…".to_owned(),
        help: "Turnscope reads your agents' history once, when it first runs. Limits show up as soon as they are read.".to_owned(),
        spoken: "Reading your agents' history".to_owned(),
    })
}

fn build_settings(s: &State) -> SettingsModel {
    let installed = s.installed();
    let agents_empty = installed.is_empty().then(|| {
        let (headline, detail, _) = words::welcome(&[], &s.agent_names(), false);
        EmptyMessage { headline, detail }
    });
    let agents_title = if s.from_disk_image {
        "Move Turnscope to Applications to connect agents"
    } else {
        "Let these agents check their limits"
    }
    .to_owned();
    let agents = installed
        .iter()
        .map(|agent| build_agent_row(s, agent))
        .collect();

    let accounts_empty = s.all().is_empty().then(|| {
        let (headline, detail, _) = words::welcome(&installed, &s.agent_names(), s.reading());
        EmptyMessage { headline, detail }
    });
    let enabled = !s.stopped();
    let account_switch = |a: &Account| AccountSwitchModel {
        id: a.id.clone(),
        provider: a.provider.clone(),
        title: a.title.clone(),
        label: a.label.clone(),
        via: words::via(a),
        called: words::called(a, true),
        shown: !a.hidden,
        enabled,
    };
    let subscriptions = s
        .all()
        .iter()
        .filter(|a| a.kind != turnscope::providers::Kind::ApiKey)
        .map(account_switch)
        .collect();
    let api_keys = s
        .all()
        .iter()
        .filter(|a| a.kind == turnscope::providers::Kind::ApiKey)
        .map(account_switch)
        .collect();

    let notify = s.status.as_ref().map(|status| status.settings.notify);
    SettingsModel {
        menu_shows: s.preferences.menu_shows,
        notify,
        can_edit_notify: notify.is_some() && !s.stopped(),
        agents_empty,
        agents_title,
        agents,
        accounts_empty,
        subscriptions,
        api_keys,
    }
}

fn build_agent_row(s: &State, agent: &Agent) -> AgentRowModel {
    let changing = s.changing_agents.get(&agent.id).copied();
    let note =
        s.agent_errors.get(&agent.id).cloned().or_else(|| {
            words::connection(agent).filter(|_| agent.connection != Connection::Available)
        });
    AgentRowModel {
        id: agent.id.clone(),
        name: agent.name.clone(),
        note,
        changing: changing.map(|to| {
            if to == Connection::Connected {
                "Connecting…".to_owned()
            } else {
                "Disconnecting…".to_owned()
            }
        }),
        connected: changing.unwrap_or(agent.connection) == Connection::Connected,
        enabled: changing.is_none() && !s.from_disk_image && !s.stopped(),
    }
}
