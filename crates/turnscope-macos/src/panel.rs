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
use crate::words::Words;

/// The key of the panel's frame as a snapshot draws it, which the image is cut to.
pub const FRAME: &str = "frame";

/// Whether the panel is drawn on the system's glass, as the menu bar app does, or on the
/// window's background, as a snapshot is.
pub static LIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn view(s: &State, cx: &mut Cx<State>) -> El {
    let ink = Ink::new(
        cx.system_dark(),
        cx.system_increased_contrast(),
        cx.accent(),
    );
    cx.foreground(ink.primary);
    // Every button is a Tab stop, and Return presses the one with focus, as on a web page.
    // Without it, Rune makes a button a Tab stop only if it says so.
    cx.web_focus();
    let pages = pages(s, &ink);
    // One spring and one hover fade for everything in the panel, which inherits them.
    panel(pages, &ink)
        .motion(SPRING)
        .hover_motion_within(HOVER, HOVER)
}

/// The panel's frame around `pages`, as the menu bar app draws it or as a snapshot does.
fn panel(pages: El, ink: &Ink) -> El {
    if LIVE.load(std::sync::atomic::Ordering::Relaxed) {
        // The panel hangs at the top of a see-through window that runs to the bottom of the
        // screen (`menu_bar::panel_place`), and grows and shrinks inside it, as the Swift
        // app's did, with its own corners, edge and shadow. A window that followed the panel's
        // height resized on every frame of the spring and dropped every second or third frame:
        // they came 16 to 24 ms apart where the display draws every 10 (measured 2026-11-18).
        //
        // A click beside or below the panel lands on that window, not on what's behind it:
        // macOS gives a window every click in its frame, drawn there or not
        // (`windowNumberAtPoint` named the panel's window 700 points below the panel, measured
        // 2026-11-18). The window kept the focus, so the panel stayed open. Rune lets clicks
        // through only its overlays, and takes the whole of this window as the panel. So a
        // click off the panel closes it, as a click outside a menu closes the menu, and goes
        // no further.
        //
        // The shadow is the frame's, outside the pages' clip, which would cut it off.
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
                    .border(0.5, ink.edge)
                    // SwiftUI's `shadow(radius: 18, y: 8)`, black at 28%.
                    .shadow(8.0, 36.0, Color::BLACK.alpha(0.28))
                    // A click on the panel, between its controls, stays on it.
                    .on_background_press(|_: &mut State| {})
                    .child(pages.clip_rounded(14.0).continuous_corners()),
            );
    }
    // The panel as `--snapshot` draws it: on the window's background, with its corners and its
    // edge, a point in from the image's edge.
    // The window's root fills it, so the frame is a child, as tall as the panel.
    col().child(
        col().key(FRAME).padding(1.0).child(
            pages
                .w(PANEL_WIDTH)
                .fill(ink.bg)
                .radius(14.0)
                .continuous_corners()
                .border(0.5, ink.edge)
                .clip_rounded(14.0),
        ),
    )
}

