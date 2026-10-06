#[path = "../../../tests/support/home.rs"]
mod home;
#[allow(dead_code)]
mod support {
    use super::home::ScratchHome;
    use rusqlite::params;
    use serde_json::{Value, json};
    use sluice_model::error::PublicError;
    use sluice_model::{
        commands::*,
        edit::PreparedEdit,
        gates::{CachedResources, StateSnapshot},
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
    pub fn edit(context: &PlanContext, doc: Value) -> PreparedEdit {
        let state = StateSnapshot::default();
        let recipes = Default::default();
        let limits = Default::default();
        let resources = CachedResources::default();
        let request = sluice_model::edit::PlanEdit::Patch(PlanPatch {
            project: ProjectSelector::Id(context.project),
            rev: context.revision,
            ops: vec![PatchOperation::Replace {
                path: "".into(),
                value: doc.try_into().unwrap(),
            }],
            start: true,
            dry_run: false,
            reason: "change".into(),
            author: Some("sam".into()),
        });
        sluice_model::edit::prepare_edit(
            &sluice_model::edit::EditSnapshot {
                revision: context.revision,
                plan: &context.plan,
                state: &state,
                signatures: &Signatures,
                recipes: &recipes,
                limits: &limits,
                resources: &resources,
                prune_eligible: None,
            },
            request,
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
            let project = ProjectId::new();
            let plan = plan(doc);
            let copy = plan.clone();
            writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    tx.sql().execute(
                        "INSERT INTO projects(project_id,name,created_at) VALUES (?1,'p','now')",
                        [project.to_string()],
                    )?;
                    plans::initialize_plan(tx, project, &copy)
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
                    revision: Revision(1),
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
            let prepared = edit(&self.context, doc);
            let plan = prepared.plan.clone();
            let project = self.context.project;
            let result = self
                .writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    plans::apply_edit(tx, project, prepared)
                })
                .await
                .unwrap();
            self.context.plan = plan;
            self.context.revision = result.rev;
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
            self.finish_seen(reservation, kind, outputs, None).await
        }
        /// Complete a run that saw its submission (`Some(version)`) or none.
        pub async fn finish_seen(
            &self,
            reservation: &Reservation,
            kind: CompletionKind,
            outputs: Value,
            submission_version: Option<u64>,
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
                            submission_version,
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
            self.writer.write(RetrySafety::NonIdempotent,move|tx|{tx.sql().execute("UPDATE steps SET status='failed',error=?3 WHERE project_id=?1 AND step_id=?2",params![project.to_string(),step.as_str(),serde_json::to_string(&PublicError::FnFailure {message:"fixture failure".into()})?])?;tx.changed(Some(project),"status");Ok(())}).await.unwrap()
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
                Err(PublicError::Invalid {
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
use sluice_model::{commands::*, error::PublicError, ids::*};
use sluice_store::{
    RetrySafety,
    attempts::*,
    plans::{self},
};
use support::*;

#[tokio::test]
async fn edits_compare_revision_and_commit_document_state_record_together() {
    let mut f = Fixture::new(json!({"steps":{}})).await;
    let candidate = json!({"steps":{"a":{"run":"empty"}}});
    let stale = edit(&f.context, candidate.clone());
    f.apply(candidate).await;
    let before = f.counts().await;
    let project = f.context.project;
    let error = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::apply_edit(tx, project, stale)
        })
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        PublicError::Conflict {
            current_rev: Some(Revision(2)),
            ..
        }
    ));
    assert_eq!(f.counts().await, before);
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Pending);
    let pair: (i64, i64) = f
        .reads
        .snapshot(|c| {
            Ok((
                c.query_row("SELECT rev FROM plans", [], |r| r.get(0))?,
                c.query_row("SELECT rev FROM plan_edits", [], |r| r.get(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(pair, (2, 2));
}
#[tokio::test]
async fn failed_composition_rolls_back_edit_state_archive_and_records() {
    let f =
        Fixture::new(json!({"steps":{"a":{"run":"core.external","outputs":{"n":"int"}}}})).await;
    f.manual("a", json!({"n":3}), false).await;
    let prepared = edit(&f.context, json!({"steps":{}}));
    let project = f.context.project;
    let before = f.counts().await;
    let error = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::apply_edit(tx, project, prepared)?;
            Err::<(), _>(
                PublicError::BadRequest {
                    message: "later command failed".into(),
                }
                .into(),
            )
        })
        .await
        .unwrap_err();
    assert!(matches!(error, PublicError::BadRequest { .. }));
    assert_eq!(f.counts().await, before);
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Succeeded);
    let removed: i64 = f
        .reads
        .snapshot(|c| Ok(c.query_row("SELECT count(*) FROM outcomes", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(removed, 0);
}
#[tokio::test]
async fn dry_run_writes_nothing_and_keeps_revision() {
    let f = Fixture::new(json!({"steps":{}})).await;
    let mut prepared = edit(&f.context, json!({"steps":{"a":{"run":"empty"}}}));
    prepared.dry_run = true;
    let before = f.counts().await;
    let project = f.context.project;
    let result = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::apply_edit(tx, project, prepared)
        })
        .await
        .unwrap();
    assert_eq!(result.rev, Revision(1));
    assert_eq!(f.counts().await, before);
    assert!(f.state().await.steps.is_empty());
}
#[tokio::test]
async fn running_step_is_guarded_again_at_apply_but_pause_and_tags_are_allowed() {
    let mut f =
        Fixture::new(json!({"steps":{"a":{"run":"echo","in":{"value":{"default":1}}}}})).await;
    let prepared = edit(&f.context, json!({"steps":{}}));
    let r = f.reserve("a", json!({"value":1})).await;
    let before = f.counts().await;
    let project = f.context.project;
    assert!(
        f.writer
            .write(RetrySafety::NonIdempotent, move |tx| plans::apply_edit(
                tx, project, prepared
            ))
            .await
            .is_err()
    );
    assert_eq!(f.counts().await, before);
    f.apply(json!({"steps":{"a":{"run":"echo","in":{"value":{"default":1}},"paused":true,"tags":["unit:lane"]}}})).await;
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Running);
    f.start(&r).await;
    let result = f
        .finish(&r, CompletionKind::Succeeded, json!({"value":1}))
        .await;
    assert_eq!(result.status, StepStatus::Succeeded);
}
#[tokio::test]
async fn a_paused_step_can_be_given_its_result_without_force_and_downstream_starts() {
    let f = Fixture::new(json!({"steps":{
        "work":{"run":"echo","in":{"value":{"default":7}},"paused":"orchestrator made the commit"},
        "land":{"run":"echo","in":{"value":{"source":"work/value"}}}
    }}))
    .await;
    f.manual("work", json!({"value":7}), false).await;
    let state = f.state().await;
    assert_eq!(state.status(&id("work")), StepStatus::Succeeded);
    assert!(state.steps[&id("work")].inputs_hash.is_some());
    assert_eq!(state.status(&id("land")), StepStatus::Pending);
    assert_eq!(
        f.counts().await.2,
        0,
        "no attempt was launched for the paused step"
    );
}
#[tokio::test]
async fn force_only_bypasses_gate_without_losing_data_hash() {
    let f=Fixture::new(json!({"inputs":{"enabled":"boolean"},"steps":{"a":{"run":"echo","in":{"value":{"default":7}},"after":["enabled"]}}})).await;
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::set_input(
                tx,
                &context,
                "enabled",
                json!(false).try_into().unwrap(),
                "sam".into(),
                "disable".into(),
            )
        })
        .await
        .unwrap();
    let context = f.context.clone();
    let before = f.counts().await;
    let request = StepSetOutput {
        project: ProjectSelector::Id(context.project),
        step: id("a"),
        outputs: map(json!({"value":7})),
        force: false,
        reason: "manual".into(),
        author: None,
    };
    assert!(
        f.writer
            .write(
                RetrySafety::NonIdempotent,
                move |tx| plans::step_set_output(tx, &context, request)
            )
            .await
            .is_err()
    );
    assert_eq!(f.counts().await, before);
    f.manual("a", json!({"value":7}), true).await;
    let state = f.state().await;
    assert_eq!(state.status(&id("a")), StepStatus::Succeeded);
    assert!(state.steps[&id("a")].inputs_hash.is_some());
}
#[tokio::test]
async fn forced_missing_data_becomes_stale_when_values_arrive() {
    let f = Fixture::new(
        json!({"inputs":{"n":"Any"},"steps":{"a":{"run":"echo","in":{"value":{"source":"n"}}}}}),
    )
    .await;
    f.manual("a", json!({"value":9}), true).await;
    assert!(f.state().await.steps[&id("a")].inputs_hash.is_none());
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::set_input(
                tx,
                &context,
                "n",
                json!(9).try_into().unwrap(),
                "sam".into(),
                "now known".into(),
            )
        })
        .await
        .unwrap();
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Stale);
}
#[tokio::test]
async fn retries_keep_visible_values_and_stale_only_changed_effective_data() {
    let f=Fixture::new(json!({"steps":{"a":{"run":"echo","in":{"value":{"default":1}}},"b":{"run":"echo","in":{"value":{"source":"a/value"}}},"gate":{"run":"empty","after":["a"]}}})).await;
    f.manual("a", json!({"value":1}), false).await;
    f.manual("b", json!({"value":1}), false).await;
    f.manual("gate", json!({}), false).await;
    let retry = f.retry(&["a"], None).await;
    assert!(retry.stopped_at.contains(&id("b")));
    assert!(retry.stopped_at.contains(&id("gate")));
    let r = f.reserve("a", json!({"value":1})).await;
    assert_eq!(
        f.state().await.steps[&id("a")].outputs,
        map(json!({"value":1}))
    );
    assert_eq!(f.state().await.status(&id("b")), StepStatus::Succeeded);
    f.finish(&r, CompletionKind::Succeeded, json!({"value":1}))
        .await;
    assert_eq!(f.state().await.status(&id("b")), StepStatus::Succeeded);
    f.retry(&["a"], None).await;
    let r = f.reserve("a", json!({"value":1})).await;
    f.finish(&r, CompletionKind::Succeeded, json!({"value":2}))
        .await;
    assert_eq!(f.state().await.status(&id("b")), StepStatus::Stale);
    assert_eq!(f.state().await.status(&id("gate")), StepStatus::Succeeded);
    f.manual("a", json!({"value":1}), false).await;
    assert_eq!(f.state().await.status(&id("b")), StepStatus::Succeeded);
}
#[tokio::test]
async fn retry_walk_crosses_pending_and_failed_but_stops_at_success_and_skip() {
    let f=Fixture::new(json!({"steps":{"a":{"run":"empty"},"pending":{"run":"empty","after":["a"]},"failed":{"run":"empty","after":["pending"],"paused":"hold"},"done":{"run":"empty","after":["a"]},"behind":{"run":"empty","after":["done"]}}})).await;
    f.manual("a", json!({}), false).await;
    f.manual("done", json!({}), false).await;
    f.fail_projection("failed").await;
    f.fail_projection("behind").await;
    let result = f.retry(&["a"], Some("fix it")).await;
    assert_eq!(result.rearmed, vec![id("failed")]);
    assert_eq!(result.stopped_at, vec![id("done")]);
    assert_eq!(f.state().await.status(&id("behind")), StepStatus::Failed);
    let (work, pause): (i64, String) = f
        .reads
        .snapshot(|c| {
            Ok(c.query_row(
                "SELECT work_generation,paused FROM steps WHERE step_id='failed'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(work, 2);
    assert_eq!(pause, "\"hold\"");
    assert_eq!(f.counts().await.3, 1);
}
#[tokio::test]
async fn retry_is_atomic_when_message_storage_fails() {
    let f = Fixture::new(json!({"steps":{"a":{"run":"empty"},"b":{"run":"empty","after":["a"]}}}))
        .await;
    f.manual("a", json!({}), false).await;
    f.fail_projection("b").await;
    let before = f.counts().await;
    let context = f.context.clone();
    let request = retry_request(context.project, &["a"], Some("feedback"));
    assert!(
        f.writer
            .write(RetrySafety::NonIdempotent, move |tx| plans::step_retry(
                tx,
                &context,
                request,
                &mut Hooks {
                    storage_fail: true,
                    ..Hooks::default()
                }
            ))
            .await
            .is_err()
    );
    assert_eq!(f.counts().await, before);
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Succeeded);
    assert_eq!(f.state().await.status(&id("b")), StepStatus::Failed);
}
#[tokio::test]
async fn external_never_reserves_and_pending_cancellation_commits_failure() {
    let f =
        Fixture::new(json!({"steps":{"a":{"run":"core.external","outputs":{"n":"int"}}}})).await;
    let request = f.request("a", json!({}), -1, None).await;
    let context = f.context.clone();
    let before = f.counts().await;
    assert!(
        f.writer
            .write(RetrySafety::NonIdempotent, move |tx| reserve(
                tx,
                &context,
                request,
                &mut Hooks::default()
            ))
            .await
            .is_err()
    );
    assert_eq!(f.counts().await, before);
    let context = f.context.clone();
    let request = StepCancel {
        expected_rev: None,
        project: ProjectSelector::Id(context.project),
        selection: StepSelection {
            steps: Some(vec![id("a")]),
            tags: None,
        },
        reason: "cancel outside work".into(),
        author: Some("sam".into()),
    };
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::step_cancel(tx, &context, request)
        })
        .await
        .unwrap();
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Failed);
    assert_eq!(f.counts().await.1, 1);
}
async fn submit(f: &Fixture, run: RunId, outputs: Value) -> Result<Option<u64>, PublicError> {
    let request = StepSubmit {
        project: f.context.project,
        step: id("a"),
        run,
        outputs: map(outputs),
        author: Some("sam".into()),
    };
    f.writer
        .write(RetrySafety::Idempotent, move |tx| step_submit(tx, request))
        .await
}
#[tokio::test]
async fn a_submission_is_checked_taken_once_and_joins_the_returned_outputs() {
    let f = Fixture::new(
        json!({"steps":{"a":{"run":"open","outputs":{"ready":"boolean","note":"string?"}},"b":{"run":"echo","in":{"value":{"source":"a/ready"}}}}}),
    )
    .await;
    let r = f.reserve("a", json!({})).await;
    f.start(&r).await;
    let before = f.counts().await;
    // An invalid submission lists its problems and changes nothing: the step still runs.
    let Err(PublicError::Invalid { errors, .. }) =
        submit(&f, r.identity.run, json!({"ready":"wrong","extra":1})).await
    else {
        panic!("an invalid submission is refused")
    };
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert_eq!(f.counts().await, before);
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Running);
    assert_eq!(
        submit(&f, r.identity.run, json!({"ready":true}))
            .await
            .unwrap(),
        Some(1)
    );
    // Submitting ended the run's agent: there is no second submission.
    let Err(PublicError::Conflict { message, .. }) =
        submit(&f, r.identity.run, json!({"ready":false})).await
    else {
        panic!("a second submission is refused")
    };
    assert!(message.contains("already submitted"), "{message}");
    // The run's completion is the step's result: what the fn returned, the submission
    // joined in.
    let outcome = f
        .finish_seen(
            &r,
            CompletionKind::Succeeded,
            json!({"report":"hello"}),
            Some(1),
        )
        .await;
    assert_eq!(outcome.status, StepStatus::Succeeded);
    assert_eq!(
        outcome.outputs,
        map(json!({"ready":true,"report":"hello","note":null}))
    );
    let state = f.state().await;
    assert!(matches!(
        sluice_model::gates::evaluate_step(
            &f.context.plan,
            &state,
            &f.context.plan.steps()[&id("b")]
        ),
        sluice_model::gates::GateDecision::Ready
    ));
}
#[tokio::test]
async fn a_send_back_reopens_a_submitted_step_as_a_new_run() {
    let f = Fixture::new(json!({"steps":{"a":{"run":"open","outputs":{"ready":"boolean"}}}})).await;
    let r = f.reserve("a", json!({})).await;
    f.start(&r).await;
    submit(&f, r.identity.run, json!({"ready":true}))
        .await
        .unwrap();
    f.finish_seen(&r, CompletionKind::Succeeded, json!({}), Some(1))
        .await;
    f.retry(&["a"], Some("fix the thing")).await;
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Pending);
    let again = f.reserve("a", json!({})).await;
    assert_ne!(again.identity.run, r.identity.run);
    assert_eq!(again.prev_run, Some(r.identity.run));
    f.start(&again).await;
    // The new run submits afresh.
    assert_eq!(
        submit(&f, again.identity.run, json!({"ready":false}))
            .await
            .unwrap(),
        Some(1)
    );
}
#[tokio::test]
async fn a_run_that_ends_without_submitting_fails_its_step_typed() {
    let f = Fixture::new(json!({"steps":{"a":{"run":"open","outputs":{"ready":"boolean"}}}})).await;
    let r = f.reserve("a", json!({})).await;
    f.start(&r).await;
    let outcome = f
        .finish(
            &r,
            CompletionKind::Succeeded,
            json!({"report":"done","session":"s-1"}),
        )
        .await;
    assert_eq!(outcome.status, StepStatus::Failed);
    let Some(PublicError::ExitedWithoutSubmit { message, session }) = outcome.error else {
        panic!("{:?}", outcome.error)
    };
    assert!(message.contains("ready"), "{message}");
    assert_eq!(session.as_deref(), Some("s-1"));
    // An agent that stopped without submitting says so through its own failure kind.
    f.retry(&["a"], None).await;
    let r = f.reserve("a", json!({})).await;
    f.start(&r).await;
    let outcome = f
        .finish(
            &r,
            CompletionKind::Failed(PublicError::AgentFailure {
                kind: "ExitedWithoutSubmit".into(),
                message: "stopped after 3 nudges".into(),
                session: Some("s-2".into()),
            }),
            json!({}),
        )
        .await;
    assert_eq!(
        outcome.error,
        Some(PublicError::ExitedWithoutSubmit {
            message: "stopped after 3 nudges".into(),
            session: Some("s-2".into()),
        })
    );
}
#[tokio::test]
async fn invalid_or_missing_final_outputs_terminalize_failure() {
    for outputs in [
        json!({}),
        json!({"value":"not an int"}),
        json!({"value":1,"extra":2}),
    ] {
        let f =
            Fixture::new(json!({"steps":{"a":{"run":"int","in":{"value":{"default":1}}}}})).await;
        let r = f.reserve("a", json!({"value":1})).await;
        let outcome = f.finish(&r, CompletionKind::Succeeded, outputs).await;
        assert_eq!(outcome.status, StepStatus::Failed);
        assert!(matches!(outcome.error, Some(PublicError::Invalid { .. })));
        assert_eq!(f.state().await.status(&id("a")), StepStatus::Failed);
    }
}

