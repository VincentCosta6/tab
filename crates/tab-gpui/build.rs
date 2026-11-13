//! Turns the design system's icon set (`src/design/icons.tsx`, 24x24
//! stroked paths) into SVG assets, so the GPUI app draws the same icons
//! from the same source.

use std::fmt::Write;
use std::path::PathBuf;

fn main() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../src/design/icons.tsx");
    println!("cargo:rerun-if-changed={}", src.display());
    let text = std::fs::read_to_string(&src).expect("src/design/icons.tsx");
    let body = text
        .split("const paths: Record<string, string> = {")
        .nth(1)
        .and_then(|s| s.split("\n};").next())
        .expect("the paths table in icons.tsx");
    let mut out = String::from("pub static ICONS: &[(&str, &str)] = &[\n");
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with("//") || line.is_empty() {
            continue;
        }
        let Some((name, rest)) = line.split_once(": \"") else {
            continue;
        };
        let name = name.trim_matches('"');
        let Some(d) = rest.strip_suffix("\",") else {
            continue;
        };
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"black\" stroke-width=\"1.75\" stroke-linecap=\"round\" stroke-linejoin=\"round\"><path d=\"{d}\"/></svg>"
        );
        writeln!(out, "    ({name:?}, {svg:?}),").unwrap();
    }
    out.push_str("];\n");
    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("icons.rs");
    std::fs::write(dest, out).unwrap();
}