/// The page shown, sliding in from the side as it changes, or fading with Reduce Motion, which
/// Rune does for every move. Going deeper, the new page comes in from the right; going back,
/// from the left.
fn pages(s: &State, ink: &Ink) -> El {
    let (enter, leave) = if s.forward {
        (Edge::Trailing, Edge::Leading)
    } else {
        (Edge::Leading, Edge::Trailing)
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

/// The panel's keys: Escape closes it from its first page, and goes back from Settings. ⌘,
/// opens Settings and ⌘Q quits.
pub fn on_key(s: &mut State, event: &KeyEvent, tx: &mut Tx) -> bool {
    if event.key == Key::Escape {
        match s.page {
            Page::Main => tx.close_window(),
            Page::Settings => go(s, Page::Main),
        }
        return true;
    }
    if event.is_command(',') {
        go(s, Page::Settings);
        return true;
    }
    if event.is_command('q') {
        tx.quit();
        return true;
    }
    false
}

/// Go to `page`, sliding.
pub fn go(s: &mut State, page: Page) {
    if s.page == page {
        return;
    }
    s.forward = page > s.page;
    s.page = page;
}

// The first page

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
    // Its buttons line up with the rows' edges, as close to the panel's corner as the rows are
    // to its side.
    page.child(footer(s, ink).padding(8.0))
}

/// Shown while the first status is on its way, or to say why none can come.
pub fn waiting(s: &State, ink: &Ink) -> El {
    let said = match &s.failure {
        Some(failure) => {
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
                    text(failure)
                        .font(LINE)
                        .color(ink.secondary)
                        .wrap()
                        .align(rune::Align::Center)
                        .selectable(true),
                );
            if s.trying_again {
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

/// The one thing to know, in large type: whether you can keep going, or, before any account
/// is found, why none is. Under it is when it's back, or what's left of a limit that isn't
/// forecast.
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
    // Accounts used this week are always shown, under a heading that says what they are.
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
    if let Some(offer) = connect_offer(s, w, ink) {
        // Apart from the accounts, as a group of its own.
        accounts = accounts.child(offer.pt(row_size::GROUPS));
    }
    accounts
}

fn account_row(s: &State, w: &Words, a: &Account, headlined: bool, ink: &Ink) -> El {
    let note = w.opened(a, headlined);
    let openable = !a.limits.is_empty() || note.is_some();
    let open = openable && s.open.as_deref() == Some(a.id.as_str());
    // Whether its limits show as lines. They say everything the line under the name would.
    let lines = open && !a.limits.is_empty();
    let used_most = s.used_most(a);
    let used = open && !used_most.is_empty();
    let labelled = a.twinned(s.all());
    let called = w.called(a, labelled);
    // What VoiceOver says for the row: its name, then, while its limits aren't shown, its
    // line and figure.
    let mut said = called.clone();

    let mut name = col()
        .key("name")
        .gap(1.0)
        .shrink()
        .min_w(0.0)
        .child(account_title(a, labelled, ink.primary, ink.secondary));
    if !lines {
        let status = w.status(a);
        said = format!("{said}, {status}");
        name = name.child(appears(
            text(status)
                .key("status")
                .font(SMALL)
                .color(ink.ink(a.standing(), ink.secondary))
                .truncate(),
        ));
    }

    // Sized to a name's line, and always given its room, so nothing moves when it shows on
    // hover, or as the keyboard comes to it.
    let can_hide = !s.stopped();
    let mut hide = icon_button(
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
    if can_hide {
        let id = a.id.clone();
        hide = hide.on_press_tx(move |s: &mut State, _: &mut Tx| crate::hide(s, &id, true));
    }

    let mut header = row()
        .key("header")
        .center_y()
        .gap(row_size::GAP)
        .w_full()
        .py(row_size::PAD_Y)
        .px(row_size::PAD_X)
        .child(account_logo(&a.provider, row_size::LOGO, ink.primary, ink))
        .child(name)
        .child(spacer().min_w(8.0))
        .child(hide);
    // Once the limit lines show, they say what the figure does, so it fades as they open.
    if !lines {
        let (figure, then) = w.row_figure(a);
        let then = then.map(|t| format!(" {t}")).unwrap_or_default();
        let left = if a.deciding_limit().is_some() {
            " left"
        } else {
            ""
        };
        let spoken = format!("{figure}{then}{left}");
        said = format!("{said}, {spoken}");
        header = header.child(appears(
            el().key("figure").child(
                changes(
                    &format!("{figure}{then}"),
                    spans(&[
                        (&figure, ink.ink(a.standing(), ink.primary), None),
                        (&then, ink.secondary, Some(400.0)),
                    ]),
                )
                .font(FIGURE)
                .label(spoken),
            ),
        ));
    }
    // The header is the row's button for the keyboard and VoiceOver, which hear it as one
    // thing: its name, line and figure, and whether it's open. The whole row opens on a click.
    // A press darkens what it lands on, so the header carries the row's padding and corners: a
    // press on it darkens the whole of a closed row. On an open one it would darken only the
    // top, a band darker than the rest, so there it doesn't. The row opens and closes as the
    // press ends, which Rune fades out over 0.2 s, on the header alone: so the shade goes as
    // the row opens, and comes back only once a row closed by a click has closed. Otherwise
    // the part clicked showed darker than the rest for 0.2 s, 229 against 242 of 255.
    header = header.label(said);
    if openable {
        let id = a.id.clone();
        header = header
            .role("button")
            .expanded(open)
            .on_press_tx(move |s: &mut State, tx: &mut Tx| toggle_open(s, &id, tx));
        // Without a press overlay, Rune stops following presses on it, and the last one's shade
        // came back with the overlay: so it keeps one too faint to see.
        let just_closed = s.closed.as_ref().is_some_and(|(id, _)| *id == a.id);
        let shade = ink.press(ink.row);
        header =
            header
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
        // 10 points under the header's line, as between the lines in it.
        let mut opened = col()
            .key("opened")
            .gap(10.0)
            .w_full()
            .pt(10.0 - row_size::PAD_Y)
            .pb(row_size::PAD_Y)
            .px(row_size::PAD_X);
        if lines {
            let last_key = a.limits.last().map(|l| l.key.clone());
            for limit in &a.limits {
                let last = note.is_none() && !used && Some(&limit.key) == last_key.as_ref();
                opened = opened.child(limit_line(w, limit, last, s.grown, ink));
            }
        }
        if let Some(note) = note {
            opened = opened.child(text(note).font(LINE).color(ink.secondary).wrap().w_full());
        }
        // They can come after the row opened. If so, they wait for their space like the rest
        // of it did.
        if used {
            opened = opened.child(appears(used_most_list(w, a, used_most, ink)));
        }
        body = body.child(appears(opened));
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
    if openable {
        let id = a.id.clone();
        body = body
            .press(ink.press(ink.row))
            .on_press_tx(move |s: &mut State, tx: &mut Tx| toggle_open(s, &id, tx));
    }
    body
}

/// Opens the account `id` in place, closing any other, or closes it if it's open.
pub fn toggle_open(s: &mut State, id: &str, tx: &mut Tx) {
    // The row's fill lands at once, while its height springs. Faded in, the open part came
    // in lighter than the part clicked, 252 against 242 of 255, and matched 0.2 s later
    // (measured 2026-11-18).
    tx.color_motion(rune::Spring::NONE);
    s.open = if s.open.as_deref() == Some(id) {
        let due = tx.clock() + 0.25;
        s.closed = Some((id.to_owned(), due));
        tx.wake_at(due);
        None
    } else {
        Some(id.to_owned())
    };
    if s.open.is_some() {
        crate::ask_used(s, id);
        // Its bars grow to their level as its lines fade in, once their space has opened.
        // From 0.05 s, the spring had them two thirds grown before the lines began to show.
        s.grown = false;
        let due = tx.clock() + f64::from(APPEAR_AFTER);
        s.grow_due = Some(due);
        tx.wake_at(due);
    }
}

/// An account's title as every row shows it, in `color`. If `labelled`, its label is beside
/// it in `label`, to tell it apart from another account with the same name, and is cut short
/// before the title is: it grows from no width into the room the title leaves. Shrinking both,
/// as before, cut "Claude Max" to "Claud…" beside a long email address.
pub fn account_title(a: &Account, labelled: bool, color: Color, label: Color) -> El {
    // Aligned by their bottoms, the label sits on the title's baseline, to the pixel in a 2x
    // snapshot. Aligned by their tops, it sat 2 points above it (November 2026).
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

/// An account where it's listed to show or hide: its title and label, and under them the
/// agents that use it, in the grays `title` and `subtitle`.
pub fn account_name(w: &Words, a: &Account, title: Color, subtitle: Color, ink: &Ink) -> El {
    let mut name = col()
        .key("name")
        .gap(1.0)
        .shrink()
        .min_w(0.0)
        .child(account_title(a, true, title, ink.tertiary));
    if let Some(via) = w.via(a) {
        name = name.child(text(via).font(SMALL).color(subtitle).truncate());
    }
    name
}

/// How wide a limit's line is: the panel, less the space beside its rows and inside a row.
const LIMIT_LINE: f32 = PANEL_WIDTH - 2.0 * 8.0 - 2.0 * row_size::PAD_X;

/// A limit's line, across the full width of a row: its name, its times and what's left,
/// above its bar.
fn limit_line(w: &Words, limit: &Limit, last: bool, grown: bool, ink: &Ink) -> El {
    let standing = limit.standing();
    let name = w.name(limit);
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
    fn said<'a>(
        name: &'a str,
        parts: &[&'a (String, Color)],
        ink: &Ink,
    ) -> Vec<(&'a str, Color, Option<f32>)> {
        let mut said: Vec<(&str, Color, Option<f32>)> = vec![(name, ink.secondary, Some(510.0))];
        for (part, color) in parts {
            said.push((" · ", ink.secondary, None));
            said.push((part.as_str(), *color, None));
        }
        said
    }
    let left = w.left(limit);
    let size = w.size(limit).map(|s| format!(" {s}")).unwrap_or_default();
    let figure_parts = [
        (left.as_str(), ink.ink(standing, ink.primary), Some(510.0)),
        (size.as_str(), ink.secondary, None),
    ];
    let figure = changes(&format!("{left}{size}"), spans(&figure_parts))
        .font(SMALL_DIGITS)
        .label(format!("{} left", w.left_of(limit)));
    // If both times don't fit beside the figure, only when it runs out shows, or else the
    // other time, cut short if it must be. The 24 points are the two 8-point gaps either side
    // of the spacer, and the spacer's own 8.
    let both: Vec<&(String, Color)> = ahead.iter().chain(window.iter()).collect();
    let room = LIMIT_LINE - width(&figure_parts, SMALL_DIGITS) - 24.0;
    let times = if width(&said(&name, &both, ink), SMALL) <= room {
        spans(&said(&name, &both, ink)).font(SMALL)
    } else {
        spans(&said(&name, &both[..both.len().min(1)], ink))
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
        .pb(if last { row_size::UNDER_BAR } else { 0.0 })
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

/// The sessions that used the limit that matters most. A session's share goes under its
/// title, at the start of the line, so the figures on the right are always what's left.
fn used_most_list(
    w: &Words,
    a: &Account,
    sessions: &[turnscope::engine::UsedMost],
    ink: &Ink,
) -> El {
    // The agent is named only if the sessions ran in more than one.
    let first = sessions.first().map(|u| u.agent.as_str());
    let agents = sessions.iter().any(|u| Some(u.agent.as_str()) != first);
    let of = a.deciding_limit().map(|l| w.of(l)).unwrap_or_default();
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
                    text(w.used(used, agents))
                        .font(SMALL_DIGITS)
                        .color(ink.secondary),
                ),
        );
    }
    list
}

/// The accounts not used this week, folded into one line, "3 more accounts", with hidden
/// accounts in a fold of their own at the end, "2 hidden".
fn unused_accounts(
    s: &State,
    w: &Words,
    unused: &[&Account],
    hidden: &[&Account],
    beside: bool,
    ink: &Ink,
) -> El {
    let label = if unused.is_empty() {
        w.hidden(hidden.len())
    } else {
        w.unused(unused.len(), beside)
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
                        s.open = None;
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
                &w.hidden(hidden.len()),
                s.hidden_shown,
                |s| s.hidden_shown = !s.hidden_shown,
                ink,
            ));
        }
        if s.hidden_shown || unused.is_empty() {
            let mut rows = col().key("hidden-rows").gap(row_size::SPACING).w_full();
            for a in hidden {
                rows = rows.child(hidden_row(s, w, a, ink));
            }
            inner = inner.child(appears(rows));
        }
        list = list.child(appears(inner));
    }
    list
}

/// A line that folds rows away, showing what it holds and a chevron.
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

/// A hidden account, with a button to show it, which is off once the engine has stopped for
/// good.
fn hidden_row(s: &State, w: &Words, a: &Account, ink: &Ink) -> El {
    let id = a.id.clone();
    let enabled = !s.stopped();
    let mut show = pill(&format!("show:{}", a.id), "Show", true, enabled, ink)
        .label(format!("Show {}", w.called(a, true)));
    if enabled {
        show = show.on_press_tx(move |s: &mut State, _: &mut Tx| crate::hide(s, &id, false));
    }
    row()
        .key(format!("hidden:{}", a.id))
        .center_y()
        .gap(row_size::GAP)
        .w_full()
        .py(row_size::PAD_Y)
        .px(row_size::PAD_X)
        .child(account_logo(&a.provider, row_size::LOGO, ink.tertiary, ink))
        .child(account_name(w, a, ink.secondary, ink.tertiary, ink))
        .child(spacer().min_w(8.0))
        .child(show)
}

/// An agent's name, and under it `note`, what's wrong with how it's connected, as an
/// account's name has its line under it. A note can be selected, to copy why connecting failed.
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

/// A row shown only while an agent in use can't ask Turnscope about its limits, because it
/// isn't connected or runs another copy: laid out as an account's, the agent's name over
/// what's wrong. As one sentence beside the buttons, "Claude Code uses another Turnscope"
/// broke over two cramped lines. It can be put away for good, one agent at a time.
fn connect_offer(s: &State, w: &Words, ink: &Ink) -> Option<El> {
    let agent = s.to_connect()?;
    let id = agent.id.clone();
    // If connecting failed, it says why instead.
    let note = s
        .agent_errors
        .get(&agent.id)
        .cloned()
        .or_else(|| w.connection(agent));
    let mut line = row()
        .key(format!("connect:{}", agent.id))
        .center_y()
        .gap(row_size::GAP)
        .w_full()
        .py(row_size::PAD_Y)
        .px(row_size::PAD_X)
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
        let connect_id = id.clone();
        line.child(
            pill("connect", verb, true, true, ink)
                .label(format!("{verb} {}", agent.name))
                .help(help)
                .on_press_tx(move |s: &mut State, _: &mut Tx| crate::connect(s, &connect_id, true)),
        )
    };
    let dismiss = round_button("dismiss-connect", "xmark", ink)
        .help("Don't suggest this again")
        .label(format!("Don't suggest connecting {} again", agent.name))
        .on_press(move |s: &mut State| s.preferences.dismiss_connect(&id));
    Some(appears(line.child(dismiss)))
}

/// Settings and Quit, and how current the data is. While the data can't be kept current, it
/// says what's shown may be out of date. While the agents' history is first read, it says
/// that. When a scheduled check found an update, a button beside Settings shows it.
fn footer(s: &State, ink: &Ink) -> El {
    let mut footer = row().key("footer").gap(2.0).w_full().center_y();
    if s.status.is_some()
        && let Some(failure) = &s.failure
    {
        let said = if s.trying_again {
            "Not up to date · trying again"
        } else {
            "Not up to date"
        };
        footer = footer.child(appears(
            text(said)
                .key("stale")
                .font(SMALL)
                .color(ink.secondary)
                .help(failure)
                .label(format!(
                    "Not up to date: {failure}.{}",
                    if s.trying_again { " Trying again." } else { "" }
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
                .pl(12.0)
                .child(spinner(ink))
                .child(text("Reading your agents' history…").font(SMALL).color(ink.secondary))
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
                .on_press(|s: &mut State| go(s, Page::Settings)),
        )
        .child(
            icon_button("quit", "power", 28.0, 28.0, true, ink)
                .label("Quit Turnscope")
                .help("Quit Turnscope")
                .help_keys(["⌘", "Q"])
                .on_press_tx(|_: &mut State, tx: &mut Tx| tx.quit()),
        )
}