#[tokio::test]
async fn regression_growing_reorder_preserves_old_steps_and_inputs_without_position_collisions() {
    let mut f = Fixture::new(json!({"inputs":{"iz":"int"},"steps":{"z":{"run":"empty"}}})).await;
    f.manual("z", json!({}), false).await;
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::set_input(
                tx,
                &context,
                "iz",
                json!(5).try_into().unwrap(),
                "sam".into(),
                "value".into(),
            )
        })
        .await
        .unwrap();
    f.apply(json!({"inputs":{"ia":"int","ib":"int","iz":"int"},"steps":{"a":{"run":"empty"},"b":{"run":"empty"},"z":{"run":"empty"}}})).await;
    let state = f.state().await;
    assert_eq!(
        state
            .steps
            .keys()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec!["a", "b", "z"]
    );
    assert_eq!(state.status(&id("z")), StepStatus::Succeeded);
    assert_eq!(state.inputs, map(json!({"iz":5})));
}

#[tokio::test]
async fn selection_is_the_union_of_explicit_ids_and_matching_tags() {
    let f=Fixture::new(json!({"steps":{"a":{"run":"empty"},"b":{"run":"empty","tags":["lane"]},"c":{"run":"empty","tags":["other"]}}})).await;
    for s in ["a", "b", "c"] {
        f.manual(s, json!({}), false).await;
    }
    let context = f.context.clone();
    let request = StepRetry {
        selection: StepSelection {
            steps: Some(vec![id("a")]),
            tags: Some(vec!["lane".into(), "unused".into()]),
        },
        ..retry_request(context.project, &["a"], None)
    };
    let result = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::step_retry(tx, &context, request, &mut Hooks::default())
        })
        .await
        .unwrap();
    assert_eq!(result.steps, vec![id("a"), id("b")]);
    assert_eq!(f.state().await.status(&id("c")), StepStatus::Succeeded);
}
#[tokio::test]
async fn cancelling_a_mixed_selection_refuses_pending_executable_without_partial_changes() {
    let f = Fixture::new(
        json!({"steps":{"x":{"run":"core.external"},"y":{"run":"empty","paused":true}}}),
    )
    .await;
    let before = f.counts().await;
    let context = f.context.clone();
    let request = StepCancel {
        expected_rev: None,
        project: ProjectSelector::Id(context.project),
        selection: StepSelection {
            steps: Some(vec![id("x"), id("y")]),
            tags: None,
        },
        reason: "stop".into(),
        author: Some("sam".into()),
    };
    assert!(
        f.writer
            .write(RetrySafety::NonIdempotent, move |tx| plans::step_cancel(
                tx, &context, request
            ))
            .await
            .is_err()
    );
    assert_eq!(f.counts().await, before);
    assert_eq!(f.state().await.status(&id("x")), StepStatus::Pending);
}
#[tokio::test]
async fn open_extra_inputs_and_optional_declared_outputs_complete_without_submission() {
    let f=Fixture::new(json!({"steps":{"a":{"run":"open","in":{"extra":{"default":{"nested":[1,2]}}},"outputs":{"note":"string?"}}}})).await;
    let r = f.reserve("a", json!({"extra":{"nested":[1,2]}})).await;
    let result = f
        .finish(&r, CompletionKind::Succeeded, json!({"report":"done"}))
        .await;
    assert_eq!(result.status, StepStatus::Succeeded);
    assert_eq!(result.outputs, map(json!({"report":"done","note":null})));
}

