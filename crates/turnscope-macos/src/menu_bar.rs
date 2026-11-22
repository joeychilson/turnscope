//! The menu bar item (docs/app.md#the-menu-bar-item): an AppKit status item, which Rune doesn't
//! make, whose click opens or closes the panel. It's one image, redrawn only when what it shows
//! changes.
//!
//! The image is drawn by AppKit when the menu bar draws it, from a drawing handler, so its
//! colors resolve in the menu bar's own appearance, which follows the wallpaper as much as the
//! system's setting. A color picked once, from the item before it was in the menu bar, came out
//! light-mode amber (#8A5303) on a dark menu bar (November 2026).

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject};
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAppearance, NSAppearanceNameAccessibilityHighContrastAqua,
    NSAppearanceNameAccessibilityHighContrastDarkAqua, NSAppearanceNameAqua,
    NSAppearanceNameDarkAqua, NSBezierPath, NSColor, NSCompositingOperation, NSFont,
    NSFontAttributeName, NSFontWeightSemibold, NSForegroundColorAttributeName, NSImage,
    NSRectFillUsingOperation, NSScreen, NSStatusBar, NSStatusItem, NSStringDrawing,
    NSVariableStatusItemLength, NSWindingRule, NSWindow,
};
use objc2_foundation::{
    NSArray, NSAttributedStringKey, NSData, NSDictionary, NSPoint, NSRect, NSSize, NSString,
};

