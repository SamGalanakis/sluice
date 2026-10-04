#[allow(dead_code)]
#[path = "../../../tests/support/messages.rs"]
mod stored_messages;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use sluice_model::{
    commands::{CommandReply, CommandRequest, MessageView},
    ids::{ProjectId, ProjectSelector},
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    messages::{self, Post},
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use sluice_web::views::{
    DashboardState, EmptyCatalog, Viewer,
    inbox::{self, CommandFuture, MessageCommands, MessageState},
    threads,
};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

struct TypedInputs;
impl messages::PlanInputSetter for TypedInputs {
    fn set_input(
        &self,
        tx: &mut sluice_store::WriteTransaction<'_>,
        update: messages::InputAnswer<'_>,
    ) -> sluice_store::Result<()> {
        let declaration: String = tx.sql().query_row(
            "SELECT declaration FROM inputs WHERE project_id=?1 AND name=?2",
            (update.project.to_string(), update.name),
            |r| r.get(0),
        )?;
        let raw: serde_json::Value = serde_json::from_str(&declaration)?;
        let ty = sluice_model::types::Type::parse(&raw["type"]).unwrap();
        sluice_model::types::check_value_at(&ty, update.value.as_value(), "input").map_err(
            |errors| sluice_model::error::PublicError::Invalid {
                message: "invalid typed input".into(),
                errors: errors.into_iter().map(|e| e.to_string()).collect(),
            },
        )?;
        tx.sql().execute(
            "UPDATE inputs SET value=?3 WHERE project_id=?1 AND name=?2",
            (
                update.project.to_string(),
                update.name,
                serde_json::to_string(update.value)?,
            ),
        )?;
        tx.changed(Some(update.project), "plan");
        Ok(())
    }
}
struct Commands {
    writer: Writer,
    requests: Mutex<Vec<CommandRequest>>,
}
impl MessageCommands for Commands {
    fn command(&self, request: CommandRequest) -> CommandFuture<'_> {
        self.requests.lock().unwrap().push(request.clone());
        let writer = self.writer.clone();
        Box::pin(async move {
            writer
                .write(RetrySafety::NonIdempotent, move |tx| match request {
                    CommandRequest::Ask(m) => Ok(CommandReply::Receipt(
                        messages::post(tx, Post::try_from(m)?, &TypedInputs)?.receipt,
                    )),
                    CommandRequest::Say(m) => Ok(CommandReply::Receipt(
                        messages::post(tx, Post::try_from(m)?, &TypedInputs)?.receipt,
                    )),
                    CommandRequest::Reply(m) => Ok(CommandReply::Receipt(
                        messages::post(tx, Post::try_from(m)?, &TypedInputs)?.receipt,
                    )),
                    CommandRequest::MarkRead(read) => {
                        messages::mark_read(tx, read)?;
                        Ok(CommandReply::Ack)
                    }
                    _ => unreachable!(),
                })
                .await
        })
    }
}
async fn fixture() -> (
    tempfile::TempDir,
    Writer,
    ProjectId,
    MessageState,
    Arc<Commands>,
) {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let project = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "fixture".parse().unwrap(),
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
    let commands = Arc::new(Commands {
        writer: writer.clone(),
        requests: Mutex::new(vec![]),
    });
    let state = MessageState {
        dashboard: DashboardState::new(
            ReadPool::open(home.path(), 2).unwrap(),
            Arc::new(EmptyCatalog),
        ),
        commands: commands.clone(),
    };
    (home, writer, project, state, commands)
}
/// A message as the home stores it (default: a worker's note to the owner on `a`).
async fn post(writer: &Writer, project: ProjectId, data: serde_json::Value) -> i64 {
    let text = |key: &str| data.get(key).and_then(|v| v.as_str()).map(str::to_owned);
    let (thread, to, body, title, ui, input) = (
        text("thread").unwrap_or_else(|| "a".into()),
        text("to").unwrap_or_else(|| "owner".into()),
        text("body").unwrap_or_else(|| "message".into()),
        text("title"),
        text("ui"),
        text("input"),
    );
    stored_messages::stored(
        writer,
        project,
        stored_messages::Stored {
            thread: &thread,
            from: "worker",
            to: Some(&to),
            body: &body,
            title: title.as_deref(),
            ui: ui.as_deref(),
            input: input.as_deref(),
            question: data["needs_reply"] == true,
            reply: None,
        },
    )
    .await
    .id
    .0
}
async fn send(
    router: &axum::Router,
    url: &str,
    body: serde_json::Value,
) -> axum::response::Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(url)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn p605_views_keep_questions_separate_and_advance_only_the_rendered_thread() {
    let (_home, writer, project, state, commands) = fixture().await;
    let b = post(
        &writer,
        project,
        serde_json::json!({"thread":"b","body":"older unread note"}),
    )
    .await;
    let a = post(
        &writer,
        project,
        serde_json::json!({"body":"newer unread note"}),
    )
    .await;
    let q = post(
        &writer,
        project,
        serde_json::json!({"needs_reply":true,"title":"Owner question"}),
    )
    .await;
    post(&writer, project, serde_json::json!({"needs_reply":true,"to":"orchestrator","thread":"private","title":"Worker question"})).await;
    let page = threads::load(
        &state.dashboard.reads,
        Some(project),
        MessageView::Inbox,
        None,
    )
    .await
    .unwrap();
    assert_eq!(page.questions.len(), 1);
    assert_eq!(page.questions[0].id(), q);
    assert_eq!(page.threads.len(), 2);
    assert_eq!(page.nav.inbox, 1);
    assert_eq!(
        commands.requests.lock().unwrap().len(),
        0,
        "GET/render must never mutate readers"
    );
    let html = page
        .render(&Viewer::default(), "/inbox", "/inbox/stream")
        .unwrap();
    assert!(
        html.as_str().find("Owner question").unwrap() < html.as_str().find("Unread notes").unwrap()
    );
    let router = inbox::router(state.clone());
    for _ in 0..2 {
        assert_eq!(
            send(
                &router,
                &format!("/projects/id/{project}/messages/read"),
                serde_json::json!({"thread":"a","through":a})
            )
            .await
            .status(),
            200
        );
    }
    let page = threads::load(
        &state.dashboard.reads,
        Some(project),
        MessageView::Inbox,
        None,
    )
    .await
    .unwrap();
    assert_eq!(page.questions.len(), 1);
    assert_eq!(page.threads.len(), 1);
    assert_eq!(page.threads[0].messages[0].id(), b);
    let questions = threads::load(
        &state.dashboard.reads,
        Some(project),
        MessageView::Questions,
        None,
    )
    .await
    .unwrap();
    assert_eq!(questions.questions.len(), 2);
    assert!(questions.threads.is_empty());
    assert!(
        matches!(&commands.requests.lock().unwrap()[0], CommandRequest::MarkRead(r) if r.identity == "owner")
    );
}
#[tokio::test]
async fn p605_typed_ui_reply_is_atomic_and_stale_buttons_conflict() {
    let (_home, writer, project, state, commands) = fixture().await;
    let question = post(&writer, project, serde_json::json!({"needs_reply":true})).await;
    let router = inbox::router(state.clone());
    let url = format!("/projects/id/{project}/messages/{question}/reply");
    let payload = serde_json::json!({"body":"","answer":{"action":"submit","params":{"value":null},"values":{"value":42,"confirmed":true}}});
    assert_eq!(send(&router, &url, payload.clone()).await.status(), 200);
    assert_eq!(
        send(&router, &url, payload).await.status(),
        StatusCode::CONFLICT
    );
    let questions = threads::load(
        &state.dashboard.reads,
        Some(project),
        MessageView::Inbox,
        None,
    )
    .await
    .unwrap();
    assert!(questions.questions.is_empty());
    assert_eq!(questions.nav.inbox, 0);
    let requests = commands.requests.lock().unwrap();
    let CommandRequest::Reply(post) = &requests[0] else {
        panic!()
    };
    assert!(post.owner);
    assert_eq!(post.run, None);
    assert_eq!(post.to_message.0, question);
    assert_eq!(post.project, ProjectSelector::Id(project));
    let json = serde_json::to_value(&post.answer).unwrap();
    assert_eq!(json["values"]["value"], 42);
    assert_eq!(json["values"]["confirmed"], true);
}
#[tokio::test]
async fn p605_invalid_input_answer_stays_open_and_close_bypasses_input() {
    let (_home, writer, project, state, _) = fixture().await;
    writer.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute("INSERT INTO inputs(project_id,name,position,declaration) VALUES(?1,'count',0,'{\"type\":\"int\"}')",[project.to_string()])?;
        tx.changed(Some(project),"plan"); Ok(())
    }).await.unwrap();
    let question = post(
        &writer,
        project,
        serde_json::json!({"needs_reply":true,"input":"count"}),
    )
    .await;
    let router = inbox::router(state.clone());
    let url = format!("/projects/id/{project}/messages/{question}/reply");
    assert_eq!(send(&router,&url,serde_json::json!({"body":"bad","answer":{"action":"submit","values":{"value":"wrong"}}})).await.status(),422);
    assert_eq!(
        threads::load(
            &state.dashboard.reads,
            Some(project),
            MessageView::Inbox,
            None
        )
        .await
        .unwrap()
        .questions
        .len(),
        1
    );
    assert_eq!(
        send(
            &router,
            &url,
            serde_json::json!({"body":"","answer":{"action":"close"}})
        )
        .await
        .status(),
        200
    );
    let thread = threads::load(
        &state.dashboard.reads,
        Some(project),
        MessageView::Thread,
        Some("a".into()),
    )
    .await
    .unwrap();
    assert_eq!(thread.threads[0].messages[0].state, "closed");
    let question = post(
        &writer,
        project,
        serde_json::json!({"needs_reply":true,"input":"count","thread":"numeric"}),
    )
    .await;
    let response = send(
        &router,
        &format!("/projects/id/{project}/messages/{question}/reply"),
        serde_json::json!({"body":"", "answer":{"action":"submit","values":{"value":7}}}),
    )
    .await;
    assert_eq!(response.status(), 200);
    let value: String = state
        .dashboard
        .reads
        .snapshot(move |sql| {
            Ok(sql.query_row(
                "SELECT value FROM inputs WHERE project_id=?1 AND name='count'",
                [project.to_string()],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(value, "7");
}
#[tokio::test]
async fn p605_pages_escape_programs_and_validate_projects_thread_and_reply_payloads() {
    let (_home, writer, project, state, _) = fixture().await;
    post(&writer,project,serde_json::json!({"needs_reply":true,"title":"<script>bad</script>","body":"<img src=x onerror=alert(1)>","ui":"root = Stack([Text(\"<script>program\")])"})).await;
    let router = inbox::router(state);
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/inbox")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let html = String::from_utf8(
        to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(!html.contains("<script>bad"));
    assert!(!html.contains("<img src=x"));
    assert!(html.contains("data-ignore-morph"));
    assert!(html.contains("/static/inbox.js"));
    for path in [
        format!("/projects/id/{project}/thread"),
        format!("/projects/id/{}/inbox", ProjectId::new()),
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(response.status().is_client_error());
    }
    assert_eq!(
        send(
            &router,
            &format!("/projects/id/{project}/messages"),
            serde_json::json!({"body":"","from":"attacker"})
        )
        .await
        .status(),
        400
    );
}

#[tokio::test]
async fn the_owner_composes_ask_or_say_to_the_thread_step_or_the_orchestrator() {
    let (_home, writer, project, state, commands) = fixture().await;
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "INSERT INTO steps(project_id,step_id,position,declaration) VALUES (?1,'work',0,'{}')",
                [project.to_string()],
            )?;
            tx.changed(Some(project), "plan");
            Ok(())
        })
        .await
        .unwrap();
    post(&writer, project, serde_json::json!({"thread":"step-work"})).await;
    post(&writer, project, serde_json::json!({"thread":"step-gone"})).await;
    let page = |thread: &str| {
        let reads = state.dashboard.reads.clone();
        let thread = thread.to_owned();
        async move {
            threads::load(&reads, Some(project), MessageView::Thread, Some(thread))
                .await
                .unwrap()
        }
    };
    let work = page("step-work").await;
    assert_eq!(work.threads[0].recipient, "work");
    let html = work
        .render(&Viewer::default(), "/thread", "/thread/stream")
        .unwrap();
    assert!(
        html.as_str()
            .contains(r#"<input type="hidden" name="to" value="work">"#)
    );
    assert!(html.as_str().contains("Message to work"));
    assert!(html.as_str().contains(r#"name="ask" value="true""#));
    assert!(!html.as_str().contains("needs_reply"));
    // A step no longer in the plan: the owner writes to the orchestrator instead.
    assert_eq!(page("step-gone").await.threads[0].recipient, "orchestrator");
    let router = inbox::router(state.clone());
    let url = format!("/projects/id/{project}/messages");
    for body in [
        serde_json::json!({"body":"which db?","to":"work","ask":true}),
        serde_json::json!({"body":"fyi","to":"orchestrator"}),
    ] {
        let response = send(&router, &url, body).await;
        assert_eq!(response.status(), 200);
        let receipt: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.unwrap())
                .unwrap();
        assert_eq!(receipt["reply"], "receipt");
        assert!(receipt["data"]["delivery"].is_string(), "{receipt}");
    }
    let requests = commands.requests.lock().unwrap().clone();
    assert!(matches!(&requests[0], CommandRequest::Ask(a) if a.owner && a.to == "work"));
    assert!(matches!(&requests[1], CommandRequest::Say(s) if s.owner && s.to == "orchestrator"));
    let thread = page("step-work").await;
    let last = &thread.threads[0].messages.last().unwrap().message;
    assert_eq!(
        (last.from.as_str(), last.to.as_deref()),
        ("owner", Some("work"))
    );
    assert_eq!(
        last.state,
        Some(sluice_model::commands::QuestionState::Open)
    );
    // A reply goes to the question's sender, so the owner gets no form to answer its own.
    let html = thread
        .render(&Viewer::default(), "/thread", "/thread/stream")
        .unwrap();
    assert!(html.as_str().contains("Awaiting reply"));
    assert!(!html.as_str().contains("Close question"));
    // A recipient is required, and only the composed message has one.
    assert_eq!(
        send(&router, &url, serde_json::json!({"body":"x"}))
            .await
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        send(&router, &url, serde_json::json!({"body":"x","to":"nobody"}))
            .await
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
}
