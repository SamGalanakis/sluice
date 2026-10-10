#[path = "../../../tests/support/home.rs"]
mod home;
#[allow(dead_code)]
#[path = "support/plan_rows.rs"]
mod plan_rows;
#[allow(dead_code)]
pub mod support {
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
    pub struct Signatures;
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
        sluice_model::plan::compile_rows(
            &sluice_model::plan_rows::PlanRows::from_document(&map(doc), None).unwrap(),
            &Signatures,
        )
        .unwrap()
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
use serde_json::{Value, json};
use sluice_model::{commands::*, events::Event, ids::*};
use sluice_store::{
    attempts::{self, *},
    messages::{self, InputAnswer, PlanInputSetter},
    plans::{self, PlanContext, RetryMessages},
    projects, resources, *,
};
use support::*;

fn post(project: ProjectId, body: &str) -> messages::Post {
    messages::Post {
        project: ProjectSelector::Id(project),
        speaker: messages::Speaker::Orchestrator,
        body: body.into(),
        verb: messages::Verb::Say {
            to: "owner".into(),
            data: None,
        },
    }
}
fn to(post: &mut messages::Post, step: &str) {
    if let messages::Verb::Say { to, .. } = &mut post.verb {
        *to = step.into();
    }
}
#[derive(Default)]
struct RealHooks {
    fail_start: bool,
    refuse_reservation: bool,
}
impl RetryMessages for RealHooks {
    fn validate_retry(
        &self,
        tx: &WriteTransaction<'_>,
        project: ProjectId,
        steps: &[StepId],
        body: &str,
        _: &str,
    ) -> Result<()> {
        projects::resolve(tx.sql(), &ProjectSelector::Id(project))?;
        assert!(!body.trim().is_empty() && body.len() <= 65536);
        for step in steps {
            let exists: bool = tx.sql().query_row(
                "SELECT EXISTS(SELECT 1 FROM steps WHERE project_id=?1 AND step_id=?2)",
                rusqlite::params![project.to_string(), step.as_str()],
                |r| r.get(0),
            )?;
            assert!(exists);
        }
        Ok(())
    }
    fn post_retry(
        &mut self,
        tx: &mut WriteTransaction<'_>,
        project: ProjectId,
        step: &StepId,
        body: &str,
        author: &str,
    ) -> Result<()> {
        let mut m = post(project, body);
        to(&mut m, step.as_str());
        if author == "owner" {
            m.speaker = messages::Speaker::Owner;
        }
        messages::post(tx, m, &messages::NoPlanInputs)?;
        Ok(())
    }
}
impl ExecutionHooks for RealHooks {
    fn assign(
        &mut self,
        tx: &mut WriteTransaction<'_>,
        id: &AttemptIdentity,
        cursor: i64,
        exact: Option<&attempts::AssignedRange>,
    ) -> Result<attempts::AssignedRange> {
        assert!(
            !self.refuse_reservation,
            "reservation replay invoked assign"
        );
        let range = messages::assign_run_range(tx, id.project, id.run, cursor, exact)?;
        Ok(attempts::AssignedRange {
            after: range.after.0,
            through: range.through.0,
        })
    }
    fn started(
        &mut self,
        tx: &mut WriteTransaction<'_>,
        id: &AttemptIdentity,
        _: &attempts::AssignedRange,
    ) -> Result<()> {
        messages::advance_cursor(tx, id.project, id.run)?;
        if self.fail_start {
            return Err(StoreError::InvalidDatabase(
                "injected after cursor advancement".into(),
            ));
        }
        Ok(())
    }
    fn hold(
        &mut self,
        tx: &mut WriteTransaction<'_>,
        id: &AttemptIdentity,
        needs: &[(String, u64)],
        _: bool,
    ) -> Result<()> {
        assert!(!self.refuse_reservation, "reservation replay invoked hold");
        resources::hold_needs(tx, id.run, &needs.iter().cloned().collect())?;
        Ok(())
    }
    fn release(&mut self, tx: &mut WriteTransaction<'_>, id: &AttemptIdentity) -> Result<()> {
        resources::release_stopped_run(tx, id.run)?;
        Ok(())
    }
}
struct Inputs(PlanContext);
impl PlanInputSetter for Inputs {
    fn set_input(&self, tx: &mut WriteTransaction<'_>, u: InputAnswer<'_>) -> Result<()> {
        assert_eq!(u.project, self.0.project);
        plans::set_input(
            tx,
            &self.0,
            u.name,
            u.value.clone(),
            u.author.into(),
            u.reason.into(),
        )
    }
}
struct ResourceAdapter;
impl projects::ResourceSettings for ResourceAdapter {
    fn set_resources(
        &self,
        tx: &mut WriteTransaction<'_>,
        project: ProjectId,
        patch: &Value,
    ) -> Result<bool> {
        resources::patch_resources(tx, project, patch, &Signatures)
    }
}
async fn real_reserve(f: &Fixture, req: Reserve, h: RealHooks) -> Reservation {
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            reserve(tx, &context, req, &mut { h })
        })
        .await
        .unwrap()
}
async fn real_finish(
    f: &Fixture,
    r: &Reservation,
    kind: CompletionKind,
    outputs: Value,
) -> CompletionResult {
    let context = f.context.clone();
    let id = r.identity.clone();
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            complete(
                tx,
                &context,
                Complete {
                    completion_id: id.run.to_string(),
                    identity: id,
                    kind,
                    outputs: map(outputs),
                    processes_gone: true,
                    submission_version: None,
                },
                &mut RealHooks::default(),
            )
        })
        .await
        .unwrap()
        .unwrap()
}
async fn feedback(f: &Fixture, step: &str, body: &str) -> Message {
    let mut m = post(f.context.project, body);
    to(&mut m, step);
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            messages::post(tx, m, &messages::NoPlanInputs).map(|p| p.message)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn real_start_hook_accepts_first_run_started() {
    let f = Fixture::new(json!({"steps":{"a":{"run":"empty"}}})).await;
    feedback(&f, "a", "first input").await;
    let r = real_reserve(
        &f,
        f.request("a", json!({}), -1, None).await,
        RealHooks::default(),
    )
    .await;
    let id = r.identity;
    let result = f
        .writer
        .write(RetrySafety::Idempotent, move |tx| {
            claim(tx, &id, guardian(id.run))?;
            started(tx, &id, &mut RealHooks::default())
        })
        .await;
    assert!(
        result.is_ok(),
        "real message adapter rejected first RunStarted: {result:?}"
    );
}

#[tokio::test]
async fn reservation_rechecks_durable_maintenance_fence() {
    let mut admitted = vec![];
    for mode in ["drain"] {
        let f = Fixture::new(json!({"steps":{"a":{"run":"empty"}}})).await;
        let request = f.request("a", json!({}), -1, None).await;
        f.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute("UPDATE maintenance SET mode=?1", [mode])?;
                tx.changed(None, "maintenance");
                Ok(())
            })
            .await
            .unwrap();
        let before = f.counts().await;
        let c = f.context.clone();
        let result = f
            .writer
            .write(RetrySafety::Idempotent, move |tx| {
                reserve(
                    tx,
                    &c,
                    request,
                    &mut RealHooks {
                        refuse_reservation: true,
                        ..Default::default()
                    },
                )
            })
            .await;
        assert!(matches!(
            result,
            Err(sluice_model::error::PublicError::Conflict { .. })
        ));
        assert_eq!(f.counts().await, before);
        if result.is_ok() {
            admitted.push(mode);
        }
    }
    assert!(
        admitted.is_empty(),
        "new reservations committed after fences: {admitted:?}"
    );
}

