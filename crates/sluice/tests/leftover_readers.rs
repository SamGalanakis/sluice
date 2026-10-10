//! No reader of the stored plan document survives the cutover (`docs/design/plan-rows.md` §9):
//! outside the converter, lane H's reference crate, the fixtures and the rehearsal harness
//! (which is built against the old release), no source, test, script or agent doc names
//! `plans.doc`, `doc FROM plans`, `.document()`, `transport()`, `Plan::parse`,
//! `PatchOperation`, `plan_patch` or `FrozenPlan`.
//!
//! It holds only once the whole group has moved its readers, so it is ignored until
//! `rw/pn-cutover` holds them. Run there with
//! `cargo test -p sluice --test leftover_readers -- --include-ignored`.

use std::path::{Path, PathBuf};

const PATTERNS: [&str; 8] = [
    "plans.doc",
    "doc FROM plans",
    ".document()",
    "transport()",
    "Plan::parse",
    "PatchOperation",
    "plan_patch",
    "FrozenPlan",
];

/// What may still name the legacy document: the converter's legacy replay and its tests, lane
/// H's test-only reference, the files that build or read legacy homes on purpose, and the
/// tests that prove the removed tool is unknown.
const EXEMPT: [&str; 13] = [
    "crates/sluice-store/src/convert.rs",
    "crates/sluice-reference/",
    "crates/sluice-model/tests/fixtures/",
    "crates/sluice-store/tests/legacy_replay.rs",
    // the converter's own tests, which read the legacy homes they convert
    "crates/sluice-store/tests/convert.rs",
    // the tests that hold plan_patch unknown to every transport and the command decoder
    "crates/sluice/tests/tool_contracts.rs",
    "crates/sluice-model/tests/command_plan_rows.rs",
    "crates/sluice/tests/schema_migrate.rs",
    "crates/sluice/tests/leftover_readers.rs",
    "tests/fixtures/",
    "tools/",
    "docs/design/",
    "target/",
];

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "node_modules" || name == "__pycache__" {
            continue;
        }
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            if !EXEMPT.iter().any(|e| format!("{relative}/").starts_with(e)) {
                walk(root, &path, out);
            }
        } else if kind.is_file()
            && !EXEMPT.iter().any(|e| relative.starts_with(e))
            && [
                ".rs", ".py", ".md", ".json", ".sql", ".toml", ".html", ".js", "",
            ]
            .iter()
            .any(|ext| name.ends_with(ext) && (!ext.is_empty() || !name.contains('.')))
        {
            out.push(path);
        }
    }
}

#[test]
#[ignore = "plan-rows integration gate: run on rw/pn-cutover once every lane moved its readers"]
fn no_reader_of_the_stored_plan_document_is_left() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = root.canonicalize().unwrap();
    let mut files = Vec::new();
    for dir in [
        "crates",
        "docs",
        "python",
        "scripts",
        "tests",
        "SPEC.md",
        "AGENTS.md",
    ] {
        let path = root.join(dir);
        if path.is_dir() {
            walk(&root, &path, &mut files);
        } else if path.is_file() {
            files.push(path);
        }
    }
    let mut left = Vec::new();
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            for pattern in PATTERNS {
                if line.contains(pattern) {
                    left.push(format!(
                        "{}:{}: {pattern}",
                        file.strip_prefix(&root).unwrap().display(),
                        number + 1
                    ));
                }
            }
        }
    }
    assert!(
        left.is_empty(),
        "{} readers of the stored plan document are left:\n{}",
        left.len(),
        left.join("\n")
    );
}
