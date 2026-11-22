//! The panel: its frame, the pages it slides between, and its first page (docs/app.md#the-panel).

use rune::{
    Color, Cx, Edge, El, Key, KeyEvent, PopupItem, PopupMenu, Transition, Tx, col, el, row, spacer,
    text,
};

use crate::settings;
use crate::standing::*;
use crate::state::{Page, State};
use crate::style::*;
use crate::symbols::Weight;
use crate::words::{self, Words};
use turnscope::engine::UsedMost;

pub const FRAME: &str = "frame";

pub fn view(s: &State, cx: &mut Cx<State>) -> El {
    let ink = Ink::new(
        cx.system_dark(),
        cx.system_increased_contrast(),
        cx.accent(),
    );
    cx.foreground(ink.primary);
    // Every button is a Tab stop and Return presses it, as on a web page: Rune's default makes
    // a button a Tab stop only if it says so.
    cx.web_focus();
    let pages = pages(s, &ink);
    panel(pages, s.live, &ink)
        .motion(SPRING)
        .hover_motion_within(HOVER, HOVER)
}

fn panel(pages: El, live: bool, ink: &Ink) -> El {
    if live {
        // The panel hangs in a see-through window that runs to the screen's bottom: a window
        // that followed its height resized each spring frame, which came 16 to 24 ms apart on
        // a 10 ms display (measured 2026-11-18). macOS gives that window every click in its
        // frame (`windowNumberAtPoint` named it 700 points below the panel), so a click off
        // the panel closes it, as a click outside a menu does.
        return col()
            .w_full()
            .h_full()
            .px(GUTTER)
            .on_background_press_tx(|_: &mut State, tx: &mut Tx| tx.close_window())
            .child(
                col()
                    .w(PANEL_WIDTH)
                    .system_material(rune::SystemMaterial::Popover)
                    .radius(14.0)
                    .continuous_corners()
                    .border(0.5, ink.quaternary)
                    // SwiftUI's `shadow(radius: 18, y: 8)`, black at 28%, on the frame: the
                    // pages' clip would cut it off.
                    .shadow(8.0, 36.0, Color::BLACK.alpha(0.28))
                    .on_background_press(|_: &mut State| {})
                    .child(pages.clip_rounded(14.0).continuous_corners()),
            );
    }
    col().child(
        col().key(FRAME).padding(1.0).child(
            pages
                .w(PANEL_WIDTH)
                .fill(ink.bg)
                .radius(14.0)
                .continuous_corners()
                .border(0.5, ink.quaternary)
                .clip_rounded(14.0),
        ),
    )
}

fn pages(s: &State, ink: &Ink) -> El {
    let (enter, leave) = match s.page {
        Page::Settings => (Edge::Trailing, Edge::Leading),
        Page::Main => (Edge::Leading, Edge::Trailing),
    };
    let slide = |edge| Transition::move_edge(edge).combined(Transition::opacity());
    let page = match s.page {
        Page::Main => first_page(s, ink).key("page:main"),
        Page::Settings => settings::page(s, ink).key("page:settings"),
    };
    col()
        .key("pages")
        .w_full()
        .child_exit(slide(leave))
        .child(page.w_full().transition(slide(enter)))
}

pub fn on_key(s: &mut State, event: &KeyEvent, tx: &mut Tx) -> bool {
    if event.key == Key::Escape {
        match s.page {
            Page::Main => tx.close_window(),
            Page::Settings => s.page = Page::Main,
        }
        return true;
    }
    if event.is_command(',') {
        s.page = Page::Settings;
        return true;
    }
    if event.is_command('q') {
        tx.quit();
        return true;
    }
    false
}

fn first_page(s: &State, ink: &Ink) -> El {
    let mut page = col().w_full();
    if s.status.is_none() {
        page = page.child(waiting(s, ink));
    } else {
        let w = s.words();
        page = page
            .child(verdict(s, &w, ink).px(20.0).pt(18.0).pb(14.0))
            .child(accounts(s, &w, ink).px(8.0));
    }
    page.child(footer(s, ink).padding(8.0))
}