#[tokio::test]
async fn real_adapter_wiring_and_rollback() {
    let f=Fixture::new(json!({"steps":{"work":{"run":"empty","needs":{"lane":1}},"land":{"run":"empty","after":["work"]}},"inputs":{"decision":"int"}})).await;
    let project = f.context.project;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            resources::patch_resources(tx, project, &json!({"lane":1}), &Signatures)
        })
        .await
        .unwrap();
    f.manual("work", json!({}), false).await;
    f.fail_projection("land").await;
    let context = f.context.clone();
    let retry = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::step_retry(
                tx,
                &context,
                retry_request(project, &["work"], Some("fix work")),
                &mut RealHooks::default(),
            )
        })
        .await
        .unwrap();
    assert_eq!(retry.rearmed, vec![id("land")]);
    let r = real_reserve(
        &f,
        f.request("work", json!({}), -1, None).await,
        RealHooks::default(),
    )
    .await;
    let run = r.identity.run;
    f.reads
        .snapshot(move |c| {
            assert_eq!(resources::held(c, project)?.get("lane"), Some(&1));
            assert_eq!(
                c.query_row(
                    "SELECT count(*) FROM message_deliveries WHERE run_id=?1",
                    [run.to_string()],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    let identity = r.identity.clone();
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            claim(tx, &identity, guardian(identity.run))?;
            started(tx, &identity, &mut RealHooks::default())
        })
        .await
        .unwrap();
    let outcome = real_finish(&f, &r, CompletionKind::Succeeded, json!({})).await;
    assert_eq!(outcome.status, StepStatus::Succeeded);
    f.reads
        .snapshot(move |c| {
            assert!(resources::held(c, project)?.is_empty());
            Ok(())
        })
        .await
        .unwrap();
    let mut ask = post(project, "pick integer");
    ask.verb = messages::Verb::Ask {
        to: "owner".into(),
        title: None,
        ui: None,
        input: Some("decision".into()),
        data: None,
    };
    let q = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            messages::post(tx, ask, &messages::NoPlanInputs).map(|p| p.message)
        })
        .await
        .unwrap();
    let mut answer = post(project, "");
    answer.speaker = messages::Speaker::Owner;
    answer.verb = messages::Verb::Reply {
        to_message: q.id,
        answer: Some(serde_json::from_value(json!({"action":"set","values":{"value":7}})).unwrap()),
    };
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            messages::post(tx, answer, &Inputs(context))
        })
        .await
        .unwrap();
    assert_eq!(f.state().await.inputs, map(json!({"decision":7})));
    // A failure after both domain writes must roll back the message and input.
    let before = f.counts().await;
    let context = f.context.clone();
    let failed = f
        .writer
        .write::<(), _>(RetrySafety::NonIdempotent, move |tx| {
            plans::set_input(
                tx,
                &context,
                "decision",
                json!(8).try_into().unwrap(),
                "sam".into(),
                "test".into(),
            )?;
            messages::post(tx, post(project, "rollback"), &messages::NoPlanInputs)?;
            Err(StoreError::InvalidDatabase("injected after writes".into()))
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(f.counts().await, before);
    assert_eq!(f.state().await.inputs, map(json!({"decision":7})));
}

#[tokio::test]
async fn project_adapters_initialize_and_rename_with_real_resources() {
    let f = Fixture::new(json!({"steps":{}})).await;
    let p = f
        .writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                projects::CreateProject {
                    name: "wired".parse().unwrap(),
                    description: String::new(),
                    icon: None,
                    resources: Some(json!({"lane":2})),
                    author: "sam".into(),
                },
                &projects::EmptyPlanInitializer,
                &ResourceAdapter,
            )
        })
        .await
        .unwrap();
    let id = p.project_id;
    let p = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::project_update(
                tx,
                &ProjectSelector::Id(id),
                projects::UpdateProject {
                    new_name: Some("renamed".parse().unwrap()),
                    resources: Some(json!({"lane":3})),
                    ..Default::default()
                },
                &ResourceAdapter,
            )
        })
        .await
        .unwrap();
    assert_eq!(p.name.as_str(), "renamed");
    assert_eq!(p.resources_rev, Revision(2));
}

