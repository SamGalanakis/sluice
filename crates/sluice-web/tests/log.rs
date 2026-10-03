use axum::{body::Body, http::Request};
use sluice_model::{
    commands::{MessageAnswer, MessagePost},
    events::Event,
    ids::Revision,
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    messages::{self, NoPlanInputs},
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use sluice_web::views::{
    DashboardState, EmptyCatalog, Viewer,
    log::{self, LogQuery},
};
use std::sync::Arc;
use tower::ServiceExt;
#[test]
fn p605_query_keeps_repeated_filters_and_rejects_invalid_or_ambiguous_cursors() {
    let query =
        LogQuery::parse("kind=message,step&kind=step&thread=a,b&thread=a&before=10").unwrap();
    assert_eq!(query.kinds, ["message", "step"]);
    assert_eq!(query.threads, ["a", "b"]);
    assert_eq!(
        LogQuery::parse(&query.query(Some(10), None)).unwrap(),
        query
    );
    for raw in [
        "before=-1",
        "after=abc",
        "before=1&after=2",
        "before=9223372036854775808",
    ] {
        assert!(LogQuery::parse(raw).is_err(), "{raw}");
    }
}
#[tokio::test]
async fn p605_log_keyset_pages_preserve_filters_hide_successful_message_calls_and_keep_failures() {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let project = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "log".parse().unwrap(),
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
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            for n in 0..115 {
                tx.append_record(
                    Some(project),
                    Event::PlanEdit {
                        rev: Revision(n + 1),
                        author: "owner".into(),
                        reason: format!("change {n} <script>"),
                        ops: vec![],
                    },
                )?;
            }
            for status in [
                sluice_model::commands::StepStatus::Succeeded,
                sluice_model::commands::StepStatus::Failed,
            ] {
                tx.append_record(
                    Some(project),
                    Event::Call {
                        call: sluice_model::ids::RunId::new(),
                        name: "message.post".into(),
                        status,
                        inputs: None,
                        outputs: None,
                        error: None,
                        direct: false,
                        author: Some("owner".into()),
                    },
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let reads = ReadPool::open(home.path(), 1).unwrap();
    let newest = log::load(&reads, Some(project), LogQuery::default())
        .await
        .unwrap();
    assert_eq!(newest.rows.len(), 50);
    assert_eq!(newest.rows.iter().filter(|r| r.kind == "call").count(), 1);
    assert!(newest.rows[0].summary.contains("failed"));
    let first_seq = newest.rows.last().unwrap().seq;
    let older = log::load(
        &reads,
        Some(project),
        LogQuery {
            before: Some(first_seq),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(!older.rows.is_empty());
    assert!(older.rows.iter().all(|r| r.seq < first_seq));
    assert!(!older.older.is_empty());
    assert!(!older.newer.is_empty());
    let returned = log::load(
        &reads,
        Some(project),
        LogQuery {
            after: Some(older.rows[0].seq),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        returned.rows.iter().map(|r| r.seq).collect::<Vec<_>>(),
        newest.rows.iter().map(|r| r.seq).collect::<Vec<_>>()
    );
    let calls = log::load(
        &reads,
        Some(project),
        LogQuery {
            kinds: vec!["call".into()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(calls.rows.len(), 2);
    let html = newest.render(&Viewer::default()).unwrap();
    assert!(!html.as_str().contains("change 1 <script>"));
    assert!(html.as_str().contains("data-preserve-attr=\"open\""));
    assert!(
        !older
            .render(&Viewer::default())
            .unwrap()
            .as_str()
            .contains("data-init=")
    );
    let router = log::router(DashboardState::new(reads, Arc::new(EmptyCatalog)));
    for (query, status) in [
        ("kind=inbox", 400),
        ("before=1&after=2", 400),
        ("kind=plan", 200),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/projects/id/{project}/log?{query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
}
#[tokio::test]
async fn p605_thread_filter_uses_message_records_and_log_retention_cannot_erase_conversations() {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let project = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "messages".parse().unwrap(),
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
    for thread in ["a", "b"] {
        let post:MessagePost=serde_json::from_value(serde_json::json!({"project":{"kind":"id","value":project.to_string()},"thread":thread,"from":"worker","to":"owner","body":thread,"needs_reply":false})).unwrap();
        writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                messages::message_post(tx, post, &NoPlanInputs)
            })
            .await
            .unwrap();
    }
    let reads = ReadPool::open(home.path(), 1).unwrap();
    let page = log::load(
        &reads,
        Some(project),
        LogQuery {
            threads: vec!["a".into()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert!(page.rows[0].summary.contains("a from worker"));
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            sluice_store::records::trim_to(tx, Some(project), 1, 1)?;
            Ok(())
        })
        .await
        .unwrap();
    let history = sluice_web::views::threads::load(
        &reads,
        Some(project),
        sluice_model::commands::MessageView::History,
        None,
    )
    .await
    .unwrap();
    assert_eq!(history.threads.len(), 2);
    let _type_contract = MessageAnswer {
        action: "close".into(),
        params: None,
        values: None,
    };
}
