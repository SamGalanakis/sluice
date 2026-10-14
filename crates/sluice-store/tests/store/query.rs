use crate::home;
use home::ScratchHome;
use rusqlite::Connection;
use sluice_model::{error::PublicError, ids::ProjectId};
use sluice_store::{
    RetrySafety, Writer,
    query::{self, QueryLimits},
};
use std::time::{Duration, Instant};

const BOMB: &str =
    "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r) SELECT sum(n) FROM r";

async fn setup() -> (ScratchHome, Writer, ProjectId) {
    let home = ScratchHome::new().unwrap();
    assert!(home.root().exists());
    assert_eq!(ScratchHome::validate(home.path()).unwrap(), home.path());
    let writer = Writer::open(home.path()).unwrap();
    let project = ProjectId::new();
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "INSERT INTO projects(project_id,name,created_at) VALUES (?1,'p','now')",
                [project.to_string()],
            )?;
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
    (home, writer, project)
}
fn run(home: &ScratchHome, sql: &str) -> query::QueryResult {
    query::query(home.path(), sql, None, None).unwrap()
}
fn json(result: &query::QueryResult) -> serde_json::Value {
    serde_json::from_slice(result.encoded()).unwrap()
}

#[tokio::test]
async fn query_refuses_writes_and_bounds_work_and_encoded_cells() {
    let (home, writer, _) = setup().await;
    {
        for sql in [
            "INSERT INTO projects(project_id,name,created_at) VALUES ('00000000-0000-0000-0000-000000000000','x','now')",
            "UPDATE projects SET description='changed'",
            "DELETE FROM projects",
            "CREATE TABLE evil(n)",
            "DROP TABLE projects",
            "ALTER TABLE projects ADD COLUMN evil TEXT",
            "CREATE VIEW evil AS SELECT 1",
            "CREATE INDEX evil ON projects(name)",
            "CREATE TEMP TABLE evil(n)",
            "CREATE VIRTUAL TABLE evil USING fts5(body)",
            "ATTACH ':memory:' AS evil",
            "DETACH main",
            "PRAGMA user_version=99",
            "PRAGMA query_only=OFF",
            "PRAGMA journal_mode=DELETE",
            "PRAGMA table_info(projects)",
            "SELECT * FROM pragma_table_info('projects')",
            "BEGIN",
            "COMMIT",
            "SAVEPOINT evil",
            "REINDEX",
            "ANALYZE",
        ] {
            assert!(
                matches!(
                    query::query(home.path(), sql, None, None),
                    Err(PublicError::BadRequest { .. })
                ),
                "{sql}"
            );
        }
        assert_eq!(
            json(&run(&home, "SELECT name,description FROM projects"))["rows"],
            serde_json::json!([["p", ""]])
        );
        let c = Connection::open(home.path().join("sluice.db")).unwrap();
        assert_eq!(
            c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            sluice_store::schema::SCHEMA_VERSION
        );
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='evil'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    {
        for (sql, duration) in [
            ("SELECT 1", Duration::ZERO),
            (BOMB, Duration::from_millis(2)),
        ] {
            let started = Instant::now();
            let error = query::query_with_limits(
                home.path(),
                sql,
                None,
                None,
                QueryLimits {
                    duration,
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains("deadline"), "{error}");
            assert!(started.elapsed() < Duration::from_secs(1));
        }
    }
    {
        for sql in [
            "SELECT hex(randomblob(100))",
            "SELECT replace(hex(zeroblob(40)),'0',char(10)) AS wide",
        ] {
            let error = query::query_with_limits(
                home.path(),
                sql,
                None,
                None,
                QueryLimits {
                    bytes: 100,
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains("cell too large"), "{error}");
        }
        let limits = QueryLimits {
            bytes: 100,
            ..Default::default()
        };
        assert!(
            query::query_with_limits(
                home.path(),
                "SELECT hex(zeroblob(100)) AS wide",
                None,
                None,
                limits
            )
            .unwrap_err()
            .to_string()
            .contains("cell too large")
        );
    }
    writer.shutdown().await.unwrap();
}
