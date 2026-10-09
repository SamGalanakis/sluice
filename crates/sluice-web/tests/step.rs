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
    let html = view.body("").unwrap();
    assert!(!html.as_str().contains("<script>"));
    assert!(html.as_str().contains("&#60;script&#62;"));
    assert!(view.retryable());
}
/// A failed step's drawer: Retry is the primary action; outputs not set yet are named on one
/// line, the set ones drawn as fields with each doc under its name.
#[test]
fn a_failed_steps_drawer_leads_with_retry_and_names_its_unset_outputs_on_one_line() {
    let plan = Plan::parse_json(br#"{"steps":{"w":{"run":"core.external","outputs":{"summary":{"type":"string","doc":"What changed"},"ready":"boolean","evidence":"string"}}}}"#, &Signatures).unwrap();
    let mut state = StateSnapshot::default();
    state.steps.insert(
        "w".parse().unwrap(),
        StepState {
            status: StepStatus::Failed,
            error: Some("engine stopped".into()),
            ..Default::default()
        },
    );
    let view = StepView::new(ProjectId::new(), &plan, &state, &"w".parse().unwrap());
    let html = view.body("").unwrap();
    let html = html.as_str();
    assert!(
        html.contains("<button name=\"action\" value=\"retry\" class=\"primary\">Retry</button>"),
        "{html}"
    );
    assert!(
        html.contains("<span class=\"quiet\">3 outputs not set yet:</span>"),
        "{html}"
    );
    assert!(!html.contains("Not set yet."), "{html}");
    assert!(!html.contains("<span class=\"f-name\">summary"), "{html}");
    assert!(
        html.contains("<span class=\"f-uname\" title=\"What changed\">summary"),
        "{html}"
    );
    // with one set, it is a field (its doc under its name) and the others one line
    let w = state
        .steps
        .get_mut(&"w".parse::<sluice_model::ids::StepId>().unwrap())
        .unwrap();
    w.outputs
        .0
        .insert("summary".into(), json!("Fixed it").try_into().unwrap());
    w.status = StepStatus::Succeeded;
    let view = StepView::new(ProjectId::new(), &plan, &state, &"w".parse().unwrap());
    let html = view.body("").unwrap();
    let html = html.as_str();
    assert!(html.contains("<span class=\"f-name\">summary</span> <span class=\"f-type\">&#34;string&#34;</span><span class=\"f-about\"><span class=\"f-doc\">What changed</span></span></dt><dd class=\"f-v\"><span class=\"v\">Fixed it</span></dd>"), "{html}");
    assert!(html.contains("2 outputs not set yet:"), "{html}");
    // a succeeded step's Retry asks first, naming what it does; never primary
    assert!(
        html.contains("<details class=\"confirm-flow\"><summary>Retry</summary>")
            && html.contains("Retrying runs it again; its outputs stay until the new run ends.")
            && !html.contains("value=\"retry\" class=\"primary\""),
        "{html}"
    );
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
    let (_home, writer, state, project) = fixture().await;
    let fake = Arc::new(Fake::default());
    let app = views::dashboard_router(state.clone())
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
        ("action=cancel&revision=1", StatusCode::CONFLICT),
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
    {
        let calls = fake.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].project, project);
        assert_eq!(calls[0].action, Action::Retry);
        assert_eq!(calls[0].author, "owner");
        assert_eq!(calls[0].message, "Try again");
    }
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET status='running' WHERE project_id=?1",
                [project.to_string()],
            )?;
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(Request::builder().uri(&path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let html = String::from_utf8(
        to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("Cancel work?"));
    assert!(html.contains("Keep running"));
    assert!(!html.contains("<p class=\"d-doc\"></p>"));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("{path}/actions"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    "action=cancel&revision=1&message=Stop+to+replan",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let calls = fake.0.lock().unwrap();
    assert_eq!(calls[1].action, Action::Cancel);
    assert_eq!(calls[1].message, "Stop to replan");
}
/// Dismiss and Undismiss edit no plan, so they need no revision; Dismiss applies only to a
/// cancel, and `next` brings the index's Dismiss back to the index.
#[tokio::test]
async fn dismiss_sets_a_cancel_aside_with_no_revision_and_goes_back_where_it_was_asked() {
    let (_home, writer, state, project) = fixture().await;
    let fake = Arc::new(Fake::default());
    let app = views::dashboard_router(state.clone())
        .layer(Extension(Registry(Arc::new(Exact))))
        .layer(Extension(Commands(fake.clone())));
    let path = format!("/projects/id/{project}/steps/work/actions");
    let post = |body: &'static str| {
        let app = app.clone();
        let path = path.clone();
        async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };
    // failed, but no cancel: nothing to dismiss
    assert_eq!(post("action=dismiss").await.status(), StatusCode::CONFLICT);
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let cancel = PublicError::Cancelled {
                message: "pivot".into(),
            };
            tx.sql().execute(
                "UPDATE steps SET error=?2 WHERE project_id=?1",
                (project.to_string(), serde_json::to_string(&cancel).unwrap()),
            )?;
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
    let response = post("action=dismiss&next=/").await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()["location"], "/");
    // not dismissed yet (the fake records, the store is unchanged): nothing to undo
    assert_eq!(
        post("action=undismiss").await.status(),
        StatusCode::CONFLICT
    );
    // a `next` off the site is not followed
    let response = post("action=dismiss&next=//elsewhere.example").await;
    assert_eq!(
        response.headers()["location"],
        format!("/projects/id/{project}/steps/work").as_str()
    );
    let calls = fake.0.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].action, Action::Dismiss);
    assert_eq!(calls[0].step.as_ref().map(|s| s.as_str()), Some("work"));
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
    // the step's own read draws it alone, its runs with it
    let step = views::board::step_detail(&state, project, Some(&registry), &work, false)
        .await
        .unwrap()
        .1
        .unwrap()
        .step;
    let unit = &board.units[0];
    let step = &step;
    assert_eq!(step.runs.len(), 1);
    assert_eq!(step.runs[0].id, live);
    let mut ended = step.clone();
    ended.runs[0].finished = "2026-10-05T12:14:00Z".into();
    ended.runs[0].seconds = Some(8040.0);
    let ended_html = ended.body("").unwrap();
    assert!(ended_html.as_str().contains(" · took 2h 14m"));
    assert!(!ended_html.as_str().contains("took took"));
    let mut timed = step.clone();
    timed.runs[0].seconds = Some(19200.0);
    assert_eq!(
        timed.cancel_prompt(),
        "It has been running for 5h 20m. Cancelling stops that run; Retry starts it over."
    );
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
    let step = views::board::step_detail(&state, project, Some(&registry), &work, false)
        .await
        .unwrap()
        .1
        .unwrap()
        .step;
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
    // its run writes as it goes: not quiet
    let dir = _home.path().join("runs").join(run.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("stderr.txt"), "working").unwrap();
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
    let step = views::board::step_detail(&state, project, Some(&registry), &work, false)
        .await
        .unwrap()
        .1
        .unwrap()
        .step;
    let html = step.body("").unwrap();
    let html = html.as_str();
    assert!(html.contains("finishing · running for <time"), "{html}");
    assert!(html.contains("<h4>Finishing</h4>"), "{html}");
    assert!(html.contains("datetime=\"2026-10-05T09:30:00Z\""), "{html}");
    assert!(
        html.contains("release <sluice-copy value=\"0123456789ab\"><code>0123456789ab</code><button type=\"button\" class=\"copy needs-js\" aria-label=\"Copy release\""),
        "{html}"
    );
}