use crate::standing::*;
use crate::state::State;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TurnscopeItemTarget"]
    struct Target;

    impl Target {
        #[unsafe(method(toggle:))]
        fn toggle(&self, _sender: Option<&AnyObject>) {
            crate::toggle_panel();
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

#[derive(Clone, PartialEq)]
struct Slot {
    provider: String,
    standing: Standing,
    figure: String,
}

#[derive(Clone, PartialEq)]
struct Drawn {
    slots: Vec<Slot>,
    spoken: String,
    out_of_date: bool,
}

struct Item {
    item: Retained<NSStatusItem>,
    _target: Retained<Target>,
    drawn: Option<Drawn>,
}

thread_local! {
    static ITEM: RefCell<Option<Item>> = const { RefCell::new(None) };
}

pub fn draw(s: &State) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    // As of now, not the panel's minute: the item counts down whether the panel is open or not.
    let words = s.words_now();
    let shown = s.in_menu_bar();
    let out_of_date = s.problem.is_some();
    let showing = Drawn {
        slots: shown
            .iter()
            .map(|a| Slot {
                provider: a.provider.clone(),
                standing: a.standing(),
                figure: words.figure(a),
            })
            .collect(),
        spoken: words.spoken_item(&shown, out_of_date),
        out_of_date,
    };
    ITEM.with(|cell| {
        let mut cell = cell.borrow_mut();
        let item = cell.get_or_insert_with(|| {
            let item =
                NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
            let target = Target::new(mtm);
            if let Some(button) = item.button(mtm) {
                unsafe {
                    button.setTarget(Some(&target));
                    button.setAction(Some(sel!(toggle:)));
                }
            }
            Item {
                item,
                _target: target,
                drawn: None,
            }
        });
        if item.drawn.as_ref() == Some(&showing) {
            return;
        }
        let Some(button) = item.item.button(mtm) else {
            return;
        };
        let image = image(&showing);
        image.setAccessibilityDescription(Some(&NSString::from_str(&showing.spoken)));
        button.setImage(Some(&image));
        item.drawn = Some(showing);
    });
}

/// The item's height, and the sizes of what's in it. A provider's mark has room around it in
/// its frame (`assets/logos/README.md`), so at 21 points Anthropic's is 16 by 11.5 and OpenAI's
/// 15.5 square. At 17 points (Anthropic's 9 tall) the item looked small beside other apps'
/// icons. At 22 (12 tall) the mark outweighed the figure, so the figure is semibold too.
const HEIGHT: f64 = 22.0;
const LOGO: f64 = 21.0;
/// Turnscope's own mark, shown when no account is in use: a little under other apps' icons,
/// which are 16 points (measured beside them in October 2026). At 16, its filled ring looked
/// bigger than they do.
const MARK: f64 = 14.0;

/// The item's image. It's a template, which the menu bar tints, unless an account it shows
/// needs attention: then it's in color, since a figure it shows is a time, or becomes one as
/// running out nears.
fn image(showing: &Drawn) -> Retained<NSImage> {
    let template = showing
        .slots
        .iter()
        .all(|slot| slot.standing == Standing::Lasts);
    let fraction = if showing.out_of_date { DIMMED } else { 1.0 };
    let font = unsafe {
        NSFont::monospacedDigitSystemFontOfSize_weight(
            NSFont::menuBarFontOfSize(0.0).pointSize(),
            NSFontWeightSemibold,
        )
    };
    let attributes = move |font: &NSFont, color: &NSColor| {
        let keys: [&NSAttributedStringKey; 2] =
            unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
        let values: [&AnyObject; 2] = [font, color];
        NSDictionary::from_slices(&keys, &values)
    };
    let measure = attributes(&font, &NSColor::blackColor());
    struct Part {
        logo: Option<Retained<NSImage>>,
        figure: Retained<NSString>,
        standing: Standing,
        width: f64,
    }
    // Each account: its logo, its figure in the menu bar's own size, standing, and text width.
    let parts: Vec<Part> = showing
        .slots
        .iter()
        .map(|slot| {
            let figure = NSString::from_str(&slot.figure);
            let width = unsafe { figure.sizeWithAttributes(Some(&measure)) }
                .width
                .ceil();
            Part {
                logo: logo(&slot.provider),
                figure,
                standing: slot.standing,
                width,
            }
        })
        .collect();
    let line = font.ascender() - font.descender();
    let width = if parts.is_empty() {
        MARK + 2.0
    } else {
        parts
            .iter()
            .map(|p| if p.logo.is_some() { LOGO + 4.0 } else { 0.0 } + p.width)
            .sum::<f64>()
            + 10.0 * (parts.len() - 1) as f64
            + 2.0
    };
    let amber = urgent(Standing::RunningOut, showing.out_of_date);
    let red = urgent(Standing::UsedUp, showing.out_of_date);
    let handler = RcBlock::new(move |_rect: NSRect| -> Bool {
        let ink = |standing: Standing| -> Retained<NSColor> {
            if template {
                return NSColor::blackColor().colorWithAlphaComponent(fraction);
            }
            match standing {
                Standing::Lasts => NSColor::labelColor().colorWithAlphaComponent(0.85 * fraction),
                Standing::RunningOut => amber.clone(),
                Standing::UsedUp => red.clone(),
            }
        };
        if parts.is_empty() {
            mark(
                NSRect::new(
                    NSPoint::new(1.0, (HEIGHT - MARK) / 2.0),
                    NSSize::new(MARK, MARK),
                ),
                &ink(Standing::Lasts),
            );
            return Bool::YES;
        }
        let mut x = 1.0;
        for part in &parts {
            if let Some(logo) = &part.logo {
                tinted(
                    logo,
                    NSRect::new(
                        NSPoint::new(x, (HEIGHT - LOGO) / 2.0),
                        NSSize::new(LOGO, LOGO),
                    ),
                    &ink(Standing::Lasts),
                );
                x += LOGO + 4.0;
            }
            // Raised a point onto the clock's baseline: centered in the item, it sat lower
            // (measured October 2026).
            let y = (HEIGHT - line) / 2.0 + 1.0;
            unsafe {
                part.figure.drawAtPoint_withAttributes(
                    NSPoint::new(x, y),
                    Some(&attributes(&font, &ink(part.standing))),
                )
            };
            x += part.width + 10.0;
        }
        Bool::YES
    });
    let image =
        NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(width, HEIGHT), false, &handler);
    image.setTemplate(template);
    image
}

