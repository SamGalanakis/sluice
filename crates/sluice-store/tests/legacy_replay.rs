//! Lane H2's replay gates for the schema-3 converter (`docs/design/plan-rows.md` §10.2 to
//! §10.6), driven through the store's own entry points: a schema-changing restore converts a
//! legacy backup into its private destination, and a schema-3 store refuses a legacy home on
//! open. The legacy homes are `tests/fixtures/legacy/` (SQL text the old release built; see
//! `tools/cutover-rehearsal`), and every check is lane H's independent reading of the rows
//! (`tests/support/plan_rows.rs`), never the converter's own code.
//!
//! Each test runs in `scripts/check` but the live-backup gate, which needs a backup-API copy
//! of a live home and the old release's oracle (see its own doc).
#[path = "../../../tests/support/home.rs"]
mod home;
#[path = "../../../tests/support/plan_rows.rs"]
mod plan_rows;

use home::ScratchHome;
use plan_rows::*;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value, json};
use sluice_model::error::PublicError;
use sluice_store::{ReadPool, Writer, backup};
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/legacy")
}
fn read(name: &str) -> String {
    std::fs::read_to_string(fixtures().join(name)).unwrap()
}
fn expected(name: &str) -> Value {
    serde_json::from_str(&read(&format!("{name}.expected.json"))).unwrap()
}
/// A backup file holding the legacy fixture, with `defect` applied.
fn backup_file(root: &Path, name: &str, defect: Option<&str>) -> PathBuf {
    let path = root.join(format!("{name}.db"));
    load_dump(&path, &read(&format!("{name}.sql"))).unwrap();
    if let Some(sql) = defect {
        Connection::open(&path).unwrap().execute_batch(sql).unwrap();
    }
    path
}
fn open(path: &Path) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}
fn scalar<T: rusqlite::types::FromSql>(sql: &Connection, query: &str) -> T {
    sql.query_row(query, [], |r| r.get(0))
        .unwrap_or_else(|e| panic!("{query}: {e}"))
}
fn project_id(sql: &Connection, name: &str) -> String {
    sql.query_row(
        "SELECT project_id FROM projects WHERE name=?1 AND deleted_at IS NULL",
        [name],
        |r| r.get(0),
    )
    .unwrap()
}
fn compact_value(value: &Value) -> String {
    compact(value.as_object().unwrap())
}

