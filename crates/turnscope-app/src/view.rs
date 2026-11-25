//! What the macOS shell draws, built from `State`.

use serde::Serialize;
use turnscope::alerts::Notify;
use turnscope::engine::{Stopped, UsedMost};
use turnscope::status::{Account, Agent, Connection, Limit};

use crate::standing::{AccountStanding, LimitStanding, Standing};
use crate::state::{Effect, MenuShows, State};
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
    pub has_status: bool,
    pub stopped: bool,
    pub can_refresh: bool,
    pub waiting: Option<WaitingModel>,
    pub verdict: Option<VerdictModel>,
    pub in_use: Vec<AccountRowModel>,
    pub recent: Vec<AccountRowModel>,
    pub unused: Vec<AccountRowModel>,
    pub hidden: Vec<AccountModel>,
    pub unused_fold_label: Option<String>,
    pub hidden_fold_label: Option<String>,
    pub connect_offer: Option<ConnectOfferModel>,
    pub footer_note: Option<FooterNoteModel>,
    pub settings: SettingsModel,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitingModel {
    /// Said beside a spinner while there's no problem.
    pub reading: String,
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
    pub collapsed_spoken: String,
    pub openable: bool,
    pub limits: Vec<LimitLineModel>,
    pub note: Option<String>,
    pub used_most_heading: Option<String>,
    pub used_most: Vec<UsedMostModel>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
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
    pub time_left_fraction: Option<f64>,
    pub standing: Standing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsedMostModel {
    pub id: String,
    pub session_id: String,
    pub title: String,
    pub detail: String,
    pub project_path: Option<String>,
}

/// An account by name, as Settings and the hidden fold list it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountModel {
    pub id: String,
    pub provider: String,
    pub title: String,
    pub label: Option<String>,
    pub via: Option<String>,
    pub called: String,
    pub shown: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectOfferModel {
    pub id: String,
    pub name: String,
    pub note: Option<String>,
    pub configured: Option<String>,
    pub changing: bool,
    pub verb: String,
    pub help: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FooterNoteModel {
    pub text: String,
    pub help: Option<String>,
    pub spoken: String,
    /// Whether a spinner goes before it.
    pub busy: bool,
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
    pub configured: Option<String>,
    pub changing: Option<String>,
    pub connected: bool,
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
    pub subscriptions: Vec<AccountModel>,
    pub api_keys: Vec<AccountModel>,
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
        spoken: words.spoken_item(&shown, s.all(), out_of_date),
        out_of_date,
    }
}

pub fn build_panel(s: &State) -> PanelModel {
    let has_status = s.status.is_some();
    let w = s.words();
    let in_use_accounts = s.in_use();
    let headlined = in_use_accounts.first().copied();

    let waiting = (!has_status).then(|| WaitingModel {
        reading: words::READING_TEXT.to_owned(),
        problem: s.problem.as_ref().map(ToString::to_string),
        retries: matches!(s.problem, Some(Stopped::Failing(_))),
    });

    let verdict = has_status.then(|| {
        let (headline, detail) = s.verdict(&w);
        VerdictModel {
            headline,
            detail,
            standing: headlined.map_or(Standing::Lasts, AccountStanding::standing),
        }
    });

    let rows = |accounts: Vec<&Account>| -> Vec<AccountRowModel> {
        accounts
            .into_iter()
            .map(|a| build_account_row(s, &w, a, headlined.is_some_and(|h| h.id == a.id)))
            .collect()
    };
    let in_use = rows(in_use_accounts);
    let recent = rows(s.recent());
    let unused = rows(s.unused());
    let hidden: Vec<AccountModel> = s.hidden().into_iter().map(account).collect();

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
        let row = build_agent_row(s, agent);
        let (verb, help) = words::connect_action(agent);
        ConnectOfferModel {
            id: row.id,
            name: row.name,
            // Settings leaves out that it isn't connected, beside its switch.
            note: row.note.or_else(|| words::connection(agent)),
            configured: row.configured,
            changing: row.changing.is_some(),
            verb: verb.to_owned(),
            help,
        }
    });

    PanelModel {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        has_status,
        stopped: s.stopped(),
        can_refresh: s.can_refresh(),
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
    let labelled = words::twinned(a, s.all());
    let called = words::called(a, labelled);
    let status = w.status(a);
    let (figure, figure_then) = words::row_figure(a);
    let figure_spoken = words::row_figure_spoken(a);
    let collapsed_spoken = if figure_spoken.is_empty() {
        format!("{called}, {status}")
    } else {
        format!("{called}, {status}, {figure_spoken}")
    };

    let limits = a.limits.iter().map(|l| build_limit_line(w, l)).collect();
    let sessions = s.used_most(a);
    let used_most = build_used_most(sessions);
    let used_most_heading = a.deciding_limit().map(words::used_most_heading);

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
        collapsed_spoken,
        openable,
        limits,
        note,
        used_most_heading,
        used_most,
    }
}