thread_local! {
    static LOGOS: RefCell<std::collections::HashMap<String, Option<Retained<NSImage>>>> = RefCell::new(Default::default());
}

fn logo(provider: &str) -> Option<Retained<NSImage>> {
    LOGOS.with(|logos| {
        if let Some(cached) = logos.borrow().get(provider) {
            return cached.clone();
        }
        let image = crate::style::provider_svg(provider).and_then(|svg| {
            let data = NSData::with_bytes(svg.as_bytes());
            NSImage::initWithData(NSImage::alloc(), &data)
        });
        logos
            .borrow_mut()
            .insert(provider.to_owned(), image.clone());
        image
    })
}

fn tinted(image: &NSImage, rect: NSRect, color: &NSColor) {
    let size = image.size();
    if size.width <= 0.0 || size.height <= 0.0 {
        return;
    }
    let scale = (rect.size.width / size.width).min(rect.size.height / size.height);
    let fitted = NSSize::new(size.width * scale, size.height * scale);
    let at = NSRect::new(
        NSPoint::new(
            rect.origin.x + (rect.size.width - fitted.width) / 2.0,
            rect.origin.y + (rect.size.height - fitted.height) / 2.0,
        ),
        fitted,
    );
    let image: Retained<NSImage> = image.into();
    let color: Retained<NSColor> = color.into();
    let tint = RcBlock::new(move |bounds: NSRect| -> Bool {
        image.drawInRect(bounds);
        color.set();
        NSRectFillUsingOperation(bounds, NSCompositingOperation::SourceAtop);
        Bool::YES
    });
    let mask = NSImage::imageWithSize_flipped_drawingHandler(fitted, false, &tint);
    mask.drawInRect(at);
}

fn mark(rect: NSRect, color: &NSColor) {
    let unit = rect.size.width / 530.0;
    let center = NSPoint::new(
        rect.origin.x + rect.size.width / 2.0,
        rect.origin.y + rect.size.height / 2.0,
    );
    let circle = |radius: f64| {
        let r = radius * unit;
        NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
            NSPoint::new(center.x - r, center.y - r),
            NSSize::new(r * 2.0, r * 2.0),
        ))
    };
    color.set();
    let ring = circle(265.0);
    ring.appendBezierPath(&circle(207.0));
    ring.setWindingRule(NSWindingRule::EvenOdd);
    ring.fill();
    let slice = NSBezierPath::bezierPath();
    slice.moveToPoint(center);
    slice.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
        center,
        169.0 * unit,
        90.0,
        -145.0,
        true,
    );
    slice.closePath();
    slice.fill();
}

thread_local! {
    static URGENT: [Retained<NSColor>; 4] = [
        dynamic(Standing::RunningOut, 1.0),
        dynamic(Standing::UsedUp, 1.0),
        dynamic(Standing::RunningOut, DIMMED),
        dynamic(Standing::UsedUp, DIMMED),
    ];
}

const DIMMED: f64 = 0.45;

fn urgent(standing: Standing, dimmed: bool) -> Retained<NSColor> {
    let index = usize::from(standing == Standing::UsedUp) + if dimmed { 2 } else { 0 };
    URGENT.with(|colors| colors[index].clone())
}

