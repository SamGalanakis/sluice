use sluice_model::commands::{MessagePost, MessageView};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    messages::{self, NoPlanInputs},
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use sluice_web::views::{Viewer, threads};
#[tokio::test]
async fn p605_history_includes_full_owner_conversations_without_marking_unrendered_messages_read() {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let project = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "history".parse().unwrap(),
                    description: String::new(),
                    icon: None,
                    resources: None,
                    author: "owner".into(),
                },
                &EmptyPlanInitializer,
                &NoResourceSettings,
            )
        })
        .await
        .unwrap()
        .project_id;
    for (thread, from, to) in [
        ("arc/a #1", "worker", "owner"),
        ("arc/a #1", "orchestrator", "worker"),
        ("private", "worker", "orchestrator"),
    ] {
        let post: MessagePost = serde_json::from_value(serde_json::json!({"project":{"kind":"id","value":project.to_string()},"thread":thread,"from":from,"to":to,"body":"**Evidence** and a decision","needs_reply":false})).unwrap();
        writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                messages::message_post(tx, post, &NoPlanInputs)
            })
            .await
            .unwrap();
    }
    let reads = ReadPool::open(home.path(), 1).unwrap();
    let history = threads::load(&reads, Some(project), MessageView::History, None)
        .await
        .unwrap();
    assert_eq!(history.threads.len(), 1);
    assert_eq!(history.threads[0].messages.len(), 2);
    assert!(history.threads[0].href().contains("thread=arc%2Fa+%231"));
    let html = history
        .render(&Viewer::default(), "/history", "/history/stream")
        .unwrap();
    assert!(!html.as_str().contains("data-read-url"));
    let thread = threads::load(
        &reads,
        Some(project),
        MessageView::Thread,
        Some("arc/a #1".into()),
    )
    .await
    .unwrap();
    let html = thread.body().unwrap();
    assert!(html.as_str().contains("<strong>Evidence</strong>"));
    assert_eq!(html.as_str().matches("data-seq=").count(), 2);
    assert!(html.as_str().contains("data-through="));
    assert_eq!(
        threads::load(&reads, Some(project), MessageView::Inbox, None)
            .await
            .unwrap()
            .threads
            .len(),
        1
    );
}