#[test]
fn a_cards_timer_reads_in_its_two_largest_units() {
    use sluice_web::views::ui::{
        duration_text as short_duration, duration_words as spoken_duration,
    };
    for (seconds, shown, said) in [
        (0.4, "<1s", "under a second"),
        (45.9, "45s", "45 seconds"),
        (60.0, "1m", "1 minute"),
        (12.0 * 60.0 + 59.0, "12m", "12 minutes"),
        (3_600.0, "1h 0m", "1 hour"),
        (
            2.0 * 3_600.0 + 14.0 * 60.0 + 30.0,
            "2h 14m",
            "2 hours 14 minutes",
        ),
        (86_400.0 + 3.0 * 3_600.0 + 59.0, "1d 3h", "1 day 3 hours"),
        (2.0 * 86_400.0, "2d 0h", "2 days"),
    ] {
        assert_eq!(short_duration(seconds), shown, "{seconds}");
        assert_eq!(spoken_duration(seconds), said, "{seconds}");
    }
}

/// The step's card on the project page, as the page draws it.
async fn card(state: &views::DashboardState, project: ProjectId) -> String {
    let app = views::dashboard_router(state.clone()).layer(Extension(Registry(Arc::new(Exact))));
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/projects/id/{project}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 1 << 22).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    let start = body.find("<a id=\"n-work\"").expect("the card");
    let end = start + body[start..].find("</a>").unwrap();
    body[start..end].to_owned()
}

