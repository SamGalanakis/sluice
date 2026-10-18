use crate::seed;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use futures_util::StreamExt;
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
    },
    resources,
};
use sluice_web::{
    settings::{self, SettingsState, StoreCommands},
    views::{DashboardState, EmptyCatalog},
};
use std::{sync::Arc, time::Duration};
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
                        limits: None,
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
    /// What section `section`'s form has seen, as the page draws it now.
    async fn seen(&self, section: &str) -> String {
        let (_, page) = self.get_page().await;
        page.split(&format!("id=\"{section}-form\""))
            .nth(1)
            .and_then(|form| form.split("name=\"seen\" value=\"").nth(1))
            .and_then(|rest| rest.split('"').next())
            .unwrap()
            .to_owned()
    }
    /// Section `section`'s Save from its form as the page drew it now, with `fields`: its
    /// answer, the section drawn anew and its notice (or the notice alone, refused).
    async fn save(&self, section: &str, fields: &[(&str, &str)]) -> String {
        let seen = self.seen(section).await;
        self.save_seen(section, &seen, fields).await
    }
    async fn save_seen(&self, section: &str, seen: &str, fields: &[(&str, &str)]) -> String {
        let mut pairs = vec![("section", section), ("seen", seen)];
        pairs.extend_from_slice(fields);
        let response = self
            .router()
            .oneshot(super::forms::post(
                self.path(),
                &format!("{section}-form"),
                &pairs,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        super::forms::text(response).await
    }
    /// The Details form's Save, every field as the project has it but `changes`.
    async fn details(&self, changes: &[(&str, &str)]) -> String {
        let p = self.project().await;
        let icon = match &p.icon {
            Some(projects::ProjectIcon::Text(t)) => t.clone(),
            _ => String::new(),
        };
        let id = self.id;
        let fallback = self
            .state
            .dashboard
            .reads
            .snapshot(move |c| sluice_store::identities::fallback(c, id))
            .await
            .unwrap()
            .unwrap_or_default();
        let name = p.name.to_string();
        let mut fields = vec![
            ("name", name.as_str()),
            ("description", p.description.as_str()),
            ("icon", icon.as_str()),
            ("fallback", fallback.as_str()),
        ];
        for (key, value) in changes {
            fields.retain(|(k, _)| k != key);
            fields.push((key, value));
        }
        self.save("details", &fields).await
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
async fn a_section_saves_what_changed_with_owner_records_safe_preview_and_resource_validation() {
    let f = Fixture::new().await;
    // one Save of Details applies what changed in it, each change its own record
    let html = f
        .details(&[
            ("name", "renamed"),
            (
                "description",
                "# Title\r\n\r\n<script>alert(1)</script> [bad](javascript:alert)",
            ),
            ("icon", "x"),
        ])
        .await;
    assert!(html.contains("role=\"status\">Saved.</p>"), "{html}");
    // the form drawn anew, its fields as saved
    assert!(
        html.contains("data: elements <div id=\"details-edit\">")
            && html.contains("id=\"project-name\" name=\"name\" value=\"renamed\""),
        "{html}"
    );
    let p = f.project().await;
    assert_eq!(
        p.description, "# Title\n\n<script>alert(1)</script> [bad](javascript:alert)",
        "a browser's line breaks are kept as one"
    );
    // a Save that changes nothing records nothing
    let html = f.details(&[]).await;
    assert!(html.contains("role=\"status\">Saved.</p>"), "{html}");
    let html = f
        .save(
            "resources",
            &[
                ("new", "workers"),
                ("capacity.:new", "number"),
                ("number.:new", "2"),
            ],
        )
        .await;
    assert!(html.contains("role=\"status\">Saved.</p>"), "{html}");
    assert!(
        html.contains("<label for=\"resource-workers\">workers</label>"),
        "{html}"
    );
    for switch in ["paused", "archived"] {
        let html = f.save("activity", &[(switch, "true")]).await;
        assert!(html.contains("role=\"status\">Saved.</p>"), "{html}");
    }
    let p = f.project().await;
    assert!(p.paused && p.archived);
    let records = f.records().await;
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
            "project.pause",
            "project.archive"
        ]
    );
    assert_eq!(records[1]["fields"], json!(["description", "icon"]));
    // the description as it reads, drawn into its place as it is typed
    let preview = super::forms::text(
        f.router()
            .oneshot(super::forms::post(
                format!("{}/preview", f.path()),
                "details-form",
                &[(
                    "description",
                    "<script>alert(1)</script> [bad](javascript:alert)",
                )],
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        preview.contains("data: selector #description-preview")
            && preview.contains("data: mode inner"),
        "{preview}"
    );
    assert!(!preview.contains("<script>"));
    assert!(!preview.contains("href=\"javascript:"));
    let rev = f.project().await.settings_rev;
    let html = f
        .save(
            "resources",
            &[
                ("resource", "workers"),
                ("capacity.workers", "number"),
                ("number.workers", "-1"),
            ],
        )
        .await;
    assert!(
        html.contains("role=\"alert\">workers: Enter a whole number of 0 or more.</p>"),
        "{html}"
    );
    assert_eq!(f.project().await.settings_rev, rev);
    let id = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            seed::put(
                tx,
                id,
                json!({"steps":{"s":{"run":"core.external","needs":{"workers":1}}}}),
            )?;
            Ok(())
        })
        .await
        .unwrap();
    // a resource's Remove (its confirmation's form) is refused while a step needs it
    let response = f
        .router()
        .oneshot(super::forms::post(
            f.path(),
            "remove-workers",
            &[("section", "remove"), ("resource", "workers")],
        ))
        .await
        .unwrap();
    let html = super::forms::text(response).await;
    assert!(
        html.contains("<p id=\"remove-workers-said\" class=\"form-said\" role=\"alert\">")
            && html.contains("a step needs it"),
        "{html}"
    );
    assert_eq!(f.project().await.settings_rev, rev);
}
#[tokio::test]
async fn duplicate_name_and_stale_revision_have_no_partial_effect_or_records() {
    let f = Fixture::new().await;
    f.writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    limits: None,
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
    // a taken name refuses the whole Save: the description sent with it is not written either
    let html = f
        .details(&[("name", "taken"), ("description", "lost edit")])
        .await;
    assert!(html.contains("role=\"alert\">"), "{html}");
    assert_eq!(f.project().await, before);
    assert_eq!(f.records().await, records);
    let html = f.details(&[("name", "Invalid Name")]).await;
    assert!(
        html.contains("role=\"alert\">Use lowercase letters, numbers, underscores or hyphens for the name.</p>"),
        "{html}"
    );
    // a Save over a change someone made to its section since the form was drawn is refused,
    // never written over theirs; another section's change is not in its way
    let seen = f.seen("details").await;
    let retiring = f.seen("retiring").await;
    let id = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::project_update(
                tx,
                &ProjectSelector::Id(id),
                projects::UpdateProject {
                    description: Some("theirs".into()),
                    author: "@lead".into(),
                    ..Default::default()
                },
                &NoResourceSettings,
            )
        })
        .await
        .unwrap();
    let html = f
        .save_seen(
            "details",
            &seen,
            &[
                ("name", "first"),
                ("description", "mine"),
                ("icon", ""),
                ("fallback", ""),
            ],
        )
        .await;
    assert!(
        html.contains("role=\"alert\">Not saved: the description changed since this page was drawn (by @lead). Reload to see it, then save again.</p>"),
        "{html}"
    );
    assert_eq!(f.project().await.description, "theirs");
    let html = f
        .save_seen(
            "retiring",
            &retiring,
            &[("prune_done_after", "6"), ("prune_keep", "")],
        )
        .await;
    assert!(html.contains("role=\"status\">Saved.</p>"), "{html}");
    assert_eq!(
        request(f.router(), "GET", "/projects/first/settings", String::new())
            .await
            .0,
        308
    );
    f.details(&[("name", "second")]).await;
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
  seed::put(tx,id,json!({"steps":{"s":{"run":"core.external"}}}))?;
  tx.sql().execute("UPDATE steps SET status='running',generation=1,work_generation=1,run_ids=?2,owner='@lead' WHERE project_id=?1 AND step_id='s'",(id.to_string(),json!([run]).to_string()))?;
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
                me: Some("@lead".into()),
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
                        "@lead",
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
    assert!(
        f.details(&[("name", "second")])
            .await
            .contains("role=\"status\">Saved.</p>")
    );
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
                    "@lead",
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
    assert!(
        html.contains("<summary id=\"delete-button\">Delete project</summary>"),
        "{html}"
    );
    let id = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            seed::put(tx, id, json!({"steps":{"s":{"run":"core.external"}}}))?;
            tx.sql().execute(
                "UPDATE steps SET status='running' WHERE project_id=?1 AND step_id='s'",
                [id.to_string()],
            )?;
            tx.changed(Some(id), "status");
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(f.delete(1).await.0, 400);
    assert!(!f.project().await.archived);
    let (_, blocked) = f.get_page().await;
    assert!(
        blocked.contains("<summary id=\"delete-button\" aria-disabled=\"true\">"),
        "{blocked}"
    );
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
    assert!(
        f.details(&[("name", "second")])
            .await
            .contains("role=\"status\">Saved.</p>")
    );
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
        // the page goes home
        while !wire.contains("data-init=\"window.location.assign('/')\"") {
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
    assert!(
        f.save("activity", &[("archived", "true")])
            .await
            .contains("role=\"status\">Saved.</p>")
    );
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
async fn images_are_validated_and_served_by_id_and_generation() {
    let f = Fixture::new().await;
    let bytes = b"\x89PNG\r\n\x1a\nfixture".to_vec();
    // the Details form with an image chosen: multipart, as a browser sends a file
    let upload = |image: Vec<u8>, seen: String| {
        let router = f.router();
        let path = f.path();
        async move {
            let boundary = "----sluiceboundary";
            let mut body = Vec::new();
            for (name, value) in [
                ("section", "details"),
                ("seen", seen.as_str()),
                ("name", "first"),
                ("description", "# Safe description"),
                ("icon", ""),
                ("fallback", ""),
            ] {
                body.extend_from_slice(
                    format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
                );
            }
            body.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"icon_file\"; filename=\"icon.png\"\r\nContent-Type: image/png\r\n\r\n").as_bytes(),
            );
            body.extend_from_slice(&image);
            body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
            let response = router
                .oneshot(
                    Request::post(path)
                        .header(
                            "content-type",
                            format!("multipart/form-data; boundary={boundary}"),
                        )
                        .header("sluice-form", "details-form")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            super::forms::text(response).await
        }
    };
    let html = upload(bytes.clone(), f.seen("details").await).await;
    assert!(html.contains("role=\"status\">Saved.</p>"), "{html}");
    let project = f.project().await;
    let url = project.icon.unwrap().url(f.id).unwrap();
    assert!(html.contains("id=\"icon-preview\""), "{html}");
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
    // letters take the image's place; the image is gone
    assert!(
        f.details(&[("icon", "text")])
            .await
            .contains("role=\"status\">Saved.</p>")
    );
    assert_eq!(request(f.router(), "GET", &url, String::new()).await.0, 404);
    let html = upload(b"bad image".to_vec(), f.seen("details").await).await;
    assert!(html.contains("role=\"alert\">"), "{html}");
    assert_eq!(f.project().await.settings_rev, Revision(3));
}