fn dynamic(standing: Standing, fraction: f64) -> Retained<NSColor> {
    let srgb = |dark: bool, more: bool| {
        let (r, g, b) = crate::style::urgency_srgb(standing, dark, more);
        NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, fraction)
    };
    let colors = [
        srgb(false, false),
        srgb(true, false),
        srgb(false, true),
        srgb(true, true),
    ];
    let names = unsafe {
        NSArray::from_slice(&[
            NSAppearanceNameAqua,
            NSAppearanceNameDarkAqua,
            NSAppearanceNameAccessibilityHighContrastAqua,
            NSAppearanceNameAccessibilityHighContrastDarkAqua,
        ])
    };
    // The block owns the colors and appearance names, and the dynamic color owns the block.
    let provider = RcBlock::new(
        move |appearance: NonNull<NSAppearance>| -> NonNull<NSColor> {
            let appearance = unsafe { appearance.as_ref() };
            let best = appearance.bestMatchFromAppearancesWithNames(&names);
            let is = |name: &NSString| best.as_deref().is_some_and(|b| b.isEqualToString(name));
            let picked = unsafe {
                if is(NSAppearanceNameAccessibilityHighContrastDarkAqua) {
                    &colors[3]
                } else if is(NSAppearanceNameAccessibilityHighContrastAqua) {
                    &colors[2]
                } else if is(NSAppearanceNameDarkAqua) {
                    &colors[1]
                } else {
                    &colors[0]
                }
            };
            NonNull::from(&**picked)
        },
    );
    unsafe { NSColor::colorWithName_dynamicProvider(None, &provider) }
}

/// Shows the item pressed while the panel is open.
pub fn highlight(on: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    ITEM.with(|cell| {
        if let Some(item) = &*cell.borrow()
            && let Some(button) = item.item.button(mtm)
        {
            button.highlight(on);
        }
    });
}

/// The item's window and its screen, once the item has its place in the menu bar: in the top
/// half of a screen. Until then, just after launch, the window is below the screen.
fn item_window(mtm: MainThreadMarker) -> Option<(Retained<NSWindow>, Retained<NSScreen>)> {
    ITEM.with(|cell| {
        let cell = cell.borrow();
        let window = cell.as_ref()?.item.button(mtm)?.window()?;
        let screen = window.screen()?;
        let frame = screen.frame();
        let placed = window.frame().origin.y > frame.origin.y + frame.size.height / 2.0;
        placed.then_some((window, screen))
    })
}

/// Whether the item has its place in the menu bar, to hang the panel from.
pub fn placed() -> bool {
    MainThreadMarker::new().is_some_and(|mtm| item_window(mtm).is_some())
}

/// Where the panel's window goes, as Rune places a window on the primary screen: in points
/// from that screen's top-left, y down. One space of points spans every screen, whatever its
/// scale. Turned into pixels with the item's screen's scale, the panel landed at half or twice
/// its place when the window was on a screen of another scale, as a Retina laptop's beside a
/// 1x display. With no place for the item, the panel is at the top right of the primary screen.
pub fn panel_place(width: f64, gutter: f64) -> Option<rune::OverlayPlacement> {
    let mtm = MainThreadMarker::new()?;
    let primary = NSScreen::screens(mtm).firstObject()?;
    let (mid, under, visible) = match item_window(mtm) {
        Some((window, screen)) => {
            let at = window.frame();
            (
                at.origin.x + at.size.width / 2.0,
                at.origin.y,
                screen.visibleFrame(),
            )
        }
        None => {
            let visible = primary.visibleFrame();
            (
                visible.origin.x + visible.size.width,
                visible.origin.y + visible.size.height,
                visible,
            )
        }
    };
    let [x, y, width, height] = panel_rect(
        mid,
        under,
        visible,
        primary.frame().size.height,
        width,
        gutter,
    );
    Some(rune::OverlayPlacement::Rect {
        x: x as f32,
        y: y as f32,
        width: width as f32,
        height: height as f32,
    })
}