pub fn waiting(s: &State, ink: &Ink) -> El {
    let said = match &s.problem {
        Some(problem) => {
            let mut said = col()
                .key("failed")
                .gap(8.0)
                .items_center()
                .child(
                    text("Turnscope can't read its data")
                        .font(STRONG)
                        .color(ink.primary),
                )
                .child(
                    text(&problem.why)
                        .font(LINE)
                        .color(ink.secondary)
                        .wrap()
                        .align(rune::Align::Center)
                        .selectable(true),
                );
            if problem.trying_again {
                said = said.child(
                    text("It tries again on its own.")
                        .font(SMALL)
                        .color(ink.tertiary),
                );
            }
            said
        }
        None => col()
            .key("reading")
            .gap(8.0)
            .items_center()
            .child(spinner(ink))
            .child(
                text("Reading your agents' history…")
                    .font(LINE)
                    .color(ink.secondary),
            ),
    };
    col()
        .key("waiting")
        .w_full()
        .min_h(140.0)
        .padding(24.0)
        .center()
        .child(said)
}

fn verdict(s: &State, w: &Words, ink: &Ink) -> El {
    let (headline, detail, standing) = s.verdict(w);
    let mut verdict = col().key("verdict").gap(4.0).w_full().child(
        changes(&headline, text(&headline))
            .font(HEADLINE)
            .color(ink.standing(standing))
            .wrap()
            .w_full()
            .role("heading")
            .selectable(true),
    );
    if !detail.is_empty() {
        verdict = verdict.child(
            text(detail)
                .key("detail")
                .font(NAME)
                .color(ink.secondary)
                .wrap()
                .w_full()
                .selectable(true),
        );
    }
    verdict
}

fn accounts(s: &State, w: &Words, ink: &Ink) -> El {
    let in_use = s.in_use();
    let recent = s.recent();
    let unused = s.unused();
    let hidden = s.hidden();
    // Up and Down move between the rows, and the folds and buttons among them. Tab comes to
    // the list once.
    let mut list = col().key("rows").w_full().role("list").focus_group();
    let mut in_use_list = col().key("in-use").gap(row_size::SPACING).w_full();
    for (i, a) in in_use.iter().enumerate() {
        in_use_list = in_use_list.child(account_row(s, w, a, i == 0, ink));
    }
    list = list.child(in_use_list);
    if !recent.is_empty() {
        let mut recent_list = col()
            .key("recent")
            .gap(row_size::SPACING)
            .w_full()
            .pt(if in_use.is_empty() {
                0.0
            } else {
                row_size::GROUPS
            })
            .child(section_title("Used this week", ink));
        for a in &recent {
            recent_list = recent_list.child(account_row(s, w, a, false, ink));
        }
        list = list.child(recent_list);
    }
    if !unused.is_empty() || !hidden.is_empty() {
        let beside = !in_use.is_empty() || !recent.is_empty();
        list =
            list.child(unused_accounts(s, w, &unused, &hidden, beside, ink).pt(row_size::SPACING));
    }
    let mut accounts = col().key("accounts").w_full().child(list);
    if let Some(offer) = connect_offer(s, ink) {
        accounts = accounts.child(offer.pt(row_size::GROUPS));
    }
    accounts
}

