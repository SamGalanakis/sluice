#[path = "../../../tests/support/home.rs"]
mod home;
#[allow(dead_code)]
#[path = "support/plan_rows.rs"]
mod plan_rows;
#[allow(dead_code)]
mod support {
    use super::home::ScratchHome;
    use rusqlite::params;
    use serde_json::{Value, json};
    use sluice_model::{
        commands::*,
        gates::StateSnapshot,
        ids::*,
        plan::{FnSignature, Plan, SignatureProvider},
        rpc::{JsonMap, decode_json},
        types::Type,
    };
    use sluice_store::{
        attempts::*,
        plans::{self, PlanContext, RetryMessages},
        *,
    };
    pub fn map(value: Value) -> JsonMap {
        decode_json(&serde_json::to_vec(&value).unwrap()).unwrap()
    }
    pub fn id(name: &str) -> StepId {
        name.parse().unwrap()
    }
    struct Signatures;
    impl SignatureProvider for Signatures {
        fn signature(&self, name: &str) -> Option<FnSignature> {
            let (inputs, outputs, open) = match name {
                "echo" => (json!({"value":"Any"}), json!({"value":"Any"}), false),
                "int" => (json!({"value":"int"}), json!({"value":"int"}), false),
                "open" => (json!({}), json!({"report":"string"}), true),
                "empty" => (json!({}), json!({}), false),
                "file" => (json!({"brief":"string"}), json!({"value":"Any"}), false),
                "core.external" => (json!({}), json!({}), true),
                _ => return None,
            };
            Some(FnSignature {
                inputs: inputs
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(n, t)| (n.clone(), Type::parse(t).unwrap()))
                    .collect(),
                outputs: outputs
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(n, t)| (n.clone(), Type::parse(t).unwrap()))
                    .collect(),
                open,
                ..FnSignature::default()
            })
        }
    }
    pub fn plan(doc: Value) -> Plan {
        Plan::parse(&map(doc), &Signatures).unwrap()
    }
    pub struct Fixture {
        pub home: ScratchHome,
        pub writer: Writer,
        pub reads: ReadPool,
        pub context: PlanContext,
    }
    impl Fixture {
        pub async fn new(doc: Value) -> Self {
            let home = ScratchHome::new().unwrap();
            assert!(home.root().exists());
            assert_eq!(ScratchHome::validate(home.path()).unwrap(), home.path());
            let writer = Writer::open(home.path()).unwrap();
            let plan = plan(doc.clone());
            let copy = plan.clone();
            let (project, revision) = writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    let project = super::plan_rows::create_project(tx, "p")?;
                    let rev = super::plan_rows::commit_document(
                        tx,
                        project,
                        &map(doc),
                        Some(&copy),
                        None,
                    )?;
                    Ok((project, rev))
                })
                .await
                .unwrap();
            let reads = ReadPool::open(home.path(), 2).unwrap();
            Self {
                home,
                writer,
                reads,
                context: PlanContext {
                    project,
                    revision,
                    plan,
                },
            }
        }
        pub async fn state(&self) -> StateSnapshot {
            let project = self.context.project;
            self.reads
                .snapshot(move |c| plans::read_state(c, project))
                .await
                .unwrap()
        }
        pub async fn counts(&self) -> (i64, i64, i64, i64) {
            self.reads
                .snapshot(|c| {
                    Ok((
                        c.query_row("SELECT count(*) FROM records", [], |r| r.get(0))?,
                        c.query_row("SELECT count(*) FROM step_results", [], |r| r.get(0))?,
                        c.query_row("SELECT count(*) FROM attempts", [], |r| r.get(0))?,
                        c.query_row("SELECT count(*) FROM messages", [], |r| r.get(0))?,
                    ))
                })
                .await
                .unwrap()
        }
        pub async fn apply(&mut self, doc: Value) {
            let plan = plan(doc.clone());
            let copy = plan.clone();
            let project = self.context.project;
            let revision = self
                .writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    super::plan_rows::commit_document(tx, project, &map(doc), Some(&copy), None)
                })
                .await
                .unwrap();
            self.context.plan = plan;
            self.context.revision = revision;
        }
        pub async fn manual(&self, step: &str, outputs: Value, force: bool) -> ResultId {
            let context = self.context.clone();
            let request = StepSetOutput {
                project: ProjectSelector::Id(context.project),
                step: id(step),
                outputs: map(outputs),
                force,
                reason: "manual".into(),
                author: Some("sam".into()),
            };
            self.writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    plans::step_set_output(tx, &context, request)
                })
                .await
                .unwrap()
        }
        pub async fn retry(&self, steps: &[&str], message: Option<&str>) -> RetryResult {
            let context = self.context.clone();
            let request = retry_request(context.project, steps, message);
            self.writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    plans::step_retry(tx, &context, request, &mut Hooks::default())
                })
                .await
                .unwrap()
        }
        pub async fn reserve(&self, step: &str, inputs: Value) -> Reservation {
            let request = self.request(step, inputs, -1, None).await;
            let context = self.context.clone();
            self.writer
                .write(RetrySafety::Idempotent, move |tx| {
                    reserve(tx, &context, request, &mut Hooks::default())
                })
                .await
                .unwrap()
        }
        pub async fn request(
            &self,
            step: &str,
            inputs: Value,
            index: i64,
            count: Option<u64>,
        ) -> Reserve {
            let state = self.state().await;
            Reserve {
                step: id(step),
                attempt: AttemptId::new(),
                run: RunId::new(),
                item_index: index,
                item_count: count,
                inputs: map(inputs),
                inputs_hash: sluice_model::plan::inputs_hash(
                    &self.context.plan,
                    &state,
                    &self.context.plan.steps()[&id(step)],
                )
                .unwrap(),
                provenance: JsonMap::default(),
                release_id: "fixture".into(),
                protocol_major: 1,
            }
        }
        pub async fn start(&self, reservation: &Reservation) {
            let identity = reservation.identity.clone();
            self.writer
                .write(RetrySafety::Idempotent, move |tx| {
                    assert!(claim(tx, &identity, guardian(identity.run))?);
                    assert!(started(tx, &identity, &mut Hooks::default())?);
                    Ok(())
                })
                .await
                .unwrap()
        }
        pub async fn finish(
            &self,
            reservation: &Reservation,
            kind: CompletionKind,
            outputs: Value,
        ) -> CompletionResult {
            let context = self.context.clone();
            let identity = reservation.identity.clone();
            self.writer
                .write(RetrySafety::Idempotent, move |tx| {
                    complete(
                        tx,
                        &context,
                        Complete {
                            completion_id: identity.run.to_string(),
                            identity,
                            kind,
                            outputs: map(outputs),
                            processes_gone: true,
                            submission_version: None,
                        },
                        &mut Hooks::default(),
                    )
                })
                .await
                .unwrap()
                .unwrap()
        }
        pub async fn fail_projection(&self, step: &str) {
            let project = self.context.project;
            let step = id(step);
            self.writer.write(RetrySafety::NonIdempotent,move|tx|{tx.sql().execute("UPDATE steps SET status='failed',error=?3 WHERE project_id=?1 AND step_id=?2",params![project.to_string(),step.as_str(),serde_json::to_string(&sluice_model::error::PublicError::FnFailure {message:"fixture failure".into()})?])?;tx.changed(Some(project),"status");Ok(())}).await.unwrap()
        }
    }
    pub fn retry_request(project: ProjectId, steps: &[&str], message: Option<&str>) -> StepRetry {
        StepRetry {
            expected_rev: None,
            project: ProjectSelector::Id(project),
            selection: StepSelection {
                steps: Some(steps.iter().map(|s| id(s)).collect()),
                tags: None,
            },
            message: message.map(str::to_owned),
            reason: Some("retry".into()),
            author: Some("sam".into()),
        }
    }
    pub fn guardian(run: RunId) -> GuardianIdentity {
        GuardianIdentity {
            unit_name: format!("fixture-{run}"),
            boot_id: "boot".into(),
            pid: 123,
            start: "start".into(),
            cgroup: "/fixture".into(),
            socket_challenge: "nonce".into(),
        }
    }
    #[derive(Default)]
    pub struct Hooks {
        pub refuse: bool,
        pub storage_fail: bool,
    }
    impl RetryMessages for Hooks {
        fn validate_retry(
            &self,
            _tx: &WriteTransaction<'_>,
            _project: ProjectId,
            _steps: &[StepId],
            _body: &str,
            _author: &str,
        ) -> sluice_store::Result<()> {
            if self.refuse {
                Err(sluice_model::error::PublicError::Invalid {
                    message: "message policy changed".into(),
                    errors: vec![],
                }
                .into())
            } else {
                Ok(())
            }
        }
        fn post_retry(
            &mut self,
            tx: &mut WriteTransaction<'_>,
            project: ProjectId,
            step: &StepId,
            body: &str,
            author: &str,
        ) -> sluice_store::Result<()> {
            if self.storage_fail {
                return Err(StoreError::InvalidDatabase(
                    "injected storage failure".into(),
                ));
            }
            let message = Message {
                id: MessageId(0),
                thread: format!("step-{step}"),
                from: author.into(),
                to: Some(step.to_string()),
                title: None,
                body: body.into(),
                verb: sluice_model::commands::MessageVerb::Say,
                to_message: None,
                answer: None,
                ui: None,
                input: None,
                data: None,
                run: None,
                at: "now".into(),
                state: None,
                answered_by: None,
            };
            let record = tx.append_record(
                Some(project),
                sluice_model::events::Event::Message(Box::new(message)),
            )?;
            tx.sql().execute("INSERT INTO messages(id,project_id,thread,\"from\",\"to\",body,at) VALUES (?1,?2,?3,?4,?5,?6,?7)",params![record.seq.0,project.to_string(),format!("step-{step}"),author,step.as_str(),body,record.at])?;
            tx.changed(Some(project), "messages");
            Ok(())
        }
    }
    impl ExecutionHooks for Hooks {
        fn assign(
            &mut self,
            tx: &mut WriteTransaction<'_>,
            id: &AttemptIdentity,
            cursor: i64,
            exact: Option<&AssignedRange>,
        ) -> sluice_store::Result<AssignedRange> {
            let range = if let Some(exact) = exact {
                exact.clone()
            } else {
                AssignedRange {after:cursor,through:tx.sql().query_row("SELECT coalesce(max(id),?3) FROM messages WHERE project_id=?1 AND \"to\"=?2 AND id>?3",params![id.project.to_string(),id.step.as_str(),cursor],|r|r.get(0))?}
            };
            tx.sql().execute("INSERT INTO message_deliveries(project_id,run_id,message_id,assigned_at) SELECT project_id,?2,id,'now' FROM messages WHERE project_id=?1 AND \"to\"=?3 AND id>?4 AND id<=?5",params![id.project.to_string(),id.run.to_string(),id.step.as_str(),range.after,range.through])?;
            Ok(range)
        }
        fn started(
            &mut self,
            tx: &mut WriteTransaction<'_>,
            id: &AttemptIdentity,
            _range: &AssignedRange,
        ) -> sluice_store::Result<()> {
            sluice_store::messages::advance_cursor(tx, id.project, id.run)?;
            tx.sql().execute(
                "UPDATE message_deliveries SET acknowledged_at='now' WHERE run_id=?1",
                [id.run.to_string()],
            )?;
            Ok(())
        }
        fn hold(
            &mut self,
            _tx: &mut WriteTransaction<'_>,
            _id: &AttemptIdentity,
            needs: &[(String, u64)],
            _scatter: bool,
        ) -> sluice_store::Result<()> {
            assert!(needs.is_empty());
            Ok(())
        }
        fn release(
            &mut self,
            tx: &mut WriteTransaction<'_>,
            id: &AttemptIdentity,
        ) -> sluice_store::Result<()> {
            tx.sql().execute("UPDATE leases SET state='released',released_at='now' WHERE run_id=?1 AND state='held'",[id.run.to_string()])?;
            Ok(())
        }
    }
}
use serde_json::json;
use support::*;

#[tokio::test]
async fn regression_running_result_retains_the_reserved_unit_when_retagged() {
    let mut f = Fixture::new(json!({"steps":{"a":{"run":"empty","tags":["unit:old"]}}})).await;
    let r = f.reserve("a", json!({})).await;
    f.apply(json!({"steps":{"a":{"run":"empty","tags":["unit:new"]}}}))
        .await;
    f.finish(
        &r,
        sluice_store::attempts::CompletionKind::Succeeded,
        json!({}),
    )
    .await;
    f.apply(json!({"steps":{}})).await;
    let unit: String = f
        .reads
        .snapshot(|c| Ok(c.query_row("SELECT unit FROM outcomes", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(unit, "old");
}