fn build_limit_line(w: &Words, limit: &Limit) -> LimitLineModel {
    let time_left_fraction = limit
        .started()
        .zip(limit.resets())
        .filter(|(start, reset)| reset > start)
        .map(|(start, reset)| ((reset - w.now) as f64 / (reset - start) as f64).clamp(0.0, 1.0))
        .filter(|fraction| (0.02..=0.98).contains(fraction));
    LimitLineModel {
        key: limit.key.clone(),
        name: words::name(limit),
        ahead: w.ahead(limit),
        window: w.window(limit),
        left: words::left(limit),
        size: words::size(limit),
        spoken: format!("{} left", words::left_of(limit)),
        left_percent: limit.left_percent,
        time_left_fraction,
        standing: limit.standing(),
    }
}

fn build_used_most(sessions: &[UsedMost]) -> Vec<UsedMostModel> {
    let first = sessions.first().map(|u| u.agent.as_str());
    let agents = sessions.iter().any(|u| Some(u.agent.as_str()) != first);
    sessions
        .iter()
        .map(|used| {
            // Strip Turnscope's `<agent>:` key prefix so copying yields the
            // agent's native session id (usable with `claude --resume`, etc.).
            let session_id = used
                .session
                .split_once(':')
                .map_or(used.session.as_str(), |(_, native)| native)
                .to_owned();
            UsedMostModel {
                id: used.session.clone(),
                session_id,
                title: used
                    .title
                    .clone()
                    .unwrap_or_else(|| words::UNTITLED_SESSION.to_owned()),
                detail: words::used(used, agents),
                project_path: used.project_path.clone(),
            }
        })
        .collect()
}

fn build_footer_note(s: &State) -> Option<FooterNoteModel> {
    if s.status.is_some()
        && let Some(problem) = &s.problem
    {
        let (text, help, spoken) = words::stale_footer(problem);
        return Some(FooterNoteModel {
            text,
            help: Some(help),
            spoken,
            busy: false,
        });
    }
    if s.reading() && !s.all().is_empty() {
        return Some(FooterNoteModel {
            text: words::READING_TEXT.to_owned(),
            help: Some(words::READING_HELP.to_owned()),
            spoken: words::READING_SPOKEN.to_owned(),
            busy: true,
        });
    }
    s.refreshing.then(|| FooterNoteModel {
        text: words::REFRESHING_TEXT.to_owned(),
        help: None,
        spoken: words::REFRESHING_SPOKEN.to_owned(),
        busy: true,
    })
}

fn build_settings(s: &State) -> SettingsModel {
    let installed = s.installed();
    let agents_empty = installed.is_empty().then(|| {
        let (headline, detail) = words::welcome(&[], &s.agent_names(), false);
        EmptyMessage { headline, detail }
    });
    let agents_title = words::agents_title(s.from_disk_image).to_owned();
    let agents = installed
        .iter()
        .map(|agent| build_agent_row(s, agent))
        .collect();

    let accounts_empty = s.all().is_empty().then(|| {
        let (headline, detail) = words::welcome(&installed, &s.agent_names(), s.reading());
        EmptyMessage { headline, detail }
    });
    let (api_keys, subscriptions) = s
        .all()
        .iter()
        .partition::<Vec<_>, _>(|a| a.kind == turnscope::providers::Kind::ApiKey);

    let notify = s.status.as_ref().map(|status| status.settings.notify);
    SettingsModel {
        menu_shows: s.preferences.menu_shows,
        notify,
        can_edit_notify: notify.is_some() && !s.stopped(),
        agents_empty,
        agents_title,
        agents,
        accounts_empty,
        subscriptions: subscriptions.into_iter().map(account).collect(),
        api_keys: api_keys.into_iter().map(account).collect(),
    }
}

fn account(a: &Account) -> AccountModel {
    AccountModel {
        id: a.id.clone(),
        provider: a.provider.clone(),
        title: a.title.clone(),
        label: a.label.clone(),
        via: words::via(a),
        called: words::called(a, true),
        shown: !a.hidden,
    }
}

