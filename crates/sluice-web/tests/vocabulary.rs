//! The generic rule (DESIGN.md, The generic rule): the dashboard knows only sluice's own
//! concepts, so no project's vocabulary appears in its view code. This reads everything the
//! dashboard serves or renders from (`src/`, `templates/`, `assets/`, but the vendored scripts
//! and Lucide's icons) and fails on any of one project's words outside a test module. Tests and
//! fixtures may use them; the gallery's sample data may not.
use std::path::{Path, PathBuf};

/// A guarded word: its letters, and whether it must stand alone (a word boundary after it too).
/// "fig" takes a digit after its hyphen ("fig-5492") or stands alone; "lash" and "fork" start a
/// word ("lashlang", "forked"); "kiln" anywhere; "landed" and "main" alone.
const WORDS: &[(&str, Boundary)] = &[
    ("fig-", Boundary::Digit),
    ("fig", Boundary::Word),
    ("lash", Boundary::Start),
    ("kiln", Boundary::Anywhere),
    ("landed", Boundary::Word),
    ("fork", Boundary::Start),
    ("main", Boundary::Word),
];
#[derive(Clone, Copy, PartialEq)]
enum Boundary {
    Anywhere,
    Start,
    Word,
    Digit,
}
/// Where "main" is generic, each with why: what sits right before and after it.
const MAIN_ALLOWED: &[(&str, &str, &str)] = &[
    ("<", "", "the HTML <main> element"),
    ("</", "", "the HTML <main> element, closed"),
    (
        "querySelector(\"",
        "",
        "a selector for the HTML <main> element",
    ),
    (
        "querySelectorAll(\"",
        "",
        "a selector for the HTML <main> element",
    ),
    ("const ", "", "a variable holding the <main> element"),
    ("aria-label=\"", "\"", "the band's nav landmark, named Main"),
    (
        "aria-label=\\\"",
        "\\\"",
        "the band's nav landmark, named Main, in a Rust string",
    ),
    (
        "",
        ".py",
        "a Python fn's entry file, as sluice lays fns out",
    ),
    ("", "_py", "the fn tool's argument for that file"),
    ("(", ")", "a Python fn's entry function, run(main)"),
    ("", "?.", "a variable holding the <main> element"),
    ("", ".setAttribute", "a variable holding the <main> element"),
    (
        "",
        ".removeAttribute",
        "a variable holding the <main> element",
    ),
    ("", "[", "a selector for the HTML <main> element"),
    ("", " {", "a CSS rule for the HTML <main> element"),
    ("", " >", "a CSS selector under the HTML <main> element"),
    ("", " :", "a CSS selector under the HTML <main> element"),
    ("", " .", "a CSS selector under the HTML <main> element"),
    ("", " #", "a CSS selector under the HTML <main> element"),
    ("", " [", "a CSS selector under the HTML <main> element"),
    (
        "",
        " a[",
        "a selector for a link under the HTML <main> element",
    ),
    (
        "",
        ",",
        "a CSS selector list naming the HTML <main> element",
    ),
    ("", "\")", "a selector for the HTML <main> element"),
    ("", "#", "a selector for the HTML <main> element by its id"),
];

fn word_char(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || c == '_')
}
/// Every place in `line` a guarded word stands, as (start, end, word).
fn hits(line: &str) -> Vec<(usize, usize, &'static str)> {
    let lower = line.to_lowercase();
    let mut out = vec![];
    for (word, boundary) in WORDS {
        let mut from = 0;
        while let Some(at) = lower[from..].find(word) {
            let start = from + at;
            let end = start + word.len();
            from = end;
            let before = lower[..start].chars().next_back();
            let after = lower[end..].chars().next();
            let ok = match boundary {
                Boundary::Anywhere => true,
                Boundary::Start => !word_char(before),
                Boundary::Word => !word_char(before) && !word_char(after) && after != Some('-'),
                Boundary::Digit => !word_char(before) && after.is_some_and(|c| c.is_ascii_digit()),
            };
            if ok {
                out.push((start, end, *word));
            }
        }
    }
    out
}
fn generic(line: &str, start: usize, end: usize, word: &str) -> bool {
    word == "main"
        && MAIN_ALLOWED.iter().any(|(before, after, _)| {
            line[..start].ends_with(before) && line[end..].starts_with(after)
        })
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("rs" | "html" | "js" | "css" | "svg")
        ) {
            out.push(path);
        }
    }
}
/// A Rust file's text before its first top-level `#[cfg(test)]`: its tests stay out.
fn code(path: &Path, text: &str) -> String {
    if path.extension().and_then(|e| e.to_str()) == Some("rs")
        && let Some(at) = text.find("\n#[cfg(test)]")
    {
        return text[..at].to_owned();
    }
    text.to_owned()
}

#[test]
fn no_project_vocabulary_in_the_view_code() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut all = vec![];
    for dir in ["src", "templates", "assets"] {
        files(&root.join(dir), &mut all);
    }
    // the vendored scripts and Lucide's icons are someone else's, served as published
    all.retain(|p| {
        let name = p.file_name().unwrap().to_string_lossy();
        !["datastar-rocket-", "zod-", "lang-core-"]
            .iter()
            .any(|v| name.starts_with(v))
            && !p.components().any(|c| c.as_os_str() == "icons")
    });
    assert!(all.len() > 30, "{all:?}");
    let mut found = vec![];
    for path in &all {
        let text = code(path, &std::fs::read_to_string(path).unwrap());
        for (n, line) in text.lines().enumerate() {
            for (start, end, word) in hits(line) {
                if !generic(line, start, end, word) {
                    found.push(format!(
                        "{}:{}: {word}: {}",
                        path.strip_prefix(root).unwrap().display(),
                        n + 1,
                        line.trim()
                    ));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "one project's words in the view code:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_guard_finds_what_it_guards_and_lets_the_page_landmark_be() {
    let caught = |line: &str| {
        hits(line)
            .into_iter()
            .any(|(s, e, w)| !generic(line, s, e, w))
    };
    for line in [
        "see FIG-5492",
        "fig-5492-work",
        "the lash plan",
        "lashlang",
        "kiln check",
        "it landed",
        "a fork",
        "on main",
        "origin/main",
    ] {
        assert!(caught(line), "{line}");
    }
    for line in [
        "a figure",
        "flash",
        "landing page",
        "the domain",
        "<main id=\"content\">",
        "document.querySelector(\"main\")",
        "main > * { grid-column: col; }",
        "aria-label=\"Main\"",
        "const main = mainStream();",
    ] {
        assert!(!caught(line), "{line}");
    }
}