fn account_row(s: &State, w: &Words, a: &Account, headlined: bool, ink: &Ink) -> El {
    let note = w.opened(a, headlined);
    let openable = !a.limits.is_empty() || note.is_some();
    let open = openable && s.open.as_deref() == Some(a.id.as_str());
    let labelled = a.twinned(s.all());
    let called = words::called(a, labelled);
    // Open, its limit lines say what the line under its name and its figure would.
    let lines = open && !a.limits.is_empty();

    let mut name = col()
        .key("name")
        .gap(1.0)
        .shrink()
        .min_w(0.0)
        .child(account_title(a, labelled, ink.primary, ink.secondary));
    // What VoiceOver says for the row.
    let mut said = called.clone();
    let mut figure = None;
    if !lines {
        let status = w.status(a);
        let (shown, spoken) = row_figure(a, ink);
        said = format!("{called}, {status}, {spoken}");
        name = name.child(appears(
            text(status)
                .key("status")
                .font(SMALL)
                .color(ink.ink(a.standing(), ink.secondary))
                .truncate(),
        ));
        figure = Some(appears(shown));
    }
    let mut header = item_row("header")
        .child(account_logo(&a.provider, row_size::LOGO, ink.primary, ink))
        .child(name)
        .child(spacer().min_w(8.0))
        .child(hide_button(s, a, &called, ink));
    if let Some(figure) = figure {
        header = header.child(figure);
    }
    header = header.label(said);
    if openable {
        // A press shades only the header: on an open row, or one a click just closed, it showed
        // as a band darker than the rest, so there it's too faint to see. With no press overlay
        // at all, Rune stops following presses.
        let shade = ink.press(ink.row);
        let just_closed = s.closed.as_ref().is_some_and(|(id, _)| *id == a.id);
        let id = a.id.clone();
        header = header
            .role("button")
            .expanded(open)
            .on_press_tx(move |s: &mut State, tx: &mut Tx| toggle_open(s, &id, tx))
            .radius(row_size::RADIUS)
            .continuous_corners()
            .press(if open || just_closed {
                shade.alpha(0.0001)
            } else {
                shade
            });
    }

    let mut body = col().key(format!("row:{}", a.id)).w_full().child(header);
    if open {
        let grown = s.grow_due.is_none();
        body = body.child(appears(opened(w, a, note, s.used_most(a), grown, ink)));
    }

    let menu_id = a.id.clone();
    let mut body = body
        // Moving between the accounts in use and those used this week, it slides to its new
        // place. A flight takes the spring set on its own element, not the one it inherits.
        .matched(format!("account:{}", a.id))
        .motion(SPRING)
        // Not a button itself: the header is the row's button.
        .role("none")
        .radius(row_size::RADIUS)
        .continuous_corners()
        .fill(if open {
            ink.wash(ink.row)
        } else {
            Color::TRANSPARENT
        })
        // Its Hide button shows while it's hovered, or the keyboard is on it.
        .hover_group()
        .hover(if open {
            Color::TRANSPARENT
        } else {
            ink.wash(ink.row)
        })
        .on_secondary_press(move |s: &mut State, at, tx: &mut Tx| {
            let id = menu_id.clone();
            let menu = PopupMenu::new()
                .item(PopupItem::new("hide", format!("Hide {called}")).enabled(!s.stopped()));
            tx.popup_menu(menu, at, move |s: &mut State, picked, _| {
                if picked == Some("hide") {
                    crate::hide(s, &id, true);
                }
            });
        });
    // A click on what opened closes it too, unshaded, as one on the open header does.
    if open {
        let id = a.id.clone();
        body = body.on_press_tx(move |s: &mut State, tx: &mut Tx| toggle_open(s, &id, tx));
    }
    body
}

/// What an open row shows under its header: its limits' lines, `note`, and what used it most.
fn opened(
    w: &Words,
    a: &Account,
    note: Option<String>,
    used_most: &[UsedMost],
    grown: bool,
    ink: &Ink,
) -> El {
    let mut opened = col()
        .key("opened")
        .gap(10.0)
        .w_full()
        .pt(10.0 - row_size::PAD_Y)
        .pb(row_size::PAD_Y)
        .px(row_size::PAD_X);
    let last = a.limits.len().saturating_sub(1);
    for (i, limit) in a.limits.iter().enumerate() {
        let ends = note.is_none() && used_most.is_empty() && i == last;
        opened = opened.child(limit_line(w, limit, ends, grown, ink));
    }
    if let Some(note) = note {
        opened = opened.child(text(note).font(LINE).color(ink.secondary).wrap().w_full());
    }
    if !used_most.is_empty() {
        opened = opened.child(appears(used_most_list(a, used_most, ink)));
    }
    opened
}

