//! The app's look, shared by every view: its spring, sizes, fonts, colors of urgency, and the
//! few controls it draws itself. It is grayscale until something needs the person: amber for a
//! limit that at this pace runs out before it resets, red for one used up. A switch that's on
//! keeps the system's accent, as every Mac app's does.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::sync::OnceLock;

use rune::{
    Color, Curve, Easing, El, Family, Icon, Span, Spring, SpringPair, TextStyle, Transition, Tx,
    el, icon, rich, row, text,
};

use crate::standing::Standing;
use crate::state::State;
use crate::symbols::{Weight, symbol};

/// One spring for the whole app: SwiftUI's `spring(response: 0.34, dampingFraction: 0.9)`,
/// which the Swift app used, as Rune's response and bounce. The panel's root sets it, and
/// every element in it inherits it.
pub const SPRING: Spring = Spring::new(0.34, 0.1);

/// How hover fades in and out: SwiftUI's `easeOut(duration: 0.12)`, as the Swift app's rows
/// did. Without it, Rune shows hover at once and fades it out over 0.1 s.
pub const HOVER: Curve = Curve::tween(0.12, Easing::EASE_OUT);

/// How long new content waits for its space to open before it fades in (`appears`).
pub const APPEAR_AFTER: f32 = 0.16;

const SF: Family = Family::Named("System Font");

/// SF's file, as the app's own font (`AppSpec::fonts`). Rune looks up a fallback font, once per
/// character, for each character beyond ASCII that none of an app's own fonts has, and SF,
/// named by family, isn't one of them. So each such character cost about 60 ms the first time
/// it showed, and the first "·" a second with the fonts it loaded: a row opened for the first
/// time took 0.3 to 0.7 s, and 20 accented letters in a title 1.2 s (measured 2026-11-18). As
/// the app's own font, a character SF has needs no lookup. Mapped, not read, so only the pages
/// Rune reads take memory: read whole, its 8 MB would have added as much.
pub fn fonts() -> &'static [&'static [u8]] {
    static FONTS: OnceLock<Vec<&'static [u8]>> = OnceLock::new();
    FONTS.get_or_init(|| {
        mapped("/System/Library/Fonts/SFNS.ttf")
            .into_iter()
            .collect()
    })
}

/// The file at `path`, mapped read-only for as long as the app runs.
fn mapped(path: &str) -> Option<&'static [u8]> {
    let file = std::fs::File::open(path).ok()?;
    let length = usize::try_from(file.metadata().ok()?.len()).ok()?;
    if length == 0 {
        return None;
    }
    // SAFETY: a private, read-only mapping of a whole file, never unmapped. It stays valid
    // once the file is closed, and the system volume it's on is read-only.
    unsafe {
        let at = libc::mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        );
        (at != libc::MAP_FAILED).then(|| std::slice::from_raw_parts(at.cast::<u8>(), length))
    }
}

/// SF at `size` and `weight`, as tall as SwiftUI sets a line of it. Measured against the Swift
/// app's snapshots: these line heights put every line within a device pixel of it.
const fn style(size: f32, weight: f32, line: f32) -> TextStyle {
    let mut s = TextStyle::new(size, weight).family(SF);
    s.line_height = line;
    s
}

// SF's own weights: medium is 510 and semibold 590 on its `wght` axis.
/// A row's name: an account, an agent or a setting.
pub const NAME: TextStyle = style(13.0, 400.0, 16.0);
/// The figure on a row's right: what's left, or what it cost.
pub const FIGURE: TextStyle = style(13.0, 590.0, 16.0).features("'tnum' 1");
/// A line of text, such as a sentence or what happens to a limit next.
pub const LINE: TextStyle = style(12.0, 400.0, 15.0);
/// Small text: the line under a name, a limit's name and figure, or a heading.
pub const SMALL: TextStyle = style(11.0, 400.0, 14.0);
pub const SMALL_DIGITS: TextStyle = style(11.0, 400.0, 14.0).features("'tnum' 1");
pub const HEADING: TextStyle = style(11.0, 590.0, 14.0);
pub const PILL: TextStyle = style(12.0, 510.0, 15.0);
pub const TITLE: TextStyle = style(15.0, 590.0, 18.0);
pub const STRONG: TextStyle = style(13.0, 590.0, 16.0);
pub const HEADLINE: TextStyle = style(20.0, 590.0, 24.0);