#[tokio::test]
async fn a_cards_timer_ticks_while_its_current_run_goes_and_holds_once_it_has_ended() {
    use sluice_model::ids::{AttemptId, RunId};
    let (_home, writer, state, project) = fixture().await;
    let set = |sql: &'static str| {
        let writer = writer.clone();
        async move {
            writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    tx.sql().execute(sql, [project.to_string()])?;
                    tx.changed(Some(project), "project");
                    Ok(())
                })
                .await
                .unwrap()
        }
    };
    let root = _home.path().to_owned();
    let run = |created: &'static str, finished: Option<&'static str>| {
        let writer = writer.clone();
        let root = root.clone();
        async move {
            let id = RunId::new();
            writer.write(RetrySafety::NonIdempotent, move |tx| {
                let attempt = AttemptId::new();
                // one live attempt a step: the one before has ended
                tx.sql().execute("UPDATE attempts SET phase='terminal' WHERE project_id=?1", [project.to_string()])?;
                tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,phase,request,inputs_hash,created_at) VALUES(?1,?2,'work','executing','{}','fixture',?3)", (attempt.to_string(),project.to_string(),created))?;
                tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,created_at,started_at,finished_at) VALUES(?1,?2,?3,'work',?4,?4,?5)", (id.to_string(),project.to_string(),attempt.to_string(),created,finished))?;
                tx.changed(Some(project), "project");
                Ok(())
            }).await.unwrap();
            // the run writes as it goes: it is not quiet
            let dir = root.join("runs").join(id.to_string());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("stderr.txt"), "working").unwrap();
        }
    };
    // failed without a run, then pending: no timer
    assert!(!card(&state, project).await.contains("took"));
    set("UPDATE steps SET status='pending' WHERE project_id=?1").await;
    assert!(!card(&state, project).await.contains("took"));

    // running: a <time data-since> at its run's start, which sluice.js ticks
    run("2026-10-05T09:00:00Z", None).await;
    set("UPDATE steps SET status='running' WHERE project_id=?1").await;
    let html = card(&state, project).await;
    assert!(
        html.contains("<time data-since=\"2026-10-05T09:00:00Z\" datetime=\"2026-10-05T09:00:00Z\" class=\"took live\" title=\"Started 2026-10-05 09:00 UTC\"><span class=\"tk\" aria-hidden=\"true\">"),
        "{html}"
    );
    assert!(html.contains("<span class=\"vh\"> for "), "{html}");
    // its name reads "running work for 2 days 5 hours" (the glyph, the id, the timer; no comma before it)
    assert!(html.contains("aria-label=\"running\""), "{html}");

    // failed after it ran, then retried and pending again: nothing until the next run starts
    set("UPDATE runs SET finished_at='2026-10-05T09:30:00Z' WHERE project_id=?1").await;
    set("UPDATE steps SET status='failed' WHERE project_id=?1").await;
    let html = card(&state, project).await;
    assert!(
        html.contains("<span class=\"took\" title=\"Took 30m\"><span aria-hidden=\"true\">30m</span><span class=\"vh\"> took 30 minutes</span></span>"),
        "{html}"
    );
    set("UPDATE steps SET status='pending' WHERE project_id=?1").await;
    assert!(
        !card(&state, project).await.contains("took"),
        "a pending step shows no timer"
    );

    // the retry runs: the timer is the current run's, not the first's
    run("2026-10-05T10:00:00Z", None).await;
    set("UPDATE steps SET status='running' WHERE project_id=?1").await;
    let html = card(&state, project).await;
    assert!(
        html.contains("<time data-since=\"2026-10-05T10:00:00Z\" datetime=\"2026-10-05T10:00:00Z\" class=\"took live\" title=\"2 runs; this one started 2026-10-05 10:00 UTC\">"),
        "{html}"
    );

    // it succeeds: how long its last run took, static and quieter
    set("UPDATE runs SET finished_at='2026-10-05T12:14:30Z' WHERE project_id=?1 AND finished_at IS NULL").await;
    set("UPDATE steps SET status='succeeded' WHERE project_id=?1").await;
    let html = card(&state, project).await;
    assert!(
        html.contains("<span class=\"took\" title=\"2 runs; this one took 2h 14m\"><span aria-hidden=\"true\">2h 14m</span><span class=\"vh\"> took 2 hours 14 minutes</span></span>"),
        "{html}"
    );
    assert!(!html.contains("data-since"), "{html}");

    // a value set by hand did not take its run's time
    set("UPDATE steps SET manual=1 WHERE project_id=?1").await;
    assert!(!card(&state, project).await.contains("took"));
}
/// An owner's cancel is a state of its own: the stop glyph and word, a "Cancelled" section with
/// the reason, and Retry a plain button, though the store keeps the step failed.
#[test]
fn a_cancelled_step_reads_as_cancelled_not_failed() {
    let plan = Plan::parse_json(
        br#"{"steps":{"w":{"run":"core.external","outputs":{"done":"boolean"}}}}"#,
        &Signatures,
    )
    .unwrap();
    let mut state = StateSnapshot::default();
    state.steps.insert(
        "w".parse().unwrap(),
        StepState {
            status: StepStatus::Failed,
            error: Some("cancelled: pivot: audit instead (Sam)".into()),
            ..Default::default()
        },
    );
    let view = StepView::new(ProjectId::new(), &plan, &state, &"w".parse().unwrap());
    assert_eq!(view.shown(), sluice_web::views::ui::Shown::Cancelled);
    assert_eq!(view.status, StepStatus::Failed);
    assert!(view.retryable() && !view.retry_first());
    let html = view.body("").unwrap();
    let html = html.as_str();
    assert!(html.contains("g-cancelled"), "{html}");
    assert!(
        html.contains("<h4>Cancelled</h4><p class=\"err-line\">pivot: audit instead (Sam)</p>"),
        "{html}"
    );
    assert!(!html.contains("Why it failed"));
    assert!(!html.contains("class=\"primary\">Retry"));
}
/// A long `after` is a few words ("6 steps: 5 done, 1 running") over the steps it names,
/// folded, sorted and linked; a cancelled or failed step offers no Pause.
#[test]
fn a_long_after_is_a_sentence_over_its_linked_steps() {
    let plan = Plan::parse_json(
        br#"{"steps":{"f":{"run":"core.external","outputs":{"done":"boolean"}},"e":{"run":"core.external","outputs":{"done":"boolean"}},"d":{"run":"core.external","outputs":{"done":"boolean"}},"c":{"run":"core.external","outputs":{"done":"boolean"}},"b":{"run":"core.external","outputs":{"done":"boolean"}},"a":{"run":"core.external","outputs":{"done":"boolean"}},"w":{"run":"core.external","outputs":{"done":"boolean"},"after":["f","e","d","c","b","a"]}}}"#,
        &Signatures,
    )
    .unwrap();
    let mut state = StateSnapshot::default();
    for id in ["a", "b", "c", "d", "e"] {
        state.steps.insert(
            id.parse().unwrap(),
            StepState {
                status: StepStatus::Succeeded,
                ..Default::default()
            },
        );
    }
    state.steps.insert(
        "f".parse().unwrap(),
        StepState {
            status: StepStatus::Running,
            ..Default::default()
        },
    );
    let view = StepView::new(ProjectId::new(), &plan, &state, &"w".parse().unwrap());
    assert_eq!(view.gates_words(), "6 steps: 5 done, 1 running");
    let html = view.body("").unwrap();
    let html = html.as_str();
    assert!(
        html.contains("<summary><span>6 steps: 5 done, 1 running</span>"),
        "{html}"
    );
    let list = &html[html.find("gate-list").unwrap()..];
    // sorted, each a link to its step
    assert!(
        list.find(">a</code>").unwrap() < list.find(">f</code>").unwrap(),
        "{list}"
    );
    assert!(list.contains("/steps/a\"><code>a</code></a>"), "{list}");
    let mut failed = StateSnapshot::default();
    failed.steps.insert(
        "w".parse().unwrap(),
        StepState {
            status: StepStatus::Failed,
            error: Some("cancelled: pivot".into()),
            ..Default::default()
        },
    );
    let view = StepView::new(ProjectId::new(), &plan, &failed, &"w".parse().unwrap());
    assert!(!view.pausable());
    assert!(!view.body("").unwrap().as_str().contains("value=\"pause\""));
}
/// A failure leads with one sentence from its kind; the pane its agent left is folded under
/// "Pane at failure", never shown as escaped JSON.
#[test]
fn a_failure_leads_with_its_sentence_and_folds_the_pane() {
    let plan = Plan::parse_json(
        br#"{"steps":{"w":{"run":"core.external","outputs":{"done":"boolean"}}}}"#,
        &Signatures,
    )
    .unwrap();
    let mut state = StateSnapshot::default();
    state.steps.insert(
        "w".parse().unwrap(),
        StepState {
            status: StepStatus::Failed,
            error: Some("engine operation deadline exceeded".into()),
            ..Default::default()
        },
    );
    let mut view = StepView::new(ProjectId::new(), &plan, &state, &"w".parse().unwrap());
    view.set_failure(sluice_web::views::failure::Failure::parse(
        r#"{"error":"agent_failure","kind":"WallCap","message":"engine operation deadline exceeded\npane at failure (last rows; whole screen: /h/runs/r/invocations/i/pane-at-failure.txt):\n  > still thinking","session":"s"}"#,
        Some(36_000.0),
    ));
    assert_eq!(view.shown(), sluice_web::views::ui::Shown::Failed);
    assert!(view.retry_first());
    let html = view.body("").unwrap();
    let html = html.as_str();
    assert!(html.contains("<h4>Why it failed</h4><p class=\"err-line\">Stopped at its wall-clock cap after 10h 0m.</p>"), "{html}");
    assert!(
        html.contains("<p class=\"err-said\">engine operation deadline exceeded</p>"),
        "{html}"
    );
    assert!(
        html.contains(
            "Pane at failure</summary><pre class=\"pane-rows\">&#62; still thinking</pre>"
        ),
        "{html}"
    );
    assert!(!html.contains("\\n"));
}
/// The index and the project's counts tell a cancel from a failure, counted apart, and a
/// project whose only stop is a cancel is not marked failed. Each stopped step is a row: its
/// glyph, its name and its failure's one sentence, failures before cancels, four rows at most
/// and "and n more" after them.
#[tokio::test]
async fn the_index_lists_each_stopped_step_as_a_row_with_its_failure() {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let id = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "stops".parse().unwrap(),
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
            for (position, (step, error)) in [
                ("broke", json!({"error":"fn_failure","message":"exit code 1"})),
                ("dropped", json!({"error":"cancelled","message":"cancel requested"})),
                ("b2", json!({"error":"agent_failure","kind":"WallCap","message":"cap"})),
                ("b3", json!({"error":"fn_failure","message":"exit code 2"})),
                ("b4", json!({"error":"fn_failure","message":"exit code 3"})),
                ("pivoted", json!({"error":"agent_failure","kind":"Cancelled","message":"cancelled during transient backoff"})),
            ]
            .into_iter()
            .enumerate()
            {
                tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,status,error) VALUES (?1,?2,?3,'{\"run\":\"core.external\"}','failed',?4)", (id.to_string(), step, position as i64, error.to_string()))?;
            }
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
    let reads = ReadPool::open(home.path(), 1).unwrap();
    let snapshot = reads
        .snapshot(|c| views::load_snapshot(c, views::FunctionCatalog::default()))
        .await
        .unwrap();
    let project = &snapshot.projects[0];
    assert_eq!(
        (
            project.counts.get(sluice_web::views::ui::Shown::Failed),
            project.counts.get(sluice_web::views::ui::Shown::Cancelled)
        ),
        (4, 2)
    );
    let stopped: Vec<(&str, bool)> = project
        .stopped
        .iter()
        .map(|s| (s.step.as_str(), s.cancelled))
        .collect();
    assert_eq!(
        stopped,
        [
            ("broke", false),
            ("b2", false),
            ("b3", false),
            ("b4", false),
            ("dropped", true),
            ("pivoted", true)
        ]
    );
    assert_eq!(project.stopped[0].headline, "Its fn failed: exit code 1.");
    assert_eq!(project.shown(), sluice_web::views::ui::Shown::Failed);
    let html = views::home::HomeView::new(&snapshot).body().unwrap();
    let rows = html
        .as_str()
        .split("<ul class=\"stopped-rows\"")
        .nth(1)
        .unwrap();
    let rows = &rows[..rows.find("</ul>").unwrap()];
    assert_eq!(rows.matches("<li").count(), 5, "{rows}");
    // with no record kept, the reason still links: its step's records on the log
    assert!(
        rows.contains("log?step=")
            && rows
                .contains("title=\"Its fn failed: exit code 1.\">Its fn failed: exit code 1.</a>"),
        "{rows}"
    );
    assert!(rows.contains("Stopped at its wall-clock cap"), "{rows}");
    assert!(rows.contains("and 2 more</a>"), "{rows}");
    assert!(
        !rows.contains("pivoted"),
        "past four rows a stop is counted: {rows}"
    );
    for step in ["b2", "b3", "b4", "dropped"] {
        writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute(
                    "DELETE FROM steps WHERE project_id=?1 AND step_id=?2",
                    (id.to_string(), step),
                )?;
                tx.changed(Some(id), "status");
                Ok(())
            })
            .await
            .unwrap();
    }
    let snapshot = reads
        .snapshot(|c| views::load_snapshot(c, views::FunctionCatalog::default()))
        .await
        .unwrap();
    let html = views::home::HomeView::new(&snapshot).body().unwrap();
    assert!(
        html.as_str()
            .contains("<li class=\"sr-cancelled\"><span class=\"g g-cancelled\""),
        "a cancel's row: {}",
        html.as_str()
    );
    assert!(!html.as_str().contains("more</a>"));
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "DELETE FROM steps WHERE project_id=?1 AND step_id='broke'",
                [id.to_string()],
            )?;
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
    let snapshot = reads
        .snapshot(|c| views::load_snapshot(c, views::FunctionCatalog::default()))
        .await
        .unwrap();
    assert_eq!(
        snapshot.projects[0].shown(),
        sluice_web::views::ui::Shown::Cancelled
    );
    writer.shutdown().await.unwrap();
}