/// The Hide button, which shows on hover. Sized to a name's line and always given its room, so
/// nothing moves when it shows.
fn hide_button(s: &State, a: &Account, called: &str, ink: &Ink) -> El {
    let can_hide = !s.stopped();
    let hide = icon_button(
        &format!("hide:{}", a.id),
        "eye.slash",
        22.0,
        16.0,
        can_hide,
        ink,
    )
    .group_opacity(0.0, 1.0)
    .label(format!("Hide {called}"))
    .help(format!("Hide {called} from the menu bar and the panel"));
    if !can_hide {
        return hide;
    }
    let id = a.id.clone();
    hide.on_press(move |s: &mut State| crate::hide(s, &id, true))
}

/// The figure on a row's right, and what VoiceOver says for it.
fn row_figure(a: &Account, ink: &Ink) -> (El, String) {
    let (figure, then) = words::row_figure(a);
    let then = then.map(|t| format!(" {t}")).unwrap_or_default();
    let left = if a.deciding_limit().is_some() {
        " left"
    } else {
        ""
    };
    let spoken = format!("{figure}{then}{left}");
    let shown = el().key("figure").child(
        changes(
            &format!("{figure}{then}"),
            rich_text(&[
                (&figure, ink.ink(a.standing(), ink.primary), None),
                (&then, ink.secondary, Some(400.0)),
            ])
            .into_el(),
        )
        .font(FIGURE)
        .label(spoken.clone()),
    );
    (shown, spoken)
}

fn toggle_open(s: &mut State, id: &str, tx: &mut Tx) {
    // The row's fill lands at once: faded, the open part came in lighter than the part
    // clicked, 252 against 242 of 255, for 0.2 s (measured 2026-11-18).
    tx.color_motion(rune::Spring::NONE);
    if s.open.as_deref() == Some(id) {
        let due = tx.clock() + 0.25;
        s.closed = Some((id.to_owned(), due));
        tx.wake_at(due);
        s.close_open();
        return;
    }
    s.open = Some(id.to_owned());
    crate::ask_used(s, id);
    // The bars grow once the lines have faded in: growing at once, they were two thirds grown
    // before the lines showed.
    let due = tx.clock() + f64::from(APPEAR_AFTER);
    s.grow_due = Some(due);
    tx.wake_at(due);
}

/// Label grows into leftover space so long emails truncate before the title does.
pub fn account_title(a: &Account, labelled: bool, color: Color, label: Color) -> El {
    // Bottom-aligned sits on the title baseline in 2x snapshots; top-aligned sat 2pt high (Nov 2026).
    let mut title = row()
        .key("title")
        .gap(6.0)
        .items_end()
        .shrink()
        .min_w(0.0)
        .child(
            text(&a.title)
                .font(NAME)
                .color(color)
                .truncate()
                .shrink()
                .min_w(0.0)
                .help_when_truncated(&a.title),
        );
    if labelled && let Some(said) = &a.label {
        title = title.child(
            text(said)
                .font(SMALL)
                .color(label)
                .truncate_middle()
                .grow(1.0),
        );
    }
    title
}

pub fn account_name(a: &Account, title: Color, subtitle: Color, ink: &Ink) -> El {
    let mut name = col()
        .key("name")
        .gap(1.0)
        .shrink()
        .min_w(0.0)
        .child(account_title(a, true, title, ink.tertiary));
    if let Some(via) = words::via(a) {
        name = name.child(text(via).font(SMALL).color(subtitle).truncate());
    }
    name
}

const LIMIT_LINE: f32 = PANEL_WIDTH - 2.0 * 8.0 - 2.0 * row_size::PAD_X;