/// The sizes every row uses, in the panel and in Settings, whether it's an account, an agent
/// or a setting. That way rows of every kind line up and read alike.
pub mod row_size {
    /// A logo's size, beside a row's name. A mark has room around it in its frame
    /// (`assets/logos/README.md`): at 16 points Anthropic's was 9 tall and looked small beside
    /// a name with a line under it, and at 20 OpenRouter's looked heavy.
    pub const LOGO: f32 = 18.0;
    /// The space from a logo to the name beside it.
    pub const GAP: f32 = 8.0;
    pub const PAD_Y: f32 = 7.0;
    pub const PAD_X: f32 = 12.0;
    /// Extra space under a row that ends in a bar: the space a line of text keeps above its
    /// letters, so the row looks as deep below as above.
    pub const UNDER_BAR: f32 = 3.0;
    /// The space between rows, so an open or hovered row doesn't touch the next.
    pub const SPACING: f32 = 4.0;
    /// The space between the accounts in use and those used this week.
    pub const GROUPS: f32 = 12.0;
    /// The corner radius of a filled row, and of a card in Settings.
    pub const RADIUS: f32 = 10.0;
    /// The height of a row in Settings with one line.
    pub const SETTING: f32 = 36.0;
}

pub const PANEL_WIDTH: f32 = 340.0;
/// Room beside the panel in its window, for its shadow: the Swift app's.
pub const GUTTER: f32 = 28.0;

/// The grays the system's label colors resolve to, the fills made from them, and the colors
/// of urgency, for one appearance.
#[derive(Clone, Copy)]
pub struct Ink {
    pub dark: bool,
    /// The window's background, which a snapshot draws the panel on instead of glass.
    pub bg: Color,
    pub primary: Color,
    pub secondary: Color,
    pub tertiary: Color,
    pub quaternary: Color,
    /// The panel's edge.
    pub edge: Color,
    /// The fill of an open or hovered row, as a `wash`.
    pub row: f32,
    pub accent: Color,
    amber: Color,
    red: Color,
}

/// Amber and red, as sRGB, in light, dark, light with Increase Contrast and dark with it. The
/// menu bar item draws them too.
///
/// Each is text as small as 11 points, so it reaches 4.5:1 against the darkest background
/// it's on: an open row on the panel's glass, about #DDDDDD in light mode and #3B3B3B in dark.
/// With Increase Contrast, which also makes the glass opaque, each reaches 7:1 against an open
/// row on the window's background, #E6E6E6 and #343434. Each keeps its hue, and as much of its
/// chroma as sRGB holds. One amber and one red for every appearance were 1.7:1 and 2.9:1 on a
/// light open row on the glass, and the red 2.9:1 on a dark one (October 2026). With Increase
/// Contrast, the open row's fill grew from 0.05 to 0.1 (`Ink::new`), and the amber and red
/// chosen for the old fill fell to 6.1 to 6.5:1, so they were made again (November 2026).
///
/// | | Light | Dark | Light, Increase Contrast | Dark, Increase Contrast |
/// |---|---|---|---|---|
/// | Amber | #8A5303, 4.7:1 | #ED941A, 4.7:1 | #6B3E00, 7.2:1 | #FFB76B, 7.2:1 |
/// | Red | #C1070E, 4.7:1 | #FE8375, 4.7:1 | #970007, 7.2:1 | #FFB3A8, 7.2:1 |
pub const AMBER: [u32; 4] = [0x8A5303, 0xED941A, 0x6B3E00, 0xFFB76B];
pub const RED: [u32; 4] = [0xC1070E, 0xFE8375, 0x970007, 0xFFB3A8];

fn srgb(hex: u32) -> Color {
    rune::color::hex(&format!("#{hex:06X}")).unwrap_or(Color::BLACK)
}

/// The overlay that, drawn over `under`, shows `to`: hover and press overlays are drawn over
/// what's under them, so each is what it adds. Both are the text's color.
fn over(under: Color, to: Color) -> Color {
    to.alpha(((to.a - under.a) / (1.0 - under.a)).max(0.0))
}

