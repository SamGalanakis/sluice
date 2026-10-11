//! The one stylesheet every page loads (`/static/style.css`): `assets/style.css` (Sluice's
//! tokens, its faces and every page's rules), then each theme family's file in
//! `assets/themes/`, by file name. A family's file comes after Sluice's defaults, so what it
//! declares wins; what it leaves out inherits Sluice's. The joined sheet is what the pages get
//! and what the tests read (`views::STYLESHEET`).
use std::{env, fs, path::PathBuf};

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
    for family in families {
        println!("cargo::rerun-if-changed={}", family.display());
        let name = family.file_name().expect("a file name").to_string_lossy();
        css.push_str(&format!("\n/* ==== themes/{name} ==== */\n"));
        css.push_str(&fs::read_to_string(&family).expect("a theme family's file"));
    }
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("style.css");
    fs::write(out, css).expect("the joined stylesheet is written");
}
