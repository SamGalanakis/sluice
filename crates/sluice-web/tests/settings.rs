use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use futures_util::{StreamExt, future::BoxFuture};
use serde_json::{Value, json};
use sluice_model::{
    commands::StepSubmit,
    error::PublicError,
    ids::{AttemptId, ProjectId, ProjectSelector, Revision, RunId},
    plan::{FnSignature, SignatureProvider},
    types::Type,
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    projects::{
        self, CreateProject, EmptyPlanInitializer, NoResourceSettings, Project, StoredWorkOnly,
        UpdateProject,
    },
    resources,
};
use sluice_web::{
    settings::{self, SettingsCommands, SettingsState, StoreCommands},
    views::{DashboardState, EmptyCatalog},
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tower::ServiceExt;
struct Resources;
impl SignatureProvider for Resources {
    fn signature(&self, name: &str) -> Option<FnSignature> {
        (name == "test.capacity").then(|| FnSignature {
            outputs: [("capacity".into(), Type::Int)].into_iter().collect(),
            ..Default::default()
        })
    }
}
impl projects::ResourceSettings for Resources {
    fn set_resources(
        &self,
        tx: &mut sluice_store::WriteTransaction<'_>,
        id: ProjectId,
        patch: &Value,
    ) -> sluice_store::Result<bool> {
        resources::patch_resources(tx, id, patch, self)
    }
}
struct Fixture {
    _home: tempfile::TempDir,
    writer: Writer,
    state: SettingsState,
    id: ProjectId,
}
impl Fixture {
    async fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let writer = Writer::open(home.path()).unwrap();
        let p = writer
            .write(RetrySafety::NonIdempotent, |tx| {
                projects::project_create(
                    tx,
                    CreateProject {
                        name: "first".parse().unwrap(),
                        description: "# Safe description".into(),
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
        let dashboard = DashboardState::new(
            ReadPool::open(home.path(), 2).unwrap(),
            Arc::new(EmptyCatalog),
        );
        let guard = Arc::new(StoredWorkOnly);
        let commands = Arc::new(StoreCommands {
            writer: writer.clone(),
            resources: Arc::new(Resources),
            deletion_guard: guard.clone(),
        });
        Self {
            _home: home,
            writer,
            state: SettingsState::new(dashboard, commands, guard),
            id: p.project_id,
        }
    }
    fn router(&self) -> Router {
        settings::router(self.state.clone())
    }
    fn path(&self) -> String {
        format!("/projects/id/{}/settings", self.id)
    }
    async fn project(&self) -> Project {
        let id = self.id;
        self.state
            .dashboard
            .reads
            .snapshot(move |c| projects::resolve(c, &ProjectSelector::Id(id)))
            .await
            .unwrap()
    }
    async fn post(&self, field: &str, value: &str, resource: &str, rev: u64) -> (u16, String) {
        request(
            self.router(),
            "POST",
            &self.path(),
            json!({"field":field,"value":value,"resource":resource,"expected_settings_rev":rev})
                .to_string(),
        )
        .await
    }
    async fn get_page(&self) -> (u16, String) {
        request(self.router(), "GET", &self.path(), String::new()).await
    }
    async fn delete(&self, rev: u64) -> (u16, String) {
        request(
            self.router(),
            "POST",
            &format!("{}/delete", self.path()),
            json!({"expected_settings_rev":rev}).to_string(),
        )
        .await
    }
    async fn records(&self) -> Vec<Value> {
        self.state
            .dashboard
            .reads
            .snapshot(|c| {
                let mut q = c.prepare(
                    "SELECT payload FROM records WHERE kind LIKE 'project.%' ORDER BY seq",
                )?;
                q.query_map([], |r| r.get::<_, String>(0))?
                    .map(|r| Ok(serde_json::from_str(&r?)?))
                    .collect()
            })
            .await
            .unwrap()
    }
}
async fn request(router: Router, method: &str, path: &str, body: String) -> (u16, String) {
    let response = router
        .oneshot(
            Request::builder()
                .uri(path)
                .method(method)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}
#[tokio::test]
async fn fields_commit_separately_with_owner_records_safe_preview_and_resource_validation() {
    let f = Fixture::new().await;
    for (n, field, value, resource) in [
        (1, "name", "renamed", ""),
        (
            2,
            "description",
            "# Title\n\n<script>alert(1)</script> [bad](javascript:alert)",
            "",
        ),
        (3, "icon", "x", ""),
        (4, "resources", "2", "workers"),
        (5, "paused", "true", ""),
        (6, "archived", "true", ""),
    ] {
        let (status, html) = f.post(field, value, resource, n).await;
        assert_eq!(status, 200, "{html}");
        assert!(html.contains("Saved"));
        assert_eq!(f.project().await.settings_rev, Revision(n + 1));
    }
    let records = f.records().await;
    assert_eq!(records.len(), 6);
    assert!(records.iter().all(|r| r["author"] == "owner"));
    assert_eq!(
        records
            .iter()
            .map(|r| r["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "project.rename",
            "project.update",
            "project.update",
            "project.update",
            "project.pause",
            "project.archive"
        ]
    );
    let (status, preview) = request(
        f.router(),
        "POST",
        &format!("{}/preview", f.path()),
        "<script>alert(1)</script> [bad](javascript:alert)".into(),
    )
    .await;
    assert_eq!(status, 200);
    assert!(!preview.contains("<script>"));
    assert!(!preview.contains("href=\"javascript:"));
    let (status, html) = f.post("resources", "-1", "workers", 7).await;
    assert_eq!(status, 400);
    assert!(html.contains("value=\"-1\""));
    assert_eq!(f.project().await.settings_rev, Revision(7));
    let id = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "INSERT INTO steps(project_id,step_id,position,declaration) VALUES (?1,'s',0,?2)",
                (
                    id.to_string(),
                    json!({"run":"core.external","needs":{"workers":1}}).to_string(),
                ),
            )?;
            tx.changed(Some(id), "plan");
            Ok(())
        })
        .await
        .unwrap();
    let (status, html) = f.post("resources", "null", "workers", 7).await;
    assert_eq!(status, 400);
    assert!(html.contains("a step needs it"));
    assert_eq!(f.project().await.settings_rev, Revision(7));
}
#[tokio::test]
async fn duplicate_name_and_stale_revision_have_no_partial_effect_or_records() {
    let f = Fixture::new().await;
    f.writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "taken".parse().unwrap(),
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
        .unwrap();
    let before = f.project().await;
    let records = f.records().await;
    assert_eq!(f.post("name", "taken", "", 1).await.0, 409);
    assert_eq!(f.post("description", "lost edit", "", 0).await.0, 409);
    assert_eq!(f.project().await, before);
    assert_eq!(f.records().await, records);
    let (status, html) = f.post("name", "Invalid Name", "", 1).await;
    assert_eq!(status, 400);
    assert!(html.contains("value=\"Invalid Name\""));
    assert_eq!(
        request(f.router(), "GET", "/projects/first/settings", String::new())
            .await
            .0,
        308
    );
    assert_eq!(f.post("name", "second", "", 1).await.0, 200);
    assert_eq!(
        request(f.router(), "GET", "/projects/first/settings", String::new())
            .await
            .0,
        404
    );
    assert_eq!(
        request(
            f.router(),
            "GET",
            "/projects/second/settings",
            String::new()
        )
        .await
        .0,
        308
    );
}
#[tokio::test]
async fn rename_preserves_live_callbacks_next_reader_and_id_stream() {
    let f = Fixture::new().await;
    let id = f.id;
    let run = RunId::new();
    let attempt = AttemptId::new();
    f.writer.write(RetrySafety::NonIdempotent,move |tx| {
  tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,status,generation,work_generation,run_ids) VALUES (?1,'s',0,'{}','running',1,1,?2)",(id.to_string(),json!([run]).to_string()))?;
  tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at) VALUES (?1,?2,'s',1,1,'executing',?3,'hash','now')",(attempt.to_string(),id.to_string(),json!({"declared":{"result":"string"}}).to_string()))?;
  tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at) VALUES (?1,?2,?3,'s',1,1,'now')",(run.to_string(),id.to_string(),attempt.to_string()))?;tx.changed(Some(id),"status");Ok(())
 }).await.unwrap();
    let response = f
        .router()
        .oneshot(
            Request::builder()
                .uri(format!("{}/stream", f.path()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let mut stream = response.into_body().into_data_stream();
    let initial = stream.next().await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&initial).contains("datastar-patch"));
    let w = f.writer.clone();
    let reads = f.state.dashboard.reads.clone();
    let next = tokio::spawn(async move {
        sluice_runtime::watch::next(
            &w,
            &reads,
            sluice_runtime::watch::NextOptions {
                projects: vec![id],
                settle: Duration::ZERO,
                timeout: Some(Duration::from_secs(3)),
                ..Default::default()
            },
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let ready = f
                .state
                .dashboard
                .reads
                .snapshot(move |c| {
                    Ok(sluice_store::messages::reader(
                        c,
                        id,
                        "orchestrator",
                        sluice_store::messages::ORCHESTRATOR_STREAM,
                        "",
                    )?
                    .heartbeat_at
                    .is_some())
                })
                .await
                .unwrap();
            if ready {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.post("name", "second", "", 1).await.0, 200);
    f.writer.write(RetrySafety::NonIdempotent,move |tx| {
  assert_eq!(sluice_store::attempts::step_submit(tx,StepSubmit {project:id,step:"s".parse().unwrap(),run,outputs:serde_json::from_value(json!({"result":"still live"}))?,author:Some("worker".into())})?,Some(1));
  let message:sluice_model::commands::Ask=serde_json::from_value(json!({"project":{"kind":"id","value":id},"to":"orchestrator","body":"callback after rename","run":run}))?;
  sluice_store::messages::post(tx,message.try_into()?,&sluice_store::messages::NoPlanInputs)?;Ok(())
 }).await.unwrap();
    let next = next.await.unwrap().unwrap();
    assert!(!next.timed_out);
    assert!(
        serde_json::to_string(&next)
            .unwrap()
            .contains("callback after rename")
    );
    let mut wire = String::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !wire.contains("second") {
            wire.push_str(&String::from_utf8_lossy(
                &stream.next().await.unwrap().unwrap(),
            ));
        }
    })
    .await
    .unwrap();
    assert!(wire.contains(&id.to_string()));
    let state = f
        .state
        .dashboard
        .reads
        .snapshot(move |c| {
            Ok((
                c.query_row(
                    "SELECT run_id FROM runs WHERE project_id=?1",
                    [id.to_string()],
                    |r| r.get::<_, String>(0),
                )?,
                sluice_store::messages::reader(
                    c,
                    id,
                    "orchestrator",
                    sluice_store::messages::ORCHESTRATOR_STREAM,
                    "",
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(state.0, run.to_string());
    assert!(state.1.cursor.0 > 0);
}
#[tokio::test]
async fn dashboard_deletion_archives_first_and_requires_current_revision_and_no_live_work() {
    let f = Fixture::new().await;
    let (_, html) = request(f.router(), "GET", &f.path(), String::new()).await;
    assert!(html.contains("Delete first?"));
    assert!(!html.contains("confirm-name"));
    assert!(!html.contains("data-disabled=\"true\""));
    let id = f.id;
    f.writer.write(RetrySafety::NonIdempotent,move |tx| {tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,status) VALUES (?1,'s',0,'{}','running')",[id.to_string()])?;tx.changed(Some(id),"status");Ok(())}).await.unwrap();
    assert_eq!(f.delete(1).await.0, 400);
    assert!(!f.project().await.archived);
    let (_, blocked) = f.get_page().await;
    assert!(blocked.contains("id=\"delete-button\" data-disabled=\"true\""));
    assert!(blocked.contains("Open the plan"));
    let view = f.state.snapshot(id).await.unwrap();
    assert_eq!(view.blocker.as_deref(), Some("project has running steps"));
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET status='pending' WHERE project_id=?1",
                [id.to_string()],
            )?;
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(f.post("name", "second", "", 1).await.0, 200);
    assert_eq!(f.delete(1).await.0, 409);
    assert!(!f.project().await.archived);
    let response = f
        .router()
        .oneshot(
            Request::builder()
                .uri(format!("{}/stream", f.path()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut stream = response.into_body().into_data_stream();
    stream.next().await.unwrap().unwrap();
    assert_eq!(f.delete(2).await.0, 303);
    assert_eq!(
        request(f.router(), "GET", &f.path(), String::new()).await.0,
        404
    );
    assert!(
        f.state
            .dashboard
            .snapshot(None)
            .await
            .unwrap()
            .projects
            .is_empty()
    );
    let mut wire = String::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !wire.contains("data-deleted") {
            wire.push_str(&String::from_utf8_lossy(
                &stream.next().await.unwrap().unwrap(),
            ));
        }
    })
    .await
    .unwrap();
    let records = f.records().await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["kind"], "project.delete");
    assert_eq!(records[0]["author"], "owner");
    assert!(!records[0].to_string().contains("confirm_name"));
}
#[tokio::test]
async fn coordinator_guard_is_visible_and_rechecked_in_delete_command() {
    struct Guard;
    impl projects::DeletionGuard for Guard {
        fn check(&self, _: ProjectId) -> sluice_store::Result<()> {
            Err(PublicError::Conflict {
                message: "guardian still owns a process".into(),
                current_rev: None,
            }
            .into())
        }
    }
    let mut f = Fixture::new().await;
    assert_eq!(f.post("archived", "true", "", 1).await.0, 200);
    let guard = Arc::new(Guard);
    f.state.deletion_guard = guard.clone();
    f.state.commands = Arc::new(StoreCommands {
        writer: f.writer.clone(),
        resources: Arc::new(Resources),
        deletion_guard: guard,
    });
    assert_eq!(
        f.state.snapshot(f.id).await.unwrap().blocker.as_deref(),
        Some("guardian still owns a process")
    );
    assert_eq!(f.delete(2).await.0, 409);
    assert_eq!(f.project().await.name.as_str(), "first");
}
#[tokio::test]
async fn injected_command_receives_id_revision_and_owner_and_preserves_errors() {
    struct Fake {
        project: Project,
        seen: Mutex<Vec<(ProjectId, UpdateProject)>>,
    }
    impl SettingsCommands for Fake {
        fn update(
            &self,
            id: ProjectId,
            request: UpdateProject,
        ) -> BoxFuture<'_, Result<Project, PublicError>> {
            self.seen.lock().unwrap().push((id, request));
            Box::pin(async {
                Err(PublicError::Conflict {
                    message: "changed by another browser".into(),
                    current_rev: Some(Revision(5)),
                })
            })
        }
        fn delete(
            &self,
            _: ProjectId,
            _: projects::DeleteProject,
        ) -> BoxFuture<'_, Result<(), PublicError>> {
            let name = self.project.name.to_string();
            Box::pin(async move { Err(PublicError::NotFound { message: name }) })
        }
        fn board(
            &self,
            _: ProjectId,
            _: projects::SetBoard,
        ) -> BoxFuture<'_, Result<projects::BoardSetOutcome, PublicError>> {
            Box::pin(async {
                Ok(projects::BoardSetOutcome {
                    rev: Revision(1),
                    warnings: vec![],
                })
            })
        }
    }
    let mut f = Fixture::new().await;
    let fake = Arc::new(Fake {
        project: f.project().await,
        seen: Mutex::new(vec![]),
    });
    f.state.commands = fake.clone();
    let (status, html) = f.post("description", "draft", "", 1).await;
    assert_eq!(status, 409);
    assert!(html.contains("changed by another browser"));
    assert!(html.contains(">draft</textarea>"));
    let seen = fake.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, f.id);
    assert_eq!(seen[0].1.description.as_deref(), Some("draft"));
    assert_eq!(seen[0].1.expected_settings_rev, Some(Revision(1)));
    assert_eq!(seen[0].1.author, "owner");
}
#[tokio::test]
async fn images_are_validated_and_served_by_id_and_generation() {
    let f = Fixture::new().await;
    let bytes = b"\x89PNG\r\n\x1a\nfixture".to_vec();
    let response = f
        .router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("{}/icon?expected_settings_rev=1", f.path()))
                .body(Body::from(bytes.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let project = f.project().await;
    let url = project.icon.unwrap().url(f.id).unwrap();
    let response = f
        .router()
        .oneshot(Request::builder().uri(&url).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap().to_vec(),
        bytes
    );
    assert_eq!(f.post("icon", "text", "", 2).await.0, 200);
    assert_eq!(request(f.router(), "GET", &url, String::new()).await.0, 404);
    let response = f
        .router()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("{}/icon?expected_settings_rev=3", f.path()))
                .body(Body::from("bad image"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(f.project().await.settings_rev, Revision(3));
}

#[tokio::test]
async fn retiring_is_set_in_hours_with_keep_patterns_and_turned_off_when_empty() {
    let f = Fixture::new().await;
    let (status, html) = f.get_page().await;
    assert_eq!(status, 200);
    assert!(html.contains("Retire done units after"));
    assert!(html.contains("Done units stay until an edit removes them"));
    let (status, html) = f.post("prune_done_after", "6", "", 1).await;
    assert_eq!(status, 200, "{html}");
    let p = f.project().await;
    assert_eq!(p.prune_done_after, Some(21600));
    assert!(html.contains("Done units retire 6h after their last step finished"));
    assert!(html.contains("id=\"prune-done-after\" name=\"value\""));
    let (status, html) = f.post("prune_keep", "ta-*, fig-? release", "", 2).await;
    assert_eq!(status, 200, "{html}");
    assert_eq!(f.project().await.prune_keep, ["ta-*", "fig-?", "release"]);
    assert!(html.contains("value=\"ta-*, fig-?, release\""));
    assert!(html.contains("except 3 patterns"));
    let (status, _) = f.post("prune_done_after", "1.5", "", 3).await;
    assert_eq!(status, 200);
    assert_eq!(f.project().await.prune_done_after, Some(5400));
    // Not a number, zero or negative is refused with the typed value kept.
    for bad in ["soon", "0", "-2"] {
        let (status, html) = f.post("prune_done_after", bad, "", 4).await;
        assert_eq!(status, 400, "{bad}");
        assert!(html.contains("Enter a number of hours"), "{html}");
        assert!(html.contains(&format!("value=\"{bad}\"")));
    }
    // Empty turns retiring off; empty patterns clear them.
    let (status, _) = f.post("prune_done_after", "", "", 4).await;
    assert_eq!(status, 200);
    let (status, _) = f.post("prune_keep", " ", "", 5).await;
    assert_eq!(status, 200);
    let p = f.project().await;
    assert_eq!((p.prune_done_after, p.prune_keep), (None, vec![]));
    let fields: Vec<_> = f
        .records()
        .await
        .into_iter()
        .map(|r| r["fields"].clone())
        .collect();
    assert_eq!(
        fields,
        [
            json!(["prune_done_after"]),
            json!(["prune_keep"]),
            json!(["prune_done_after"]),
            json!(["prune_done_after"]),
            json!(["prune_keep"])
        ]
    );
}