impl Ink {
    /// The grays are the system's label colors, from the Swift app, which drew with them.
    /// Each is black in light mode and white in dark, at an opacity. With Increase Contrast
    /// (`more`), each is the system's own for it, read from `AquaAX.car` and `DarkAquaAX.car`
    /// in `SystemAppearance.bundle` (macOS 26.3). The panel's edge is `separatorColor`, the
    /// same gray as `quaternaryLabelColor`. The open or hovered row's fill is
    /// `tertiarySystemFill`; the Swift app's was the text's color at 0.06, which is the same
    /// to within a hundredth.
    ///
    /// | | Light | Dark | Light, Increase Contrast | Dark, Increase Contrast |
    /// |---|---|---|---|---|
    /// | `labelColor` | 0.85 | 0.85 | 1 | 1 |
    /// | `secondaryLabelColor` | 0.5 | 0.55 | 0.76 | 0.7 |
    /// | `tertiaryLabelColor` | 0.26 | 0.25 | 0.56 | 0.5 |
    /// | `quaternaryLabelColor`, `separatorColor` | 0.1 | 0.1 | 0.35 | 0.3 |
    /// | `tertiarySystemFill` | 0.05 | 0.05 | 0.1 | 0.1 |
    ///
    /// Before, only amber and red changed with Increase Contrast. The edge was the text's color
    /// at 0.12, 0.1 in every appearance.
    pub fn new(dark: bool, more: bool, accent: Color) -> Ink {
        let base = if dark { Color::WHITE } else { Color::BLACK };
        let (p, s, t, q) = match (dark, more) {
            (false, false) => (0.85, 0.5, 0.26, 0.1),
            (true, false) => (0.85, 0.55, 0.25, 0.1),
            (false, true) => (1.0, 0.76, 0.56, 0.35),
            (true, true) => (1.0, 0.7, 0.5, 0.3),
        };
        let at = usize::from(dark) + 2 * usize::from(more);
        Ink {
            dark,
            bg: if dark { srgb(0x1E1E1E) } else { Color::WHITE },
            primary: base.alpha(p),
            secondary: base.alpha(s),
            tertiary: base.alpha(t),
            quaternary: base.alpha(q),
            edge: base.alpha(q),
            row: if more { 0.1 } else { 0.06 },
            accent,
            amber: srgb(AMBER[at]),
            red: srgb(RED[at]),
        }
    }

    /// The color for something with `standing`: the text's own color while it lasts, amber
    /// while it's running out, red once it's used up.
    pub fn standing(&self, standing: Standing) -> Color {
        match standing {
            Standing::Lasts => self.primary,
            Standing::RunningOut => self.amber,
            Standing::UsedUp => self.red,
        }
    }

    /// Text about something with `standing`: amber or red when it needs attention, else
    /// `lasting`.
    pub fn ink(&self, standing: Standing, lasting: Color) -> Color {
        if standing == Standing::Lasts {
            lasting
        } else {
            self.standing(standing)
        }
    }

    /// The text's color at `opacity`, as a fill: SwiftUI's `Color.primary.opacity(_:)`, which
    /// the Swift app's fills were.
    pub fn wash(&self, opacity: f32) -> Color {
        self.primary.alpha(self.primary.a * opacity)
    }

    /// The overlay hover draws on a fill that's `wash(under)`, to show `wash(to)`.
    pub fn hover(&self, under: f32, to: f32) -> Color {
        over(self.wash(under), self.wash(to))
    }

    /// The overlay a press draws on a fill that's `wash(under)`: 0.06 darker, as the Swift
    /// app's buttons were, from 0.08 to 0.14 and from 0.14 to 0.2.
    pub fn press(&self, under: f32) -> Color {
        over(self.wash(under), self.wash(under + 0.06))
    }
}

/// New content fades in once its space has opened, and fades out at once, so nothing moving
/// passes over text.
pub fn appears(e: El) -> El {
    e.transition(Transition::opacity().springs(SpringPair {
        enter: Spring::new(0.2, 0.0),
        exit: Spring::new(0.08, 0.0),
    }))
    .enter_delay(APPEAR_AFTER)
}

// Logos

mod logos {
    include!(concat!(env!("OUT_DIR"), "/logos.rs"));
}

/// The SVG of the logo at `path` in `assets/logos`, such as "providers/anthropic".
pub fn logo_svg(path: &str) -> Option<&'static str> {
    logos::LOGOS
        .iter()
        .find(|(name, _)| *name == path)
        .map(|(_, svg)| *svg)
}

thread_local! {
    static ICONS: RefCell<HashMap<String, Option<&'static Icon>>> = RefCell::new(HashMap::new());
}

/// The logo at `path` as a Rune icon, which draws in its element's color.
fn logo_icon(path: &str) -> Option<&'static Icon> {
    ICONS.with(|icons| {
        *icons
            .borrow_mut()
            .entry(path.to_owned())
            .or_insert_with(|| {
                let svg = logo_svg(path)?;
                let view_box = svg.split("viewBox=\"").nth(1)?.split('"').next()?;
                let v: Vec<f32> = view_box
                    .split_whitespace()
                    .filter_map(|n| n.parse().ok())
                    .collect();
                let [x, y, width, _] = v[..] else { return None };
                let body = &svg[svg.find('>')? + 1..svg.rfind("</svg>")?];
                // Rune's icons have their viewBox at the origin: move one that isn't.
                let body = if x != 0.0 || y != 0.0 {
                    format!("<g transform=\"translate({} {})\">{body}</g>", -x, -y)
                } else {
                    body.to_owned()
                };
                let icon = Icon {
                    name: Box::leak(path.to_owned().into_boxed_str()),
                    view: width,
                    body: Box::leak(body.into_boxed_str()),
                };
                Some(&*Box::leak(Box::new(icon)))
            })
    })
}

