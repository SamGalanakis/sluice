use sluice_model::{error::PublicError, ids::ProjectId};
use sluice_store::{
    RetrySafety, Writer,
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use sluice_web::views::*;
pub struct FixtureCatalog;
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

pub async fn create(writer: &Writer) -> ProjectId {
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
            let statuses = ["succeeded","succeeded",if index==0 {"running"} else {"failed"},"pending"];
            let steps: serde_json::Map<String, serde_json::Value> = statuses.iter().enumerate().map(|(n,status)| (format!("work-{n}"), serde_json::json!({"run":"core.external","doc":if *status=="running" {"Build the shared dashboard"} else {"Review and validate"}}))).collect();
            crate::seed::put(tx, project.project_id, serde_json::json!({"steps": steps}))?;
            let c=tx.sql();
            for (n,status) in statuses.into_iter().enumerate(){
                c.execute("UPDATE steps SET status=?3 WHERE project_id=?1 AND step_id=?2",(project.project_id.to_string(),format!("work-{n}"),status))?;
            }
            tx.changed(Some(project.project_id), "project");
            Ok(())
        }).await.unwrap();
        if index == 0 {
            first = Some(project.project_id);
        }
        println!("PROJECT {} {}", project.project_id, name);
    }
    first.unwrap()
}
pub async fn seed(_writer: &Writer, _id: ProjectId) {}
pub async fn configure(_state: &mut PageState, _writer: &Writer, _id: ProjectId) {}
pub fn extra_router(writer: Writer, id: ProjectId) -> axum::Router {
    axum::Router::new().route(
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
    )
}

pub fn layer(router: axum::Router) -> axum::Router {
    router
}
