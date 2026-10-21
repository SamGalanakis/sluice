//! The kit is the only way a page draws a button, a section's head or a write (DESIGN.md,
//! Components): `ui::button`/`ui::link_with` (`ui::Btn`), `ui::SectionHead`/`ui::section_head`
//! and `ui::post_form`. The bug this catches: a hand-written `class="btn"`, `<h2>` or
//! `data-on:submit` drifting from the kit's size, tone, busy state and failure notice, as the
//! plan's heads, Mark as seen and the settings' headings once did.
use std::path::{Path, PathBuf};

/// Raw markup the kit owns, as it is written in a template and in a Rust string.
const RAW: [(&str, &str); 4] = [
    ("class=\"btn", "a button: ui::button or ui::link_with"),
    ("class=\\\"btn", "a button: ui::button or ui::link_with"),
    ("<h2", "a section's head: ui::SectionHead"),
    ("data-on:submit", "a write: ui::post_form"),
];
/// The few that are the kit's exceptions, each with why: (file, the line's distinctive text).
const ALLOWED: [(&str, &str, &str); 6] = [
    (
        "src/views/panel.rs",
        "<h2 id=\\\"board-h\\\"",
        "the board's head is its program's title beside its icon, the board column's name, not a section of the page",
    ),
    (
        "templates/step_band.html",
        "<h2 id=\"d-title\"",
        "a step's title in the drawer: the page's h1 is the plan's, so the drawer's own title is a level down",
    ),
    (
        "src/views/step.rs",
        "<h2 id=\\\"d-title\\\"",
        "the same drawer title, for a step the plan no longer has",
    ),
    (
        "src/views/plan.rs",
        "<h2 id=\\\"at-work\\\">At work</h2>",
        "At work's head stands on its rows' grid, the strip's window over the strip column, which a section head's row cannot",
    ),
    (
        "src/views/gallery.rs",
        "<h2>At work</h2>",
        "the gallery's stand-in for that same head",
    ),
    (
        "templates/side_nav.html",
        "<h2 class=\"sb-sec\">Projects</h2>",
        "the sidebar's list label, not a section of the page",
    ),
];

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            // the kit itself
            if !path.ends_with("views/ui") {
                files(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs" || e == "html") {
            out.push(path);
        }
    }
}

#[test]
fn pages_draw_buttons_heads_and_writes_through_the_kit() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = vec![];
    files(&root.join("src"), &mut paths);
    files(&root.join("templates"), &mut paths);
    let mut found = vec![];
    for path in paths {
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let text = std::fs::read_to_string(&path).unwrap();
        for (n, line) in text.lines().enumerate() {
            for (raw, kit) in RAW {
                if !line.contains(raw) {
                    continue;
                }
                let allowed = ALLOWED
                    .iter()
                    .any(|(file, at, _)| rel == *file && line.contains(at));
                if !allowed {
                    found.push(format!("{rel}:{}: raw `{raw}` ({kit})", n + 1));
                }
            }
        }
    }
    found.dedup();
    assert!(found.is_empty(), "{}", found.join("\n"));
}
