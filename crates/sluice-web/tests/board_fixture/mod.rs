//! The seeded home the board tests draw: a project `lanes` with two units, a failed step, an
//! output and a board using every data component; a project `plain` without a board.
#![allow(dead_code)]
use crate::seed;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use sluice_model::{
    commands::{CommandReply, CommandRequest, Delivery, MessageReceipt},
    error::PublicError,
    ids::{MessageId, ProjectId, ProjectSelector, Revision},
    plan::{FnSignature, SignatureProvider},
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings, StoredWorkOnly},
};
use sluice_web::{
    settings::{SettingsState, StoreCommands},
    views::{
        DashboardState, EmptyCatalog, PageState, board,
        inbox::{CommandFuture, MessageCommands, MessageState},
        page_router,
    },
};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

pub struct Registry;
impl SignatureProvider for Registry {
    fn signature(&self, name: &str) -> Option<FnSignature> {
        matches!(name, "custom.open" | "core.external").then(|| FnSignature {
            open: true,
            ..Default::default()
        })
    }
}
impl board::RegistrySource for Registry {
    fn signatures(&self, _: ProjectId) -> Result<board::RegistrySnapshot, PublicError> {
        Ok(board::RegistrySnapshot {
            version: "fixture".into(),
            functions: ["custom.open", "core.external"]
                .into_iter()
                .map(|n| (n.into(), self.signature(n).unwrap()))
                .collect(),
        })
    }
}
/// Records what the dashboard asks the coordinator, and answers a say with a receipt.
#[derive(Default)]
pub struct Commands(pub Mutex<Vec<CommandRequest>>);
impl MessageCommands for Commands {
    fn command(&self, request: CommandRequest) -> CommandFuture<'_> {
        self.0.lock().unwrap().push(request);
        Box::pin(async {
            Ok(CommandReply::Receipt(MessageReceipt {
                id: MessageId(9),
                to: "orchestrator".into(),
                thread: "owner".into(),
                delivery: Delivery::Delivered,
                run: None,
            }))
        })
    }
}

pub const BOARD: &str = r#"root = Stack([title, doc, lanes, row, failed, summary, table, bars, line, broken, form])
title = Heading("Lanes", 1)
doc = Doc("Nothing written yet.")
lanes = Units()
row = Stack([steps, done, failing], "row")
steps = Metric("Steps", "SELECT count(*) FROM steps WHERE project_id = ?")
done = Metric("Succeeded", "SELECT count(*) FROM steps WHERE project_id = ?1 AND status = 'succeeded'")
failing = Count("Failed", "failed")
failed = StepStatus("beta-build")
summary = Output("alpha-build", "summary")
table = Query("SELECT step_id, status FROM steps WHERE project_id = ? ORDER BY position", "Every step")
bars = Chart("bar", "SELECT status, count(*) FROM steps WHERE project_id = ? GROUP BY status ORDER BY status", "Steps by status")
line = Chart("line", "SELECT position, position * 2 FROM steps WHERE project_id = ? ORDER BY position")
broken = Query("SELECT missing FROM nowhere")
form = Form("lane", [lane, note], [go, skip])
lane = Input("lane", "Lane", null, null, "beta", ["required"])
note = Textarea("note", "Note", null, null, ["minLength:3"])
go = Button("Retry the lane", "retry_lane", {force: true})
skip = Button("Skip", "skip", null, "secondary")
"#;

/// The lanes project's description: a lead paragraph, then more the page folds.
pub const DESCRIPTION: &str = "Lanes: two units of <work>.\n\nHow work is done here:\n\n- one lane per unit\n- reviews follow builds";