#[tokio::test]
async fn a_witness_holds_until_one_of_its_projects_witnessed_rows_changes() {
    let f = Fixture::new(json!({"steps":{"a":{"run":"empty"},"b":{"run":"empty"}}})).await;
    let project = f.context.project;
    let other = ProjectId::new();
    let write = |sql: &'static str, id: ProjectId| {
        f.writer.write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(sql, [id.to_string()])?;
            tx.changed(Some(id), "status");
            Ok(())
        })
    };
    write(
        "INSERT INTO projects(project_id,name,created_at) VALUES (?1,'q','now')",
        other,
    )
    .await
    .unwrap();
    // Taken before the snapshot the witness is read in, as an edit takes it.
    let mark = f.writer.row_mark();
    let witness = f
        .reads
        .snapshot(move |c| Ok(plans::Witness::read_with_state(c, project)?.0))
        .await
        .unwrap();
    let holds = || {
        let witness = witness.clone();
        f.writer.write(RetrySafety::Idempotent, move |tx| {
            witness.holds_since(tx, project, mark)
        })
    };
    assert!(holds().await.unwrap());
    // Another project's row: not this project's, so the witness holds unread.
    write("UPDATE projects SET name='r' WHERE project_id=?1", other)
        .await
        .unwrap();
    assert!(holds().await.unwrap());
    // This project's row, in a column the witness does not read: compared, and it holds.
    write(
        "UPDATE steps SET delivery_cursor=1 WHERE project_id=?1 AND step_id='a'",
        project,
    )
    .await
    .unwrap();
    assert!(holds().await.unwrap());
    // A column it reads: it no longer holds.
    f.manual("a", json!({}), false).await;
    assert!(!holds().await.unwrap());
}