/// Everything §10.4 promises about a converted home, checked against the fixture's
/// expectations (each revision's document as the old store stored it) and its source.
fn check_converted(source: &Path, converted: &Path, expected: &Value) {
    let sql = open(converted);
    let before = open(source);
    assert_eq!(
        scalar::<i64>(&sql, "SELECT schema_version FROM home_meta"),
        3
    );
    assert_eq!(scalar::<i64>(&sql, "PRAGMA user_version"), 3);
    assert_eq!(
        scalar::<i64>(
            &sql,
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'"
        ),
        27
    );
    // The document is never stored: no plans.doc, no plan_edits.ops, no board slots.
    for (table, column) in [
        ("plans", "doc"),
        ("plan_edits", "ops"),
        ("projects", "board_slots"),
    ] {
        let present: bool = sql
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
                [table, column],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!present, "{table}.{column} is dropped");
    }
    assert_eq!(
        scalar::<i64>(
            &sql,
            "SELECT count(*) FROM sqlite_schema WHERE name='board_slots'"
        ),
        0
    );
    // Identities are kept: every project, step, attempt, run and result id.
    for query in [
        "SELECT group_concat(project_id) FROM (SELECT project_id FROM projects ORDER BY project_id)",
        "SELECT group_concat(k) FROM (SELECT project_id||'/'||step_id||'/'||generation||'/'||work_generation||'/'||status||'/'||coalesce(outputs,'')||'/'||coalesce(result_id,'') AS k FROM steps ORDER BY k)",
        "SELECT group_concat(k) FROM (SELECT project_id||'/'||name||'/'||coalesce(value,'')||'/'||generation AS k FROM inputs ORDER BY k)",
        "SELECT group_concat(attempt_id) FROM (SELECT attempt_id FROM attempts ORDER BY attempt_id)",
        "SELECT group_concat(run_id) FROM (SELECT run_id FROM runs ORDER BY run_id)",
        "SELECT group_concat(result_id) FROM (SELECT result_id FROM step_results ORDER BY result_id)",
        "SELECT group_concat(seq) FROM (SELECT seq FROM records ORDER BY seq)",
    ] {
        assert_eq!(
            scalar::<Option<String>>(&sql, query),
            scalar::<Option<String>>(&before, query),
            "{query}"
        );
    }
    // Every record at payload version 2; plan.edit records carry their revision's changes.
    assert_eq!(
        scalar::<i64>(
            &sql,
            "SELECT count(*) FROM records WHERE payload_version<>2"
        ),
        0
    );
    assert_eq!(
        scalar::<i64>(
            &sql,
            "SELECT count(*) FROM records WHERE kind='plan.edit' AND json_type(payload,'$.ops') IS NOT NULL"
        ),
        0
    );
    assert_eq!(
        scalar::<i64>(
            &sql,
            "SELECT count(*) FROM records r JOIN plan_edits e
               ON e.project_id=r.project_id AND e.rev=json_extract(r.payload,'$.rev')
             WHERE r.kind='plan.edit' AND json(json_extract(r.payload,'$.changes'))<>json(e.changes)"
        ),
        0,
        "a plan.edit record's changes are its revision's"
    );
    // No completion snapshot is left in any attempt, either copy.
    assert_eq!(
        scalar::<i64>(
            &sql,
            "SELECT count(*) FROM attempts WHERE json_type(request,'$.provenance.runtime.completion') IS NOT NULL
               OR json_type(provenance,'$.runtime.completion') IS NOT NULL"
        ),
        0
    );

    let projects = expected["projects"].as_array().unwrap();
    assert_eq!(
        scalar::<i64>(&sql, "SELECT count(*) FROM plans"),
        projects.len() as i64,
        "a deleted project has no plan to convert"
    );
    for project in projects {
        let name = project["name"].as_str().unwrap();
        let id = project_id(&sql, name);
        let revisions = project["revisions"].as_array().unwrap();
        // §10.4 step 3 and §10.5: every revision's rows export the document it had.
        let rebuilt = rebuild_history(&sql, &id).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(rebuilt.len(), revisions.len(), "{name}: every revision");
        for ((rev, export), stored) in rebuilt.iter().zip(revisions) {
            assert_eq!(*rev, stored["rev"].as_u64().unwrap());
            assert_eq!(
                export,
                &compact_value(&stored["document"]),
                "{name} rev {rev}: rebuilt from changes"
            );
        }
        let origin: Value = serde_json::from_str(
            &sql.query_row(
                "SELECT changes FROM plan_edits WHERE project_id=?1 AND rev=1",
                [&id],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            origin,
            json!([{"op": "header.put", "root_order": ["steps"]}])
        );
        // The stored rows are the last revision, with nothing renumbered out of order.
        let last = revisions.last().unwrap()["document"].clone();
        let rows = authored_rows(&sql, &id).unwrap();
        assert_eq!(
            compact(&rows.export()),
            compact_value(&last),
            "{name}: final rows"
        );
        let root_order: Vec<String> = serde_json::from_str(
            &sql.query_row(
                "SELECT root_order FROM plans WHERE project_id=?1",
                [&id],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
        )
        .unwrap();
        let keys: Vec<&String> = last.as_object().unwrap().keys().collect();
        assert_eq!(
            root_order.iter().collect::<Vec<_>>(),
            keys,
            "{name}: root order"
        );
        let rev: i64 = sql
            .query_row("SELECT rev FROM plans WHERE project_id=?1", [&id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            rev as usize,
            revisions.len(),
            "{name}: the revision is kept"
        );
        // Inputs exactly as written, not as the old projection normalized them.
        for (input, declaration) in last
            .get("inputs")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let stored: String = sql
                .query_row(
                    "SELECT declaration FROM inputs WHERE project_id=?1 AND name=?2",
                    [&id, input],
                    |r| r.get(0),
                )
                .unwrap();
            let stored: Value = serde_json::from_str(&stored).unwrap();
            assert_eq!(
                serde_json::to_string(&stored).unwrap(),
                serde_json::to_string(declaration).unwrap(),
                "{name}.{input}: as written"
            );
        }
        // §2.5: the index rows are what the declarations derive.
        check_indexes(&sql, &id).unwrap_or_else(|d| panic!("{name}: {d:#?}"));
        // §2.2: the CHECK-held step columns agree with the declarations.
        assert_eq!(
            scalar::<i64>(
                &sql,
                &format!(
                    "SELECT count(*) FROM steps WHERE project_id='{id}' AND (
                       paused IS NOT (CASE json_type(declaration,'$.paused')
                         WHEN 'true' THEN 'true'
                         WHEN 'text' THEN json_quote(json_extract(declaration,'$.paused'))
                         ELSE 'false' END)
                       OR run IS NOT json_extract(declaration,'$.run')
                       OR priority IS NOT coalesce(json_extract(declaration,'$.priority'),0)
                       OR needs IS NOT json_extract(declaration,'$.needs'))"
                )
            ),
            0,
            "{name}: paused, run, priority and needs follow the declaration"
        );
        assert_eq!(
            scalar::<i64>(
                &sql,
                &format!("SELECT state_epoch FROM plans WHERE project_id='{id}'")
            ),
            0
        );
    }
}