/// A prune prepared before a retry of its member is stale at commit (the retry moved the
/// state epoch), so it writes nothing and the retried work stays.
#[tokio::test]
async fn prepared_prune_rechecks_done_after_retry() {
    let f = Fixture::new(json!({"steps":{"work":{"run":"empty"}}})).await;
    f.manual("work", json!({}), false).await;
    let context = f.context.clone();
    let (commit, evidence) = f
        .reads
        .snapshot(move |c| prune_commit(c, &context, time::OffsetDateTime::now_utc()))
        .await
        .unwrap();
    assert_eq!(commit.state.removed, vec![id("work")]);
    f.retry(&["work"], Some("resume unfinished review")).await;
    let project = f.context.project;
    let before = f.counts().await;
    let result = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::commit_plan_edit(tx, project, &commit, Some(&evidence))
        })
        .await
        .unwrap();
    assert_eq!(
        result,
        plans::CommitOutcome::Stale,
        "stale prune must not delete explicitly retried pending work"
    );
    assert_eq!(f.counts().await, before);
    assert_eq!(f.state().await.status(&id("work")), StepStatus::Pending);
}

/// A prune of every done unit the evidence names, as its preparation would hand it over.
fn prune_commit(
    c: &rusqlite::Connection,
    context: &PlanContext,
    cutoff: time::OffsetDateTime,
) -> Result<(
    sluice_model::plan_rows::PlanEditCommit,
    plans::PruneEligibility,
)> {
    let evidence = plans::prune_eligible(c, context, cutoff)?;
    let units: Vec<UnitName> = evidence.units().to_vec();
    let members: Vec<StepId> = context
        .plan
        .units()
        .iter()
        .filter(|(name, _)| units.contains(name))
        .flat_map(|(_, unit)| unit.steps.clone())
        .collect();
    let current = plans::read_plan_rows(c, context.project)?;
    let mut next = current.clone();
    next.steps.retain(|row| !members.contains(&row.step));
    let mut commit = crate::plan_rows::rows_commit(
        c,
        context.project,
        &current,
        &next,
        None,
        "sluice",
        "retire done units",
    );
    commit.prune = Some(sluice_model::units::PruneSet {
        units,
        steps: members,
        kept: Default::default(),
    });
    Ok((commit, evidence))
}

