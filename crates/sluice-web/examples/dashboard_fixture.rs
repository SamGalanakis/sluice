//! Test-only router used by the shared-layout Chromium gate. Owns a temporary
//! home and never starts a scheduler or coordinator payload.
use sluice_model::commands::{CommandReply, CommandRequest, MessagePost};
use sluice_model::{error::PublicError, ids::ProjectId};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use sluice_web::views::inbox::{CommandFuture, MessageCommands, MessageState};
use sluice_web::views::*;
use std::sync::Arc;
struct FixtureResources;
impl sluice_model::plan::SignatureProvider for FixtureResources {
    fn signature(&self, name: &str) -> Option<sluice_model::plan::FnSignature> {
        (name == "fixture.capacity").then(|| sluice_model::plan::FnSignature {
            outputs: [("capacity".into(), sluice_model::types::Type::Int)]
                .into_iter()
                .collect(),
            ..Default::default()
        })
    }
}
impl projects::ResourceSettings for FixtureResources {
    fn set_resources(
        &self,
        tx: &mut sluice_store::WriteTransaction<'_>,
        project: ProjectId,
        patch: &serde_json::Value,
    ) -> sluice_store::Result<bool> {
        sluice_store::resources::patch_resources(tx, project, patch, self)
    }
}
struct FixtureCommands(Writer);
impl MessageCommands for FixtureCommands {
    fn command(&self, request: CommandRequest) -> CommandFuture<'_> {
        let writer = self.0.clone();
        Box::pin(async move {
            writer
                .write(RetrySafety::NonIdempotent, move |tx| match request {
                    CommandRequest::MessagePost(post) => Ok(CommandReply::Posted {
                        id: sluice_store::messages::message_post(
                            tx,
                            post,
                            &sluice_store::messages::NoPlanInputs,
                        )?
                        .id,
                    }),
                    CommandRequest::MarkRead(read) => {
                        sluice_store::messages::mark_read(tx, read)?;
                        Ok(CommandReply::Ack)
                    }
                    _ => Err(PublicError::BadRequest {
                        message: "fixture only supports message commands".into(),
                    }
                    .into()),
                })
                .await
        })
    }
}
struct FixtureCatalog;
impl CatalogSource for FixtureCatalog {
    fn catalog(&self, project: Option<ProjectId>) -> Result<FunctionCatalog, PublicError> {
        let mut entries = vec![];
        for (scope, name, doc) in [
            ("builtin", "core.external", "Work performed outside sluice."),
            (
                "builtin",
                "core.value",
                "Pass a typed value to the next step.",
            ),
            (
                "global",
                "agents.codex",
                "Start or continue an agent session.",
            ),
            (
                "global",
                "git.land",
                "Land reviewed work with a checked commit.",
            ),
            (
                "global",
                "broken.function",
                "This function needs attention.",
            ),
        ] {
            entries.push(FunctionView {
                name: name.into(),
                scope: scope.into(),
                doc: doc.into(),
                inputs: vec![PortView {
                    name: "prompt".into(),
                    ty: "string?".into(),
                }],
                outputs: vec![PortView {
                    name: "result".into(),
                    ty: "{ready: boolean, summary: string}".into(),
                }],
                error: if name == "broken.function" {
                    "Collision: two definitions have the same name.".into()
                } else {
                    String::new()
                },
            });
        }
        if project.is_some() {
            entries.push(FunctionView {
                name: "project.review".into(),
                scope: "project".into(),
                doc: "Review this project's changes.".into(),
                inputs: vec![],
                outputs: vec![],
                error: String::new(),
            });
        }
        Ok(FunctionCatalog {
            version: "fixture-1".into(),
            entries,
        })
    }
}
struct BoardFixtureRegistry;
impl sluice_model::plan::SignatureProvider for BoardFixtureRegistry {
    fn signature(&self, name: &str) -> Option<sluice_model::plan::FnSignature> {
        match name {
            "custom.open" | "core.external" => Some(sluice_model::plan::FnSignature {
                open: true,
                ..Default::default()
            }),
            _ => None,
        }
    }
}
impl board::RegistrySource for BoardFixtureRegistry {
    fn signatures(&self, _: ProjectId) -> Result<board::RegistrySnapshot, PublicError> {
        use sluice_model::plan::SignatureProvider;
        Ok(board::RegistrySnapshot {
            version: "board-fixture-1".into(),
            functions: ["custom.open", "core.external"]
                .into_iter()
                .map(|n| (n.into(), self.signature(n).unwrap()))
                .collect(),
        })
    }
}
async fn board_fixture(writer: &Writer) {
    use serde_json::json;
    let project = writer.write(RetrySafety::NonIdempotent, |tx| projects::project_create(tx, CreateProject { name: "board-fixture".parse().unwrap(), description: "A plan of tagged units, with queued work and every v2 relation.\n\nOpen a card to inspect inputs, outputs, its thread and runs.".into(), icon: None, resources: None, author: "owner".into() }, &EmptyPlanInitializer, &NoResourceSettings)).await.unwrap().project_id;
    writer.write(RetrySafety::NonIdempotent, move |tx| {
        let doc = json!({"inputs":{"enabled":{"type":"boolean","doc":"Allow the final review."}},"steps":{
            "source":{"run":"custom.open","outputs":{"ok":"boolean","text":"string"},"tags":["unit:build"]},
            "failed":{"run":"custom.open","doc":"The build failed. Inspect the error and retry with feedback.","in":{"prompt":{"default":"Review the patch and explain the failure.\nKeep the original output and check the source."}},"outputs":{"summary":"string"},"tags":["unit:build","exit"]},
            "handoff":{"run":"custom.open","in":{"data":{"source":"source/text"}},"tags":["unit:review"]},
            "order":{"run":"custom.open","after":["failed"],"tags":["unit:review"]},
            "yes":{"run":"custom.open","after":["source/ok"],"tags":["unit:review"]},
            "no":{"run":"custom.open","after":["!source/ok"],"tags":["unit:review"]},
            "cleanup":{"run":"custom.open","after":["no?"],"tags":["unit:review"]},
            "unit-gate":{"run":"custom.open","after":["unit:build"],"tags":["unit:delivery"]},
            "unit-tolerant":{"run":"custom.open","after":["unit:finished?"],"tags":["unit:delivery"]},
            "enabled-work":{"run":"custom.open","after":["enabled"],"tags":["unit:delivery"]},
            "queued":{"run":"custom.open","doc":"Queued for the cached cpu capacity.","needs":{"cpu":1}},
            "done":{"run":"custom.open","tags":["unit:finished"]},
            "skipped":{"run":"custom.open","tags":["unit:finished"]}},"outputs":{"result":{"source":"source/text"}}});
        let plan = sluice_model::plan::Plan::parse_json(&serde_json::to_vec(&doc).unwrap(), &BoardFixtureRegistry).unwrap();
        tx.sql().execute("DELETE FROM plans WHERE project_id=?1",[project.to_string()])?;
        sluice_store::plans::initialize_plan(tx,project,&plan)?;
        for (id,status,outputs) in [("source","succeeded",json!({"ok":true,"text":"A verified patch"})),("failed","failed",json!({"summary":"Preserved output from the previous attempt."})),("no","skipped",json!({})),("done","succeeded",json!({})),("skipped","skipped",json!({}))] {
            tx.sql().execute("UPDATE steps SET status=?3,outputs=?4 WHERE project_id=?1 AND step_id=?2",(project.to_string(),id,status,outputs.to_string()))?;
        }
        let error=PublicError::BadRequest { message: "Build failed at the review gate.\nThe expected summary was missing. <untrusted error>".into() };
        tx.sql().execute("UPDATE steps SET error=?2 WHERE project_id=?1 AND step_id='failed'",(project.to_string(),serde_json::to_string(&error).unwrap()))?;
        tx.sql().execute("INSERT INTO resources(scope,project_id,name,declaration,capacity) VALUES(?1,?1,'cpu','0',0)",[project.to_string()])?;
        let run=sluice_model::ids::RunId::new();let attempt=sluice_model::ids::AttemptId::new();
        tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,phase,request,inputs_hash,created_at,finished_at) VALUES(?1,?2,'failed','terminal',?3,'fixture','2026-10-03T12:00:00Z','2026-10-03T12:10:00Z')",(attempt.to_string(),project.to_string(),json!({"inputs":{"prompt":"Frozen prompt for this failed attempt. The drawer should use this value."}}).to_string()))?;
        tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,created_at,started_at,finished_at,result) VALUES(?1,?2,?3,'failed','2026-10-03T12:00:00Z','2026-10-03T12:00:00Z','2026-10-03T12:10:00Z',?4)",(run.to_string(),project.to_string(),attempt.to_string(),json!({"status":"failed"}).to_string()))?;
        tx.sql().execute("INSERT INTO sessions(run_id,project_id,engine,cwd,session_id,recorded_at) VALUES(?1,?2,'codex','/scratch','fixture-session','2026-10-03T12:00:00Z')",(run.to_string(),project.to_string()))?;
        tx.sql().execute("INSERT INTO messages(id,project_id,thread,\"from\",\"to\",body,needs_reply,at) VALUES(90001,?1,'step-failed','failed','orchestrator','Please inspect the failed build.',1,'2026-10-03T12:10:00Z')",[project.to_string()])?;
        Ok(())
    }).await.unwrap();
    println!("BOARD {project}");
}
#[tokio::main]
async fn main() {
    let home = tempfile::tempdir().unwrap();
    let writer = Writer::open(home.path()).unwrap();
    let mut first = None;
    for (index, name, description, archived, paused) in [
        (
            0,
            "sluice",
            "A calm board for supervising agents. The dashboard keeps the work, its running steps and questions in one place.",
            false,
            false,
        ),
        (
            1,
            "a-project-with-a-long-name-that-wraps-on-a-phone",
            "A long description with <script>untrusted content</script>, https://example.org/a/very/long/path and enough prose to check the two-line description measure.\n\nA second paragraph stays out of the index.",
            false,
            true,
        ),
        (
            2,
            "archived-work",
            "Finished work remains available in the archived list.",
            true,
            false,
        ),
    ] {
        let project = writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                projects::project_create(
                    tx,
                    CreateProject {
                        name: name.parse().unwrap(),
                        description: description.into(),
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
        writer.write(RetrySafety::NonIdempotent,move |tx| {
            let c=tx.sql();
            c.execute("UPDATE projects SET archived=?1,paused=?2 WHERE project_id=?3",(archived,paused,project.project_id.to_string()))?;
            for (n,status) in ["succeeded","succeeded",if index==0 {"running"} else {"failed"},"pending"].into_iter().enumerate(){
                c.execute("INSERT INTO steps(project_id,step_id,position,declaration,status) VALUES(?1,?2,?3,?4,?5)",(project.project_id.to_string(),format!("work-{n}"),n as i64,format!(r#"{{"run":"core.external","doc":"{}"}}"#,if status=="running" {"Build the shared dashboard"} else {"Review and validate"}),status))?;
            }
            tx.changed(Some(project.project_id), "project");
            Ok(())
        }).await.unwrap();
        if index == 0 {
            first = Some(project.project_id);
        }
        println!("PROJECT {} {}", project.project_id, name);
    }
    let fixture_project = first.unwrap();
    let form = r#"root = Stack([Heading("Choose the release limit"), Text("The number is sent as a JSON number."), Form("limit", [Input("value", "Limit", "1 to 10", "number", "3", ["required", "min:1", "max:10"]), Checkbox("confirmed", "I checked the evidence", false)], [Button("Apply", "submit"), Button("Close", "close", {}, "secondary")])])"#;
    for (index, thread, from, to, title, body, needs_reply, ui) in [
        (
            0,
            "release",
            "work-2",
            "owner",
            "Approve the release",
            "The checks passed. **Choose a limit** before continuing.",
            true,
            Some(form),
        ),
        (
            1,
            "stale",
            "work-2",
            "owner",
            "A stale approval",
            "A second browser can answer this question while this form is open.",
            true,
            Some("root = Stack([Button(\"Approve\", \"approve\")])"),
        ),
        (
            2,
            "step-work-2",
            "work-2",
            "orchestrator",
            "Where should this run?",
            "The worker needs a directory, and this question belongs in Questions.",
            true,
            None,
        ),
        (
            3,
            "decision",
            "orchestrator",
            "owner",
            "The selected approach",
            "We kept the small typed command boundary. The alternative was a web-owned writer. The transaction tests support the choice; the adapter can be replaced without changing the pages.",
            false,
            None,
        ),
        (
            4,
            "long-conversation",
            "owner",
            "orchestrator",
            "Review the evidence",
            "Please record the decision and the test evidence here.",
            false,
            None,
        ),
    ] {
        let post: MessagePost = serde_json::from_value(serde_json::json!({"project":{"kind":"id","value":fixture_project.to_string()},"thread":thread,"from":from,"to":to,"title":title,"body":body,"needs_reply":needs_reply,"ui":ui})).unwrap();
        let message = writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                sluice_store::messages::message_post(
                    tx,
                    post,
                    &sluice_store::messages::NoPlanInputs,
                )
            })
            .await
            .unwrap();
        println!("MESSAGE {index} {}", message.id.0);
    }
    for n in 0..16 {
        let post: MessagePost = serde_json::from_value(serde_json::json!({"project":{"kind":"id","value":fixture_project.to_string()},"thread":"long-conversation","from":if n%2==0 {"work-2"} else {"orchestrator"},"to":if n%2==0 {"orchestrator"} else {"owner"},"body":format!("### Evidence {}\n\nA paragraph explaining the result and its consequences. The test checked the rendered thread and preserved questions in the other conversation.\n\n```text\n{}\n```", n+1, "long-path/".repeat(14)),"needs_reply":false})).unwrap();
        writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                sluice_store::messages::message_post(
                    tx,
                    post,
                    &sluice_store::messages::NoPlanInputs,
                )
            })
            .await
            .unwrap();
    }
    board_fixture(&writer).await;
    let state = DashboardState::new(
        ReadPool::open(home.path(), 2).unwrap(),
        Arc::new(FixtureCatalog),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    println!("URL http://{}", listener.local_addr().unwrap());
    let id = first.unwrap();
    writer.write(RetrySafety::NonIdempotent, move |tx| {
        projects::project_update(tx, &sluice_model::ids::ProjectSelector::Id(id), projects::UpdateProject {
            resources:Some(serde_json::json!({"workers":3,"dynamic":{"capacity_fn":"fixture.capacity"}})),
            author:"owner".into(), ..Default::default()
        }, &FixtureResources)?;
        sluice_store::resources::observe_capacity(tx, id, "dynamic", 1, Err(PublicError::FnFailure {message:"Capacity service is unavailable. Last good capacity retained.".into()}))?;
        Ok(())
    }).await.unwrap();
    let guard = Arc::new(projects::StoredWorkOnly);
    let commands = Arc::new(sluice_web::settings::StoreCommands {
        writer: writer.clone(),
        resources: Arc::new(FixtureResources),
        deletion_guard: guard.clone(),
    });
    let settings = sluice_web::settings::SettingsState::new(state.clone(), commands, guard);
    let router = dashboard_router(state.clone())
        .merge(inbox::router(MessageState {
            dashboard: state.clone(),
            commands: Arc::new(FixtureCommands(writer.clone())),
        }))
        .merge(log::router(state))
        .merge(sluice_web::settings::router(settings))
        .layer(axum::Extension(board::Registry(Arc::new(
            BoardFixtureRegistry,
        ))))
        .route(
            "/fixture/rename",
            axum::routing::post(move || {
                let writer = writer.clone();
                async move {
                    writer
                        .write(RetrySafety::NonIdempotent, move |tx| {
                            projects::project_update(
                                tx,
                                &sluice_model::ids::ProjectSelector::Id(id),
                                projects::UpdateProject {
                                    new_name: Some("renamed-dashboard".parse().unwrap()),
                                    author: "owner".into(),
                                    ..Default::default()
                                },
                                &NoResourceSettings,
                            )
                        })
                        .await
                        .unwrap();
                    axum::http::StatusCode::NO_CONTENT
                }
            }),
        );
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .unwrap();
}