/// A fresh home's schema, for the equivalence check.
fn fresh_schema() -> std::collections::BTreeMap<String, String> {
    let fresh = ScratchHome::new().unwrap();
    drop(Writer::open(fresh.path()).unwrap());
    schema_shape(&open(&fresh.path().join("sluice.db"))).unwrap()
}

#[test]
fn every_legacy_home_converts_with_round_trip_equality_at_every_revision() {
    let fresh = fresh_schema();
    for name in ["schema1", "schema1_old", "schema2"] {
        let home = ScratchHome::new().unwrap();
        let source = backup_file(home.root(), name, None);
        let unchanged = logical_dump(&open(&source)).unwrap();
        let destination = home.root().join("restored");
        backup::restore_into_fresh_home(&source, &destination)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            logical_dump(&open(&source)).unwrap(),
            unchanged,
            "{name}: the backup is read only"
        );
        let converted = destination.join("sluice.db");
        check_converted(&source, &converted, &expected(name));
        // §2.1: a converted home and a fresh one are schema-equivalent.
        let differences = schema_differences(&fresh, &schema_shape(&open(&converted)).unwrap());
        assert!(differences.is_empty(), "{name}: {differences:#?}");
        // And it is a home the schema-3 store opens.
        drop(ReadPool::open(&destination, 1).unwrap_or_else(|e| panic!("{name}: {e}")));
        drop(Writer::open(&destination).unwrap_or_else(|e| panic!("{name}: {e}")));
    }
}

#[test]
fn each_defect_blocks_the_conversion_naming_its_project_and_rev_and_writes_nothing() {
    let defects: Vec<Value> = serde_json::from_str(&read("defects.json")).unwrap();
    for defect in defects {
        let case = defect["case"].as_str().unwrap();
        let home = ScratchHome::new().unwrap();
        let source = backup_file(home.root(), "schema1", defect["sql"].as_str());
        let unchanged = logical_dump(&open(&source)).unwrap();
        let destination = home.root().join("restored");
        let refused = backup::restore_into_fresh_home(&source, &destination)
            .err()
            .unwrap_or_else(|| panic!("{case}: {} must block", defect["about"]));
        let message = refused.to_string();
        let project = defect["project"].as_str().unwrap();
        let rev = defect["rev"].to_string();
        assert!(
            message.contains(project) && message.contains(&rev),
            "{case}: the blocker names project {project} and rev {rev}: {message}"
        );
        assert_eq!(logical_dump(&open(&source)).unwrap(), unchanged, "{case}");
        let converted = destination.join("sluice.db");
        assert!(
            !converted.exists() || scalar::<i64>(&open(&converted), "PRAGMA user_version") != 3,
            "{case}: no converted home is left behind"
        );
    }
}

