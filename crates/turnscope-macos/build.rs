//! Builds the logos into the app: `logos.rs` emits each SVG in `assets/logos/`
//! as a pre-parsed static `rune::Icon` (for the panel) and raw SVG (for the
//! menu bar), so a new logo needs no code and the app parses nothing at runtime.

use std::fmt::Write as _;
use std::path::Path;

fn parse_icon(svg: &str) -> Option<(f32, String)> {
    let view_box = svg.split("viewBox=\"").nth(1)?.split('"').next()?;
    let mut nums = view_box
        .split_whitespace()
        .filter_map(|n| n.parse::<f32>().ok());
    let (x, y, width) = (nums.next()?, nums.next()?, nums.next()?);
    let _height = nums.next()?;
    if nums.next().is_some() {
        return None;
    }
    let body = &svg[svg.find('>')? + 1..svg.rfind("</svg>")?];
    // Rune's icons have their viewBox at the origin: move one that isn't.
    let body = if x != 0.0 || y != 0.0 {
        format!("<g transform=\"translate({} {})\">{body}</g>", -x, -y)
    } else {
        body.to_owned()
    };
    Some((width, body))
}

fn main() {
    let logos = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/logos");
    println!("cargo::rerun-if-changed={}", logos.display());
    let mut provider_svgs = Vec::new();
    let mut provider_icons = Vec::new();
    let mut agent_icons = Vec::new();
    for kind in ["agents", "providers"] {
        let folder = logos.join(kind);
        println!("cargo::rerun-if-changed={}", folder.display());
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        let mut files: Vec<_> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "svg"))
            .collect();
        files.sort();
        for path in files {
            println!("cargo::rerun-if-changed={}", path.display());
            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let svg = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("couldn't read {}: {error}", path.display()));
            if kind == "providers" {
                provider_svgs.push((name.to_owned(), path.display().to_string()));
            }
            let key = format!("{kind}/{name}");
            if let Some((view, body)) = parse_icon(&svg) {
                let entry = (name.to_owned(), key, view, body);
                if kind == "providers" {
                    provider_icons.push(entry);
                } else {
                    agent_icons.push(entry);
                }
            }
        }
    }
    let mut out = String::new();
    let _ = writeln!(out, "pub static PROVIDER_SVGS: &[(&str, &str)] = &[");
    for (name, path) in &provider_svgs {
        let _ = writeln!(out, "    ({name:?}, include_str!({path:?})),");
    }
    let _ = writeln!(out, "];\n");
    for (table, entries) in [
        ("PROVIDER_ICONS", &provider_icons),
        ("AGENT_ICONS", &agent_icons),
    ] {
        let _ = writeln!(out, "pub static {table}: &[(&str, rune::Icon)] = &[");
        for (name, key, view, body) in entries {
            let _ = writeln!(
                out,
                "    ({name:?}, rune::Icon {{ name: {key:?}, view: {view:?}f32, body: {body:?} }}),"
            );
        }
        let _ = writeln!(out, "];\n");
    }
    let file = Path::new(&std::env::var("OUT_DIR").unwrap_or_default()).join("logos.rs");
    if let Err(error) = std::fs::write(&file, out) {
        panic!("couldn't write {}: {error}", file.display());
    }
}
