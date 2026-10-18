//! Schema-3 homes require a drained conversion before execution budgets are enabled.

use super::home;
use home::ScratchHome;
use rusqlite::{Connection, params};
use sluice_model::ids::HomeId;
use sluice_store::{ReadPool, Writer, backup, schema::SCHEMA_VERSION};
use std::path::Path;

const SCHEMA_3: &str = include_str!("../../migrations/0003.sql");

/// A schema-3 home as a release from before the column left it.
fn before_the_column(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let c = Connection::open(dir.join("sluice.db")).unwrap();
    c.execute_batch(SCHEMA_3).unwrap();
    c.execute(
        "INSERT INTO home_meta(singleton,home_id,format_major,schema_version) VALUES (1,?1,1,3)",
        params![HomeId::new().to_string()],
    )
    .unwrap();
    c.pragma_update(None, "application_id", 0x534c5543).unwrap();
    c.pragma_update(None, "user_version", 3).unwrap();
    c.pragma_update(None, "journal_mode", "WAL").unwrap();
}
fn has_column(dir: &Path) -> bool {
    Connection::open(dir.join("sluice.db"))
        .unwrap()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('steps') WHERE name='bypass')",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn slow_schema_3_requires_drained_conversion_and_preserves_project_metadata() {
    let scratch = ScratchHome::new().unwrap();
    let home = scratch.path();
    before_the_column(home);
    assert!(!has_column(home));
    let refused = ReadPool::open(home, 1)
        .err()
        .expect("a reader waits for the writer");
    assert!(
        refused.to_string().contains("run \"sluice home migrate\""),
        "{refused}"
    );
    assert!(Writer::open(home).is_err());
    let sql = Connection::open(home.join("sluice.db")).unwrap();
    let project = sluice_model::ids::ProjectId::new().to_string();
    sql.execute("INSERT INTO projects(project_id,name,description,created_at) VALUES (?1,'preserved','original description','now')", [&project]).unwrap();
    sql.execute_batch("ALTER TABLE projects ADD COLUMN fallback TEXT")
        .unwrap();
    sql.execute(
        "UPDATE projects SET fallback='@owner' WHERE project_id=?1",
        [&project],
    )
    .unwrap();
    let attempt = sluice_model::ids::AttemptId::new().to_string();
    sql.execute("INSERT INTO attempts(attempt_id,phase,request,inputs_hash,created_at) VALUES (?1,'reserved','{}','x','now')", [&attempt]).unwrap();
    let refusal = sluice_store::convert::convert_home(&home.join("sluice.db")).unwrap_err();
    assert!(refusal.to_string().contains("live work"), "{refusal}");
    assert_eq!(
        sql.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert!(!has_column(home));
    sql.execute(
        "UPDATE attempts SET phase='terminal',finished_at='now' WHERE attempt_id=?1",
        [&attempt],
    )
    .unwrap();
    let report = sluice_store::convert::convert_home(&home.join("sluice.db")).unwrap();
    assert_eq!(report.from_schema, 3);
    assert_eq!(report.projects.len(), 1);
    assert_eq!(
        sql.query_row(
            "SELECT description,fallback,limits FROM projects WHERE project_id=?1",
            [&project],
            |row| Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?
            ))
        )
        .unwrap(),
        ("original description".into(), "@owner".into(), None)
    );
    drop(Writer::open(home).unwrap());
    assert!(has_column(home));
    ReadPool::open(home, 1).unwrap();
    assert_eq!(SCHEMA_VERSION, 4);
    // A fresh home has it from the start.
    let fresh = ScratchHome::new().unwrap();
    drop(Writer::open(fresh.path()).unwrap());
    assert!(has_column(fresh.path()));
}

#[test]
fn slow_a_restored_backup_from_before_the_column_gains_it() {
    let scratch = ScratchHome::new().unwrap();
    let old = scratch.root().join("old");
    before_the_column(&old);
    let restored = scratch.root().join("restored");
    backup::restore_into_fresh_home(&old.join("sluice.db"), &restored).unwrap();
    assert!(has_column(&restored));
}