fn limit_line(w: &Words, limit: &Limit, ends: bool, grown: bool, ink: &Ink) -> El {
    let standing = limit.standing();
    let name = words::name(limit);
    // When it runs out, in amber; then when it resets, or when it's back if it's used up, in
    // red. Only the times are colored, not the dots.
    let ahead = w.ahead(limit).map(|said| (said, ink.standing(standing)));
    let window = w.window(limit).map(|said| {
        (
            said,
            if standing == Standing::UsedUp {
                ink.standing(standing)
            } else {
                ink.secondary
            },
        )
    });
    let said = |parts: &[&(String, Color)]| {
        let mut spans: Vec<(&str, Color, Option<f32>)> = Vec::with_capacity(1 + 2 * parts.len());
        spans.push((&name, ink.secondary, Some(510.0)));
        for (part, color) in parts {
            spans.push((" · ", ink.secondary, None));
            spans.push((part.as_str(), *color, None));
        }
        rich_text(&spans)
    };
    let left = words::left(limit);
    let size = words::size(limit)
        .map(|s| format!(" {s}"))
        .unwrap_or_default();
    let figure_rich = rich_text(&[
        (&left, ink.ink(standing, ink.primary), Some(510.0)),
        (&size, ink.secondary, None),
    ]);
    let room = LIMIT_LINE - figure_rich.width(SMALL_DIGITS) - 24.0;
    let figure = changes(&format!("{left}{size}"), figure_rich.into_el())
        .font(SMALL_DIGITS)
        .label(format!("{} left", words::left_of(limit)));
    // If both times don't fit beside the figure, only when it runs out shows, or else the
    // other time, cut short if it must be. The 24 points are the two 8-point gaps either side
    // of the spacer, and the spacer's own 8.
    let both: Vec<&(String, Color)> = ahead.iter().chain(window.iter()).collect();
    let full = said(&both);
    let times = if full.width(SMALL) <= room {
        full.into_el().font(SMALL)
    } else {
        said(&both[..both.len().min(1)])
            .into_el()
            .font(SMALL)
            .truncate()
            .shrink()
            .min_w(0.0)
    };
    let level = (limit.left_percent as f32 / 100.0).clamp(0.0, 1.0);
    let mut fill = el()
        .key("level")
        .h(5.0)
        .w_frac(if grown { level } else { 0.0 })
        .radius(2.5)
        .fill(ink.ink(standing, ink.secondary));
    if level > 0.0 {
        fill = fill.min_w(5.0);
    }
    col()
        .key(format!("limit:{}", limit.key))
        .gap(5.0)
        .w_full()
        .pb(if ends { row_size::UNDER_BAR } else { 0.0 })
        .child(
            row()
                .gap(8.0)
                .w_full()
                .child(times)
                .child(spacer().min_w(8.0))
                .child(figure),
        )
        .child(
            el().key("bar")
                .h(5.0)
                .w_full()
                .radius(2.5)
                .fill(ink.quaternary)
                .child(fill)
                .a11y_hidden(),
        )
}

fn used_most_list(a: &Account, sessions: &[UsedMost], ink: &Ink) -> El {
    let first = sessions.first().map(|u| u.agent.as_str());
    let agents = sessions.iter().any(|u| Some(u.agent.as_str()) != first);
    let of = a.deciding_limit().map(words::of).unwrap_or_default();
    let mut list = col().key("used-most").gap(8.0).w_full().child(
        text(format!("Used most {of}"))
            .font(HEADING)
            .color(ink.secondary),
    );
    for used in sessions {
        list = list.child(
            col()
                .key(format!("session:{}", used.session))
                .gap(1.0)
                .w_full()
                .child(
                    text(used.title.as_deref().unwrap_or("Untitled session"))
                        .font(LINE)
                        .color(ink.primary)
                        .truncate(),
                )
                .child(
                    text(words::used(used, agents))
                        .font(SMALL_DIGITS)
                        .color(ink.secondary),
                ),
        );
    }
    list
}

