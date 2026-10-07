#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use rusqlite::Connection;
use sluice_model::{error::PublicError, events::Event, ids::ProjectId};
use sluice_store::{ChangeKey, ReadPool, RetrySafety, StoreError, Writer};
use std::time::Duration;

fn event() -> Event {
    Event::ProjectUpdate {
        fields: vec!["description".into()],
        reason: None,
        author: "test".into(),
    }
}
fn key(project: ProjectId) -> ChangeKey {
    ChangeKey::new(Some(project), "status")
}
async fn setup() -> (ScratchHome, Writer, ReadPool, ProjectId) {
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
    let reads = ReadPool::open(home.path(), 2).unwrap();
    (home, writer, reads, project)
}
async fn count(reads: &ReadPool, table: &'static str) -> i64 {
    reads
        .snapshot(move |c| {
            Ok(c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?)
        })
        .await
        .unwrap()
}
async fn version(reads: &ReadPool, project: ProjectId) -> i64 {
    reads.cursor(vec![key(project)]).await.unwrap().versions[&key(project)]
}

#[tokio::test]
async fn unknown_major_or_schema_and_python_are_refused_without_rewriting() {
    for column in ["format_major", "schema_version", "user_version"] {
        let home = ScratchHome::new().unwrap();
        let writer = Writer::open(home.path()).unwrap();
        writer.shutdown().await.unwrap();
        let c = Connection::open(home.path().join("sluice.db")).unwrap();
        if column == "user_version" {
            c.pragma_update(None, column, 99).unwrap();
        } else {
            c.execute(&format!("UPDATE home_meta SET {column}=99"), [])
                .unwrap();
        }
        drop(c);
        let error = Writer::open(home.path()).err().unwrap();
        if column == "format_major" {
            assert!(matches!(error, StoreError::UnsupportedFormat { found: 99 }));
        } else {
            assert!(matches!(error, StoreError::UnsupportedSchema { found: 99 }));
        }
    }
    let home = ScratchHome::new().unwrap();
    let c = Connection::open(home.path().join("sluice.db")).unwrap();
    c.execute_batch("CREATE TABLE projects(name TEXT PRIMARY KEY); PRAGMA user_version=6;")
        .unwrap();
    drop(c);
    assert!(matches!(
        Writer::open(home.path()),
        Err(StoreError::PythonFormat)
    ));
    let c = Connection::open(home.path().join("sluice.db")).unwrap();
    assert_eq!(
        c.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        6
    );
}

#[tokio::test]
async fn panic_rolls_back_and_actor_and_read_pool_remain_usable() {
    let (_home, writer, reads, project) = setup().await;
    let error = writer
        .write(
            RetrySafety::NonIdempotent,
            move |tx| -> sluice_store::Result<()> {
                tx.append_record(Some(project), event())?;
                panic!("request failure");
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(error, PublicError::Storage { .. }));
    assert_eq!(count(&reads, "records").await, 0);
    assert!(matches!(
        reads
            .snapshot(|_| -> sluice_store::Result<()> {
                panic!("reader failure");
            })
            .await,
        Err(StoreError::RequestPanicked)
    ));
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.append_record(Some(project), event())?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(count(&reads, "records").await, 1);
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn commands_cannot_commit_in_a_closure_or_write_without_invalidation() {
    let (_home, writer, reads, project) = setup().await;
    let error = writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.append_record(Some(project), event())?;
            tx.sql().execute_batch("COMMIT")?;
            Ok(())
        })
        .await
        .unwrap_err();
    assert!(matches!(error, PublicError::Storage { .. }));
    assert_eq!(count(&reads, "records").await, 0);
    let error = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            tx.sql()
                .execute("UPDATE projects SET description='untracked'", [])?;
            Ok(())
        })
        .await
        .unwrap_err();
    assert!(matches!(error, PublicError::Invalid { .. }));
    assert_eq!(version(&reads, project).await, 1);
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn writer_uses_full_sync_foreign_keys_and_busy_timeout() {
    let (_home, writer, _reads, _) = setup().await;
    let settings = writer
        .write(RetrySafety::Idempotent, |tx| {
            let read = |name| -> sluice_store::Result<i64> {
                Ok(tx.sql().pragma_query_value(None, name, |r| r.get(0))?)
            };
            let values = (
                read("synchronous")?,
                read("foreign_keys")?,
                read("busy_timeout")?,
            );
            assert!(tx.sql().execute_batch("PRAGMA foreign_keys=OFF").is_err());
            Ok(values)
        })
        .await
        .unwrap();
    assert_eq!(settings, (2, 1, 5000));
    writer.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caller_cancellation_after_admission_does_not_cancel_commit() {
    let (_home, writer, reads, project) = setup().await;
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let job_writer = writer.clone();
    let job = tokio::spawn(async move {
        job_writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.append_record(Some(project), event())?;
                entered.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            })
            .await
    });
    entered_rx.await.unwrap();
    job.abort();
    assert!(job.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    writer.shutdown().await.unwrap(); // Drains the admitted request before releasing flock.
    assert_eq!(count(&reads, "records").await, 1);
    assert!(
        writer
            .write(RetrySafety::Idempotent, |_| Ok(()))
            .await
            .is_err()
    );
}
