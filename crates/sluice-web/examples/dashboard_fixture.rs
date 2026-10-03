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
