//! The app's look, shared by every view: its spring, sizes, fonts, colors of urgency, and the
//! few controls it draws itself. It is grayscale until something needs the person: amber for a
//! limit that at this pace runs out before it resets, red for one used up. A switch that's on
//! keeps the system's accent, as every Mac app's does.

use std::os::fd::AsRawFd;
use std::sync::OnceLock;

use rune::{
    Color, Curve, Easing, El, Family, Icon, Span, Spring, SpringPair, TextStyle, Transition, Tx,
    el, icon, rich, row, text,
};

use crate::standing::Standing;
use crate::state::State;
use crate::symbols::{Weight, symbol};

/// SwiftUI's `spring(response: 0.34, dampingFraction: 0.9)`, as Rune's response and bounce.
pub const SPRING: Spring = Spring::new(0.34, 0.1);
/// SwiftUI's `easeOut(duration: 0.12)`. Without it Rune shows hover at once and fades it out
/// over 0.1 s.
pub const HOVER: Curve = Curve::tween(0.12, Easing::EASE_OUT);
pub const APPEAR_AFTER: f32 = 0.16;

const SF: Family = Family::Named("System Font");

/// SF's file, as the app's own font. Rune looks up a fallback font for each character beyond
/// ASCII that the app's own fonts lack, and SF named by family isn't one: a row opened for the
/// first time took 0.3 to 0.7 s, and 20 accented letters 1.2 s (measured 2026-11-18). Mapped,
/// not read, so only the pages Rune reads take memory, not all 8 MB.
pub fn fonts() -> &'static [&'static [u8]] {
    static FONTS: OnceLock<Option<&'static [u8]>> = OnceLock::new();
    FONTS
        .get_or_init(|| mapped("/System/Library/Fonts/SFNS.ttf"))
        .as_slice()
}

fn mapped(path: &str) -> Option<&'static [u8]> {
    let file = std::fs::File::open(path).ok()?;
    let length = usize::try_from(file.metadata().ok()?.len()).ok()?;
    if length == 0 {
        return None;
    }
    // SAFETY: private read-only mapping of a file on the read-only system volume, never unmapped.
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

/// Line heights measured against SwiftUI snapshots to land within a device pixel.
const fn style(size: f32, weight: f32, line: f32) -> TextStyle {
    let mut s = TextStyle::new(size, weight).family(SF);
    s.line_height = line;
    s
}

// SF's own `wght` axis: medium = 510, semibold = 590.
pub const NAME: TextStyle = style(13.0, 400.0, 16.0);
pub const STRONG: TextStyle = style(13.0, 590.0, 16.0);
pub const FIGURE: TextStyle = STRONG.features("'tnum' 1");
pub const LINE: TextStyle = style(12.0, 400.0, 15.0);
pub const SMALL: TextStyle = style(11.0, 400.0, 14.0);
pub const SMALL_DIGITS: TextStyle = SMALL.features("'tnum' 1");
pub const HEADING: TextStyle = style(11.0, 590.0, 14.0);
const PILL: TextStyle = style(12.0, 510.0, 15.0);
pub const TITLE: TextStyle = style(15.0, 590.0, 18.0);
pub const HEADLINE: TextStyle = style(20.0, 590.0, 24.0);

pub mod row_size {
    /// At 16pt Anthropic's mark was 9pt tall and looked small; at 20pt OpenRouter's looked heavy.
    pub const LOGO: f32 = 18.0;
    pub const GAP: f32 = 8.0;
    pub const PAD_Y: f32 = 7.0;
    pub const PAD_X: f32 = 12.0;
    /// Balances visual padding below a progress bar with the line-height leading above text.
    pub const UNDER_BAR: f32 = 3.0;
    pub const SPACING: f32 = 4.0;
    pub const GROUPS: f32 = 12.0;
    pub const RADIUS: f32 = 10.0;
    pub const SETTING: f32 = 36.0;
}

pub const PANEL_WIDTH: f32 = 340.0;
pub const GUTTER: f32 = 28.0;

