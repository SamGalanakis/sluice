use sluice_store::{ReadPool, RetrySafety, Writer};
use sluice_web::views::{home::HomeView, *};
use std::sync::Arc;

#[tokio::test]
async fn runner_line_follows_the_scheduler_lease_and_is_versioned() {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let state = DashboardState::new(
        ReadPool::open(home.path(), 1).unwrap(),
        Arc::new(EmptyCatalog),
    );
    let lease = |owner: Option<&'static str>| {
        writer.write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE maintenance SET scheduler_owner=?1 WHERE singleton=1",
                [owner],
            )?;
            tx.changed(None, "scheduler");
            Ok(())
        })
    };
    let render = |snapshot: &DashboardSnapshot| {
        // the band says it (`HomeView::head`)
        HomeView::new(snapshot).head().as_str().to_owned()
    };
    // No loop holds the lease: nothing new starts, and the home page says so.
    let stopped = state.snapshot(None).await.unwrap();
    assert!(stopped.runner_stopped);
    assert!(render(&stopped).contains("Runner stopped"));
    lease(Some("loop")).await.unwrap();
    let running = state.snapshot(None).await.unwrap();
    assert!(!running.runner_stopped);
    assert!(!render(&running).contains("Runner stopped"));
    assert_ne!(stopped.version(), running.version());
    // The holder's connection closing releases the lease.
    lease(None).await.unwrap();
    assert!(state.snapshot(None).await.unwrap().runner_stopped);
}