fn build_agent_row(s: &State, agent: &Agent) -> AgentRowModel {
    let changing = s.changing_agents.get(&agent.id).copied();
    let note = s
        .agent_errors
        .get(&agent.id)
        .map(|(_, error)| error.clone())
        .or_else(|| words::connection(agent).filter(|_| agent.connection != Connection::Available));
    AgentRowModel {
        id: agent.id.clone(),
        name: agent.name.clone(),
        note,
        configured: agent.configured.clone(),
        changing: changing.map(|to| words::agent_changing(to).to_owned()),
        connected: changing.unwrap_or(agent.connection) == Connection::Connected,
        enabled: changing.is_none() && !s.from_disk_image && !s.stopped(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::example;
    use turnscope::engine::Shown;
    use turnscope::time::MINUTE;

    #[test]
    fn time_left_fraction_is_set_only_for_fixed_active_windows() {
        let state = State::fixed(example());
        let panel = build_panel(&state);
        // Claude Max 5 hours: 3h left of 5h -> 0.60.
        let five = &panel.in_use[0].limits[0];
        assert!((five.time_left_fraction.unwrap() - 0.6).abs() < 1e-6);
        // Claude Max Weekly: 50h left of 168h -> ~0.2976.
        let week = &panel.in_use[0].limits[1];
        assert!((week.time_left_fraction.unwrap() - (50.0 / 168.0)).abs() < 1e-3);
        // ChatGPT Pro: primary reset since read, secondary rolling -> neither has a tick.
        assert_eq!(panel.recent[0].limits[0].time_left_fraction, None);
        assert_eq!(panel.recent[0].limits[1].time_left_fraction, None);
        // Nor 5 minutes into 5 hours, where the tick would sit on the bar's end.
        let mut early = example();
        early.status.at = early.status.accounts[0].limits[0].started().unwrap() + 5 * MINUTE;
        let panel = build_panel(&State::fixed(early));
        assert_eq!(panel.in_use[0].limits[0].time_left_fraction, None);
    }

    #[test]
    fn the_fold_says_what_it_holds_and_folds_hidden_accounts_only_beside_others() {
        let mut shown = example();
        let folds = |shown: &Shown| {
            let panel = build_panel(&State::fixed(shown.clone()));
            (panel.unused_fold_label, panel.hidden_fold_label)
        };
        assert_eq!(
            folds(&shown),
            (Some("2 more accounts".into()), Some("1 hidden".into()))
        );
        for account in &mut shown.status.accounts {
            account.hidden |= !account.in_use && !account.recent;
        }
        assert_eq!(folds(&shown), (Some("3 hidden".into()), None));
    }

    #[test]
    fn a_row_shows_its_label_only_beside_another_account_with_its_title() {
        let mut shown = example();
        let label = |shown: &Shown| {
            let panel = build_panel(&State::fixed(shown.clone()));
            panel.in_use[0].label.clone()
        };
        assert_eq!(label(&shown), None);
        shown.status.accounts[1].title = "Claude Max".into();
        assert_eq!(label(&shown), Some("me@example.com".into()));
    }

    #[test]
    fn the_footer_says_why_the_status_is_old_before_what_is_read() {
        let mut state = State::fixed(example());
        let note = |state: &State| build_panel(state).footer_note;
        state.refreshing = true;
        assert_eq!(
            note(&state),
            Some(FooterNoteModel {
                text: "Refreshing limits…".into(),
                help: None,
                spoken: "Refreshing limits".into(),
                busy: true,
            })
        );
        state.status.as_mut().unwrap().history_read = false;
        assert_eq!(
            note(&state),
            Some(FooterNoteModel {
                text: "Reading your agents' history…".into(),
                help: Some(
                    "Turnscope reads your agents' history once, when it first runs. \
                     Limits show up as soon as they are read."
                        .into()
                ),
                spoken: "Reading your agents' history".into(),
                busy: true,
            })
        );
        state.problem = Some(Stopped::Failing("Offline".into()));
        assert_eq!(
            note(&state),
            Some(FooterNoteModel {
                text: "Not up to date · trying again".into(),
                help: Some("Offline".into()),
                spoken: "Not up to date: Offline. Trying again.".into(),
                busy: false,
            })
        );
    }

    #[test]
    fn voiceover_reads_a_rows_figure_as_it_shows_and_why_it_has_none_once() {
        let mut shown = example();
        let key = shown
            .status
            .accounts
            .iter()
            .position(|a| a.id == "anthropic:api")
            .unwrap();
        shown.status.accounts[key].state = turnscope::status::State::Unread {
            why: turnscope::status::Stale::SignIn,
        };
        let spoken = |shown: &turnscope::engine::Shown, title: &str| {
            let panel = build_panel(&State::fixed(shown.clone()));
            let row = panel.unused.into_iter().find(|r| r.title == title).unwrap();
            (row.figure, row.collapsed_spoken)
        };
        assert_eq!(
            spoken(&shown, "OpenCode Go"),
            ("—".into(), "OpenCode Go, Sign in again".into())
        );
        assert_eq!(
            spoken(&shown, "Anthropic API key"),
            (
                "$0".into(),
                "Anthropic API key, Sign in again, $0 this month".into()
            )
        );
        shown.status.accounts[key].spend.as_mut().unwrap().partial = true;
        assert_eq!(
            spoken(&shown, "Anthropic API key"),
            ("—".into(), "Anthropic API key, Sign in again".into())
        );
    }

    #[test]
    fn used_most_strips_agent_prefix_from_copied_session_id() {
        let built = build_used_most(&[UsedMost {
            session: "claude-code:0f6e-1234".into(),
            project: Some("turnscope".into()),
            project_path: Some("/tmp/turnscope".into()),
            title: None,
            agent: "claude-code".into(),
            share_percent: Some(40.0),
        }]);
        assert_eq!(built[0].id, "claude-code:0f6e-1234");
        assert_eq!(built[0].session_id, "0f6e-1234");
        assert_eq!(built[0].title, words::UNTITLED_SESSION);
    }
}