#[derive(Clone, Copy)]
pub struct Ink {
    pub dark: bool,
    pub bg: Color,
    pub primary: Color,
    pub secondary: Color,
    pub tertiary: Color,
    pub quaternary: Color,
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
const AMBER: [u32; 4] = [0x8A5303, 0xED941A, 0x6B3E00, 0xFFB76B];
const RED: [u32; 4] = [0xC1070E, 0xFE8375, 0x970007, 0xFFB3A8];

/// The sRGB channels (0..=1) of amber or red for `standing` in the given
/// appearance, shared with the AppKit menu bar item.
pub fn urgency_srgb(standing: Standing, dark: bool, more: bool) -> (f64, f64, f64) {
    let at = usize::from(dark) + 2 * usize::from(more);
    let hex = match standing {
        Standing::UsedUp => RED[at],
        Standing::RunningOut | Standing::Lasts => AMBER[at],
    };
    let channel = |shift: u32| f64::from((hex >> shift) & 0xFF) / 255.0;
    (channel(16), channel(8), channel(0))
}

fn srgb(hex: u32) -> Color {
    let channel = |shift: u32| rune::color::srgb_decode(((hex >> shift) & 0xFF) as f32 / 255.0);
    Color {
        r: channel(16),
        g: channel(8),
        b: channel(0),
        a: 1.0,
    }
}

/// The overlay that, drawn over `under`, shows `to`: hover and press overlays are drawn over
/// what's under them, so each is what it adds. Both are the text's color.
fn over(under: Color, to: Color) -> Color {
    to.alpha(((to.a - under.a) / (1.0 - under.a)).max(0.0))
}

impl Ink {
    /// The grays are macOS's system label colors (black in light mode, white in dark, at the
    /// opacities read from `AquaAX.car` and `DarkAquaAX.car` in `SystemAppearance.bundle`,
    /// macOS 26.3). The panel's edge is `separatorColor`, the same gray as
    /// `quaternaryLabelColor`. An open or hovered row's fill is 0.06 (0.1 with Increase
    /// Contrast).
    ///
    /// | | Light | Dark | Light, Increase Contrast | Dark, Increase Contrast |
    /// |---|---|---|---|---|
    /// | `labelColor` | 0.85 | 0.85 | 1 | 1 |
    /// | `secondaryLabelColor` | 0.5 | 0.55 | 0.76 | 0.7 |
    /// | `tertiaryLabelColor` | 0.26 | 0.25 | 0.56 | 0.5 |
    /// | `quaternaryLabelColor`, `separatorColor` | 0.1 | 0.1 | 0.35 | 0.3 |
    /// | row fill | 0.06 | 0.06 | 0.1 | 0.1 |
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
            row: if more { 0.1 } else { 0.06 },
            accent,
            amber: srgb(AMBER[at]),
            red: srgb(RED[at]),
        }
    }

    pub fn standing(&self, standing: Standing) -> Color {
        match standing {
            Standing::Lasts => self.primary,
            Standing::RunningOut => self.amber,
            Standing::UsedUp => self.red,
        }
    }

    pub fn ink(&self, standing: Standing, lasting: Color) -> Color {
        if standing == Standing::Lasts {
            lasting
        } else {
            self.standing(standing)
        }
    }

    pub fn wash(&self, opacity: f32) -> Color {
        self.primary.alpha(self.primary.a * opacity)
    }

    pub fn hover(&self, under: f32, to: f32) -> Color {
        over(self.wash(under), self.wash(to))
    }

    /// 0.06 darker, as the Swift app's buttons were.
    pub fn press(&self, under: f32) -> Color {
        over(self.wash(under), self.wash(under + 0.06))
    }
}

/// Delays fade-in until layout space has opened so moving rows don't sweep over text.
pub fn appears(e: El) -> El {
    e.transition(Transition::opacity().springs(SpringPair {
        enter: Spring::new(0.2, 0.0),
        exit: Spring::new(0.08, 0.0),
    }))
    .enter_delay(APPEAR_AFTER)
}

mod logos {
    include!(concat!(env!("OUT_DIR"), "/logos.rs"));
}

pub fn provider_svg(provider: &str) -> Option<&'static str> {
    logos::PROVIDER_SVGS
        .iter()
        .find(|(name, _)| *name == provider)
        .map(|(_, svg)| *svg)
}

