#[allow(dead_code)]
#[path = "../../../tests/support/messages.rs"]
mod stored_messages;
use sluice_model::{
    error::PublicError,
    ids::{ProjectId, RecordSeq, RunId},
};
use sluice_runtime::watch::*;
use sluice_store::{ReadPool, RetrySafety, Writer, messages};
use std::{path::PathBuf, time::Duration};
use stored_messages::{Stored, stored};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;
struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("sluice-test-p3-04-{}", RunId::new()));
        let p = sluice_process::host::guard_scratch_home(&p).unwrap();
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn setup() -> (Home, Writer, ReadPool, ProjectId) {
    let h = Home::new();
    let w = Writer::open(&h.0).unwrap();
    let r = ReadPool::open(&h.0, 2).unwrap();
    let p = ProjectId::new();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "INSERT INTO projects(project_id,name,created_at) VALUES (?1,'p','now')",
            [p.to_string()],
        )?;
        tx.changed(Some(p), "projects");
        Ok(())
    })
    .await
    .unwrap();
    (h, w, r, p)
}
async fn post(w: &Writer, p: ProjectId, thread: &str, body: &str, question: bool) {
    stored(
        w,
        p,
        Stored {
            thread,
            from: "worker",
            to: Some("orchestrator"),
            body,
            question,
            ..Stored::default()
        },
    )
    .await;
}
#[tokio::test]
async fn watch_flushes_wakes_and_holds_notes_until_question() {
    let (_h, w, r, p) = setup().await;
    let (mut sink, source) = tokio::io::duplex(65536);
    let stop = CancellationToken::new();
    let task_stop = stop.clone();
    let task_w = w.clone();
    let task_r = r.clone();
    let opts = NextOptions {
        projects: vec![p],
        since_seq: Some(RecordSeq(0)),
        settle: Duration::ZERO,
        ..NextOptions::default()
    };
    let stream =
        tokio::spawn(async move { watch(&task_w, &task_r, opts, &mut sink, task_stop).await });
    let mut lines = BufReader::new(source).lines();
    post(&w, p, "q", "note", false).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), lines.next_line())
            .await
            .is_err()
    );
    post(&w, p, "q", "question", true).await;
    let first = tokio::time::timeout(Duration::from_secs(2), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(first.contains("note"));
    assert!(second.contains("question"));
    stop.cancel();
    stream.await.unwrap().unwrap();
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn kinds_threads_and_from_now_filter_stream_without_notification_race() {
    let (_h, w, r, p) = setup().await;
    post(&w, p, "q", "before", true).await;
    let options = NextOptions {
        projects: vec![p],
        timeout: Some(Duration::from_millis(150)),
        settle: Duration::ZERO,
        kinds: vec!["message".into()],
        threads: vec!["q".into()],
        all: true,
        ..NextOptions::default()
    };
    let task_w = w.clone();
    let task_r = r.clone();
    let waiter = tokio::spawn(async move { next(&task_w, &task_r, options).await });
    // Observe the heartbeat rather than sleeping to guess when subscription is ready.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let pos = r
                .snapshot(move |sql| {
                    messages::reader(sql, p, "orchestrator", messages::ORCHESTRATOR_STREAM, "")
                })
                .await
                .unwrap();
            if pos.heartbeat_at.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    post(&w, p, "r", "elsewhere", true).await;
    post(&w, p, "q", "after", true).await;
    let got = waiter.await.unwrap().unwrap();
    assert_eq!(got.records.len(), 1);
    assert!(
        render(&got, sluice_model::commands::Settles::Full, 600, true)
            .unwrap()
            .contains("after")
    );
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn cursor_then_subscribe_rechecks_commit_in_the_gap() {
    let (_h, w, r, p) = setup().await;
    let keys = vec![sluice_store::ChangeKey::new(Some(p), "log")];
    let cursor = r.cursor(keys).await.unwrap();
    post(&w, p, "q", "gap", true).await;
    let mut subscription = r.subscribe_after_cursor(&w, cursor).await.unwrap();
    tokio::time::timeout(Duration::from_millis(200), subscription.wait())
        .await
        .unwrap()
        .unwrap();
    let got = next(
        &w,
        &r,
        NextOptions {
            projects: vec![p],
            since_seq: Some(RecordSeq(0)),
            timeout: Some(Duration::ZERO),
            settle: Duration::ZERO,
            ..NextOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(got.records.len(), 1);
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn invalid_filters_and_expired_stream_cursor_return_public_errors() {
    let (_h, w, r, p) = setup().await;
    let bad = next(
        &w,
        &r,
        NextOptions {
            projects: vec![p],
            since_seq: Some(RecordSeq(0)),
            kinds: vec!["nope".into()],
            ..NextOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(bad, PublicError::BadRequest { .. }));
    post(&w, p, "q", "old", true).await;
    post(&w, p, "q", "new", true).await;
    w.write(RetrySafety::NonIdempotent, move |tx| {
        sluice_store::records::trim_to(tx, Some(p), 1, 1)?;
        Ok(())
    })
    .await
    .unwrap();
    let mut out = tokio::io::sink();
    let error = watch(
        &w,
        &r,
        NextOptions {
            projects: vec![p],
            since_seq: Some(RecordSeq(0)),
            ..NextOptions::default()
        },
        &mut out,
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, PublicError::CursorExpired { .. }));
    w.shutdown().await.unwrap();
}