fn unused_accounts(
    s: &State,
    w: &Words,
    unused: &[&Account],
    hidden: &[&Account],
    beside: bool,
    ink: &Ink,
) -> El {
    let label = if unused.is_empty() {
        words::hidden(hidden.len())
    } else {
        words::unused(unused.len(), beside)
    };
    let mut list = col()
        .key("unused")
        .gap(row_size::SPACING)
        .w_full()
        .child(fold(
            "fold:unused",
            &label,
            s.unused_open,
            |s| {
                s.unused_open = !s.unused_open;
                if !s.unused_open {
                    if s.open
                        .as_ref()
                        .is_some_and(|open| s.unused().iter().any(|a| &a.id == open))
                    {
                        s.close_open();
                    }
                    s.hidden_shown = false;
                }
            },
            ink,
        ));
    if s.unused_open {
        let mut inner = col().key("unused-rows").gap(row_size::SPACING).w_full();
        for a in unused {
            inner = inner.child(account_row(s, w, a, false, ink));
        }
        if !hidden.is_empty() && !unused.is_empty() {
            inner = inner.child(fold(
                "fold:hidden",
                &words::hidden(hidden.len()),
                s.hidden_shown,
                |s| s.hidden_shown = !s.hidden_shown,
                ink,
            ));
        }
        if !hidden.is_empty() && (s.hidden_shown || unused.is_empty()) {
            let mut rows = col().key("hidden-rows").gap(row_size::SPACING).w_full();
            for a in hidden {
                rows = rows.child(hidden_row(s, a, ink));
            }
            inner = inner.child(appears(rows));
        }
        list = list.child(appears(inner));
    }
    list
}

fn fold(key: &str, label: &str, open: bool, toggle: fn(&mut State), ink: &Ink) -> El {
    let chevron = sf("chevron.right", 10.0, Weight::Semibold, ink, ink.tertiary).rotate(if open {
        std::f32::consts::FRAC_PI_2
    } else {
        0.0
    });
    row()
        .key(key)
        .center_y()
        .gap(8.0)
        .w_full()
        .py(row_size::PAD_Y)
        .px(row_size::PAD_X)
        .radius(row_size::RADIUS)
        .continuous_corners()
        .hover(ink.wash(ink.row))
        .press(ink.press(ink.row))
        .role("button")
        .label(label)
        .expanded(open)
        .focusable(true)
        .child(text(label).font(LINE).color(ink.secondary))
        .child(spacer().min_w(6.0))
        .child(chevron)
        .on_press(toggle)
}

fn hidden_row(s: &State, a: &Account, ink: &Ink) -> El {
    let id = a.id.clone();
    let enabled = !s.stopped();
    let mut show = pill(&format!("show:{}", a.id), "Show", true, enabled, ink)
        .label(format!("Show {}", words::called(a, true)));
    if enabled {
        show = show.on_press(move |s: &mut State| crate::hide(s, &id, false));
    }
    item_row(format!("hidden:{}", a.id))
        .child(account_logo(&a.provider, row_size::LOGO, ink.tertiary, ink))
        .child(account_name(a, ink.secondary, ink.tertiary, ink))
        .child(spacer().min_w(8.0))
        .child(show)
}

pub fn agent_name(agent: &Agent, note: Option<String>, ink: &Ink) -> El {
    let mut name = col()
        .key("name")
        .gap(1.0)
        .shrink()
        .min_w(0.0)
        .child(text(&agent.name).font(NAME).color(ink.primary));
    if let Some(note) = note {
        name = name.child(
            text(note)
                .font(SMALL)
                .color(ink.secondary)
                .wrap()
                .selectable(true),
        );
    }
    name
}