#[tokio::test]
async fn delete_invalidates_project_log_and_questions_subscriptions() {
    let f = Fixture::new(json!({"steps":{}})).await;
    let project = f.context.project;
    let mut q = post(project, "question");
    q.verb = messages::Verb::Ask {
        to: "owner".into(),
        title: None,
        ui: None,
        input: None,
        data: None,
    };
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            messages::post(tx, q, &messages::NoPlanInputs)
        })
        .await
        .unwrap();
    let p = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::project_update(
                tx,
                &ProjectSelector::Id(project),
                projects::UpdateProject {
                    archived: Some(true),
                    ..Default::default()
                },
                &ResourceAdapter,
            )
        })
        .await
        .unwrap();
    let keys = [
        "plan",
        "messages",
        "resources",
        "artifacts",
        "log",
        "questions",
        "edits",
        "outcomes",
        "readers",
        "settings",
        "status",
    ]
    .into_iter()
    .map(|view| ChangeKey::new(Some(project), view))
    .collect::<Vec<_>>();
    let before = f.reads.cursor(keys.clone()).await.unwrap();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::project_delete(
                tx,
                &ProjectSelector::Id(project),
                projects::DeleteProject {
                    confirm_name: p.name.to_string(),
                    expected_settings_rev: p.settings_rev,
                    author: "sam".into(),
                },
                &projects::StoredWorkOnly,
            )
        })
        .await
        .unwrap();
    let after = f.reads.cursor(keys).await.unwrap();
    assert_ne!(
        before, after,
        "deletion removed log and questions without invalidating either durable cursor"
    );
    for (key, previous) in &before.versions {
        assert_eq!(
            after.versions[key],
            previous + 1,
            "deleted view {key:?} did not advance"
        );
    }
}

