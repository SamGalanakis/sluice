//! The legacy fixtures (`tests/fixtures/legacy/` at the repository root) are what the old
//! release made and reads: each loads into a home the old store opens (the old backup and the
//! interim schema 2 brought forward as their writer would), the oracle replays every revision
//! to the document the old store stored, each defect case makes the oracle name its project
//! and revision, and the live home holds the live work its expectations count.

use cutover_rehearsal::{dump, legacy};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/legacy")
}
fn read(name: &str) -> String {
    std::fs::read_to_string(fixtures().join(name)).unwrap()
}
/// A home holding the fixture's database.
fn home(name: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    drop(dump::load(&home.join("sluice.db"), &read(&format!("{name}.sql"))).unwrap());
    (root, home)
}

fn check_oracle(sql: &rusqlite::Connection, expected: &Value) {
    let replayed = legacy::replay(sql).unwrap();
    let projects = expected["projects"].as_array().unwrap();
    let names: Vec<&str> = replayed.iter().map(|p| p.name.as_str()).collect();
    let expected_names: Vec<&str> = projects
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, expected_names, "deleted projects have no plan");
    for (oracle, stored) in replayed.iter().zip(projects) {
        assert!(
            oracle.blockers.is_empty(),
            "{}: {:?}",
            oracle.name,
            oracle.blockers
        );
        let stored = stored["revisions"].as_array().unwrap();
        assert_eq!(oracle.revisions.len(), stored.len());
        for (replayed, stored) in oracle.revisions.iter().zip(stored) {
            assert_eq!(replayed.rev, stored["rev"].as_u64().unwrap());
            assert_eq!(
                legacy::compact(&replayed.document),
                legacy::compact(stored["document"].as_object().unwrap()),
                "{} rev {}",
                oracle.name,
                replayed.rev
            );
        }
    }
}

#[test]
fn each_fixture_is_a_home_the_old_release_reads_and_its_history_replays_to_what_was_stored() {
    for (name, schema) in [
        ("schema1", 1),
        ("schema1_old", 1),
        ("schema2", 2),
        ("live", 1),
    ] {
        let expected: Value =
            serde_json::from_str(&read(&format!("{name}.expected.json"))).unwrap();
        assert_eq!(expected["from_schema"], schema, "{name}");
        let (_root, home) = home(name);
        let sql = rusqlite::Connection::open(home.join("sluice.db")).unwrap();
        let marked: i64 = sql
            .query_row("SELECT schema_version FROM home_meta", [], |r| r.get(0))
            .unwrap();
        assert_eq!(marked, schema, "{name}");
        check_oracle(&sql, &expected);
        drop(sql);
        // The old writer opens it (bringing an older one forward), then readers do.
        drop(sluice_store::Writer::open(&home).unwrap_or_else(|e| panic!("{name}: {e}")));
        drop(sluice_store::ReadPool::open(&home, 1).unwrap_or_else(|e| panic!("{name}: {e}")));
    }
    // The older homes lack the later columns until their writer adds them.
    let (_root, home) = home("schema1_old");
    let sql = rusqlite::Connection::open(home.join("sluice.db")).unwrap();
    let has = |table: &str, column: &str| -> bool {
        sql.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
            [table, column],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(has("projects", "board_rev") && !has("projects", "board_slots"));
    assert!(!has("steps", "progress") && !has("runs", "stopped"));
}

#[test]
fn the_fixtures_hold_what_the_conversion_is_tested_on() {
    let (_root, home) = home("schema1");
    let sql = rusqlite::Connection::open(home.join("sluice.db")).unwrap();
    let scalar = |query: &str| -> i64 { sql.query_row(query, [], |r| r.get(0)).unwrap() };
    // Trimmed records: some plan.edit records are gone, every plan_edits row is not.
    assert!(scalar("SELECT record_floor FROM home_meta") > 0);
    assert!(
        scalar("SELECT count(*) FROM plan_edits")
            > scalar("SELECT count(*) FROM records WHERE kind='plan.edit'")
    );
    // An input row the old projection normalized away from its authored spelling.
    assert_eq!(
        scalar(
            "SELECT count(*) FROM inputs WHERE declaration='{\"type\":\"boolean\",\"doc\":null}'"
        ),
        1
    );
    // Attempts carrying the completion snapshot twice, one of them disagreeing with history.
    assert_eq!(
        scalar(
            "SELECT count(*) FROM attempts WHERE json_type(request,'$.provenance.runtime.completion')='object' AND json_type(provenance,'$.runtime.completion')='object'"
        ),
        3
    );
    assert_eq!(
        scalar(
            "SELECT count(*) FROM attempts WHERE json_extract(request,'$.provenance.runtime.completion.document.steps.a.doc')='tampered'"
        ),
        1
    );
    // An archived project, a deleted one with no plan rows, a retired board slot.
    assert_eq!(
        scalar("SELECT count(*) FROM projects WHERE archived=1 AND deleted_at IS NULL"),
        1
    );
    assert_eq!(
        scalar(
            "SELECT count(*) FROM projects p WHERE deleted_at IS NOT NULL AND NOT EXISTS (SELECT 1 FROM plans WHERE project_id=p.project_id)"
        ),
        1
    );
    assert_eq!(
        scalar("SELECT count(*) FROM projects WHERE board_slots IS NOT NULL"),
        1
    );
}

#[test]
fn each_defect_makes_the_oracle_name_its_project_and_revision() {
    let defects: Vec<Value> = serde_json::from_str(&read("defects.json")).unwrap();
    for defect in defects {
        let case = defect["case"].as_str().unwrap();
        if !matches!(case, "gap" | "origin" | "patch" | "final") {
            continue; // Rows and records the oracle does not replay.
        }
        let (_root, home) = home("schema1");
        let sql = rusqlite::Connection::open(home.join("sluice.db")).unwrap();
        sql.execute_batch(defect["sql"].as_str().unwrap()).unwrap();
        let replayed = legacy::replay(&sql).unwrap();
        let project = defect["project"].as_str().unwrap();
        let blocked: Vec<&legacy::ProjectHistory> =
            replayed.iter().filter(|p| !p.blockers.is_empty()).collect();
        assert_eq!(blocked.len(), 1, "{case}: {blocked:?}");
        assert_eq!(blocked[0].name, project, "{case}");
        let rev = defect["rev"].as_u64().unwrap().to_string();
        assert!(
            blocked[0].blockers[0].contains(&rev),
            "{case}: {:?} names rev {rev}",
            blocked[0].blockers
        );
    }
}

#[test]
fn the_live_home_holds_the_live_work_it_counts() {
    let expected: Value = serde_json::from_str(&read("live.expected.json")).unwrap();
    let (_root, home) = home("live");
    let sql = rusqlite::Connection::open(home.join("sluice.db")).unwrap();
    let counts: (i64, i64, i64, i64) = sql
        .query_row(
            "SELECT (SELECT count(*) FROM attempts WHERE phase<>'terminal'),
                    (SELECT count(*) FROM runs WHERE finished_at IS NULL),
                    (SELECT count(*) FROM leases WHERE state IN ('waiting','held')),
                    (SELECT count(*) FROM calls WHERE status='running')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    let live = &expected["live"];
    assert_eq!(
        counts,
        (
            live["attempts"].as_i64().unwrap(),
            live["runs"].as_i64().unwrap(),
            live["leases"].as_i64().unwrap(),
            live["calls"].as_i64().unwrap()
        )
    );
    assert!(counts.0 > 0 && counts.2 > 0 && counts.3 > 0);
}
