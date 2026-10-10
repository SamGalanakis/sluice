//! The contract's `validation.differential` fixture runs through the reference: each case's
//! operations applied to its base give exactly its candidate, and the reference refuses that
//! candidate with exactly its errors.

use indexmap::IndexMap;
use sluice_reference::{
    harness::{Verdict, differential_cases, differential_edit, run_differential},
    ops,
};

#[test]
fn every_differential_case_runs_through_the_reference() {
    let cases = differential_cases();
    assert!(cases.len() >= 5, "the fixture's cases");
    for case in &cases {
        let edit = differential_edit(case);
        let applied = ops::apply(
            &edit.base,
            &edit.ops,
            edit.start,
            &IndexMap::new(),
            &sluice_reference::harness::Open,
        )
        .unwrap_or_else(|e| panic!("{}: operations refused: {e:?}", case.case));
        assert_eq!(
            serde_json::to_string(&ops::document_of(&applied.rows)).unwrap(),
            serde_json::to_string(&case.candidate).unwrap(),
            "{}: the candidate",
            case.case
        );
        assert_eq!(
            run_differential(case),
            Verdict::Invalid(case.errors.clone()),
            "{}",
            case.case
        );
    }
}

/// The reference is a dev-dependency only: no crate of the workspace lists it among its
/// dependencies or build dependencies, so no binary links it.
#[test]
fn no_crate_depends_on_the_reference_outside_its_tests() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    for entry in std::fs::read_dir(&crates).unwrap() {
        let manifest = entry.unwrap().path().join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let mut section = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                section = line.to_owned();
            } else if line.starts_with("sluice-reference") {
                assert_eq!(
                    section,
                    "[dev-dependencies]",
                    "{} names sluice-reference outside [dev-dependencies]",
                    manifest.display()
                );
            }
        }
    }
}