/// The panel's window, `[x, y, width, height]` in points from the primary screen's top-left,
/// y down. The panel, `width` points wide, hangs centered under `mid`, 6 points below `under`,
/// the item's bottom edge, kept 6 points inside `visible`, the visible frame of the item's
/// screen. `gutter` points beside it hold its shadow, and the window runs to the bottom of
/// `visible`. `mid`, `under` and `visible` are AppKit's, y up from the primary screen's
/// bottom-left; `primary_height` is that screen's height.
fn panel_rect(
    mid: f64,
    under: f64,
    visible: NSRect,
    primary_height: f64,
    width: f64,
    gutter: f64,
) -> [f64; 4] {
    // Not `clamp`, which panics on a screen narrower than the panel: there it's at the left.
    let left = (mid - width / 2.0)
        .min(visible.origin.x + visible.size.width - width - 6.0)
        .max(visible.origin.x + 6.0);
    let top = under - 6.0;
    [
        left - gutter,
        primary_height - top,
        width + 2.0 * gutter,
        top - visible.origin.y,
    ]
}

/// The height of the screen the panel opens on (`panel_place`), below the menu bar, in points.
pub fn screen_height() -> Option<f64> {
    let mtm = MainThreadMarker::new()?;
    let screen = match item_window(mtm) {
        Some((_, screen)) => screen,
        None => NSScreen::screens(mtm).firstObject()?,
    };
    Some(screen.visibleFrame().size.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
        NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
    }

    /// A Retina laptop's screen, 1512 by 982 points, as the primary, with a 33-point menu bar
    /// and the Dock hidden.
    const LAPTOP: f64 = 982.0;
    fn laptop() -> NSRect {
        rect(0.0, 0.0, 1512.0, LAPTOP - 33.0)
    }

    #[test]
    fn the_panel_hangs_under_the_item_and_runs_to_the_bottom_of_the_screen() {
        // An item centered at 1200, its bottom at the menu bar's.
        let [x, y, width, height] = panel_rect(1200.0, 949.0, laptop(), LAPTOP, 340.0, 28.0);
        assert_eq!(x, 1200.0 - 170.0 - 28.0);
        // 6 points below the menu bar, which is 33 tall.
        assert_eq!(y, 39.0);
        assert_eq!(width, 396.0);
        assert_eq!(height, 943.0);
        // With the Dock at the bottom, the window stops above it.
        let docked = rect(0.0, 80.0, 1512.0, LAPTOP - 33.0 - 80.0);
        assert_eq!(
            panel_rect(1200.0, 949.0, docked, LAPTOP, 340.0, 28.0)[3],
            863.0
        );
    }

    #[test]
    fn the_panel_stays_inside_the_screen() {
        let [x, ..] = panel_rect(1500.0, 949.0, laptop(), LAPTOP, 340.0, 28.0);
        assert_eq!(x + 28.0 + 340.0, 1512.0 - 6.0);
        let [x, ..] = panel_rect(10.0, 949.0, laptop(), LAPTOP, 340.0, 28.0);
        assert_eq!(x + 28.0, 6.0);
        // A screen narrower than the panel has it at the left.
        let narrow = rect(0.0, 0.0, 300.0, 949.0);
        let [x, ..] = panel_rect(150.0, 949.0, narrow, LAPTOP, 340.0, 28.0);
        assert_eq!(x + 28.0, 6.0);
    }

    #[test]
    fn on_a_screen_of_another_scale_the_panel_is_placed_in_points() {
        // A 1x display, 1920 by 1080 points, above the laptop and to its left, with a 25-point
        // menu bar. The item is there, centered at -300. Points are the same on both screens,
        // so nothing here depends on either's scale.
        let display = rect(-1920.0, LAPTOP, 1920.0, 1080.0 - 25.0);
        let under = LAPTOP + 1080.0 - 25.0;
        let [x, y, width, height] = panel_rect(-300.0, under, display, LAPTOP, 340.0, 28.0);
        assert_eq!(x, -300.0 - 170.0 - 28.0);
        // Above the laptop's top, 25 + 6 points below the display's.
        assert_eq!(y, -1080.0 + 25.0 + 6.0);
        assert_eq!(width, 396.0);
        assert_eq!(height, 1080.0 - 25.0 - 6.0);
    }
}
