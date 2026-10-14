//! The one stylesheet every page loads (`/static/style.css`): `assets/style.css` (Sluice's
//! tokens, its faces and every page's rules), then each theme family's file in
//! `assets/themes/`, by file name. A family's file comes after Sluice's defaults, so what it
//! declares wins; what it leaves out inherits Sluice's. The joined sheet is what the pages get
//! and what the tests read (`views::STYLESHEET`).
//!
//! The faces are Sluice's own files (`assets/fonts/`, each family's woff2 files beside its
//! licence, every file's sha256 in `assets/fonts/SHA256SUMS`). A rule names a face by its file
//! (`url(/static/fonts/<file>.woff2)`) and this versions the URL with the first 16 hex digits of
//! the file's sha256, the fingerprint the dashboard serves it under for good; it writes the faces'
//! asset list (`fonts.rs`) for the dashboard to embed. A rule naming a file the manifest lacks,
//! or a vendored face no rule names, stops the build.
use std::{collections::BTreeMap, env, fs, path::PathBuf};

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
        let text = fs::read_to_string(&family).expect("a theme family's file");
        css.push_str(&format!("\n/* ==== themes/{name} ==== */\n"));
        css.push_str(&text);
    }
    let fonts = assets.join("fonts");
    let manifest = fonts.join("SHA256SUMS");
    println!("cargo::rerun-if-changed={}", manifest.display());
    let faces = vendored_faces(&fs::read_to_string(&manifest).expect("assets/fonts/SHA256SUMS"));
    let css = version_faces(&css, &faces);
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    fs::write(out.join("style.css"), css).expect("the joined stylesheet is written");
    let mut list = String::from("&[\n");
    for (name, (path, _)) in &faces {
        let file = fonts.join(path);
        println!("cargo::rerun-if-changed={}", file.display());
        list.push_str(&format!(
            "    Asset {{ names: &[{:?}], media_type: \"font/woff2\", bytes: include_bytes!({:?}) }},\n",
            format!("fonts/{name}"),
            file.display().to_string(),
        ));
    }
    list.push_str("]\n");
    fs::write(out.join("fonts.rs"), list).expect("the faces' asset list is written");
}

/// Each vendored face in the manifest (`<sha256>  <family>/<file>.woff2` a line), by file name:
/// its path under `assets/fonts/` and its sha256.
fn vendored_faces(manifest: &str) -> BTreeMap<String, (String, String)> {
    let mut faces = BTreeMap::new();
    for line in manifest.lines().filter(|l| l.ends_with(".woff2")) {
        let (sha, path) = line
            .split_once("  ")
            .expect("a manifest line: <sha256>  <path>");
        assert!(
            sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
            "{line}: a sha256"
        );
        let name = path.rsplit('/').next().expect("a file name").to_owned();
        let earlier = faces.insert(name.clone(), (path.to_owned(), sha.to_owned()));
        assert!(earlier.is_none(), "two vendored faces named {name}");
    }
    faces
}

/// The sheet with each face's URL versioned by its file's sha256.
fn version_faces(css: &str, faces: &BTreeMap<String, (String, String)>) -> String {
    const URL: &str = "url(/static/fonts/";
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    let mut used = std::collections::BTreeSet::new();
    while let Some(at) = rest.find(URL) {
        let start = at + URL.len();
        let end = start + rest[start..].find(')').expect("a url's end");
        let name = &rest[start..end];
        let (_, sha) = faces
            .get(name)
            .unwrap_or_else(|| panic!("{name}: a face the stylesheet names is not vendored"));
        used.insert(name);
        out.push_str(&rest[..end]);
        out.push_str("?v=");
        out.push_str(&sha[..16]);
        rest = &rest[end..];
    }
    out.push_str(rest);
    for name in faces.keys() {
        assert!(
            used.contains(name.as_str()),
            "{name}: a vendored face no rule names"
        );
    }
    out
}