#[test]
fn a_backup_holding_live_work_is_refused_with_its_counts_and_nothing_is_written() {
    let live = expected("live")["live"].clone();
    let home = ScratchHome::new().unwrap();
    let source = backup_file(home.root(), "live", None);
    let unchanged = logical_dump(&open(&source)).unwrap();
    let destination = home.root().join("restored");
    let refused = backup::restore_into_fresh_home(&source, &destination)
        .expect_err("a backup with live work is refused")
        .into_public(false);
    assert_eq!(
        refused,
        PublicError::Invalid {
            message: format!(
                "this backup holds live work ({} attempts, {} runs, {} leases, {} calls); a schema-changing restore needs a backup taken with nothing live",
                live["attempts"], live["runs"], live["leases"], live["calls"]
            ),
            errors: vec![],
        }
    );
    assert_eq!(logical_dump(&open(&source)).unwrap(), unchanged);
    assert!(
        !destination.join("sluice.db").exists(),
        "nothing is written to the destination"
    );
}

#[test]
fn a_schema_3_store_refuses_a_legacy_home_on_open_without_touching_it() {
    for (name, schema) in [("schema1", 1), ("schema1_old", 1), ("schema2", 2)] {
        let home = ScratchHome::new().unwrap();
        let database = home.path().join("sluice.db");
        load_dump(&database, &read(&format!("{name}.sql"))).unwrap();
        let bytes = std::fs::read(&database).unwrap();
        let expected = format!(
            "this home is at schema {schema}; run \"sluice home migrate\" to bring it to schema 3"
        );
        let writer = match Writer::open(home.path()) {
            Ok(_) => panic!("{name}: the writer refuses"),
            Err(error) => error.to_string(),
        };
        assert_eq!(writer, expected, "{name}");
        let reader = match ReadPool::open(home.path(), 1) {
            Ok(_) => panic!("{name}: readers refuse"),
            Err(error) => error.to_string(),
        };
        assert_eq!(reader, expected, "{name}");
        assert_eq!(
            std::fs::read(&database).unwrap(),
            bytes,
            "{name}: not a byte changed"
        );
    }
}

#[test]
fn a_fresh_home_is_schema_3_and_its_compact_reads_use_the_covering_indexes() {
    let home = ScratchHome::new().unwrap();
    drop(Writer::open(home.path()).unwrap());
    let sql = open(&home.path().join("sluice.db"));
    assert_eq!(
        scalar::<i64>(&sql, "SELECT schema_version FROM home_meta"),
        3
    );
    for (query, index) in [
        (
            "SELECT step_id, position, run, priority, unit, paused, status FROM steps
             WHERE project_id='p' ORDER BY position, step_id",
            "steps_compact",
        ),
        (
            "SELECT step_id, position, run, priority, unit, paused, status FROM steps
             WHERE project_id='p' AND position > 3 ORDER BY position, step_id LIMIT 201",
            "steps_compact",
        ),
        (
            "SELECT step_id, position, run, priority, unit, paused, status FROM steps
             WHERE project_id='p' AND unit='u' ORDER BY position, step_id",
            "steps_unit_compact",
        ),
        (
            "SELECT step_id, position, run, priority, unit, paused, status FROM steps
             WHERE project_id='p' AND status='pending' ORDER BY position, step_id",
            "steps_status_compact",
        ),
        (
            "SELECT step_id, needs, priority FROM steps
             WHERE project_id='p' AND status='pending' AND needs IS NOT NULL",
            "steps_needs",
        ),
    ] {
        let plan = query_plan(&sql, query).unwrap().join(" | ");
        assert!(
            plan.contains(&format!("USING COVERING INDEX {index}")),
            "{query}\n  plans as: {plan}"
        );
        assert!(!plan.contains("TEMP B-TREE"), "{query}\n  sorts: {plan}");
    }
}