pub struct Fixture {
    pub _home: tempfile::TempDir,
    pub writer: Writer,
    pub id: ProjectId,
    pub plain: ProjectId,
    pub dashboard: DashboardState,
    pub commands: Arc<Commands>,
}
impl Fixture {
    pub async fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let writer = Writer::open(home.path()).unwrap();
        let create = |name: &'static str, description: &'static str| {
            let writer = writer.clone();
            async move {
                writer
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
                    .unwrap()
                    .project_id
            }
        };
        let id = create("lanes", DESCRIPTION).await;
        let plain = create("plain", "").await;
        writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                let doc = json!({"steps":{
                    "alpha-build":{"run":"custom.open","outputs":{"summary":"string"},"tags":["unit:alpha"]},
                    "alpha-review":{"run":"custom.open","in":{"s":{"source":"alpha-build/summary"}},"tags":["unit:alpha"]},
                    "beta-build":{"run":"custom.open","after":["alpha-build"],"tags":["unit:beta"]},
                    "beta-review":{"run":"custom.open","doc":"Check the Parser output","after":["beta-build","alpha-review"],"tags":["unit:beta"]}}});
                seed::put(tx, id, doc)?;
                for (step, status, outputs) in [
                    ("alpha-build", "succeeded", json!({"summary": "Built <12> crates"})),
                    ("beta-build", "failed", json!({})),
                ] {
                    tx.sql().execute(
                        "UPDATE steps SET status=?3,outputs=?4 WHERE project_id=?1 AND step_id=?2",
                        (id.to_string(), step, status, outputs.to_string()),
                    )?;
                }
                let error = PublicError::BadRequest {
                    message: "tests failed".into(),
                };
                tx.sql().execute(
                    "UPDATE steps SET error=?2 WHERE project_id=?1 AND step_id='beta-build'",
                    (id.to_string(), serde_json::to_string(&error).unwrap()),
                )?;
                projects::board_set(
                    tx,
                    &ProjectSelector::Id(id),
                    projects::SetBoard {
                        program: Some(BOARD.into()),
                        expected_rev: Some(Revision(0)),
                        reason: None,
                        author: "orch".into(),
                    },
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let dashboard = DashboardState::new(
            ReadPool::open(home.path(), 2).unwrap(),
            Arc::new(EmptyCatalog),
        );
        Self {
            _home: home,
            writer,
            id,
            plain,
            dashboard,
            commands: Arc::default(),
        }
    }
    /// Another project, `name`, with the plan `doc` and the given steps' statuses.
    pub async fn project(
        &self,
        name: &'static str,
        doc: Value,
        statuses: &[(&'static str, &'static str)],
    ) -> ProjectId {
        let statuses = statuses.to_vec();
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                let id = projects::project_create(
                    tx,
                    CreateProject {
                        name: name.parse().unwrap(),
                        description: String::new(),
                        icon: None,
                        resources: None,
                        author: "owner".into(),
                    },
                    &EmptyPlanInitializer,
                    &NoResourceSettings,
                )?
                .project_id;
                seed::put(tx, id, doc)?;
                for (step, status) in &statuses {
                    tx.sql().execute(
                        "UPDATE steps SET status=?3 WHERE project_id=?1 AND step_id=?2",
                        (id.to_string(), step, status),
                    )?;
                }
                Ok(id)
            })
            .await
            .unwrap()
    }
    pub fn router(&self) -> Router {
        let mut pages = PageState::new(self.dashboard.clone());
        pages.log = true;
        pages.messages = Some(MessageState {
            dashboard: self.dashboard.clone(),
            commands: self.commands.clone(),
        });
        pages.settings = Some(SettingsState::new(
            self.dashboard.clone(),
            Arc::new(StoreCommands {
                writer: self.writer.clone(),
                resources: Arc::new(NoResourceSettings),
                deletion_guard: Arc::new(StoredWorkOnly),
            }),
            Arc::new(StoredWorkOnly),
        ));
        page_router(pages).layer(axum::Extension(board::Registry(Arc::new(Registry))))
    }
    pub async fn get(&self, path: &str) -> (StatusCode, String) {
        let response = self
            .router()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }
    pub async fn post(&self, path: &str, form: &[(&str, &str)]) -> (StatusCode, Value) {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        let response = self
            .router()
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("accept", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice(&body).unwrap_or_else(|_| json!(String::from_utf8_lossy(&body))),
        )
    }
}

