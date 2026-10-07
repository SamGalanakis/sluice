#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use rusqlite::Connection;
use sluice_model::{
    error::PublicError,
    ids::{AttemptId, InvocationId, ProjectId, ResultId, RunId},
};
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
    writer.write(RetrySafety::NonIdempotent, move |tx| {
        let p = project.to_string();
        tx.sql().execute("INSERT INTO projects(project_id,name,created_at) VALUES (?1,'p','now')", [&p])?;
        tx.sql().execute("INSERT INTO plans(project_id,rev,doc) VALUES (?1,2,'{}')", [&p])?;
        tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,status) VALUES (?1,'a',0,'{}','pending')", [&p])?;
        tx.sql().execute("INSERT INTO step_results(result_id,project_id,step_id,generation,unit,declaration,status,outputs,recorded_at,removed_at) VALUES (?1,?2,'removed',1,'old-unit','{}','succeeded','{\"value\":3}','now','later')", (ResultId::new().to_string(), &p))?;
        let attempt = AttemptId::new().to_string();
        let run = RunId::new().to_string();
        tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,phase,request,inputs_hash,created_at) VALUES (?1,?2,'a','reserved','{}','hash','now')", (&attempt, &p))?;
        tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,created_at) VALUES (?1,?2,?3,'a','now')", (&run, &p, &attempt))?;
        tx.sql().execute("INSERT INTO calls(call_id,project_id,run_id,fn,status,inputs,created_at) VALUES (?1,?2,?3,'test','pending','{}','now')", (InvocationId::new().to_string(), &p, &run))?;
        for n in 1..=5 {
            tx.sql().execute("INSERT INTO records(project_id,at,kind,payload,thread) VALUES (?1,'now','message','{\"kind\":\"message\"}','t')", [&p])?;
            let seq = tx.sql().last_insert_rowid();
            tx.sql().execute("INSERT INTO messages(id,project_id,thread,\"from\",body,needs_reply,at) VALUES (?1,?2,'t','test',?3,?4,'now')", (seq, &p, format!("m{n}"), i64::from(n == 1)))?;
        }
        tx.sql().execute("INSERT INTO records(project_id,at,kind,payload,step_id) VALUES (?1,'now','step.status','{\"kind\":\"step.status\",\"from\":\"pending\",\"to\":\"running\"}','a')", [&p])?;
        tx.sql().execute("INSERT INTO plan_edits(project_id,rev,seq,at,author,reason,ops) VALUES (?1,2,6,'now','test','fixture','[]')", [&p])?;
        tx.changed(Some(project), "status");
        Ok(())
    }).await.unwrap();
    (home, writer, project)
}
fn run(home: &ScratchHome, sql: &str) -> query::QueryResult {
    query::query(home.path(), sql, None, None).unwrap()
}
fn json(result: &query::QueryResult) -> serde_json::Value {
    serde_json::from_slice(result.encoded()).unwrap()
}

#[tokio::test]
async fn authorizer_denies_mutation_ddl_attachment_pragmas_and_transactions() {
    let (home, writer, _) = setup().await;
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
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn deadline_is_enforced_before_execution_and_during_recursive_work() {
    let (home, writer, _) = setup().await;
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
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn raw_and_json_escaped_oversized_cells_are_explicit_errors() {
    let (home, writer, _) = setup().await;
    for sql in [
        "SELECT hex(randomblob(2000000))",
        "SELECT replace(hex(zeroblob(400000)),'0',char(10)) AS wide",
    ] {
        let error = query::query(home.path(), sql, None, None).unwrap_err();
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
    writer.shutdown().await.unwrap();
}