/// An account's logo, in `color`, or a key if there's none for its provider.
pub fn account_logo(provider: &str, size: f32, color: Color, ink: &Ink) -> El {
    logo(
        &format!("providers/{provider}"),
        "key.fill",
        size,
        color,
        ink,
    )
}

/// An agent's logo, in `color`, or a terminal if there's none for it.
pub fn agent_logo(agent: &str, size: f32, color: Color, ink: &Ink) -> El {
    logo(
        &format!("agents/{agent}"),
        "terminal.fill",
        size,
        color,
        ink,
    )
}

fn logo(path: &str, fallback: &'static str, size: f32, color: Color, ink: &Ink) -> El {
    match logo_icon(path) {
        Some(i) => icon(i, size).color(color).a11y_hidden(),
        None => el()
            .size(size, size)
            .center()
            .child(sf(fallback, 13.0, Weight::Regular, ink, color))
            .a11y_hidden(),
    }
}

/// An SF Symbol at its own size, in the gray `color`.
pub fn sf(name: &'static str, points: f32, weight: Weight, ink: &Ink, color: Color) -> El {
    let s = symbol(name, points, weight, ink.dark);
    el().size(s.size.0, s.size.1)
        .pixels(&s.pixels)
        .opacity(color.a)
}

// Controls. Hover is drawn by Rune (`hover`, `hover_group`, `group_*`), which doesn't run the
// view. Kept in the app's state, it ran the view each time the pointer came onto a row or a
// button, 6 times in one pass down the panel, and a row lost its hover when the pointer came
// back to it from its Hide button (November 2026).

/// A small icon button that fills on hover and darkens when pressed, like the system's toolbar
/// buttons, or is dimmed if it can't be used now. Within a row it's no taller than the line it
/// sits on, so showing it never moves the row.
pub fn icon_button(
    key: &str,
    symbol_name: &'static str,
    width: f32,
    height: f32,
    enabled: bool,
    ink: &Ink,
) -> El {
    let points = if height > 24.0 { 13.0 } else { 11.0 };
    let button = el()
        .key(key)
        .size(width, height)
        .center()
        .radius(width.min(height) / 4.0)
        .continuous_corners()
        .role("button");
    if !enabled {
        // It takes its clicks and does nothing, so they don't open the row it's in.
        return button
            .disabled(true)
            .opacity(0.5)
            .on_press(|_: &mut State| {})
            .child(sf(symbol_name, points, Weight::Medium, ink, ink.secondary));
    }
    // Gray, and the text's color while hovered.
    let symbol = sf(symbol_name, points, Weight::Medium, ink, ink.primary)
        .group_opacity(ink.secondary.a / ink.primary.a, 1.0);
    button
        .hover_group()
        .hover(ink.hover(0.0, 0.08))
        .press(ink.press(0.08))
        .child(symbol)
}

/// The one action on a row: quiet, and filling on hover, or dimmed if it can't be used now. In
/// a `choice`, only the chosen one is filled before it's hovered, and the rest are gray.
pub fn pill(key: &str, title: &str, filled: bool, enabled: bool, ink: &Ink) -> El {
    let rest = if filled { 0.08 } else { 0.0 };
    let said = text(title)
        .font(PILL)
        .color(if filled { ink.primary } else { ink.secondary });
    let pill = el()
        .key(key)
        .h(22.0)
        .px(10.0)
        .center()
        .radius(11.0)
        .fill(ink.wash(rest))
        .role("button")
        .label(title);
    if !enabled {
        return pill.disabled(true).opacity(0.5).child(said);
    }
    pill.hover_group()
        .hover(ink.hover(rest, 0.14))
        .press(ink.press(0.14))
        .child(said.group_color(ink.primary))
}

