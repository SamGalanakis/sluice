use super::stored_messages;
use sluice_model::ids::{ProjectId, RecordSeq, RunId};
use sluice_runtime::watch::*;
use sluice_store::{ReadPool, RetrySafety, Writer};
use std::{path::PathBuf, time::Duration};
use stored_messages::{Stored, stored};
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