pub fn account_logo(provider: &str, size: f32, color: Color, ink: &Ink) -> El {
    logo(
        logos::PROVIDER_ICONS,
        provider,
        "key.fill",
        size,
        color,
        ink,
    )
}

pub fn agent_logo(agent: &str, size: f32, color: Color, ink: &Ink) -> El {
    logo(logos::AGENT_ICONS, agent, "terminal.fill", size, color, ink)
}

fn logo(
    table: &'static [(&'static str, Icon)],
    name: &str,
    fallback: &'static str,
    size: f32,
    color: Color,
    ink: &Ink,
) -> El {
    match table.iter().find(|(key, _)| *key == name) {
        Some((_, i)) => icon(i, size).color(color).a11y_hidden(),
        None => el()
            .size(size, size)
            .center()
            .child(sf(fallback, 13.0, Weight::Regular, ink, color))
            .a11y_hidden(),
    }
}

pub fn sf(name: &'static str, points: f32, weight: Weight, ink: &Ink, color: Color) -> El {
    let s = symbol(name, points, weight, ink.dark);
    el().size(s.size.0, s.size.1)
        .pixels(&s.pixels)
        .opacity(color.a)
}

fn hover_sf(name: &'static str, points: f32, weight: Weight, ink: &Ink) -> El {
    sf(name, points, weight, ink, ink.primary).group_opacity(ink.secondary.a / ink.primary.a, 1.0)
}

pub fn item_row(key: impl Into<String>) -> El {
    row()
        .key(key)
        .center_y()
        .gap(row_size::GAP)
        .w_full()
        .py(row_size::PAD_Y)
        .px(row_size::PAD_X)
}

// Controls hover with Rune's `hover` and `hover_group`, which don't run the view: hover kept in
// the app's state ran it 6 times in one pass down the panel (November 2026).

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
        // Consume clicks so they don't bubble and open the enclosing row.
        return button
            .disabled(true)
            .opacity(0.5)
            .on_press(|_: &mut State| {})
            .child(sf(symbol_name, points, Weight::Medium, ink, ink.secondary));
    }
    button
        .hover_group()
        .hover(ink.hover(0.0, 0.08))
        .press(ink.press(0.08))
        .child(hover_sf(symbol_name, points, Weight::Medium, ink))
}

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

/// A symbol in a circle the size of a pill, as its partner: a bare symbol beside a pill looked
/// too small.
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
        .child(hover_sf(symbol_name, 10.0, Weight::Semibold, ink))
}

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

pub fn section_title(title: &str, ink: &Ink) -> El {
    text(title)
        .key("section")
        .font(HEADING)
        .color(ink.secondary)
        .px(12.0)
        .pb(4.0)
        .role("heading")
}

pub fn spinner(ink: &Ink) -> El {
    // 0.7 of a circle from three o'clock, as SwiftUI's trimmed circle.
    static ARC: Icon = Icon {
        name: "spinner",
        view: 12.0,
        body: "<path d=\"M10.5 6 A4.5 4.5 0 1 1 7.39 1.72\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\"/>",
    };
    icon(&ARC, 9.0)
        .color(ink.secondary)
        .looping(rune::LoopSpec::rotation(0.9))
        .a11y_hidden()
}

/// `e`, keyed by what it says, so a change fades the old words out as the new fade in. Rune's
/// rolling digits are in its kit, which the app doesn't use.
pub fn changes(said: &str, e: El) -> El {
    e.key(format!("said:{said}"))
        .transition(Transition::opacity())
}

pub struct RichText {
    content: String,
    spans: Vec<Span>,
}

impl RichText {
    pub fn width(&self, style: TextStyle) -> f32 {
        rune::text::measure(&self.content, &self.spans, style, None).0
    }

    pub fn into_el(self) -> El {
        rich(self.content, self.spans)
    }
}

pub fn rich_text(parts: &[(&str, Color, Option<f32>)]) -> RichText {
    let mut content = String::new();
    let mut spans = Vec::with_capacity(parts.len());
    for (said, color, weight) in parts {
        if said.is_empty() {
            continue;
        }
        let start = content.len();
        content.push_str(said);
        let mut span = Span::new(start, content.len()).color(*color);
        if let Some(weight) = weight {
            span = span.weight(*weight);
        }
        spans.push(span);
    }
    RichText { content, spans }
}
