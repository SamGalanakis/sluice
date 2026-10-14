use crate::plan_html;
use crate::seed;
use axum::{
    Extension,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::json;
use sluice_model::{error::PublicError, ids::ProjectId, plan::FnSignature};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use sluice_web::views::{
    self,
    board::{Registry, RegistrySnapshot, RegistrySource},
    step::{Action, CommandService, Commands, OwnerCommand},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tower::ServiceExt;

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
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            seed::put(
                tx,
                project,
                json!({"steps":{"work":{"run":"custom.open","outputs":{"ready":"boolean"}}}}),
            )?;
            tx.sql().execute(
                "UPDATE steps SET status='failed' WHERE project_id=?1 AND step_id='work'",
                [project.to_string()],
            )?;
            tx.changed(Some(project), "project");
            Ok(())
        })
        .await
        .unwrap();
    let state =
        views::DashboardState::new(ReadPool::open(home.path(), 2).unwrap(), Arc::new(Catalog));
    (home, writer, state, project)
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
async fn durable_detail_uses_current_generation_frozen_inputs_and_live_submissions() {
    use sluice_model::ids::{AttemptId, RunId};
    let (_home, writer, state, project) = fixture().await;
    let live = RunId::new();
    writer.write(RetrySafety::NonIdempotent, move |tx| {
        seed::put(tx, project, json!({"steps":{"work":{"run":"custom.open","in":{"prompt":{"default":"new plan value"}},"outputs":{"ready":"boolean"}}}}))?;
        tx.sql().execute("UPDATE steps SET generation=2,status='running' WHERE project_id=?1 AND step_id='work'", [project.to_string()])?;
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
    assert_eq!(timed.cancel_prompt(), "Running for 5h 20m.");
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
    assert!(
        html.contains("<b>Finishing</b> <time data-since="),
        "{html}"
    );
    assert!(
        html.contains("<h4 id=\"ov-fin-h\">Finishing</h4>"),
        "{html}"
    );
    assert!(html.contains("datetime=\"2026-10-05T09:30:00Z\""), "{html}");
    assert!(
        html.contains("release <sluice-copy value=\"0123456789ab\"><code>0123456789ab</code><button type=\"button\" class=\"copy needs-js\" aria-label=\"Copy release\""),
        "{html}"
    );
}

/// The step's place on the project page, as the page draws it: its item in Needs you, its row
/// at work, its card of what finished; "" when it is only counted.
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
    match plan_html::place(&body, "work") {
        "stopped" => plan_html::stopped(&body, "work").to_owned(),
        "done" => {
            let cards = plan_html::band(&body, "plan-finished");
            let at = cards.find("/steps/work\">").expect("its card");
            let start = cards[..at].rfind("<li class=\"fin-i\"").unwrap();
            cards[start..at + cards[at..].find("</li>").unwrap()].to_owned()
        }
        "" => String::new(),
        _ => plan_html::row(&body, "work").to_owned(),
    }
}

#[tokio::test]
async fn a_steps_timer_on_the_plan_ticks_while_its_current_run_goes_and_holds_once_it_has_ended() {
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
    let html = card(&state, project).await;
    assert!(
        !html.contains("data-since") && !html.contains(" after "),
        "{html}"
    );
    set("UPDATE steps SET status='pending' WHERE project_id=?1").await;
    let html = card(&state, project).await;
    assert!(!html.contains("data-since"), "{html}");

    // running for days, alone: a long run, its time a <time data-since> at its run's start,
    // which the page ticks
    run("2026-10-05T09:00:00Z", None).await;
    set("UPDATE steps SET status='running' WHERE project_id=?1").await;
    let html = card(&state, project).await;
    let pill = &html[html.find("<span class=\"pill\">").expect("its pill")..];
    assert!(
        pill.contains(
            "<time data-since=\"2026-10-05T09:00:00Z\" datetime=\"2026-10-05T09:00:00Z\""
        ),
        "{html}"
    );

    // failed after it ran: how long that run took, still; then retried and pending again:
    // nothing until the next run starts
    set("UPDATE runs SET finished_at='2026-10-05T09:30:00Z' WHERE project_id=?1").await;
    set("UPDATE steps SET status='failed' WHERE project_id=?1").await;
    let html = card(&state, project).await;
    assert!(html.contains(" failed after 30m."), "{html}");
    assert!(!html.contains("data-since"), "{html}");
    set("UPDATE steps SET status='pending' WHERE project_id=?1").await;
    let html = card(&state, project).await;
    assert!(
        !html.contains("data-since"),
        "a pending step shows no timer: {html}"
    );

    // the retry runs: the timer is the current run's, not the first's, and its steps say
    // which run
    run("2026-10-05T10:00:00Z", None).await;
    set("UPDATE steps SET status='running' WHERE project_id=?1").await;
    let html = card(&state, project).await;
    let pill = &html[html.find("<span class=\"pill\">").expect("its pill")..];
    let pill = &pill[..pill.find("</span><span class=\"ro").unwrap()];
    assert!(
        pill.contains("<time data-since=\"2026-10-05T10:00:00Z\""),
        "{pill}"
    );
    assert!(html.contains(">run 2<"), "{html}");

    // it succeeds: done, how long its last run took, static
    set("UPDATE runs SET finished_at='2026-10-05T12:14:30Z' WHERE project_id=?1 AND finished_at IS NULL").await;
    set("UPDATE steps SET status='succeeded' WHERE project_id=?1").await;
    let html = card(&state, project).await;
    assert!(html.contains(" · took 2h 14m</p>"), "{html}");
    assert!(!html.contains("data-since"), "{html}");

    // a value set by hand did not take its run's time
    set("UPDATE steps SET manual=1 WHERE project_id=?1").await;
    assert!(!card(&state, project).await.contains("took"));
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
            let failed = [
                ("broke", json!({"error":"fn_failure","message":"exit code 1"})),
                ("dropped", json!({"error":"cancelled","message":"cancel requested"})),
                ("b2", json!({"error":"agent_failure","kind":"WallCap","message":"cap"})),
                ("b3", json!({"error":"fn_failure","message":"exit code 2"})),
                ("b4", json!({"error":"fn_failure","message":"exit code 3"})),
                ("pivoted", json!({"error":"agent_failure","kind":"Cancelled","message":"cancelled during transient backoff"})),
            ];
            let steps: serde_json::Map<String, serde_json::Value> = failed
                .iter()
                .map(|(step, _)| (step.to_string(), json!({"run": "core.external"})))
                .collect();
            seed::put(tx, id, json!({ "steps": steps }))?;
            for (step, error) in failed {
                tx.sql().execute("UPDATE steps SET status='failed',error=?3 WHERE project_id=?1 AND step_id=?2", (id.to_string(), step, error.to_string()))?;
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
    let rows = html.as_str().split("<ul class=\"pm-rows\"").nth(1).unwrap();
    let rows = &rows[..rows.find("</ul>").unwrap()];
    assert_eq!(
        rows.matches("<li class=\"pm-row pm-stopped").count(),
        4,
        "{rows}"
    );
    // with no record kept, the reason still links: its step's records on the log
    assert!(
        rows.contains("log?step=")
            && rows
                .contains("title=\"Its fn failed: exit code 1.\">Its fn failed: exit code 1.</a>"),
        "{rows}"
    );
    assert!(rows.contains("Stopped at its wall-clock cap"), "{rows}");
    assert!(rows.contains("and 2 more stopped</a>"), "{rows}");
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
            .contains("<li class=\"pm-row pm-stopped sr-cancelled\"><span class=\"g g-cancelled\""),
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
        .find("<article class=\"mod d-sec d-now\"")
        .expect("a Now module")..];
    let now = &now[..now.find("</article>").unwrap()];
    // Now is the Overview tab's first module, its head a level under the drawer's panel heads
    assert!(
        now.contains("aria-labelledby=\"ov-now-h\"><h4 id=\"ov-now-h\">Now</h4>"),
        "{now}"
    );
    // its latest exchange as message cards: its own latest (to the orchestrator), its first
    // words cut with the whole a link away; then what was said to it since, identifiers whole
    let this = "<span class=\"who who-this\">This step</span>";
    let orchestrator = "<span class=\"who who-old\">orchestrator</span>";
    let own = now
        .find(&format!(
            "<p class=\"mc-h\"><b>{this}</b><span class=\"mc-to\"> to </span>{orchestrator}"
        ))
        .unwrap_or_else(|| panic!("{now}"));
    let to_it = now
        .find(&format!(
            "<p class=\"mc-h\"><b>{orchestrator}</b><span class=\"mc-to\"> to </span>{this}"
        ))
        .unwrap_or_else(|| panic!("the message to it: {now}"));
    assert!(own < to_it, "{now}");
    assert!(now.contains("datetime=\"2026-10-05T09:40:00Z\""), "{now}");
    assert!(
        now.contains("<div class=\"mc-b\"><p>Landed the seams on main.")
            && now.contains(
                "…</p></div><p class=\"mc-more\"><a href=\"#message-900002\">Read it whole</a></p>"
            ),
        "{now}"
    );
    assert!(
        !now.contains("Start with the seams"),
        "the latest only: {now}"
    );
    assert!(
        now[to_it..].contains(
            "<code>lash-core-store__unit_test</code> and intent_hash_golden_vector are red."
        ),
        "{now}"
    );
    // its last word was days ago and its run has written nothing since: its status line says
    // it has gone quiet, and since when
    let status = &drawer[drawer.find("<p class=\"status-line\">").unwrap()..];
    let status = &status[..status.find("</p>").unwrap()];
    assert!(
        status.contains("<span class=\"dot quiet\"")
            && status.contains("<span class=\"pill quiet\">quiet ")
            && status.contains("not heard from since <time data-ago=\"2026-10-05T09:40:00Z\""),
        "{status}"
    );
    // the Now section comes before everything else under the head
    assert!(
        drawer.find("d-now").unwrap()
            < drawer
                .find("<h3 class=\"tp-h\">Inputs</h3>")
                .unwrap_or(usize::MAX)
    );
    // on its own page its name is the band's h1, and the page's sections h2 and h3 under it
    let band = step.band("p", None).unwrap();
    assert!(
        band.as_str()
            .contains("<h1 id=\"d-title\" class=\"sh-t\" title=\"work\">work</h1>"),
        "{}",
        band.as_str()
    );
    let page = step.page_body("p", "", None).unwrap();
    let page = page.as_str();
    assert!(
        page.contains("<h2 class=\"tp-h\">Overview</h2>")
            && page.contains("<h3 id=\"ov-now-h\">Now</h3>")
            && !page.contains("<h1")
            && !page.contains("<h4"),
        "{page}"
    );
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
        assert_eq!(calls[0].revision, 2);
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
    // back to the step's own address (303), so a reload posts nothing again
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response.headers()["location"].to_str().unwrap().to_owned();
    assert!(
        location.starts_with(&format!("{path}?notice=")),
        "{location}"
    );
    let html = body(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri(&location)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert!(html.starts_with("<!doctype html>"), "{html}");
    assert!(
        html.contains("<div class=\"notice\" role=\"alert\">")
            && html.contains("<p>Nothing was done: this step changed since the page drew it, and it is stale now. Look again, then retry. What you wrote is kept in its box: Retry sends it.</p>"),
        "{html}"
    );
    assert!(
        html.contains("<details class=\"sh-fb\" data-preserve-attr=\"open\" open><summary")
            && html.contains("data-ignore-morph placeholder=\"What its next run should do differently\">Look again</textarea>"),
        "{html}"
    );
    assert_eq!(fake.0.lock().unwrap().len(), 1, "nothing more was done");
    // an action that no longer applies says so; a script gets JSON with the matching code
    let response = post("action=cancel&revision=2".into(), "application/json").await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let json: serde_json::Value = serde_json::from_str(&body(response).await).unwrap();
    assert_eq!(json["error"], "conflict", "{json}");
    assert_eq!(
        json["message"],
        "Nothing was done: Cancel does not apply to a stale step."
    );
    // a succeeded step's Retry asks first: what was typed waits in the box its button opens
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET status='succeeded' WHERE project_id=?1",
                [project.to_string()],
            )?;
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
    let response = post(
        format!("action=retry&revision=0&seen={seen}&message=Use+the+12h+cap"),
        "text/html",
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response.headers()["location"].to_str().unwrap().to_owned();
    let html = body(
        app.clone()
            .oneshot(
                Request::builder()
                    .uri(&location)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert!(
        html.contains("and it is succeeded now. Look again, then retry. What you wrote is kept: Retry opens it again.</p>")
            && html.contains("data-ignore-morph>Use the 12h cap</textarea>"),
        "{html}"
    );
}