#[tokio::test]
async fn run_range_adapter_includes_feedback_for_unstarted_scatter_retry() {
    let f = Fixture::new(
        json!({"steps":{"batch":{"run":"int","scatter":"value","in":{"value":{"default":[1,2]}}}}}),
    )
    .await;
    // The first batch has an empty assigned range; retry feedback must create one.

    let a = real_reserve(
        &f,
        f.request("batch", json!({"value":1}), 0, Some(2)).await,
        RealHooks::default(),
    )
    .await;
    let b = real_reserve(
        &f,
        f.request("batch", json!({"value":2}), 1, Some(2)).await,
        RealHooks::default(),
    )
    .await;
    f.start(&a).await;
    real_finish(&f, &a, CompletionKind::Succeeded, json!({"value":1})).await;
    real_finish(
        &f,
        &b,
        CompletionKind::Lost {
            message: "never started".into(),
        },
        json!({}),
    )
    .await;
    let context = f.context.clone();
    let project = context.project;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::step_retry(
                tx,
                &context,
                retry_request(project, &["batch"], Some("new feedback")),
                &mut RealHooks::default(),
            )
        })
        .await
        .unwrap();
    let request = f.request("batch", json!({"value":2}), 1, Some(2)).await;
    let run = request.run;
    let c = f.context.clone();
    let r = f
        .writer
        .write(RetrySafety::Idempotent, move |tx| {
            reserve(tx, &c, request, &mut RealHooks::default())
        })
        .await
        .unwrap();
    let body=f.reads.snapshot(move|c| {
        let mut q=c.prepare("SELECT body FROM messages m JOIN message_deliveries d ON d.message_id=m.id WHERE d.run_id=?1 ORDER BY m.id")?;
        Ok(q.query_map([run.to_string()],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }).await.unwrap();
    assert!(
        body.iter().any(|s| s == "new feedback"),
        "retry assigned only {body:?}, range {:?}",
        r.messages
    );
}

#[tokio::test]
async fn action_registration_has_run_event_identity() {
    let f = Fixture::new(json!({"steps":{"work":{"run":"empty"},"land":{"run":"empty"}}})).await;
    f.manual("work", json!({}), false).await;
    let r = f.reserve("land", json!({})).await;
    let project = f.context.project;
    let run = r.identity.run;
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            let target = completion_target(tx, project, &id("work"))?.unwrap();
            register_completion_action(
                tx,
                RegisterCompletionAction {
                    project,
                    run,
                    target,
                    message: "fix".into(),
                    author: "sam".into(),
                },
                &RealHooks::default(),
            )
        })
        .await
        .unwrap();
    let rows = f
        .reads
        .snapshot(move |c| {
            let mut q = c.prepare(
                "SELECT kind,run_id FROM records WHERE kind='run.completion_action.register'",
            )?;
            Ok(q.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap();
    assert!(
        rows.iter()
            .any(|(kind, id)| kind.starts_with("run.") && id.as_deref() == Some(&run.to_string())),
        "registration cannot be filtered by its run: {rows:?}"
    );
    let target = f
        .reads
        .snapshot(move |c| {
            let durable: String = c.query_row(
                "SELECT completion_action FROM runs WHERE run_id=?1",
                [run.to_string()],
                |r| r.get(0),
            )?;
            let durable: Value = serde_json::from_str(&durable)?;
            let mut target = None;
            for kind in ["run", "run.completion_action.register"] {
                let page = sluice_store::records::read_records(
                    c,
                    Some(project),
                    &sluice_store::records::RecordFilter {
                        kinds: vec![kind.into()],
                        ..Default::default()
                    },
                )?
                .into_page()?;
                assert_eq!(page.records.len(), 1);
                match &page.records[0].event {
                    Event::RunCompletionActionRegistered {
                        run: event_run,
                        target: frozen,
                        message,
                        author,
                    } => {
                        assert_eq!(*event_run, run);
                        assert_eq!(serde_json::to_value(frozen)?, durable["target"]);
                        assert_eq!(message, "fix");
                        assert_eq!(author, "sam");
                        target = Some(frozen.clone());
                    }
                    event => panic!("unexpected registration event: {event:?}"),
                }
            }
            Ok(target.unwrap())
        })
        .await
        .unwrap();
    let before = f.counts().await;
    assert!(
        f.writer
            .write(RetrySafety::Idempotent, move |tx| {
                register_completion_action(
                    tx,
                    RegisterCompletionAction {
                        project,
                        run,
                        target,
                        message: "fix".into(),
                        author: "second author".into(),
                    },
                    &RealHooks::default(),
                )
            })
            .await
            .unwrap()
    );
    assert_eq!(f.counts().await, before);
}

#[tokio::test]
async fn resource_refusal_rolls_back_assigned_messages_and_attempt() {
    let f = Fixture::new(json!({"steps":{"work":{"run":"empty","needs":{"lane":1}}}})).await;
    let project = f.context.project;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            resources::patch_resources(tx, project, &json!({"lane":0}), &Signatures)
        })
        .await
        .unwrap();
    feedback(&f, "work", "must remain for next attempt").await;
    let before = f.counts().await;
    let req = f.request("work", json!({}), -1, None).await;
    let c = f.context.clone();
    let out = f
        .writer
        .write(RetrySafety::Idempotent, move |tx| {
            reserve(tx, &c, req, &mut RealHooks::default())
        })
        .await;
    assert!(out.is_err());
    assert_eq!(f.counts().await, before);
    assert_eq!(f.state().await.status(&id("work")), StepStatus::Pending);
    f.reads
        .snapshot(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM message_deliveries", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn real_completion_retries_atomically_and_replay_does_not_release_new_hold() {
    let f=Fixture::new(json!({"steps":{"work":{"run":"empty","needs":{"lane":1}},"land":{"run":"empty","after":["work"]}}})).await;
    let project = f.context.project;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            resources::patch_resources(tx, project, &json!({"lane":1}), &Signatures)
        })
        .await
        .unwrap();
    f.manual("work", json!({}), false).await;
    let land = real_reserve(
        &f,
        f.request("land", json!({}), -1, None).await,
        RealHooks::default(),
    )
    .await;
    let run = land.identity.run;
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            let target = completion_target(tx, project, &id("work"))?.unwrap();
            register_completion_action(
                tx,
                RegisterCompletionAction {
                    project,
                    run,
                    target,
                    message: "correct result".into(),
                    author: "sam".into(),
                },
                &RealHooks::default(),
            )
        })
        .await
        .unwrap();
    let out = real_finish(
        &f,
        &land,
        CompletionKind::Rejected {
            message: "review refused".into(),
        },
        json!({}),
    )
    .await;
    assert!(matches!(
        out.action,
        Some(CompletionActionOutcome::Applied(_))
    ));
    assert_eq!(f.state().await.status(&id("land")), StepStatus::Pending);
    let work = real_reserve(
        &f,
        f.request("work", json!({}), -1, None).await,
        RealHooks::default(),
    )
    .await;
    assert!(work.messages.through > 0);
    let before = f.counts().await;
    let replay = real_finish(
        &f,
        &land,
        CompletionKind::Rejected {
            message: "review refused".into(),
        },
        json!({}),
    )
    .await;
    assert_eq!(out, replay);
    assert_eq!(f.counts().await, before);
    f.reads
        .snapshot(move |c| {
            assert_eq!(resources::held(c, project)?.get("lane"), Some(&1));
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn real_scatter_adapter_uses_supplied_cursor_and_keeps_feedback() {
    let f = Fixture::new(
        json!({"steps":{"batch":{"run":"int","scatter":"value","in":{"value":{"default":[1,2]}}}}}),
    )
    .await;
    // The first batch has an empty assigned range; retry feedback must create one.

    let a = real_reserve(
        &f,
        f.request("batch", json!({"value":1}), 0, Some(2)).await,
        RealHooks::default(),
    )
    .await;
    let b = real_reserve(
        &f,
        f.request("batch", json!({"value":2}), 1, Some(2)).await,
        RealHooks::default(),
    )
    .await;
    f.start(&a).await;
    real_finish(&f, &a, CompletionKind::Succeeded, json!({"value":1})).await;
    real_finish(
        &f,
        &b,
        CompletionKind::Lost {
            message: "never started".into(),
        },
        json!({}),
    )
    .await;
    let context = f.context.clone();
    let project = context.project;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::step_retry(
                tx,
                &context,
                retry_request(project, &["batch"], Some("new feedback")),
                &mut RealHooks::default(),
            )
        })
        .await
        .unwrap();
    let request = f.request("batch", json!({"value":2}), 1, Some(2)).await;
    let run = request.run;
    let c = f.context.clone();
    let r = f
        .writer
        .write(RetrySafety::Idempotent, move |tx| {
            reserve(tx, &c, request, &mut RealHooks::default())
        })
        .await
        .unwrap();
    let body=f.reads.snapshot(move|c| {
        let mut q=c.prepare("SELECT body FROM messages m JOIN message_deliveries d ON d.message_id=m.id WHERE d.run_id=?1 ORDER BY m.id")?;
        Ok(q.query_map([run.to_string()],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?)
    }).await.unwrap();
    assert!(
        body.iter().any(|s| s == "new feedback"),
        "retry assigned only {body:?}, range {:?}",
        r.messages
    );
}

#[tokio::test]
async fn start_hook_failure_rolls_back_evidence_cursor_and_versions() {
    let f = Fixture::new(json!({"steps":{"a":{"run":"empty"}}})).await;
    feedback(&f, "a", "input").await;
    let request = f.request("a", json!({}), -1, None).await;
    let reservation = real_reserve(&f, request, RealHooks::default()).await;
    let identity = reservation.identity.clone();
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            claim(tx, &identity, guardian(identity.run))
        })
        .await
        .unwrap();
    let project = f.context.project;
    let keys = ["status", "messages", "log"]
        .map(|view| ChangeKey::new(Some(project), view))
        .to_vec();
    let before = f.reads.cursor(keys.clone()).await.unwrap();
    let counts = f.counts().await;
    let identity = reservation.identity.clone();
    let result = f
        .writer
        .write(RetrySafety::Idempotent, move |tx| {
            started(
                tx,
                &identity,
                &mut RealHooks {
                    fail_start: true,
                    ..Default::default()
                },
            )
        })
        .await;
    assert!(result.is_err());
    assert_eq!(before, f.reads.cursor(keys).await.unwrap());
    assert_eq!(counts, f.counts().await);
    let run = reservation.identity.run;
    f.reads.snapshot(move |c| {
        let (phase, start, cursor): (String, Option<String>, i64) = c.query_row(
            "SELECT a.phase,r.started_at,s.delivery_cursor FROM runs r JOIN attempts a USING(attempt_id) JOIN steps s USING(project_id,step_id) WHERE r.run_id=?1",
            [run.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        assert_eq!(phase, "claimed");
        assert!(start.is_none());
        assert_eq!(cursor, 0);
        Ok(())
    }).await.unwrap();
    let identity = reservation.identity;
    assert!(
        f.writer
            .write(RetrySafety::Idempotent, move |tx| started(
                tx,
                &identity,
                &mut RealHooks::default()
            ))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn maintenance_preserves_empty_reservation_replay_and_completion() {
    for mode in ["drain"] {
        let f = Fixture::new(json!({"steps":{"a":{"run":"empty"}}})).await;
        let request = f.request("a", json!({}), -1, None).await;
        let original = real_reserve(&f, request.clone(), RealHooks::default()).await;
        assert_eq!(
            original.messages,
            attempts::AssignedRange {
                after: 0,
                through: 0
            }
        );
        feedback(&f, "a", "late input").await;
        f.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute("UPDATE maintenance SET mode=?1", [mode])?;
                tx.changed(None, "maintenance");
                Ok(())
            })
            .await
            .unwrap();
        let before = f.counts().await;
        let replay = real_reserve(
            &f,
            request.clone(),
            RealHooks {
                refuse_reservation: true,
                ..Default::default()
            },
        )
        .await;
        assert_eq!(replay, original);
        assert_eq!(f.counts().await, before);
        let context = f.context.clone();
        let changed = Reserve {
            release_id: "different".into(),
            ..request
        };
        assert!(matches!(
            f.writer
                .write(RetrySafety::Idempotent, move |tx| reserve(
                    tx,
                    &context,
                    changed,
                    &mut RealHooks::default()
                ))
                .await,
            Err(sluice_model::error::PublicError::Conflict { .. })
        ));
        let result = real_finish(
            &f,
            &original,
            CompletionKind::Lost {
                message: "stopped during maintenance".into(),
            },
            json!({}),
        )
        .await;
        assert_eq!(result.status, StepStatus::Failed);
        let before = f.counts().await;
        let replay = real_finish(
            &f,
            &original,
            CompletionKind::Lost {
                message: "stopped during maintenance".into(),
            },
            json!({}),
        )
        .await;
        assert_eq!(replay, result);
        assert_eq!(f.counts().await, before);
    }
}

#[tokio::test]
async fn scatter_siblings_freeze_empty_and_nonempty_ranges_before_late_feedback() {
    for initial_message in [false, true] {
        let f = Fixture::new(json!({"steps":{"batch":{"run":"int","scatter":"value","in":{"value":{"default":[1,2]}}}}})).await;
        if initial_message {
            feedback(&f, "batch", "initial").await;
        }
        let first = real_reserve(
            &f,
            f.request("batch", json!({"value":1}), 0, Some(2)).await,
            RealHooks::default(),
        )
        .await;
        let late = feedback(&f, "batch", "late").await;
        let second = real_reserve(
            &f,
            f.request("batch", json!({"value":2}), 1, Some(2)).await,
            RealHooks::default(),
        )
        .await;
        assert_eq!(second.messages, first.messages);
        assert!(second.messages.through < late.id.0);
        let project = f.context.project;
        f.reads
            .snapshot(move |c| {
                assert_eq!(
                    c.query_row(
                        "SELECT count(*) FROM message_deliveries WHERE message_id=?1",
                        [late.id.0],
                        |r| r.get::<_, i64>(0)
                    )?,
                    0
                );
                assert_eq!(
                    c.query_row(
                        "SELECT delivery_cursor FROM steps WHERE project_id=?1",
                        [project.to_string()],
                        |r| r.get::<_, i64>(0)
                    )?,
                    0
                );
                Ok(())
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn age_filtered_prune_freezes_cutoff_and_current_results() {
    // Same status and plan revision do not prove that the current result is old: a new
    // result moves the state epoch (the commit is stale), and evidence frozen before it is
    // refused even under tokens read after it.
    for replace_result in [false, true] {
        for fresh_tokens in [false, true] {
            let f =
                Fixture::new(json!({"steps":{"work":{"run":"empty","tags":["unit:done"]}}})).await;
            f.manual("work", json!({}), false).await;
            let context = f.context.clone();
            let cutoff = time::OffsetDateTime::now_utc();
            let (mut commit, evidence) = f
                .reads
                .snapshot(move |c| {
                    let excluded =
                        plans::prune_eligible(c, &context, cutoff - time::Duration::hours(1))?;
                    assert!(excluded.units().is_empty());
                    let found = prune_commit(c, &context, cutoff)?;
                    assert_eq!(found.1.units(), &["done".parse::<UnitName>().unwrap()]);
                    Ok(found)
                })
                .await
                .unwrap();
            if replace_result {
                f.manual("work", json!({}), false).await;
            }
            let before = f.counts().await;
            let project = f.context.project;
            let result = f
                .writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    if fresh_tokens {
                        commit.tokens = crate::plan_rows::tokens(tx.sql(), project);
                    }
                    plans::commit_plan_edit(tx, project, &commit, Some(&evidence))
                })
                .await;
            match (replace_result, fresh_tokens) {
                (false, _) => {
                    assert_eq!(
                        result.unwrap(),
                        plans::CommitOutcome::Committed(Revision(3))
                    );
                    assert!(!f.state().await.steps.contains_key(&id("work")));
                    continue;
                }
                (true, false) => assert_eq!(result.unwrap(), plans::CommitOutcome::Stale),
                (true, true) => assert!(matches!(
                    result,
                    Err(sluice_model::error::PublicError::Conflict { .. })
                )),
            }
            assert_eq!(f.counts().await, before);
            assert_eq!(f.state().await.status(&id("work")), StepStatus::Succeeded);
        }
    }
}
