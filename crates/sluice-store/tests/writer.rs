#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use rusqlite::Connection;
use sluice_model::{
    error::PublicError,
    events::Event,
    ids::{HomeId, ProjectId},
};
use sluice_store::{ChangeKey, ReadPool, RetrySafety, StoreError, Writer, WriterOptions};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::Duration,
};

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
async fn fresh_schema_has_23_strict_tables_wal_and_stable_home() {
    let (home, writer, reads, _) = setup().await;
    let c = Connection::open(home.path().join("sluice.db")).unwrap();
    let strict: i64 = c.query_row("SELECT count(*) FROM pragma_table_list WHERE schema='main' AND type='table' AND name NOT LIKE 'sqlite_%' AND strict=1", [], |r| r.get(0)).unwrap();
    assert_eq!(strict, 23);
    assert_eq!(
        c.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    let first: String = reads
        .snapshot(|c| Ok(c.query_row("SELECT home_id FROM home_meta", [], |r| r.get(0))?))
        .await
        .unwrap();
    first.parse::<HomeId>().unwrap();
    drop(c);
    writer.shutdown().await.unwrap();
    let reopened = Writer::open(home.path()).unwrap();
    let second: String = reads
        .snapshot(|c| Ok(c.query_row("SELECT home_id FROM home_meta", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(first, second);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn rollback_keeps_state_records_versions_and_notifications_unchanged() {
    let (_home, writer, reads, project) = setup().await;
    let before = version(&reads, project).await;
    let notifications = writer.subscribe();
    let error = writer
        .write(
            RetrySafety::NonIdempotent,
            move |tx| -> sluice_store::Result<()> {
                tx.sql().execute(
                    "UPDATE projects SET description='gone' WHERE project_id=?1",
                    [project.to_string()],
                )?;
                tx.changed(Some(project), "status");
                tx.append_record(Some(project), event())?;
                Err(PublicError::Conflict {
                    message: "precondition".into(),
                    current_rev: None,
                }
                .into())
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(error, PublicError::Conflict { .. }));
    assert_eq!(count(&reads, "records").await, 0);
    assert_eq!(version(&reads, project).await, before);
    assert!(!notifications.has_changed().unwrap());
    let description: String = reads
        .snapshot(|c| Ok(c.query_row("SELECT description FROM projects", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(description, "");
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn foreign_keys_are_enforced_and_failed_commit_recovers() {
    let (_home, writer, reads, project) = setup().await;
    for deferred in [false, true] {
        let error = writer
            .write(RetrySafety::Idempotent, move |tx| {
                if deferred {
                    tx.sql().execute_batch("PRAGMA defer_foreign_keys=ON")?;
                }
                tx.sql().execute(
                    "INSERT INTO plans(project_id,rev,doc) VALUES (?1,1,'{}')",
                    [ProjectId::new().to_string()],
                )?;
                tx.changed(Some(project), "status");
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(matches!(error, PublicError::Invalid { .. }), "{error:?}");
        assert_eq!(count(&reads, "plans").await, 0);
    }
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "INSERT INTO plans(project_id,rev,doc) VALUES (?1,1,'{}')",
                [project.to_string()],
            )?;
            tx.changed(Some(project), "plan");
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(count(&reads, "plans").await, 1);
    writer.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_callers_have_one_writer_and_monotonic_transaction_order() {
    let (_home, writer, reads, project) = setup().await;
    let thread_ids = Arc::new(Mutex::new(BTreeSet::new()));
    let mut tasks = Vec::new();
    for _ in 0..24 {
        let writer = writer.clone();
        let thread_ids = Arc::clone(&thread_ids);
        tasks.push(tokio::spawn(async move {
            let mut seqs = Vec::new();
            for _ in 0..8 {
                let ids = Arc::clone(&thread_ids);
                seqs.push(writer.write(RetrySafety::NonIdempotent, move |tx| {
                    ids.lock().unwrap().insert(format!("{:?}", std::thread::current().id()));
                    tx.sql().execute("UPDATE projects SET resources_rev=resources_rev+1 WHERE project_id=?1", [project.to_string()])?;
                    tx.changed(Some(project), "status");
                    let record = tx.append_record(Some(project), event())?;
                    let revision: i64 = tx.sql().query_row("SELECT resources_rev FROM projects", [], |r| r.get(0))?;
                    assert_eq!(record.seq.0, revision); Ok(record.seq.0)
                }).await.unwrap());
            }
            assert!(seqs.windows(2).all(|p| p[0] < p[1])); seqs
        }));
    }
    let mut all = BTreeSet::new();
    for task in tasks {
        all.extend(task.await.unwrap());
    }
    assert_eq!(
        all.into_iter().collect::<Vec<_>>(),
        (1..=192).collect::<Vec<_>>()
    );
    assert_eq!(thread_ids.lock().unwrap().len(), 1);
    assert_eq!(version(&reads, project).await, 193);
    writer.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notification_is_published_only_after_commit_and_versions_coalesce() {
    let (_home, writer, reads, project) = setup().await;
    let mut notifications = writer.subscribe();
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let job_writer = writer.clone();
    let job = tokio::spawn(async move {
        job_writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.append_record(Some(project), event())?;
                tx.changed(Some(project), "status");
                tx.changed(Some(project), "status");
                entered.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            })
            .await
    });
    entered_rx.await.unwrap();
    assert!(!notifications.has_changed().unwrap());
    assert_eq!(count(&reads, "records").await, 0);
    assert_eq!(version(&reads, project).await, 1);
    release.send(()).unwrap();
    notifications.changed().await.unwrap();
    assert!(notifications.borrow().changed.contains(&key(project)));
    assert_eq!(count(&reads, "records").await, 1);
    assert_eq!(version(&reads, project).await, 2);
    job.await.unwrap().unwrap();
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn subscribe_recheck_catches_commit_between_cursor_and_subscribe() {
    let (_home, writer, reads, project) = setup().await;
    let cursor = reads.cursor(vec![key(project)]).await.unwrap();
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
    let mut sub = reads.subscribe_after_cursor(&writer, cursor).await.unwrap();
    let after = tokio::time::timeout(Duration::from_secs(1), sub.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.versions[&key(project)], 2);
    assert_eq!(sub.cursor(), &after);
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn coalesced_unrelated_wake_rechecks_all_durable_interests() {
    let (_home, writer, reads, project) = setup().await;
    let mut sub = reads.subscribe(&writer, vec![key(project)]).await.unwrap();
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
    writer
        .write(RetrySafety::NonIdempotent, |tx| {
            tx.changed(None, "unrelated");
            Ok(())
        })
        .await
        .unwrap();
    let cursor = tokio::time::timeout(Duration::from_secs(1), sub.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cursor.versions[&key(project)], 2);
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn commits_to_other_keys_wake_no_durable_read_and_a_watched_commit_reads_once() {
    let (_home, writer, reads, project) = setup().await;
    let mut sub = reads.subscribe(&writer, vec![key(project)]).await.unwrap();
    let wait = sub.wait();
    tokio::pin!(wait);
    // Let the wait take its first durable read and park on notifications.
    tokio::select! {
        biased;
        _ = &mut wait => panic!("nothing changed yet"),
        _ = tokio::time::sleep(Duration::from_millis(100)) => {}
    }
    let before = reads.snapshots();
    // Each commit is observed on its own (both futures share this task, and the
    // notification precedes the write's reply), so none is a coalesced gap.
    for _ in 0..50 {
        tokio::select! {
            biased;
            _ = &mut wait => panic!("woke for an unwatched key"),
            written = writer.write(RetrySafety::NonIdempotent, move |tx| {
                tx.changed(None, "unrelated");
                tx.changed(Some(project), "log");
                Ok(())
            }) => written.unwrap(),
        }
    }
    assert_eq!(reads.snapshots(), before, "unwatched commits must not read");
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
    let cursor = tokio::time::timeout(Duration::from_secs(1), wait)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cursor.versions[&key(project)], 2);
    assert_eq!(reads.snapshots(), before + 1);
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn flock_refuses_second_handle_and_shutdown_releases_it() {
    let (home, writer, _reads, _) = setup().await;
    assert!(matches!(
        Writer::open(home.path()),
        Err(StoreError::WriterLocked)
    ));
    assert!(matches!(
        Writer::open(home.path().join("../home")),
        Err(StoreError::WriterLocked)
    ));
    writer.shutdown().await.unwrap();
    writer.shutdown().await.unwrap();
    let second = Writer::open(home.path()).unwrap();
    second.shutdown().await.unwrap();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_consistent_while_writer_commits_and_releases_afterwards() {
    let (home, writer, reads, project) = setup().await;
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    let pool = reads.clone();
    let snapshot = tokio::spawn(async move {
        pool.snapshot(move |c| {
            let before: i64 = c.query_row("SELECT count(*) FROM records", [], |r| r.get(0))?;
            entered.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let after: i64 = c.query_row("SELECT count(*) FROM records", [], |r| r.get(0))?;
            assert!(
                c.execute_batch("UPDATE projects SET description='illegal'")
                    .is_err()
            );
            Ok((before, after))
        })
        .await
    });
    entered_rx.await.unwrap();
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.append_record(Some(project), event())?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(count(&reads, "records").await, 1);
    release.send(()).unwrap();
    assert_eq!(snapshot.await.unwrap().unwrap(), (0, 0));
    let c = Connection::open(home.path().join("sluice.db")).unwrap();
    let (busy, frames, done): (i64, i64, i64) = c
        .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(busy, 0);
    assert_eq!(frames, done);
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn unique_conflicts_and_shape_constraints_map_to_public_errors() {
    let (_home, writer, _reads, project) = setup().await;
    let duplicate = writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "INSERT INTO projects(project_id,name,created_at) VALUES (?1,'p','now')",
                [ProjectId::new().to_string()],
            )?;
            tx.changed(None, "projects");
            Ok(())
        })
        .await
        .unwrap_err();
    assert!(matches!(duplicate, PublicError::Conflict { .. }));
    for sql in [
        "UPDATE projects SET paused='yes'",
        "UPDATE projects SET archived=2",
        "INSERT INTO plans(project_id,rev,doc) SELECT project_id,1,'[]' FROM projects",
    ] {
        let invalid = writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute_batch(sql)?;
                tx.changed(Some(project), "status");
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(
            matches!(invalid, PublicError::Invalid { .. }),
            "{invalid:?}"
        );
    }
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn busy_is_retryable_only_for_idempotent_requests() {
    let (home, writer, _reads, _) = setup().await;
    writer.shutdown().await.unwrap();
    let writer = Writer::open_with_options(
        home.path(),
        WriterOptions {
            busy_timeout: Duration::from_millis(10),
            queue_capacity: 2,
        },
    )
    .unwrap();
    let holder = Connection::open(home.path().join("sluice.db")).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    for safety in [RetrySafety::NonIdempotent, RetrySafety::Idempotent] {
        let error = writer
            .write(safety, |tx| {
                tx.append_record(None, event())?;
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(
            matches!(error, PublicError::Busy { retryable, .. } if retryable == safety.is_idempotent())
        );
    }
    holder.execute_batch("ROLLBACK").unwrap();
    writer
        .write(RetrySafety::Idempotent, |tx| {
            tx.append_record(None, event())?;
            Ok(())
        })
        .await
        .unwrap();
    writer.shutdown().await.unwrap();
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

#[tokio::test]
async fn active_attempt_unique_index_is_per_step_generation_and_item() {
    let (_home, writer, reads, project) = setup().await;
    let first = sluice_model::ids::AttemptId::new().to_string();
    let active = first.clone();
    writer.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,generation,item_index,phase,request,inputs_hash,created_at) VALUES (?1,?2,'a',1,0,'reserved','{}','hash','now')", (first, project.to_string()))?;
        tx.changed(Some(project), "status"); Ok(())
    }).await.unwrap();
    let duplicate = writer.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,generation,item_index,phase,request,inputs_hash,created_at) VALUES (?1,?2,'a',1,0,'reserved','{}','hash','now')", (sluice_model::ids::AttemptId::new().to_string(), project.to_string()))?;
        tx.changed(Some(project), "status"); Ok(())
    }).await.unwrap_err();
    assert!(matches!(duplicate, PublicError::Conflict { .. }));
    writer.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute("UPDATE attempts SET phase='terminal' WHERE attempt_id=?1", [active])?;
        tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,generation,item_index,phase,request,inputs_hash,created_at) VALUES (?1,?2,'a',1,0,'reserved','{}','hash','now')", (sluice_model::ids::AttemptId::new().to_string(), project.to_string()))?;
        tx.changed(Some(project), "status"); Ok(())
    }).await.unwrap();
    assert_eq!(count(&reads, "attempts").await, 2);
    writer.shutdown().await.unwrap();
}
