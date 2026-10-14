#[allow(dead_code)]
#[path = "../../../../tests/support/executable.rs"]
mod executable;
#[allow(dead_code)]
#[path = "../../../../tests/support/home.rs"]
mod home;
#[path = "../../../../tests/support/plan_rows.rs"]
mod index_plan_rows;
#[allow(dead_code)]
#[path = "../../../sluice-store/tests/support/plan_rows.rs"]
mod store_plan_rows;
#[allow(dead_code)]
#[path = "../../../../tests/support/messages.rs"]
mod stored_messages;

mod adopt_fast;
mod agent_models;
mod calls;
mod coordinator;
mod dispatch_ext;
mod drain;
mod edit_contention;
mod edit_replies;
mod git_gh;
mod inline;
mod jev;
mod message_builtins;
mod next;
mod notify;
mod plan_rows_gates;
mod plan_scale;
mod python_helper;
mod registry;
mod retire;
mod scheduler;
mod socket_errors;
mod verify;
mod watch;

async fn file_ready(
    path: &std::path::Path,
    ready: impl Fn(&std::path::Path) -> bool,
    limit: std::time::Duration,
) {
    use ::notify::Watcher;
    let (send, mut events) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = ::notify::recommended_watcher(move |event| {
        let _ = send.send(event);
    })
    .unwrap();
    watcher
        .watch(
            path.parent().unwrap(),
            ::notify::RecursiveMode::NonRecursive,
        )
        .unwrap();
    tokio::time::timeout(limit, async {
        while !ready(path) {
            events.recv().await.expect("watcher stopped").unwrap();
        }
    })
    .await
    .expect("fixture file never became ready");
}

mod scope;

mod bypass;
