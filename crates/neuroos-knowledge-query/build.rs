//! C7 / design.md §3: `ui/tokens/tokens.json` is the single source of truth for
//! every colour in the product, and nothing outside it may hard-code a hex
//! value. The graph view is a self-contained HTML file with no build step of
//! its own, so this script turns the token file into the CSS custom-property
//! blocks that `graph_view.rs` embeds (design.md §11: "build generates
//! tokens.css").
//!
//! A token named `family.work` becomes `--family-work`, so the stylesheet and
//! the JavaScript that reads `--family-<family>` keep working unchanged.

// rules.md §5's no-panic rule is about the daemons. A build script has no
// caller to return an error to: failing loudly at build time is the only
// correct behaviour for a malformed or missing token file, and it is exactly
// what §5's "startup invariant" carve-out describes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::{env, fs};

const TOKENS: &str = "../../ui/tokens/tokens.json";

fn main() {
    println!("cargo:rerun-if-changed={TOKENS}");
    println!("cargo:rerun-if-changed=build.rs");

    let raw = fs::read_to_string(TOKENS)
        .unwrap_or_else(|e| panic!("design.md §3 token file {TOKENS} is unreadable: {e}"));
    let tokens: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{TOKENS} is not valid JSON: {e}"));

    let dark = theme(&tokens, "dark");
    let light = theme(&tokens, "light");
    assert_eq!(
        dark.keys().collect::<Vec<_>>(),
        light.keys().collect::<Vec<_>>(),
        "{TOKENS}: the dark and light themes must define the same token names"
    );

    // Light is the default `:root`, dark comes in under `prefers-color-scheme`,
    // and both `data-theme` blocks let the in-page toggle override the media
    // query (design.md §3.5).
    let mut css = String::new();
    css.push_str(&format!(":root {{\n{}}}\n", decls(&light)));
    css.push_str(&format!(
        "@media (prefers-color-scheme: dark) {{\n:root:not([data-theme=\"light\"]) {{\n{}}}\n}}\n",
        decls(&dark)
    ));
    css.push_str(&format!(
        ":root[data-theme=\"light\"] {{\n{}}}\n",
        decls(&light)
    ));
    css.push_str(&format!(
        ":root[data-theme=\"dark\"] {{\n{}}}\n",
        decls(&dark)
    ));

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo always sets OUT_DIR"))
        .join("design_tokens.css");
    fs::write(&out, css).unwrap_or_else(|e| panic!("cannot write {}: {e}", out.display()));
}

fn theme(tokens: &serde_json::Value, name: &str) -> BTreeMap<String, String> {
    tokens["color"][name]
        .as_object()
        .unwrap_or_else(|| panic!("{TOKENS}: color.{name} must be an object"))
        .iter()
        .map(|(k, v)| {
            let hex = v
                .as_str()
                .unwrap_or_else(|| panic!("{TOKENS}: color.{name}.{k} must be a string"));
            assert!(
                hex.len() == 7
                    && hex.starts_with('#')
                    && hex[1..].chars().all(|c| c.is_ascii_hexdigit()),
                "{TOKENS}: color.{name}.{k} = {hex:?} is not a #RRGGBB hex value"
            );
            (k.replace('.', "-"), hex.to_ascii_lowercase())
        })
        .collect()
}

fn decls(theme: &BTreeMap<String, String>) -> String {
    theme
        .iter()
        .map(|(name, hex)| format!("  --{name}: {hex};\n"))
        .collect()
}
