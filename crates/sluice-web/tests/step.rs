use axum::{
    Extension,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::json;
use sluice_model::{
    commands::StepStatus,
    error::PublicError,
    gates::{StateSnapshot, StepState},
    ids::ProjectId,
    plan::{FnSignature, Plan, SignatureProvider},
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use sluice_web::views::{
    self,
    board::{Registry, RegistrySnapshot, RegistrySource},
    step::{Action, CommandService, Commands, OwnerCommand, StepView},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;
struct Signatures;
impl SignatureProvider for Signatures {
    fn signature(&self, _: &str) -> Option<FnSignature> {
        Some(FnSignature {
            open: true,
            ..Default::default()
        })
    }
}
#[test]
fn detail_preserves_missing_null_default_bindings_and_escapes_error() {
    let plan=Plan::parse_json(br#"{"steps":{"a":{"run":"core.external","outputs":{"nil":"Any"}},"b":{"run":"core.external","doc":"<script>bad</script>","in":{"missing":{"source":"a/nil"},"literal":{"default":null},"file":{"file":"/tmp/a"}},"outputs":{"ready":"boolean"}}}}"#,&Signatures).unwrap();
    let mut state = StateSnapshot::default();
    state.steps.insert(
        "b".parse().unwrap(),
        StepState {
            status: StepStatus::Failed,
            error: Some("bad <script>error</script>".into()),
            ..Default::default()
        },
    );
    let view = StepView::new(ProjectId::new(), &plan, &state, &"b".parse().unwrap());
    assert!(
        !view
            .inputs
            .iter()
            .find(|f| f.name == "missing")
            .unwrap()
            .available
    );
    let literal = view.inputs.iter().find(|f| f.name == "literal").unwrap();
    assert!(literal.available);
    assert_eq!(literal.value, "null");
    assert!(
        view.inputs
            .iter()
            .find(|f| f.name == "file")
            .unwrap()
            .value
            .contains("/tmp/a")
    );
    let html = view.body().unwrap();
    assert!(!html.as_str().contains("<script>"));
    assert!(html.as_str().contains("&#60;script&#62;"));
    assert!(view.retryable());
}
struct Catalog;
impl views::CatalogSource for Catalog {
    fn catalog(&self, _: Option<ProjectId>) -> Result<views::FunctionCatalog, PublicError> {
        Ok(views::FunctionCatalog::default())
    }
}
struct Exact;
impl RegistrySource for Exact {
    fn signatures(&self, _: ProjectId) -> Result<RegistrySnapshot, PublicError> {
        Ok(RegistrySnapshot {
            version: "1".into(),
            functions: vec![(
                "custom.open".into(),
                FnSignature {
                    open: true,
                    ..Default::default()
                },
            )],
        })
    }
}
#[derive(Default)]
struct Fake(Mutex<Vec<OwnerCommand>>);
impl CommandService for Fake {
    fn execute(
        &self,
        command: OwnerCommand,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicError>> + Send + '_>> {
        self.0.lock().unwrap().push(command);
        Box::pin(async { Ok(()) })
    }
}
async fn fixture() -> (tempfile::TempDir, Writer, views::DashboardState, ProjectId) {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let project = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "drawer".parse().unwrap(),
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
    writer.write(RetrySafety::NonIdempotent,move|tx|{let doc=json!({"steps":{"work":{"run":"custom.open","outputs":{"ready":"boolean"}}}});tx.sql().execute("UPDATE plans SET doc=?2 WHERE project_id=?1",(project.to_string(),doc.to_string()))?;tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,status) VALUES(?1,'work',0,?2,'failed')",(project.to_string(),doc["steps"]["work"].to_string()))?;tx.changed(Some(project), "project");Ok(())}).await.unwrap();
    let state =
        views::DashboardState::new(ReadPool::open(home.path(), 2).unwrap(), Arc::new(Catalog));
    (home, writer, state, project)
}
#[tokio::test]
async fn router_uses_injected_exact_signatures_and_owner_commands() {
    let (_home, _writer, state, project) = fixture().await;
    let fake = Arc::new(Fake::default());
    let app = views::dashboard_router(state)
        .layer(Extension(Registry(Arc::new(Exact))))
        .layer(Extension(Commands(fake.clone())));
    let path = format!("/projects/id/{project}/steps/work");
    let response = app
        .clone()
        .oneshot(Request::builder().uri(&path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("custom.open"));
    assert!(body.contains("ready"));
    assert!(body.contains("Thread"));
    for (body, status) in [
        ("action=retry&revision=0", StatusCode::CONFLICT),
        ("action=cancel&revision=1", StatusCode::BAD_REQUEST),
        (
            "action=retry&revision=1&message=Try+again",
            StatusCode::SEE_OTHER,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("{path}/actions"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    let calls = fake.0.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].project, project);
    assert_eq!(calls[0].action, Action::Retry);
    assert_eq!(calls[0].author, "owner");
    assert_eq!(calls[0].message, "Try again");
}
#[tokio::test]
async fn deleted_or_old_names_and_missing_steps_are_not_rendered() {
    let (_home, writer, state, project) = fixture().await;
    let app = views::dashboard_router(state).layer(Extension(Registry(Arc::new(Exact))));
    for (path, status) in [
        ("/projects/drawer".into(), StatusCode::TEMPORARY_REDIRECT),
        ("/projects/old-name".into(), StatusCode::NOT_FOUND),
        (
            format!("/projects/id/{project}/steps/missing"),
            StatusCode::NOT_FOUND,
        ),
    ] {
        assert_eq!(
            app.clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            status
        );
    }
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE projects SET deleted_at='2026-10-03T12:00:00Z' WHERE project_id=?1",
                [project.to_string()],
            )?;
            tx.changed(Some(project), "project");
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        app.oneshot(
            Request::builder()
                .uri(format!("/projects/id/{project}"))
                .body(Body::empty())
                .unwrap()
        )
        .await
        .unwrap()
        .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn durable_detail_uses_current_generation_frozen_inputs_and_live_submissions() {
    use sluice_model::ids::{AttemptId, RunId};
    let (_home, writer, state, project) = fixture().await;
    let live = RunId::new();
    writer.write(RetrySafety::NonIdempotent, move |tx| {
        let doc = json!({"steps":{"work":{"run":"custom.open","in":{"prompt":{"default":"new plan value"}},"outputs":{"ready":"boolean"}}}});
        tx.sql().execute("UPDATE plans SET doc=?2 WHERE project_id=?1", (project.to_string(),doc.to_string()))?;
        tx.sql().execute("UPDATE steps SET generation=2,status='running',declaration=?2 WHERE project_id=?1 AND step_id='work'", (project.to_string(),doc["steps"]["work"].to_string()))?;
        for (generation,run,finished,prompt) in [(1,RunId::new(),Some("old end"),"old generation"),(2,live,None,"frozen attempted value")] {
            let attempt=AttemptId::new();
            tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,generation,phase,request,inputs_hash,created_at) VALUES(?1,?2,'work',?3,'terminal',?4,'fixture','now')", (attempt.to_string(),project.to_string(),generation,json!({"inputs":{"prompt":prompt}}).to_string()))?;
            tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,created_at,finished_at) VALUES(?1,?2,?3,'work',?4,'now',?5)", (run.to_string(),project.to_string(),attempt.to_string(),generation,finished))?;
            tx.sql().execute("INSERT INTO submissions(run_id,project_id,step_id,outputs,at) VALUES(?1,?2,'work',?3,'now')", (run.to_string(),project.to_string(),json!({"ready":generation==2}).to_string()))?;
        }
        tx.sql().execute("INSERT INTO messages(id,project_id,thread,\"from\",\"to\",body,needs_reply,at) VALUES(91001,?1,'step-work','owner','orchestrator','Only the thread links this to work',1,'2026-10-04T00:00:00Z')", [project.to_string()])?;
        tx.changed(Some(project), "project");
        Ok(())
    }).await.unwrap();
    let work = "work".parse().unwrap();
    let registry = Registry(Arc::new(Exact));
    let (_, board) = views::board::snapshot(&state, project, Some(&registry))
        .await
        .unwrap();
    // The board's cards carry no run detail; the step's page loads it.
    assert!(board.units[0].steps[0].runs.is_empty());
    let (_, board, step) = views::board::step_snapshot(&state, project, Some(&registry), &work)
        .await
        .unwrap();
    let unit = &board.units[0];
    assert_eq!(unit.steps[0].runs.len(), 1);
    let step = &step;
    assert_eq!(step.runs.len(), 1);
    assert_eq!(step.runs[0].id, live);
    assert_eq!(step.inputs[0].value, "frozen attempted value");
    assert_eq!(step.outputs[0].value, "true");
    assert_eq!(step.outputs[0].source, "Submitted so far");
    assert_eq!((step.messages, step.awaiting), (1, 1));
    assert_eq!(unit.last_message, "Only the thread links this to work");
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE runs SET finished_at='now' WHERE run_id=?1",
                [live.to_string()],
            )?;
            tx.changed(Some(project), "project");
            Ok(())
        })
        .await
        .unwrap();
    let (_, _, step) = views::board::step_snapshot(&state, project, Some(&registry), &work)
        .await
        .unwrap();
    assert!(!step.outputs[0].available);
}

#[tokio::test]
async fn a_running_step_that_has_submitted_reads_finishing_on_its_card_and_drawer() {
    use sluice_model::ids::{AttemptId, RunId};
    let (_home, writer, state, project) = fixture().await;
    let run = RunId::new();
    writer.write(RetrySafety::NonIdempotent, move |tx| {
        let attempt = AttemptId::new();
        tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,phase,request,inputs_hash,created_at) VALUES(?1,?2,'work','executing','{}','fixture','now')", (attempt.to_string(),project.to_string()))?;
        tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,release_id,created_at,started_at) VALUES(?1,?2,?3,'work','0123456789abcdef0123456789abcdef01234567-89abcdef','2026-10-05T09:00:00Z','2026-10-05T09:00:00Z')", (run.to_string(),project.to_string(),attempt.to_string()))?;
        tx.sql().execute("UPDATE steps SET status='running',run_ids=?2 WHERE project_id=?1 AND step_id='work'", (project.to_string(),json!([run]).to_string()))?;
        tx.changed(Some(project), "project");
        Ok(())
    }).await.unwrap();
    let work = "work".parse().unwrap();
    let registry = Registry(Arc::new(Exact));
    let (_, board) = views::board::snapshot(&state, project, Some(&registry))
        .await
        .unwrap();
    assert_eq!(board.units[0].steps[0].caption(), "");
    writer.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute("INSERT INTO submissions(run_id,project_id,step_id,outputs,at) VALUES(?1,?2,'work','{\"ready\":true}','2026-10-05T09:30:00Z')", (run.to_string(),project.to_string()))?;
        tx.changed(Some(project), "project");
        Ok(())
    }).await.unwrap();
    let (_, board) = views::board::snapshot(&state, project, Some(&registry))
        .await
        .unwrap();
    assert_eq!(board.units[0].steps[0].caption(), "finishing");
    let (_, _, step) = views::board::step_snapshot(&state, project, Some(&registry), &work)
        .await
        .unwrap();
    let html = step.body().unwrap();
    let html = html.as_str();
    assert!(
        html.contains("<span class=\"tag\">finishing</span>"),
        "{html}"
    );
    assert!(html.contains("<h3>Finishing</h3>"), "{html}");
    assert!(html.contains("datetime=\"2026-10-05T09:30:00Z\""), "{html}");
    assert!(
        html.contains("release <code style=\"white-space:nowrap\">0123456789ab</code>"),
        "{html}"
    );
}
