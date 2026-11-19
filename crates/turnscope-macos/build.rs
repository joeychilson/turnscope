//! Builds the logos into the app: `logos.rs` lists each SVG in `assets/logos/` by
//! its path there without `.svg`, as `providers/anthropic`, so a new logo
//! needs no code and the app reads no files to draw one.

use std::path::Path;

fn main() {
    let logos = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/logos");
    println!("cargo::rerun-if-changed={}", logos.display());
    let mut found = Vec::new();
    for kind in ["agents", "providers"] {
        let folder = logos.join(kind);
        println!("cargo::rerun-if-changed={}", folder.display());
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "svg")
                && let Some(name) = path.file_stem().and_then(|stem| stem.to_str())
            {
                found.push(format!(
                    "    ({:?}, include_str!({:?})),",
                    format!("{kind}/{name}"),
                    path.display().to_string()
                ));
            }
        }
    }
    found.sort();
    let table = format!(
        "/// Every logo in `assets/logos/`: its path there and its SVG.\npub static LOGOS: &[(&str, &str)] = &[\n{}\n];\n",
        found.join("\n")
    );
    let out = Path::new(&std::env::var("OUT_DIR").unwrap_or_default()).join("logos.rs");
    if let Err(error) = std::fs::write(&out, table) {
        panic!("couldn't write {}: {error}", out.display());
    }
}
