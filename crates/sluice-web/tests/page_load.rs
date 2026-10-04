//! A page renders one consistent snapshot: live runs whose files change while it renders never
//! fail it, its stream's first batch agrees with what it drew, and a server error always says
//! what went wrong.
use axum::{
    Extension, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    response::IntoResponse,
};
use futures_util::{StreamExt, future::BoxFuture};
use serde_json::{Value, json};
use sluice_model::{
    commands::{CommandReply, CommandRequest},
    error::PublicError,
    ids::{AttemptId, ProjectId, RunId},
    plan::FnSignature,
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use sluice_web::{
    http,
    mcp::CommandService,
    views::{
        CatalogSource, DashboardState, EmptyCatalog, FunctionCatalog, PageState,
        board::{Registry, RegistrySnapshot, RegistrySource},
    },
};
use std::{
    io::Write as _,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tower::ServiceExt;

struct Commands;
impl CommandService for Commands {
    fn command(&self, _: CommandRequest) -> BoxFuture<'_, Result<CommandReply, PublicError>> {
        Box::pin(async { Ok(CommandReply::Ack) })
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
struct Unavailable;
impl CatalogSource for Unavailable {
    fn catalog(&self, _: Option<ProjectId>) -> Result<FunctionCatalog, PublicError> {
        Err(PublicError::Storage {
            message: "the fn catalog is unavailable".into(),
        })
    }
}

const STEPS: [&str; 4] = ["w0", "w1", "w2", "w3"];

/// A project whose four steps are running, each with a live run and its run directory.
async fn live_home(
    catalog: Arc<dyn CatalogSource>,
) -> (tempfile::TempDir, Writer, Router, ProjectId, Vec<RunId>) {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let id = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "live".parse().unwrap(),
                    description: "Agent lanes writing to their run files.".into(),
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
    let runs: Vec<RunId> = STEPS.iter().map(|_| RunId::new()).collect();
    let stored = runs.clone();
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let steps: serde_json::Map<String, Value> = STEPS
                .iter()
                .map(|s| (s.to_string(), json!({"run": "custom.open"})))
                .collect();
            let doc = json!({ "steps": steps });
            tx.sql().execute(
                "UPDATE plans SET doc=?2 WHERE project_id=?1",
                (id.to_string(), doc.to_string()),
            )?;
            for (position, (step, run)) in STEPS.iter().zip(&stored).enumerate() {
                let attempt = AttemptId::new();
                tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,status,generation,work_generation,run_ids) VALUES (?1,?2,?3,?4,'running',1,1,?5)", (id.to_string(), step, position as i64, doc["steps"][step].to_string(), json!([run]).to_string()))?;
                tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at) VALUES (?1,?2,?3,1,1,'executing',?4,'hash','2026-10-04T00:00:00Z')", (attempt.to_string(), id.to_string(), step, json!({"inputs": {}}).to_string()))?;
                tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at,started_at) VALUES (?1,?2,?3,?4,1,1,'2026-10-04T00:00:00Z','2026-10-04T00:00:00Z')", (run.to_string(), id.to_string(), attempt.to_string(), step))?;
            }
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
    for run in &runs {
        let dir = home.path().join("runs").join(run.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stderr.txt"), "starting\n").unwrap();
    }
    let pages = PageState::new(DashboardState::new(
        ReadPool::open(home.path(), 4).unwrap(),
        catalog,
    ));
    let app = http::router(
        pages,
        Arc::new(Commands),
        tokio_util::sync::CancellationToken::new(),
    )
    .layer(Extension(Registry(Arc::new(Exact))));
    (home, writer, app, id, runs)
}

/// Append to every run's stderr.txt as fast as possible until dropped, as live agent lanes do.
struct Churn {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<u64>>,
}
impl Churn {
    fn start(home: &std::path::Path, runs: &[RunId]) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let files: Vec<_> = runs
            .iter()
            .map(|r| home.join("runs").join(r.to_string()).join("stderr.txt"))
            .collect();
        let running = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut writes = 0;
            while !running.load(Ordering::Relaxed) {
                for file in &files {
                    let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(file) else {
                        return writes;
                    };
                    writeln!(f, "tick {writes}").unwrap();
                    writes += 1;
                }
            }
            writes
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
    fn finish(mut self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap()
    }
}
impl Drop for Churn {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

async fn get(app: &Router, path: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(header::HOST, "127.0.0.1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 4 << 20).await.unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

/// The `ver` a page was drawn at, from its body's escaped `data-signals`.
fn drawn_version(page: &str) -> String {
    let start = page
        .find("&#34;ver&#34;:&#34;")
        .expect("page has a ver signal")
        + 19;
    let end = start + page[start..].find("&#34;").unwrap();
    page[start..end].to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pages_render_while_live_runs_write_their_files() {
    let (home, writer, app, id, runs) = live_home(Arc::new(EmptyCatalog)).await;
    let churn = Churn::start(home.path(), &runs);
    let pages = [
        "/".to_owned(),
        format!("/projects/id/{id}"),
        format!("/projects/id/{id}/units/w1"),
        format!("/projects/id/{id}/steps/w2"),
        format!("/fns?project={id}"),
    ];
    let mut served = 0;
    for _ in 0..40 {
        for path in &pages {
            let (status, body) = get(&app, path).await;
            assert_eq!(status, StatusCode::OK, "{path}: {body}");
            served += 1;
        }
    }
    let writes = churn.finish();
    assert_eq!(served, 200);
    assert!(
        writes > 1000,
        "the run files kept changing ({writes} writes)"
    );
    writer.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stream_opened_at_the_drawn_version_sends_no_redraw() {
    let (home, writer, app, id, runs) = live_home(Arc::new(EmptyCatalog)).await;
    let churn = Churn::start(home.path(), &runs);
    // The home page draws each run's last activity to the second, so only pages that do not
    // show it are expected to match while the files change.
    for path in [
        format!("/projects/id/{id}"),
        format!("/projects/id/{id}/units/w1"),
    ] {
        let (status, page) = get(&app, &path).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let ver = drawn_version(&page);
        let stream = format!(
            "{path}/stream?datastar={}",
            url::form_urlencoded::byte_serialize(json!({ "ver": ver }).to_string().as_bytes())
                .collect::<String>()
        );
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&stream)
                    .header(header::HOST, "127.0.0.1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{stream}");
        let mut body = response.into_body().into_data_stream();
        let first = String::from_utf8(body.next().await.unwrap().unwrap().to_vec()).unwrap();
        assert!(
            first.contains("datastar-patch-signals") && first.contains(r#"{"stale":false}"#),
            "{path}: the first batch only confirms the drawn page: {first}"
        );
        assert!(
            !first.contains("datastar-patch-elements"),
            "{path}: {first}"
        );
    }
    churn.finish();
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn page_errors_carry_their_message() {
    let (_home, writer, app, id, _runs) = live_home(Arc::new(Unavailable)).await;
    for path in [
        "/".to_owned(),
        format!("/projects/id/{id}"),
        format!("/projects/id/{id}/steps/w0"),
        "/fns".to_owned(),
    ] {
        let (status, body) = get(&app, &path).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{path}");
        let error: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            error,
            json!({"error": "storage", "message": "the fn catalog is unavailable"}),
            "{path}"
        );
    }
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_empty_error_response_is_named_by_its_status() {
    for (status, kind, message) in [
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "busy",
            "Service Unavailable",
        ),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage",
            "Internal Server Error",
        ),
        (StatusCode::BAD_GATEWAY, "storage", "Bad Gateway"),
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            "bad_request",
            "Payload Too Large",
        ),
        (StatusCode::NOT_FOUND, "not_found", "Not Found"),
    ] {
        let response = http::error_json(status.into_response()).await;
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let error: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(error["error"], kind, "{status}");
        assert_eq!(error["message"], message, "{status}");
    }
    let response =
        http::error_json((StatusCode::INTERNAL_SERVER_ERROR, "disk full").into_response()).await;
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({"error": "storage", "message": "disk full"})
    );
}

#[tokio::test]
async fn fingerprinted_assets_are_cached_for_good() {
    let (_home, writer, app, _id, _runs) = live_home(Arc::new(EmptyCatalog)).await;
    let (_, page) = get(&app, "/").await;
    let start = page.find("/static/style.css?v=").unwrap();
    let linked = &page[start..start + page[start..].find('"').unwrap()];
    for (path, cache) in [
        (linked, "public, max-age=31536000, immutable"),
        ("/static/style.css", "public, max-age=0, must-revalidate"),
        (
            "/static/style.css?v=0000",
            "public, max-age=0, must-revalidate",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(header::HOST, "127.0.0.1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], cache, "{path}");
    }
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_plan_edited_between_renders_is_drawn_at_once() {
    let (_home, writer, app, id, _runs) = live_home(Arc::new(EmptyCatalog)).await;
    let page = format!("/projects/id/{id}");
    let (_, before) = get(&app, &page).await;
    assert!(before.contains("data-step=\"w3\"") && !before.contains("data-step=\"added\""));
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let doc: String = tx.sql().query_row(
                "SELECT doc FROM plans WHERE project_id=?1",
                [id.to_string()],
                |r| r.get(0),
            )?;
            let mut doc: Value = serde_json::from_str(&doc).unwrap();
            doc["steps"]["added"] = json!({"run": "custom.open"});
            tx.sql().execute(
                "UPDATE plans SET doc=?2 WHERE project_id=?1",
                (id.to_string(), doc.to_string()),
            )?;
            tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,status) VALUES (?1,'added',9,?2,'pending')", (id.to_string(), doc["steps"]["added"].to_string()))?;
            tx.changed(Some(id), "plan");
            Ok(())
        })
        .await
        .unwrap();
    let (status, after) = get(&app, &page).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert!(after.contains("data-step=\"added\""));
    writer.shutdown().await.unwrap();
}