/// A running step's drawer and page lead with what it is doing now: its thread's latest
/// message (who, when, its first words), then its live progress; the page's sections are h2s
/// under its h1, the drawer's h3s under its h2, so no level is skipped.
#[tokio::test]
async fn a_running_step_says_what_it_is_doing_now() {
    use sluice_model::ids::{AttemptId, RunId};
    let (_home, writer, state, project) = fixture().await;
    let run = RunId::new();
    let long = format!(
        "**Landed** the seams on main. {}",
        "Then the rest of the lane. ".repeat(30)
    );
    writer.write(RetrySafety::NonIdempotent, move |tx| {
        let attempt = AttemptId::new();
        tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,phase,request,inputs_hash,created_at) VALUES(?1,?2,'work','executing','{}','fixture','now')", (attempt.to_string(),project.to_string()))?;
        tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,created_at,started_at) VALUES(?1,?2,?3,'work','2026-10-05T09:00:00Z','2026-10-05T09:00:00Z')", (run.to_string(),project.to_string(),attempt.to_string()))?;
        tx.sql().execute("UPDATE steps SET status='running',run_ids=?2 WHERE project_id=?1 AND step_id='work'", (project.to_string(),json!([run]).to_string()))?;
        tx.sql().execute("INSERT INTO messages(id,project_id,thread,\"from\",\"to\",body,at) VALUES (900001,?1,'step-work','orchestrator','work','Start with the seams.','2026-10-05T09:01:00Z')", [project.to_string()])?;
        tx.sql().execute("INSERT INTO messages(id,project_id,thread,\"from\",\"to\",body,at) VALUES (900002,?1,'step-work','work','orchestrator',?2,'2026-10-05T09:40:00Z')", (project.to_string(), long))?;
        tx.sql().execute("INSERT INTO messages(id,project_id,thread,\"from\",\"to\",body,at) VALUES (900003,?1,'step-work','orchestrator','work','Rebase first: `lash-core-store__unit_test` and intent_hash_golden_vector are red.','2026-10-05T09:50:00Z')", [project.to_string()])?;
        tx.changed(Some(project), "project");
        Ok(())
    }).await.unwrap();
    let work = "work".parse().unwrap();
    let registry = Registry(Arc::new(Exact));
    let step = views::board::step_detail(&state, project, Some(&registry), &work, false)
        .await
        .unwrap()
        .1
        .unwrap()
        .step;
    let drawer = step.body("").unwrap();
    let drawer = drawer.as_str();
    let now = &drawer[drawer
        .find("<section class=\"d-sec d-now\">")
        .expect("a Now section")..];
    let now = &now[..now.find("</section>").unwrap()];
    // Now is the Overview tab's first section, a level under the drawer's panel heads
    assert!(
        now.starts_with("<section class=\"d-sec d-now\"><h4>Now</h4>"),
        "{now}"
    );
    // its latest exchange as messages: its own latest (to the orchestrator), its first words
    let this = "<span class=\"who who-this\">This step</span>";
    let orchestrator = "<span class=\"who\">Orchestrator</span>";
    assert!(
        now.contains(&format!("<p class=\"mg-head\">{this}")),
        "{now}"
    );
    assert!(now.contains("datetime=\"2026-10-05T09:40:00Z\""), "{now}");
    assert!(
        now.contains("<p class=\"m-text\">Landed the seams on main."),
        "{now}"
    );
    assert!(
        now.contains("…</p>") && now.contains("Read it in the thread"),
        "{now}"
    );
    assert!(
        !now.contains("Start with the seams"),
        "the latest only: {now}"
    );
    // its own voice leads; what was said to it since comes under it, named by who sent it,
    // identifiers whole
    let own = now.find(&format!("<p class=\"mg-head\">{this}")).unwrap();
    let to_it = &now[now
        .find(&format!("<p class=\"mg-head\">{orchestrator}"))
        .expect("the message to it")..];
    assert!(own < now.len() - to_it.len(), "{now}");
    assert!(to_it.contains(this), "{to_it}");
    assert!(
        now.contains("class=\"mg mg-out\"") && now.contains("class=\"mg mg-in\""),
        "{now}"
    );
    assert!(
        to_it.contains(
            "<code>lash-core-store__unit_test</code> and intent_hash_golden_vector are red."
        ),
        "{to_it}"
    );
    // it last said something days ago (its own message) and its run has written nothing since:
    // quiet, and Now says for how long
    assert!(
        now.contains("<span>Nothing written for <time data-since=\"2026-10-05T09:40:00Z\""),
        "{now}"
    );
    // the Now section comes before everything else under the head
    assert!(
        drawer.find("d-now").unwrap()
            < drawer
                .find("<h3 class=\"tp-h\">Inputs</h3>")
                .unwrap_or(usize::MAX)
    );
    let page = step.page_body("p", None, "", None).unwrap();
    let page = page.as_str();
    assert!(page.contains("<h1 id=\"d-title\">work</h1>"), "{page}");
    assert!(
        page.contains("<h2 class=\"tp-h\">Overview</h2>")
            && page.contains("<h3>Now</h3>")
            && !page.contains("<h4"),
        "{page}"
    );
}

