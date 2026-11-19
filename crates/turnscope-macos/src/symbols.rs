//! SF Symbols, drawn by AppKit into pixels Rune shows. Rune can't draw them itself: they're not
//! in a font it can read. AppKit draws each once per name, size, weight and color.

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSBitmapImageRep, NSColor, NSDeviceRGBColorSpace, NSFontWeight, NSFontWeightMedium,
    NSFontWeightRegular, NSFontWeightSemibold, NSGraphicsContext, NSImage,
    NSImageSymbolConfiguration,
};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString};
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weight {
    Regular,
    Medium,
    Semibold,
}

/// A symbol drawn: its pixels and its size in points.
#[derive(Clone)]
pub struct Symbol {
    pub pixels: rune::Pixels,
    pub size: (f32, f32),
}

type Key = (&'static str, u32, Weight, bool);

thread_local! {
    static DRAWN: RefCell<HashMap<Key, Symbol>> = RefCell::new(HashMap::new());
}

/// The symbol `name` at `points` and `weight`, opaque white if `white` or else black, at 2×. Its
/// element's opacity makes it a gray: a palette color with alpha came out with the alpha twice.
pub fn symbol(name: &'static str, points: f32, weight: Weight, white: bool) -> Symbol {
    let key = (name, points.to_bits(), weight, white);
    if let Some(s) = DRAWN.with(|d| d.borrow().get(&key).cloned()) {
        return s;
    }
    let s = draw(name, points, weight, white).unwrap_or_else(|| Symbol {
        pixels: rune::Pixels::new(),
        size: (0.0, 0.0),
    });
    DRAWN.with(|d| d.borrow_mut().insert(key, s.clone()));
    s
}

fn draw(name: &str, points: f32, weight: Weight, white: bool) -> Option<Symbol> {
    let scale = 2.0;
    unsafe {
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(name),
            None,
        )?;
        let w: NSFontWeight = match weight {
            Weight::Regular => NSFontWeightRegular,
            Weight::Medium => NSFontWeightMedium,
            Weight::Semibold => NSFontWeightSemibold,
        };
        let sized = NSImageSymbolConfiguration::configurationWithPointSize_weight(points as f64, w);
        let v = if white { 1.0 } else { 0.0 };
        let color = NSColor::colorWithSRGBRed_green_blue_alpha(v, v, v, 1.0);
        let palette = NSImageSymbolConfiguration::configurationWithPaletteColors(
            &NSArray::from_retained_slice(&[color]),
        );
        let config = sized.configurationByApplyingConfiguration(&palette);
        let image = image.imageWithSymbolConfiguration(&config)?;
        let size: NSSize = image.size();
        let (pw, ph) = (
            (size.width * scale).ceil() as isize,
            (size.height * scale).ceil() as isize,
        );
        let rep: Retained<NSBitmapImageRep> = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            pw,
            ph,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            0,
            0,
        )?;
        rep.setSize(size);
        let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&context));
        image.drawInRect(NSRect::new(NSPoint::new(0.0, 0.0), size));
        context.flushGraphics();
        NSGraphicsContext::restoreGraphicsState_class();
        let data = rep.bitmapData();
        let stride = rep.bytesPerRow() as usize;
        let mut out = Vec::with_capacity((pw * ph * 4) as usize);
        for y in 0..ph as usize {
            for x in 0..pw as usize {
                let p = data.add(y * stride + x * 4);
                let a = *p.add(3);
                // The bitmap is premultiplied; Rune takes straight alpha.
                let un = |c: u8| {
                    if a == 0 {
                        0
                    } else {
                        ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8
                    }
                };
                out.extend_from_slice(&[un(*p), un(*p.add(1)), un(*p.add(2)), a]);
            }
        }
        let pixels = rune::Pixels::new();
        pixels.set(pw as u32, ph as u32, out);
        Some(Symbol {
            pixels,
            size: (size.width as f32, size.height as f32),
        })
    }
}