/// A symbol in a circle as tall as a filled pill, and filled as one is: the pill's partner, as
/// a close button beside "Reconnect". A bare symbol beside a pill looked too small. Its symbol
/// is gray, and the text's color while hovered, so the pill stays the one to press.
pub fn round_button(key: &str, symbol_name: &'static str, ink: &Ink) -> El {
    let rest = 0.08;
    el().key(key)
        .size(22.0, 22.0)
        .center()
        .radius(11.0)
        .fill(ink.wash(rest))
        .role("button")
        .hover_group()
        .hover(ink.hover(rest, 0.14))
        .press(ink.press(0.14))
        .child(
            sf(symbol_name, 10.0, Weight::Semibold, ink, ink.primary)
                .group_opacity(ink.secondary.a / ink.primary.a, 1.0),
        )
}

/// One of a few options, as pills in a row with the chosen one filled. The arrow keys move
/// between them, and Tab comes to the chosen one.
pub fn choice<T: Copy + PartialEq + 'static>(
    key: &str,
    label: &str,
    options: &[(T, &str)],
    selected: T,
    pick: fn(&mut State, T, &mut Tx),
    ink: &Ink,
) -> El {
    let mut pills = row()
        .key(key)
        .gap(2.0)
        .role("radiogroup")
        .label(label)
        .focus_group();
    for (i, (value, title)) in options.iter().enumerate() {
        let value = *value;
        let chosen = value == selected;
        pills = pills.child(
            pill(&format!("{key}:{i}"), title, chosen, true, ink)
                .role("radio")
                .checked(chosen)
                .focus_selected(chosen)
                .on_press_tx(move |s: &mut State, tx: &mut Tx| pick(s, value, tx)),
        );
    }
    pills
}

/// An on and off switch, on in the system's accent, drawn here so a snapshot draws it as the
/// screen does.
pub fn switch(key: &str, label: &str, on: bool, enabled: bool, ink: &Ink) -> El {
    let knob = el()
        .key("knob")
        .size(14.0, 14.0)
        .radius(7.0)
        .fill(Color::WHITE)
        .shadow(0.5, 1.0, Color::BLACK.alpha(0.18));
    let track = row()
        .key(key)
        .size(30.0, 18.0)
        .padding(2.0)
        .radius(9.0)
        .fill(if on { ink.accent } else { ink.wash(0.16) })
        .center_y()
        .role("switch")
        .label(label)
        .checked(on)
        .disabled(!enabled)
        .focusable(enabled)
        .opacity(if enabled { 1.0 } else { 0.5 });
    if on {
        track.child(rune::spacer()).child(knob)
    } else {
        track.child(knob).child(rune::spacer())
    }
}

/// A section's heading, in Settings and the panel.
pub fn section_title(title: &str, ink: &Ink) -> El {
    text(title)
        .key("section")
        .font(HEADING)
        .color(ink.secondary)
        .px(12.0)
        .pb(4.0)
        .role("heading")
}

/// A small arc that turns while something is in progress, or holds still with Reduce Motion.
pub fn spinner(ink: &Ink) -> El {
    static ARC: Icon = Icon {
        name: "spinner",
        view: 12.0,
        // A circle 0.7 of the way round, from three o'clock, as SwiftUI's trimmed circle is.
        body: "<path d=\"M10.5 6 A4.5 4.5 0 1 1 7.39 1.72\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\"/>",
    };
    icon(&ARC, 9.0)
        .color(ink.secondary)
        .looping(rune::LoopSpec::rotation(0.9))
        .a11y_hidden()
}

/// `said`, keyed by what it says, so a change fades the old words out as the new ones fade in
/// over them. The Swift app rolled a figure's digits (`numericText`); Rune's rolling text is in
/// its kit, which the app doesn't use.
pub fn changes(said: &str, e: El) -> El {
    e.key(format!("said:{said}"))
        .transition(Transition::opacity())
}

/// `text` with the spans `parts` gives, each in its own color, and at `weight` where given.
pub fn spans(parts: &[(&str, Color, Option<f32>)]) -> El {
    let (content, spans) = spanned(parts);
    rich(content, spans)
}

/// The text and spans `spans` draws.
pub fn spanned(parts: &[(&str, Color, Option<f32>)]) -> (String, Vec<Span>) {
    let mut content = String::new();
    let mut spans = Vec::new();
    for (said, color, weight) in parts {
        let start = content.len();
        content.push_str(said);
        let mut span = Span::new(start, content.len()).color(*color);
        if let Some(weight) = weight {
            span = span.weight(*weight);
        }
        spans.push(span);
    }
    (content, spans)
}

/// How wide `parts` is set in `style`, in points.
pub fn width(parts: &[(&str, Color, Option<f32>)], style: TextStyle) -> f32 {
    let (content, spans) = spanned(parts);
    rune::text::measure(&content, &spans, style, None).0
}