/// The live home's every historical revision, converted (§12 lane H2's done-when). Run on the
/// integration branch against a read-only backup-API copy of the live home, with the oracle
/// the old release's harness wrote for the same copy:
///
/// ```text
/// SLUICE_LIVE_COPY=/tmp/h2.x/home/sluice.db SLUICE_LIVE_ORACLE=/tmp/h2.x/oracle.json \
///   cargo test -p sluice-store --test legacy_replay -- --ignored live_backup
/// ```
///
/// A project the oracle cannot replay (an unknown origin) must block the conversion by name.
#[test]
#[ignore = "plan-rows integration gate: needs SLUICE_LIVE_COPY and SLUICE_LIVE_ORACLE"]
fn a_live_backup_converts_every_historical_revision_or_names_what_blocks_it() {
    let copy = std::env::var_os("SLUICE_LIVE_COPY")
        .map(PathBuf::from)
        .expect("SLUICE_LIVE_COPY: a backup-API copy of the live home's sluice.db");
    let oracle: Value = serde_json::from_slice(
        &std::fs::read(std::env::var_os("SLUICE_LIVE_ORACLE").expect(
            "SLUICE_LIVE_ORACLE: `cutover-rehearsal oracle --home <copy>` of the same copy",
        ))
        .unwrap(),
    )
    .unwrap();
    assert!(copy.starts_with("/tmp"), "only a copy under /tmp");
    let home = ScratchHome::new().unwrap();
    let source = home.root().join("live.db");
    std::fs::copy(&copy, &source).unwrap();
    let destination = home.root().join("restored");
    let projects = oracle["projects"].as_array().unwrap();
    let blocked: Vec<&str> = projects
        .iter()
        .filter(|p| !p["blockers"].as_array().unwrap().is_empty())
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    let restored = backup::restore_into_fresh_home(&source, &destination);
    if !blocked.is_empty() {
        let message = restored
            .expect_err("an unreplayable history blocks")
            .to_string();
        for name in &blocked {
            assert!(
                message.contains(name),
                "the blocker names {name}: {message}"
            );
        }
        return;
    }
    restored.unwrap();
    let expected = json!({"projects": projects.iter().map(|p| {
        let mut p = p.as_object().unwrap().clone();
        p.retain(|k, _| k == "name" || k == "revisions");
        Value::Object(p)
    }).collect::<Vec<_>>()});
    check_converted(&source, &destination.join("sluice.db"), &expected);
}

#[test]
fn the_expectations_name_every_revision_of_every_legacy_plan() {
    // Runs now: the fixtures and their expectations agree with each other, so the gates above
    // check the converter, not a stale fixture.
    for name in ["schema1", "schema1_old", "schema2", "live"] {
        let expected = expected(name);
        let home = ScratchHome::new().unwrap();
        let source = backup_file(home.root(), name, None);
        let sql = open(&source);
        for project in expected["projects"].as_array().unwrap() {
            let id = project_id(&sql, project["name"].as_str().unwrap());
            let revisions = project["revisions"].as_array().unwrap();
            let rev: i64 = sql
                .query_row("SELECT rev FROM plans WHERE project_id=?1", [&id], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(rev as usize, revisions.len());
            let doc: Map<String, Value> = serde_json::from_str(
                &sql.query_row("SELECT doc FROM plans WHERE project_id=?1", [&id], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
            )
            .unwrap();
            assert_eq!(
                compact(&doc),
                compact_value(&revisions.last().unwrap()["document"])
            );
        }
    }
}
