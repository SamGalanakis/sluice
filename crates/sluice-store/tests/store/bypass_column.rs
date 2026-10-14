//! `steps.bypass` is an added schema-3 column (`ADDED_COLUMNS`): a home from before it gains
//! it when its writer opens it, a restore of a backup from before it gains it on its private
//! copy, and the schema version stays 3.

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
async fn slow_a_home_from_before_the_bypass_column_gains_it_at_schema_3() {
    let scratch = ScratchHome::new().unwrap();
    let home = scratch.path();
    before_the_column(home);
    assert!(!has_column(home));
    let refused = ReadPool::open(home, 1)
        .err()
        .expect("a reader waits for the writer");
    assert!(
        refused
            .to_string()
            .contains("is missing until the home's writer opens it"),
        "{refused}"
    );
    drop(Writer::open(home).unwrap());
    assert!(has_column(home));
    ReadPool::open(home, 1).unwrap();
    assert_eq!(SCHEMA_VERSION, 3);
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