#[tokio::test]
async fn retiring_is_set_in_hours_with_keep_patterns_and_turned_off_when_empty() {
    let f = Fixture::new().await;
    let (status, html) = f.get_page().await;
    assert_eq!(status, 200);
    assert!(html.contains("Retire done units after"));
    // off is the tag alone: no sentence explains it
    assert!(html.contains(">Off</span>") && !html.contains("Done units stay"));
    let retire = |after: &'static str, keep: &'static str| {
        let f = &f;
        async move {
            f.save(
                "retiring",
                &[("prune_done_after", after), ("prune_keep", keep)],
            )
            .await
        }
    };
    let html = retire("6", "").await;
    assert!(html.contains("role=\"status\">Saved.</p>"), "{html}");
    assert_eq!(f.project().await.prune_done_after, Some(21600));
    assert!(
        html.contains("id=\"prune-done-after\" name=\"prune_done_after\""),
        "{html}"
    );
    let (_, page) = f.get_page().await;
    assert!(page.contains("Done units retire after 6h"));
    let html = retire("6", "ta-*, fig-? release").await;
    assert!(html.contains("role=\"status\">Saved.</p>"), "{html}");
    assert_eq!(f.project().await.prune_keep, ["ta-*", "fig-?", "release"]);
    assert!(html.contains("value=\"ta-*, fig-?, release\""), "{html}");
    let (_, page) = f.get_page().await;
    assert!(page.contains("except 3 patterns"));
    assert!(
        retire("1.5", "ta-*, fig-?, release")
            .await
            .contains("role=\"status\">Saved.</p>")
    );
    assert_eq!(f.project().await.prune_done_after, Some(5400));
    // Not a number, zero or negative is refused, the form as typed
    for bad in ["soon", "0", "-2"] {
        let html = retire(bad, "ta-*, fig-?, release").await;
        assert!(
            html.contains("role=\"alert\">Enter a number of hours"),
            "{html}"
        );
    }
    // Empty turns retiring off; empty patterns clear them.
    assert!(
        retire("", "ta-*, fig-?, release")
            .await
            .contains("role=\"status\">Saved.</p>")
    );
    assert!(retire("", " ").await.contains("role=\"status\">Saved.</p>"));
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

/// The fallback is a field like any other: a select of the project's registered sessions and
/// "none", applied in place and recorded as the owner's; Sessions lists each registered session
/// with when it was last seen (its heartbeat or its last message), and an unregistered name or
/// the old shared address is never offered.
#[tokio::test]
async fn the_fallback_is_chosen_from_the_projects_sessions_which_are_listed_with_when_each_was_seen()
 {
    let f = Fixture::new().await;
    let (_, page) = f.get_page().await;
    assert!(
        page.contains("<dl id=\"sessions\" class=\"settings-fields\" hidden>"),
        "{page}"
    );
    let id = f.id;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            for (who, beat) in [
                ("@lash-f4", Some("2026-10-13T10:00:00Z")),
                ("@figments", None),
                ("orchestrator", Some("2026-10-13T10:00:00Z")),
            ] {
                tx.sql().execute(
                    "INSERT INTO readers(project_id,identity,stream,thread,cursor,heartbeat_at) VALUES (?1,?2,'orchestrator','',0,?3)",
                    (id.to_string(), who, beat),
                )?;
            }
            tx.sql().execute(
                "INSERT INTO messages(id,project_id,thread,\"from\",\"to\",body,at) VALUES (900000,?1,'@figments','@figments','owner','Maps redrawn.','2026-10-13T11:00:00Z')",
                [id.to_string()],
            )?;
            tx.changed(Some(id), "readers");
            Ok(())
        })
        .await
        .unwrap();
    let (_, page) = f.get_page().await;
    let select = page
        .split("<select id=\"project-fallback\"")
        .nth(1)
        .and_then(|s| s.split("</select>").next())
        .unwrap();
    assert_eq!(
        select.matches("<option").count(),
        3,
        "none and the two sessions: {select}"
    );
    assert!(
        select.contains("<option value=\"\" selected>none</option>"),
        "{select}"
    );
    assert!(
        select.contains("<option value=\"@figments\">@figments</option>"),
        "{select}"
    );
    assert!(!select.contains("orchestrator"), "{select}");
    let sessions = page
        .split("<!--sessions-->")
        .nth(1)
        .and_then(|s| s.split("<!--/sessions-->").next())
        .unwrap();
    assert!(!sessions.contains("hidden"), "{sessions}");
    assert!(
        sessions.contains("<span class=\"own\">@figments</span> · seen <time")
            && sessions.contains("datetime=\"2026-10-13T11:00:00Z\""),
        "{sessions}"
    );
    assert!(
        sessions.contains("<span class=\"own\">@lash-f4</span> · seen <time")
            && sessions.contains("datetime=\"2026-10-13T10:00:00Z\""),
        "{sessions}"
    );
    assert!(!sessions.contains("orchestrator"), "{sessions}");

    assert!(
        f.details(&[("fallback", "@lash-f4")])
            .await
            .contains("role=\"status\">Saved.</p>")
    );
    let fallback = |f: &Fixture| {
        let id = f.id;
        let reads = f.state.dashboard.reads.clone();
        async move {
            reads
                .snapshot(move |c| sluice_store::identities::fallback(c, id))
                .await
                .unwrap()
        }
    };
    assert_eq!(fallback(&f).await.as_deref(), Some("@lash-f4"));
    let (_, page) = f.get_page().await;
    assert!(
        page.contains("<option value=\"@lash-f4\" selected>@lash-f4</option>"),
        "{page}"
    );
    let record = f.records().await.pop().unwrap();
    assert_eq!(record["fields"], json!(["fallback"]), "{record}");
    assert_eq!(record["author"], "owner", "{record}");
    // a name no session registered is refused, and nothing changes
    let body = f.details(&[("fallback", "@laed")]).await;
    assert!(
        body.contains("role=\"alert\">")
            && body.contains("@laed is not a session of project first"),
        "{body}"
    );
    assert_eq!(fallback(&f).await.as_deref(), Some("@lash-f4"));
    // none clears it
    assert!(
        f.details(&[("fallback", "")])
            .await
            .contains("role=\"status\">Saved.</p>")
    );
    assert_eq!(fallback(&f).await, None);
}