fn connect_offer(s: &State, ink: &Ink) -> Option<El> {
    let agent = s.to_connect()?;
    let id = agent.id.clone();
    let note = s
        .agent_errors
        .get(&agent.id)
        .cloned()
        .or_else(|| words::connection(agent));
    let mut line = item_row(format!("connect:{}", agent.id))
        .child(agent_logo(&agent.id, row_size::LOGO, ink.primary, ink))
        .child(agent_name(agent, note, ink))
        .child(spacer().min_w(8.0));
    line = if s.changing_agents.contains_key(&agent.id) {
        line.child(spinner(ink).label(format!("Connecting {}", agent.name)))
    } else {
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
        // Rune's `disabled` doesn't suppress `on_press`, so attach only when enabled.
        let enabled = !s.stopped();
        let mut connect = pill("connect", verb, true, enabled, ink)
            .label(format!("{verb} {}", agent.name))
            .help(help);
        if enabled {
            let connect_id = id.clone();
            connect = connect.on_press(move |s: &mut State| crate::connect(s, &connect_id, true));
        }
        line.child(connect)
    };
    let dismiss = round_button("dismiss-connect", "xmark", ink)
        .help("Don't suggest this again")
        .label(format!("Don't suggest connecting {} again", agent.name))
        .on_press(move |s: &mut State| s.preferences.dismiss_connect(&id));
    Some(appears(line.child(dismiss)))
}

fn footer(s: &State, ink: &Ink) -> El {
    let mut footer = row().key("footer").gap(2.0).w_full().center_y();
    if s.status.is_some()
        && let Some(problem) = &s.problem
    {
        let said = if problem.trying_again {
            "Not up to date · trying again"
        } else if problem.why == turnscope::db::UPDATED_SINCE {
            // This copy was replaced while it ran, as an update by hand does.
            "Not up to date · start Turnscope again"
        } else {
            "Not up to date · update Turnscope"
        };
        // Cut short before the buttons: at full length beside "Update available" it hid Quit.
        footer = footer.child(appears(
            text(said)
                .key("stale")
                .font(SMALL)
                .color(ink.secondary)
                .truncate()
                .shrink()
                .min_w(0.0)
                .help(&problem.why)
                .label(format!(
                    "Not up to date: {}.{}",
                    problem.why,
                    if problem.trying_again {
                        " Trying again."
                    } else {
                        ""
                    }
                ))
                .pl(12.0),
        ));
    } else if s.reading() && !s.all().is_empty() {
        // Before any account is found, the headline says this instead.
        footer = footer.child(appears(
            row()
                .key("reading")
                .gap(6.0)
                .center_y()
                .shrink()
                .min_w(0.0)
                .pl(12.0)
                .child(spinner(ink))
                .child(
                    text("Reading your agents' history…")
                        .font(SMALL)
                        .color(ink.secondary)
                        .truncate()
                        .shrink()
                        .min_w(0.0),
                )
                .help("Turnscope reads your agents' history once, when it first runs. Limits show up as soon as they are read.")
                .label("Reading your agents' history"),
        ));
    }
    footer = footer.child(spacer());
    if s.update_available {
        footer = footer.child(appears(
            pill("update", "Update available", true, true, ink)
                .help("Shows what's new, and asks before installing it")
                .mr(4.0)
                .on_press_tx(|_: &mut State, tx: &mut Tx| {
                    tx.close_window();
                    crate::updater::check();
                }),
        ));
    }
    footer
        .child(
            icon_button("settings", "gearshape", 28.0, 28.0, true, ink)
                .label("Settings")
                .help("Settings")
                .help_keys(["⌘", ","])
                .on_press(|s: &mut State| s.page = Page::Settings),
        )
        .child(
            icon_button("quit", "power", 28.0, 28.0, true, ink)
                .label("Quit Turnscope")
                .help("Quit Turnscope")
                .help_keys(["⌘", "Q"])
                .on_press_tx(|_: &mut State, tx: &mut Tx| tx.quit()),
        )
}
