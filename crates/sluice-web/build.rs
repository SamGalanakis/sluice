//! The one stylesheet every page loads (`/static/style.css`): `assets/style.css` (Sluice's
//! tokens, its faces and every page's rules), then each theme family's file in
//! `assets/themes/`, by file name. A family's file comes after Sluice's defaults, so what it
//! declares wins; what it leaves out inherits Sluice's. The joined sheet is what the pages get
//! and what the tests read (`views::STYLESHEET`).
//!
//! Last, the display face's floor (DESIGN.md, Themes): a head drawn at N px times the head
//! scale reads its face from `--head-face-N`, and this works each one out, for Sluice's scale
//! and for every family block that sets its own, as the display face when N times the scale
//! comes to 19px or more and the text face under that.
use std::{env, fs, path::PathBuf};

/// The smallest size a display face sets text at, in px.
const DISPLAY_FLOOR: f64 = 19.0;

fn main() {
    let assets =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("cargo sets the manifest dir"))
            .join("assets");
    let base = assets.join("style.css");
    let themes = assets.join("themes");
    println!("cargo::rerun-if-changed={}", base.display());
    println!("cargo::rerun-if-changed={}", themes.display());
    let mut families: Vec<PathBuf> = fs::read_dir(&themes)
        .expect("assets/themes holds the theme families")
        .map(|entry| entry.expect("a theme file").path())
        .filter(|path| path.extension().is_some_and(|e| e == "css"))
        .collect();
    families.sort();
    let mut css = fs::read_to_string(&base).expect("assets/style.css");
    let sizes = head_sizes(&css);
    let scale = blocks(&css, ":root, [data-theme]")
        .into_iter()
        .find_map(head_scale)
        .expect("Sluice's head scale");
    let mut floor = format!(
        "\n/* ==== the display face's floor (build.rs) ==== */\n{}",
        faces(":root, [data-theme]", &sizes, scale)
    );
    for family in families {
        println!("cargo::rerun-if-changed={}", family.display());
        let name = family.file_name().expect("a file name").to_string_lossy();
        let text = fs::read_to_string(&family).expect("a theme family's file");
        for (selector, body) in theme_blocks(&text) {
            if let Some(scale) = head_scale(body) {
                floor.push_str(&faces(&format!("[data-theme]{selector}"), &sizes, scale));
            }
        }
        css.push_str(&format!("\n/* ==== themes/{name} ==== */\n"));
        css.push_str(&text);
    }
    css.push_str(&floor);
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("style.css");
    fs::write(out, css).expect("the joined stylesheet is written");
}

/// Every N the sheet's heads read `--head-face-N` for, smallest first.
fn head_sizes(css: &str) -> Vec<u32> {
    let mut sizes: Vec<u32> = css
        .match_indices("var(--head-face-")
        .map(|(at, needle)| {
            let digits: String = css[at + needle.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse().expect("a head face's size in px")
        })
        .collect();
    sizes.sort_unstable();
    sizes.dedup();
    sizes
}

/// The body of each rule whose selector is exactly `selector`.
fn blocks<'a>(css: &'a str, selector: &str) -> Vec<&'a str> {
    let needle = format!("\n{selector} {{");
    css.match_indices(&needle)
        .map(|(at, _)| {
            let start = at + needle.len();
            &css[start..start + css[start..].find("\n}").expect("a rule's end")]
        })
        .collect()
}

/// A family file's theme rules (`[data-theme^="<family>-"]` and `[data-theme="<id>"]`): each
/// selector with its body.
fn theme_blocks(css: &str) -> Vec<(&str, &str)> {
    let mut out = vec![];
    let mut rest = css;
    while let Some(at) = rest.find("\n[data-theme") {
        let line = &rest[at + 1..];
        let Some(open) = line.find(" {") else { break };
        let Some(close) = line.find("\n}") else {
            break;
        };
        out.push((&line[..open], &line[open + 2..close]));
        rest = &line[close..];
    }
    out
}

/// The `--head-scale` a rule's body sets, if it sets one.
fn head_scale(body: &str) -> Option<f64> {
    let at = body.find("--head-scale:")?;
    let value = &body[at + "--head-scale:".len()..];
    let value = value[..value.find(';')?].trim();
    Some(value.parse().expect("a head scale is a number"))
}

/// The rule that sets each `--head-face-N` at `scale` for the elements `selector` matches.
fn faces(selector: &str, sizes: &[u32], scale: f64) -> String {
    let mut rule = format!("{selector} {{\n");
    for size in sizes {
        let face = if f64::from(*size) * scale >= DISPLAY_FLOOR - 1e-9 {
            "display"
        } else {
            "text"
        };
        rule.push_str(&format!("  --head-face-{size}: var(--font-{face});\n"));
    }
    rule.push_str("}\n");
    rule
}
