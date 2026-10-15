use sluice_model::{error::PublicError, ids::ProjectId};
use sluice_store::{
    RetrySafety, Writer,
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use sluice_web::views::{PageState, board, step};
use std::sync::{Arc, OnceLock};
/// The owner's set-aside writes (Dismiss and Undo on a cancel, × and Undo on any item of Needs
/// you), put straight through the store, so they work in review; every other action is refused
/// as the fixture has no runner.
struct FixtureCommands;
static WRITER: OnceLock<Writer> = OnceLock::new();
impl step::CommandService for FixtureCommands {
    fn execute(
        &self,
        command: step::OwnerCommand,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<(), PublicError>> + Send + '_>> {
        Box::pin(async move {
            let writer = WRITER.get().cloned().ok_or_else(|| PublicError::Rejected {
                message: "the fixture's store is not open".into(),
            })?;
            writer
                .write(RetrySafety::Idempotent, move |tx| match (&command.action, command.step) {
                    (step::Action::Dismiss | step::Action::Undismiss, Some(step)) => {
                        sluice_store::messages::dismiss(
                            tx,
                            sluice_model::commands::StepDismiss {
                                project: command.project,
                                step,
                                dismissed: command.action == step::Action::Dismiss,
                            },
                        )
                    }
                    (step::Action::SetAside | step::Action::Restore, _) => {
                        sluice_store::messages::needs_dismiss(
                            tx,
                            sluice_model::commands::NeedsDismiss {
                                project: command.project,
                                items: command.items,
                                dismissed: command.action == step::Action::SetAside,
                            },
                        )
                    }
                    _ => Err(PublicError::Rejected {
                        message: "the fixture has no runner".into(),
                    }
                    .into()),
                })
                .await
        })
    }
}
/// The board-fixture's board: a heading, its document and the failed build's last word.
const BOARD: &str = r#"root = Stack([title, doc, latest])
title = Heading("Release", 1)
doc = Doc("Nothing written yet.")
latest = LatestMessage("step-failed")
"#;
/// What the orchestrator wrote on that board.
const BOARD_DOC: &str = "## Where the release stands\n\nThe build failed at the review gate, so **review** and **delivery** wait on it. Retry the build once the review gate's fixture is fixed.\n\n- review: blocked behind the build\n- delivery: blocked at the unit gate\n- the cached cpu run: queued for capacity\n\nThe patch itself is verified; only the gate stands between it and delivery.";
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
pub async fn seed(writer: &Writer, _id: ProjectId) {
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
        crate::seed::put(tx, project, doc)?;
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
    // its board, with a document of its own, so ?view=board and ?view=both have one to show
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let selector = sluice_model::ids::ProjectSelector::Id(project);
            projects::board_set(
                tx,
                &selector,
                projects::SetBoard {
                    program: Some(BOARD.into()),
                    expected_rev: None,
                    reason: None,
                    author: "orch".into(),
                },
            )?;
            projects::board_doc_write(
                tx,
                &selector,
                projects::WriteBoardDoc {
                    markdown: BOARD_DOC.into(),
                    expected_rev: None,
                    reason: None,
                    author: "orch".into(),
                },
            )
        })
        .await
        .unwrap();
    println!("BOARD {project}");
    let paused = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "board-paused".parse().unwrap(),
                    description: "Work is held for review.".into(),
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
            let doc = json!({"steps":{
            "held":{"run":"custom.open","outputs":{"ok":"boolean"},"tags":["unit:build"]},
            "review":{"run":"custom.open","after":["held"],"tags":["unit:review"]},
            "deliver":{"run":"custom.open","after":["review"],"tags":["unit:review"]}}});
            crate::seed::put(tx, paused, doc)?;
            tx.sql().execute(
                "UPDATE projects SET paused=1 WHERE project_id=?1",
                [paused.to_string()],
            )?;
            tx.changed(Some(paused), "project");
            Ok(())
        })
        .await
        .unwrap();
    println!("BOARD-PAUSED {paused}");
}

pub async fn configure(_state: &mut PageState, writer: &Writer, _id: ProjectId) {
    let _ = WRITER.set(writer.clone());
}
pub fn layer(router: axum::Router) -> axum::Router {
    router
        .layer(axum::Extension(board::Registry(Arc::new(
            BoardFixtureRegistry,
        ))))
        .layer(axum::Extension(step::Commands(Arc::new(FixtureCommands))))
}
