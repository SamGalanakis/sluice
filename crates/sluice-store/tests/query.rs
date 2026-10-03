#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use rusqlite::{Connection, types::Value};
use sluice_model::{
    error::PublicError,
    ids::{AttemptId, InvocationId, ProjectId, ResultId, RunId},
};
use sluice_store::{
    RetrySafety, Writer,
    query::{self, QueryCell, QueryLimits},
};
use std::time::{Duration, Instant};

const BOMB: &str =
    "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r) SELECT sum(n) FROM r";

async fn setup() -> (ScratchHome, Writer, ProjectId) {
    let home = ScratchHome::new().unwrap();
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
async fn public_relations_return_real_columns_and_retained_values() {
    let (home, writer, project) = setup().await;
    for name in [
        "outcomes",
        "messages",
        "steps",
        "runs",
        "calls",
        "log",
        "step_changes",
        "edits",
        "questions",
    ] {
        let result = run(&home, &format!("SELECT * FROM {name}"));
        assert!(!result.columns().is_empty(), "{name}");
        assert!(!result.rows().is_empty(), "{name}");
        assert!(!result.truncated(), "{name}");
        assert!(
            result.columns().contains(&"project_id".to_string()),
            "{name}"
        );
    }
    let result = query::query(
        home.path(),
        "SELECT step_id,status FROM steps WHERE project_id=?",
        Some(&[Value::Text(project.to_string())]),
        None,
    )
    .unwrap();
    assert_eq!(
        json(&result),
        serde_json::json!({"columns":["step_id","status"],"rows":[["a","pending"]],"truncated":false})
    );
    assert_eq!(
        json(&run(
            &home,
            "SELECT unit,json_extract(outputs,'$.value') FROM outcomes"
        ))["rows"],
        serde_json::json!([["old-unit", 3]])
    );
    assert_eq!(
        json(&run(&home, "SELECT \"to\" FROM step_changes"))["rows"],
        serde_json::json!([["running"]])
    );
    assert_eq!(
        json(&run(&home, "SELECT max(rev) FROM edits"))["rows"],
        serde_json::json!([[2]])
    );
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn parameters_bind_all_supported_sql_types_and_do_not_inject() {
    let (home, writer, _) = setup().await;
    let result = query::query(
        home.path(),
        "SELECT ?+?, ?, ?, ?",
        Some(&[
            Value::Integer(2),
            Value::Integer(3),
            Value::Text("'; DROP TABLE projects; --".into()),
            Value::Null,
            Value::Real(1.25),
        ]),
        None,
    )
    .unwrap();
    assert_eq!(
        result.rows()[0],
        vec![
            QueryCell::Integer(5),
            QueryCell::Text("'; DROP TABLE projects; --".into()),
            QueryCell::Null,
            QueryCell::Real(1.25)
        ]
    );
    assert_eq!(
        json(&run(&home, "SELECT count(*) FROM projects"))["rows"],
        serde_json::json!([[1]])
    );
    assert!(query::query(home.path(), "SELECT ?", None, None).is_err());
    assert!(query::query(home.path(), "SELECT 1", Some(&[Value::Integer(1)]), None).is_err());
    assert!(
        query::query(home.path(), "SELECT ?", Some(&[Value::Blob(vec![1])]), None)
            .unwrap_err()
            .to_string()
            .contains("binary data")
    );
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn row_limit_truncation_and_exact_and_default_limits() {
    let (home, writer, _) = setup().await;
    let limited = query::query(
        home.path(),
        "SELECT body FROM messages ORDER BY id",
        None,
        Some(2),
    )
    .unwrap();
    assert_eq!(json(&limited)["rows"], serde_json::json!([["m1"], ["m2"]]));
    assert!(limited.truncated());
    for limit in [5, 10] {
        assert!(
            !query::query(home.path(), "SELECT body FROM messages", None, Some(limit))
                .unwrap()
                .truncated()
        );
    }
    let many = run(
        &home,
        "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r LIMIT 300) SELECT n FROM r",
    );
    assert_eq!(many.rows().len(), 200);
    assert!(many.truncated());
    let max = query::query(
        home.path(),
        "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r LIMIT 1001) SELECT n FROM r",
        None,
        Some(1000),
    )
    .unwrap();
    assert_eq!(max.rows().len(), 1000);
    assert!(max.truncated());
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_limits_are_bad_requests() {
    let (home, writer, _) = setup().await;
    for limit in [0, 1001, usize::MAX] {
        assert!(matches!(
            query::query(home.path(), "SELECT 1", None, Some(limit)),
            Err(PublicError::BadRequest { .. })
        ));
    }
    for limits in [
        QueryLimits {
            vm_operations: 0,
            ..Default::default()
        },
        QueryLimits {
            bytes: query::MAX_BYTES + 1,
            ..Default::default()
        },
        QueryLimits {
            duration: Duration::from_secs(3),
            ..Default::default()
        },
        QueryLimits {
            vm_operations: 250001,
            ..Default::default()
        },
    ] {
        assert!(matches!(
            query::query_with_limits(home.path(), "SELECT 1", None, None, limits),
            Err(PublicError::BadRequest { .. })
        ));
    }
    writer.shutdown().await.unwrap();
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
        "PRAGMA user_version=2",
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
        1
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
async fn unsafe_functions_and_vacuum_create_no_files() {
    let (home, writer, _) = setup().await;
    let out = home.root().join("never.db");
    for sql in [
        format!("VACUUM INTO '{}'", out.display()),
        "SELECT load_extension('/tmp/never.so')".into(),
        "SELECT LOAD_EXTENSION('/tmp/never.so')".into(),
        "SELECT sqlite_log(1,'never')".into(),
        format!("SELECT writefile('{}','never')", out.display()),
        "SELECT readfile('/etc/passwd')".into(),
        "SELECT fts3_tokenizer('simple')".into(),
        "SELECT eval('DELETE FROM projects')".into(),
    ] {
        assert!(
            query::query(home.path(), &sql, None, None).is_err(),
            "{sql}"
        );
    }
    assert!(!out.exists());
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn vm_operation_budget_interrupts_recursive_bomb_and_connection_recovers() {
    let (home, writer, _) = setup().await;
    let started = Instant::now();
    let error = query::query(home.path(), BOMB, None, None).unwrap_err();
    assert!(matches!(error, PublicError::BadRequest { .. }));
    assert!(error.to_string().contains("VM-operation"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(3));
    let error = query::query_with_limits(
        home.path(),
        BOMB,
        None,
        None,
        QueryLimits {
            vm_operations: 100,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("VM-operation"));
    assert_eq!(
        json(&run(&home, "SELECT 1"))["rows"],
        serde_json::json!([[1]])
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

#[tokio::test]
async fn blobs_invalid_text_and_nonfinite_reals_are_not_lossy() {
    let (home, writer, _) = setup().await;
    let error = query::query(home.path(), "SELECT x'0102' AS icon", None, None)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("binary data")
            && error.contains("hex(\"icon\")")
            && error.contains("length(\"icon\")")
    );
    assert_eq!(
        json(&run(&home, "SELECT hex(x'0102'),length(x'0102')"))["rows"],
        serde_json::json!([["0102", 2]])
    );
    for sql in ["SELECT CAST(x'ff' AS TEXT)", "SELECT 1e999"] {
        assert!(matches!(
            query::query(home.path(), sql, None, None),
            Err(PublicError::Invalid { .. })
        ));
    }
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn whole_encoded_response_stays_inside_byte_budget() {
    let (home, writer, _) = setup().await;
    let result = run(
        &home,
        "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r LIMIT 200) SELECT n,hex(randomblob(8000)) FROM r",
    );
    assert!(result.truncated());
    assert!(!result.rows().is_empty() && result.rows().len() < 200);
    assert!(result.encoded().len() <= query::MAX_BYTES);
    assert_eq!(
        json(&result)["rows"].as_array().unwrap().len(),
        result.rows().len()
    );
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn byte_truncation_stops_before_a_later_invalid_row_and_releases_snapshot() {
    let (home, writer, _) = setup().await;
    let result = query::query_with_limits(home.path(),"SELECT 'first row' AS value UNION ALL SELECT 'crossing row' UNION ALL SELECT json('invalid json')",None,None,QueryLimits {bytes:70,..Default::default()}).unwrap();
    assert_eq!(
        json(&result),
        serde_json::json!({"columns":["value"],"rows":[["first row"]],"truncated":true})
    );
    assert!(result.encoded().len() <= 70);
    let c = Connection::open(home.path().join("sluice.db")).unwrap();
    assert_eq!(
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn column_names_and_empty_results_still_consume_the_output_budget() {
    let (home, writer, _) = setup().await;
    let sql = format!("SELECT 1 AS \"{}\" WHERE 0", "x".repeat(200));
    assert!(
        query::query_with_limits(
            home.path(),
            &sql,
            None,
            None,
            QueryLimits {
                bytes: 100,
                ..Default::default()
            }
        )
        .unwrap_err()
        .to_string()
        .contains("columns")
    );
    let result = run(&home, "SELECT 1 AS duplicate,2 AS duplicate WHERE 0");
    assert_eq!(
        json(&result),
        serde_json::json!({"columns":["duplicate","duplicate"],"rows":[],"truncated":false})
    );
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn one_statement_handles_quoted_semicolons_and_comments() {
    let (home, writer, _) = setup().await;
    for sql in [
        "SELECT 1; SELECT 2",
        "SELECT 1; DROP TABLE projects",
        "",
        ";",
        "-- only comment",
        "SELECT 1\0",
        "SELECT 1; ;",
    ] {
        assert!(
            query::query(home.path(), sql, None, None).is_err(),
            "{sql:?}"
        );
    }
    for sql in [
        "SELECT ';' AS \"semi;colon\"; -- trailing ;\n /* ; */",
        "SELECT 'it''s ; fine'",
        "/* ; */ SELECT 1; /* trailing */",
    ] {
        assert!(!run(&home, sql).truncated());
    }
    assert!(query::query(home.path(), &"SELECT 1;".repeat(11000), None, None).is_err());
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn syntax_and_database_errors_map_to_invalid_without_creating_a_database() {
    let home = ScratchHome::new().unwrap();
    assert!(matches!(
        query::query(home.path(), "SELECT 1", None, None),
        Err(PublicError::Invalid { .. })
    ));
    assert!(!home.path().join("sluice.db").exists());
    let writer = Writer::open(home.path()).unwrap();
    for sql in [
        "SELECT missing FROM projects",
        "SELECT (",
        "SELECT json('broken')",
    ] {
        assert!(matches!(
            query::query(home.path(), sql, None, None),
            Err(PublicError::Invalid { .. })
        ));
    }
    writer.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reader_sees_committed_snapshot_without_blocking_writer_or_holding_wal() {
    let (home, writer, project) = setup().await;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let background = writer.clone();
    let task = tokio::spawn(async move {
        background
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute(
                    "UPDATE projects SET description='in-flight' WHERE project_id=?1",
                    [project.to_string()],
                )?;
                ready_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                tx.changed(Some(project), "status");
                Ok(())
            })
            .await
            .unwrap();
    });
    ready_rx.await.unwrap();
    assert_eq!(
        json(&run(&home, "SELECT description FROM projects"))["rows"],
        serde_json::json!([[""]])
    );
    release_tx.send(()).unwrap();
    task.await.unwrap();
    for _ in 0..20 {
        assert_eq!(
            json(&run(&home, "SELECT description FROM projects"))["rows"],
            serde_json::json!([["in-flight"]])
        );
    }
    let c = Connection::open(home.path().join("sluice.db")).unwrap();
    assert_eq!(
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn busy_is_a_retryable_read_error() {
    let (home, writer, _) = setup().await;
    writer.shutdown().await.unwrap();
    let c = Connection::open(home.path().join("sluice.db")).unwrap();
    c.execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE")
        .unwrap();
    let result = query::query(home.path(), "SELECT 1", None, None);
    assert!(
        matches!(
            result,
            Err(PublicError::Busy {
                retryable: true,
                ..
            })
        ),
        "{result:?}"
    );
    c.execute_batch("ROLLBACK").unwrap();
}
