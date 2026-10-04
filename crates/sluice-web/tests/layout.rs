use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use sluice_web::views::{home::HomeView, *};
use std::sync::Arc;
use tower::ServiceExt;
fn fixture() -> DashboardSnapshot {
    DashboardSnapshot {
        projects: vec![ProjectView {
            id: "019b7c00-0000-7000-8000-000000000001".parse().unwrap(),
            name: "Project <script> & \"name\"".into(),
            description: "# A description\n\nAnother paragraph".into(),
            icon_text: "X".into(),
            icon_url: String::new(),
            paused: false,
            archived: false,
            changed: "2026-10-03T12:00:00Z".into(),
            counts: Counts {
                failed: 1,
                pending: 2,
                ..Counts::default()
            },
            running: vec![],
            failed_steps: vec!["broken".into()],
        }],
        inbox: 2,
        runner_stopped: true,
        functions: FunctionCatalog {
            version: "one".into(),
            entries: vec![FunctionView {
                name: "agent.<unsafe>".into(),
                scope: "global".into(),
                doc: "<img src=x onerror=alert()>".into(),
                inputs: vec![PortView {
                    name: "prompt".into(),
                    ty: "string".into(),
                }],
                outputs: vec![],
                error: "Collision <unsafe>".into(),
            }],
        },
    }
}
#[test]
fn shared_layout_escapes_labels_marks_current_tab_and_has_two_settings_controls() {
    let snapshot = fixture();
    let project = snapshot.projects[0].id;
    let html = home::render_functions(&snapshot, Some(project), &Viewer::default()).unwrap();
    let html = html.as_str();
    assert!(!html.contains("<script> &"));
    assert!(html.contains("Project &#60;script&#62; &#38; &#34;name&#34;"));
    assert!(!html.contains("<img src=x onerror"));
    assert!(html.contains("aria-label=\"Project settings\""));
    assert!(html.contains("aria-label=\"Display preferences\""));
    assert!(html.contains(&format!("/projects/id/{}/settings", project)));
    assert!(html.contains("/fns?project="));
    assert_eq!(html.matches("class=\"top\"").count(), 1);
    assert!(html.contains("aria-current=\"page\">Functions"));
    assert!(html.contains("Collision &#60;unsafe&#62;"));
    assert!(html.contains("data-signals="));
    assert!(html.contains("/static/style.css?v="));
}
#[test]
fn home_groups_archives_and_includes_runner_status_failure_title_and_no_script_content() {
    let mut snapshot = fixture();
    let mut old = snapshot.projects[0].clone();
    old.archived = true;
    old.name = "Old".into();
    snapshot.projects.push(old);
    let view = HomeView::new(&snapshot);
    let html = view
        .render(
            &snapshot,
            &Viewer {
                theme: Some("dark".into()),
                types: true,
            },
        )
        .unwrap();
    assert!(html.as_str().contains("Archived (1)"));
    assert!(html.as_str().contains("Runner stopped"));
    assert!(html.as_str().contains("1 failed · Projects · sluice"));
    assert!(html.as_str().contains("data-theme=\"dark\""));
    assert!(html.as_str().contains("class=\"show-types\""));
    assert!(html.as_str().contains("data-preserve-attr=\"open\""));
    assert!(html.as_str().contains("Stopped:"));
}
#[tokio::test]
async fn router_and_facade_read_a_fixture_store_and_render_first_load_without_a_stream() {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "fixture".parse().unwrap(),
                    description: "text".into(),
                    icon: None,
                    resources: None,
                    author: "owner".into(),
                },
                &EmptyPlanInitializer,
                &NoResourceSettings,
            )
        })
        .await
        .unwrap();
    let state = DashboardState::new(
        ReadPool::open(home.path(), 1).unwrap(),
        Arc::new(EmptyCatalog),
    );
    let snapshot = state.snapshot(None).await.unwrap();
    assert_eq!(snapshot.projects.len(), 1);
    assert_eq!(snapshot.projects[0].name, "fixture");
    let router = dashboard_router(state);
    for path in [
        "/",
        "/fns",
        "/static/style.css",
        "/static/nav.js",
        "/static/datastar-rocket-1.0.4.js",
        "/static/logo.svg",
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{path}");
        let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        assert!(!body.is_empty());
    }
}
#[tokio::test]
async fn a_catalog_that_changes_on_every_read_is_read_once_per_snapshot() {
    struct Changing(std::sync::atomic::AtomicUsize);
    impl CatalogSource for Changing {
        fn catalog(
            &self,
            _: Option<sluice_model::ids::ProjectId>,
        ) -> Result<FunctionCatalog, sluice_model::error::PublicError> {
            Ok(FunctionCatalog {
                version: self
                    .0
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    .to_string(),
                entries: vec![],
            })
        }
    }
    let home = tempfile::tempdir().unwrap();
    let _writer = Writer::open(home.path()).unwrap();
    let state = DashboardState::new(
        ReadPool::open(home.path(), 1).unwrap(),
        Arc::new(Changing(std::sync::atomic::AtomicUsize::new(0))),
    );
    // Each snapshot renders the one catalog it read; a change never defers or fails it.
    assert_eq!(state.snapshot(None).await.unwrap().functions.version, "0");
    assert_eq!(state.snapshot(None).await.unwrap().functions.version, "1");
}
#[tokio::test]
async fn display_preferences_validate_cookies_and_empty_scope_stays_global() {
    let home = tempfile::tempdir().unwrap();
    let _writer = Writer::open(home.path()).unwrap();
    let router = dashboard_router(DashboardState::new(
        ReadPool::open(home.path(), 1).unwrap(),
        Arc::new(EmptyCatalog),
    ));
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/fns?project=")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/settings")
                .body(Body::from("theme=dark&types=0&types=1&next=%2Ffns"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 303);
    assert_eq!(response.headers()["location"], "/fns");
    assert_eq!(response.headers().get_all("set-cookie").iter().count(), 2);
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/settings")
                .body(Body::from("theme=script&next=%2F%2Fevil.example"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
}
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
        HomeView::new(snapshot)
            .render(snapshot, &Viewer::default())
            .unwrap()
            .as_str()
            .to_owned()
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
