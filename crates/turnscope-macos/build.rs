//! Builds `logos.rs`: for each SVG in `assets/logos/agents` and `providers`, its name and a
//! `rune::Icon` for the panel, and for a provider the SVG itself, which the menu bar item draws
//! with AppKit. A new logo needs no code.

use std::path::Path;

/// The viewBox's width and what's inside `<svg>`. Rune's icons have their viewBox at the
/// origin, so one that isn't is moved there.
fn icon(svg: &str) -> Option<(f32, String)> {
    let view_box = svg.split("viewBox=\"").nth(1)?.split('"').next()?;
    let numbers: Vec<f32> = view_box
        .split_whitespace()
        .map(|n| n.parse().ok())
        .collect::<Option<_>>()?;
    let [x, y, width, _height] = numbers[..] else {
        return None;
    };
    let body = &svg[svg.find('>')? + 1..svg.rfind("</svg>")?];
    if x == 0.0 && y == 0.0 {
        return Some((width, body.to_owned()));
    }
    Some((
        width,
        format!("<g transform=\"translate({} {})\">{body}</g>", -x, -y),
    ))
}

fn main() {
    let logos = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/logos");
    println!("cargo::rerun-if-changed={}", logos.display());
    let mut out = String::new();
    for (kind, table, with_svg) in [
        ("agents", "AGENTS", false),
        ("providers", "PROVIDERS", true),
    ] {
        let folder = logos.join(kind);
        let mut files: Vec<_> = std::fs::read_dir(&folder)
            .unwrap_or_else(|error| panic!("couldn't read {}: {error}", folder.display()))
            .map(|entry| entry.expect("a logo folder's entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "svg"))
            .collect();
        files.sort();
        let svg_type = if with_svg { ", &str" } else { "" };
        out += &format!("pub static {table}: &[(&str, rune::Icon{svg_type})] = &[\n");
        for path in files {
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .expect("a logo's file name");
            let svg = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("couldn't read {}: {error}", path.display()));
            let (view, body) = icon(&svg)
                .unwrap_or_else(|| panic!("{} has no viewBox of 4 numbers", path.display()));
            let key = format!("{kind}/{name}");
            out += &format!(
                "    ({name:?}, rune::Icon {{ name: {key:?}, view: {view:?}f32, body: {body:?} }}"
            );
            if with_svg {
                out += &format!(", include_str!({:?})", path.display().to_string());
            }
            out += "),\n";
        }
        out += "];\n";
    }
    let file = Path::new(&std::env::var("OUT_DIR").expect("Cargo sets OUT_DIR")).join("logos.rs");
    std::fs::write(&file, out)
        .unwrap_or_else(|error| panic!("couldn't write {}: {error}", file.display()));
}