/// The `lane` recipe the matrix tests write: three stages, a title from the ticket and the
/// spec file's heading, and a view of the ticket and the unit's last message.
pub fn lane_recipe() -> Value {
    json!({"name":"lane","params":{"ticket":"string","spec":"string"},
        "title":"{ticket}: {spec}",
        "view":"root = Stack([Param(\"ticket\"), LastMessage(80)], \"row\")",
        "steps":{
            "{unit}-fork":{"run":"custom.open"},
            "{unit}-work":{"run":"custom.open","in":{"ticket":{"default":"{ticket}"},"spec":{"file":"{spec}"}},"after":["{unit}-fork"]},
            "{unit}-land":{"run":"custom.open","after":["{unit}-work"]}}})
}
impl Fixture {
    /// A project `titled` whose recipe `lane` has a view: lanes l1 (running), l2 (failed) and
    /// l3 (waiting, its fork after l2's land and its land after `probe`, a running step of its
    /// own), a unit `report` after l1's land, a unit `kit` of two steps (a box), a step with a
    /// doc, one with a literal prompt and one with neither; and the recipe `rough`, whose view
    /// does not check, with its unit r1.
    pub async fn titled(&self) -> ProjectId {
        let specs = self._home.path().join("specs");
        std::fs::create_dir_all(&specs).unwrap();
        let mut steps = serde_json::Map::new();
        for (unit, ticket, heading, extra_after) in [
            ("l1", "FIG-1", "Fix the cron driver", None),
            ("l2", "FIG-2", "Stop the parser leak", None),
            ("l3", "FIG-3", "Ship the docs", Some("l2-land")),
        ] {
            let spec = specs.join(format!("{unit}.md"));
            std::fs::write(&spec, format!("Intro line\n\n# {heading}\n\nThe body.")).unwrap();
            let tags = json!([format!("unit:{unit}")]);
            let mut fork = json!({"run":"custom.open","tags":tags});
            if let Some(after) = extra_after {
                fork["after"] = json!([after]);
            }
            steps.insert(format!("{unit}-fork"), fork);
            steps.insert(
                format!("{unit}-work"),
                json!({"run":"custom.open","tags":tags,"after":[format!("{unit}-fork")],
                "in":{"ticket":{"default":ticket},"spec":{"file":spec.to_str().unwrap()}}}),
            );
            let mut land = vec![format!("{unit}-work")];
            if unit == "l3" {
                land.push("probe".into());
            }
            steps.insert(
                format!("{unit}-land"),
                json!({"run":"custom.open","tags":tags,"after":land}),
            );
        }
        steps.insert(
            "report".into(),
            json!({"run":"custom.open","after":["l1-land"]}),
        );
        steps.insert(
            "watch".into(),
            json!({"run":"custom.open","doc":"Watches main for red\n\nIt says so on the thread."}),
        );
        steps.insert(
            "bare".into(),
            json!({"run":"custom.open","in":{"spec":{"default":"# Bare heading\nbody"}}}),
        );
        steps.insert("plain".into(), json!({"run":"custom.open"}));
        steps.insert(
            "probe".into(),
            json!({"run":"custom.open","doc":"Probes the parser under load"}),
        );
        // a unit of two steps that is no recipe's: a box, its line inside it drawn
        steps.insert(
            "kit-a".into(),
            json!({"run":"custom.open","tags":["unit:kit"]}),
        );
        steps.insert(
            "kit-b".into(),
            json!({"run":"custom.open","tags":["unit:kit"],"after":["kit-a"]}),
        );
        steps.insert(
            "r1-only".into(),
            json!({"run":"custom.open","tags":["unit:r1"]}),
        );
        let id = self
            .project(
                "titled",
                json!({"steps": steps}),
                &[
                    ("l1-fork", "succeeded"),
                    ("l1-work", "running"),
                    ("l2-fork", "succeeded"),
                    ("l2-work", "failed"),
                    ("watch", "running"),
                    ("probe", "running"),
                    ("kit-a", "running"),
                ],
            )
            .await;
        let recipes = self
            ._home
            .path()
            .join("projects")
            .join(id.to_string())
            .join("recipes");
        std::fs::create_dir_all(&recipes).unwrap();
        std::fs::write(
            recipes.join("lane.json"),
            serde_json::to_vec(&lane_recipe()).unwrap(),
        )
        .unwrap();
        std::fs::write(
            recipes.join("rough.json"),
            serde_json::to_vec(
                &json!({"name":"rough","view":"root = Stack([Output(\"nope\", \"x\")])",
                "steps":{"{unit}-only":{"run":"custom.open"}}}),
            )
            .unwrap(),
        )
        .unwrap();
        id
    }
}