/// A fn's failure reads as its traceback's last exception, the traceback folded; a hint how to
/// resume sits with the actions, its tool call as code.
#[test]
fn a_fn_failure_reads_as_its_exception_with_the_resume_hint_by_retry() {
    let plan = Plan::parse_json(
        br#"{"steps":{"w":{"run":"core.external","outputs":{"done":"boolean"}}}}"#,
        &Signatures,
    )
    .unwrap();
    let mut state = StateSnapshot::default();
    state.steps.insert(
        "w".parse().unwrap(),
        StepState {
            status: StepStatus::Failed,
            error: Some("exit code 1".into()),
            ..Default::default()
        },
    );
    let mut view = StepView::new(ProjectId::new(), &plan, &state, &"w".parse().unwrap());
    view.set_failure(sluice_web::views::failure::Failure::parse(
        r#"{"error":"fn_failure","message":"exit code 1\nremains active.\ncodex: still waiting.\nTraceback (most recent call last):\n  File \"x.py\", line 1, in main\nRuntimeError: codex ran past the wall-clock cap of 600 min (SLUICE_AGENT_MAX_MIN)\nsession: s-1. To resume it, bind the step's session input to it and retry: step_set_input(project, step, \"session\", \"s-1\"), then step_retry."}"#,
        None,
    ));
    let html = view.body("").unwrap();
    let html = html.as_str();
    assert!(
        html.contains("<p class=\"err-line\">Stopped at its wall-clock cap after 10h 0m.</p>"),
        "{html}"
    );
    assert!(
        html.contains("<p class=\"err-text\">…remains active.\ncodex: still waiting.</p>"),
        "{html}"
    );
    assert!(
        html.contains(
            "Traceback</summary><pre class=\"pane-rows\">Traceback (most recent call last):"
        ),
        "{html}"
    );
    assert!(
        html.contains("<p class=\"d-resume meta\">To resume it, bind the step&#39;s session input to it and retry: <code>step_set_input(project, step, &#34;session&#34;, &#34;s-1&#34;)</code>, then step_retry.</p>"),
        "{html}"
    );
    assert!(!html.contains("Its fn failed"), "{html}");
}
/// A step action the page asked for and sluice did not take comes back to the step's own page
/// with a notice saying why and the feedback typed with it kept in its box (a script gets the
/// error as JSON); one posted after a plan edit that left the step as drawn simply applies, at
/// the plan's new revision.
#[tokio::test]
async fn a_refused_action_comes_back_to_its_step_saying_why_with_its_feedback_kept() {
    let (_home, writer, state, project) = fixture().await;
    let fake = Arc::new(Fake::default());
    let app = views::dashboard_router(state.clone())
        .layer(Extension(Registry(Arc::new(Exact))))
        .layer(Extension(Commands(fake.clone())));
    let path = format!("/projects/id/{project}/steps/work");
    let body = |response: axum::response::Response| async move {
        String::from_utf8(
            to_bytes(response.into_body(), 1 << 22)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap()
    };
    let page = body(
        app.clone()
            .oneshot(Request::builder().uri(&path).body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;
    let seen = page
        .split("name=\"seen\" value=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_owned();
    assert_eq!(seen, "failed:false:0", "{page}");
    let post = |form: String, accept: &'static str| {
        let app = app.clone();
        let path = format!("{path}/actions");
        async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("accept", accept)
                    .body(Body::from(form))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };
    // the plan moved on (revision 0 is behind) but the step is as drawn: it applies, at the
    // plan's revision now
    let response = post(
        format!("action=retry&revision=0&seen={seen}&message=Look+again"),
        "text/html",
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    {
        let calls = fake.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].revision, 1);
        assert_eq!(calls[0].message, "Look again");
    }
    // the step changed since: nothing is done, and its page says so over it, the feedback in
    // its box and the box open
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET status='stale' WHERE project_id=?1",
                [project.to_string()],
            )?;
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
    let response = post(
        format!("action=retry&revision=0&seen={seen}&message=Look+again"),
        "text/html,application/xhtml+xml",
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let html = body(response).await;
    assert!(html.starts_with("<!doctype html>"), "{html}");
    assert!(
        html.contains("<div class=\"notice\" role=\"alert\">")
            && html.contains("<p>Nothing was done: this step changed since the page drew it, and it is stale now. Look again, then retry. What you wrote is kept in its box: Retry sends it.</p>"),
        "{html}"
    );
    assert!(
        html.contains("<details data-preserve-attr=\"open\" open><summary>Feedback for retry</summary>")
            && html.contains("data-ignore-morph placeholder=\"Optional feedback for the next attempt\">Look again</textarea>"),
        "{html}"
    );
    assert_eq!(fake.0.lock().unwrap().len(), 1, "nothing more was done");
    // an action that no longer applies says so; a script gets JSON with the matching code
    let response = post("action=cancel&revision=1".into(), "application/json").await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let json: serde_json::Value = serde_json::from_str(&body(response).await).unwrap();
    assert_eq!(json["error"], "conflict", "{json}");
    assert_eq!(
        json["message"],
        "Nothing was done: Cancel does not apply to a stale step."
    );
}
