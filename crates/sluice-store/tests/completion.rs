#[path = "../../../tests/support/home.rs"]
mod home;
#[allow(dead_code)]
mod support {
    use super::home::ScratchHome;
    use rusqlite::params;
    use serde_json::{Value, json};
    use sluice_model::{
        commands::*,
        edit::PreparedEdit,
        gates::{CachedResources, StateSnapshot},
        ids::*,
        plan::{FnSignature, Plan, SignatureProvider, Snapshot},
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
        let snapshot = Snapshot {
            revision: context.revision,
            document: context.plan.document().clone(),
        };
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
                snapshot: &snapshot,
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
                needs_reply: false,
                reply_to: None,
                answer: None,
                ui: None,
                input: None,
                data: None,
                run: None,
                at: "now".into(),
                claimed_by: None,
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
use sluice_model::{commands::*, error::PublicError, rpc::JsonMap};
use sluice_store::{RetrySafety, attempts::*, plans};
use support::*;

async fn setup(after: bool) -> (Fixture, Reservation, CompletionActionTarget) {
    let after = if after { json!(["work"]) } else { json!([]) };
    let f=Fixture::new(json!({"steps":{"work":{"run":"core.external","outputs":{"ready":"boolean"}},"land":{"run":"empty","after":after}}})).await;
    f.manual("work", json!({"ready":true}), false).await;
    let land = f.reserve("land", json!({})).await;
    f.start(&land).await;
    let project = f.context.project;
    let run = land.identity.run;
    let target = f
        .writer
        .write(RetrySafety::Idempotent, move |tx| {
            let target = completion_target(tx, project, &id("work"))?.unwrap();
            assert!(register_completion_action(
                tx,
                RegisterCompletionAction {
                    project,
                    run,
                    target: target.clone(),
                    message: "please fix".into(),
                    author: "sam".into()
                },
                &Hooks::default()
            )?);
            Ok(target)
        })
        .await
        .unwrap();
    (f, land, target)
}
fn rejected() -> CompletionKind {
    CompletionKind::Rejected {
        message: "project refuses landing".into(),
    }
}
#[tokio::test]
async fn rejected_completion_commits_failure_and_retries_target_with_newly_failed_land() {
    let (f, land, _) = setup(true).await;
    let result = f.finish(&land, rejected(), json!({})).await;
    assert_eq!(result.status, StepStatus::Failed);
    let Some(CompletionActionOutcome::Applied(retry)) = result.action else {
        panic!("action not applied")
    };
    assert_eq!(retry.steps, vec![id("work")]);
    assert_eq!(retry.rearmed, vec![id("land")]);
    assert_eq!(f.state().await.status(&id("work")), StepStatus::Pending);
    assert_eq!(f.state().await.status(&id("land")), StepStatus::Pending);
    assert_eq!(f.counts().await.3, 1);
    let failure: String = f
        .reads
        .snapshot(|c| {
            Ok(c.query_row(
                "SELECT status FROM step_results WHERE step_id='land'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(failure, "failed");
}
#[tokio::test]
async fn explicit_retry_intervening_before_completion_records_conflict_and_failure() {
    let (f, land, target) = setup(true).await;
    f.retry(&["work"], Some("explicit retry")).await;
    let result = f.finish(&land, rejected(), json!({})).await;
    let Some(CompletionActionOutcome::Conflict(conflict)) = result.action else {
        panic!("missing conflict")
    };
    assert_eq!(conflict.expected, target);
    assert_ne!(conflict.current.as_ref().unwrap().work, target.work);
    assert_eq!(result.status, StepStatus::Failed);
    assert_eq!(f.state().await.status(&id("land")), StepStatus::Failed);
    assert_eq!(f.counts().await.3, 1);
}
#[tokio::test]
async fn removed_or_replaced_target_does_not_abort_terminal_transaction() {
    for replacement in [false, true] {
        let (mut f, land, target) = setup(false).await;
        f.apply(json!({"steps":{"land":{"run":"empty","after":[]}}}))
            .await;
        if replacement {
            f.apply(json!({"steps":{"work":{"run":"core.external","outputs":{"ready":"boolean"}},"land":{"run":"empty","after":[]}}})).await;
            f.manual("work", json!({"ready":true}), false).await;
        }
        let result = f.finish(&land, rejected(), json!({})).await;
        let Some(CompletionActionOutcome::Conflict(conflict)) = result.action else {
            panic!("missing conflict")
        };
        assert_eq!(result.status, StepStatus::Failed);
        assert_eq!(f.state().await.status(&id("land")), StepStatus::Failed);
        assert_eq!(f.counts().await.3, 0);
        if replacement {
            assert_ne!(conflict.current.unwrap().generation, target.generation);
        } else {
            assert_eq!(conflict.current, None);
        }
    }
}
#[tokio::test]
async fn intervening_completed_attempt_has_a_different_result_and_attempt_identity() {
    let (f, land, target) = setup(false).await;
    // Accepting a newer manual result changes ResultId even with the same values.
    f.manual("work", json!({"ready":true}), false).await;
    let result = f.finish(&land, rejected(), json!({})).await;
    let Some(CompletionActionOutcome::Conflict(conflict)) = result.action else {
        panic!("missing conflict")
    };
    assert_ne!(conflict.current.unwrap().result, target.result);
    assert_eq!(f.state().await.status(&id("land")), StepStatus::Failed);
    // A real newer execution also changes the completed AttemptId.
    let f = Fixture::new(json!({"steps":{"work":{"run":"empty"},"land":{"run":"empty"}}})).await;
    let r = f.reserve("work", json!({})).await;
    f.finish(&r, CompletionKind::Succeeded, json!({})).await;
    let land = f.reserve("land", json!({})).await;
    let project = f.context.project;
    let run = land.identity.run;
    let target = f
        .writer
        .write(RetrySafety::Idempotent, move |tx| {
            let target = completion_target(tx, project, &id("work"))?.unwrap();
            register_completion_action(
                tx,
                RegisterCompletionAction {
                    project,
                    run,
                    target: target.clone(),
                    message: "fix".into(),
                    author: "sam".into(),
                },
                &Hooks::default(),
            )?;
            Ok(target)
        })
        .await
        .unwrap();
    assert_eq!(target.result_attempt, Some(r.identity.attempt));
    f.retry(&["work"], None).await;
    let newer = f.reserve("work", json!({})).await;
    f.finish(&newer, CompletionKind::Succeeded, json!({})).await;
    let result = f.finish(&land, rejected(), json!({})).await;
    let Some(CompletionActionOutcome::Conflict(conflict)) = result.action else {
        panic!("missing conflict")
    };
    assert_eq!(
        conflict.current.unwrap().result_attempt,
        Some(newer.identity.attempt)
    );
    assert_eq!(f.state().await.status(&id("land")), StepStatus::Failed);
}
#[tokio::test]
async fn completion_replay_returns_persisted_applied_decision_and_changes_nothing() {
    let (f, land, _) = setup(true).await;
    let original = f.finish(&land, rejected(), json!({})).await;
    f.manual("work", json!({"ready":false}), false).await;
    let before = f.counts().await;
    let state = f.state().await;
    let replay = f.finish(&land, CompletionKind::Succeeded, json!({})).await;
    assert_eq!(replay, original);
    assert_eq!(f.counts().await, before);
    assert_eq!(f.state().await, state);
}
#[tokio::test]
async fn conflict_replay_never_retries_a_later_valid_target() {
    let (f, land, _) = setup(false).await;
    f.retry(&["work"], None).await;
    let original = f.finish(&land, rejected(), json!({})).await;
    assert!(matches!(
        original.action,
        Some(CompletionActionOutcome::Conflict(_))
    ));
    f.manual("work", json!({"ready":true}), false).await;
    let before = f.counts().await;
    let replay = f.finish(&land, rejected(), json!({})).await;
    assert_eq!(replay, original);
    assert_eq!(f.counts().await, before);
    assert_eq!(f.state().await.status(&id("work")), StepStatus::Succeeded);
}
#[tokio::test]
async fn success_cancellation_process_loss_unknown_and_generic_failure_discard_actions() {
    for kind in [
        CompletionKind::Succeeded,
        CompletionKind::Cancelled {
            message: "cancelled".into(),
        },
        CompletionKind::Lost {
            message: "lost".into(),
        },
        CompletionKind::Unknown {
            message: "uncertain".into(),
        },
        CompletionKind::Failed(PublicError::FnFailure {
            message: "other error".into(),
        }),
    ] {
        let (f, land, _) = setup(false).await;
        let result = f.finish(&land, kind, json!({})).await;
        assert_eq!(result.action, Some(CompletionActionOutcome::Discarded));
        assert_eq!(f.state().await.status(&id("work")), StepStatus::Succeeded);
        assert_eq!(f.counts().await.3, 0);
    }
}
#[tokio::test]
async fn semantic_message_refusal_commits_failure_and_visible_conflict_without_retry() {
    let (f, land, _) = setup(true).await;
    let context = f.context.clone();
    let identity = land.identity;
    let outcome = f
        .writer
        .write(RetrySafety::Idempotent, move |tx| {
            complete(
                tx,
                &context,
                Complete {
                    completion_id: "policy-conflict".into(),
                    identity,
                    kind: rejected(),
                    outputs: JsonMap::default(),
                    processes_gone: true,
                    submission_version: None,
                },
                &mut Hooks {
                    refuse: true,
                    ..Hooks::default()
                },
            )
        })
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(outcome.action,Some(CompletionActionOutcome::Conflict(CompletionActionConflict{message,..})) if message=="message policy changed")
    );
    assert_eq!(f.state().await.status(&id("land")), StepStatus::Failed);
    assert_eq!(f.state().await.status(&id("work")), StepStatus::Succeeded);
    assert_eq!(f.counts().await.3, 0);
}
#[tokio::test]
async fn action_registration_is_idempotent_but_cannot_recapture_a_new_target() {
    let (f, land, target) = setup(false).await;
    let request = RegisterCompletionAction {
        project: f.context.project,
        run: land.identity.run,
        target: target.clone(),
        message: "please fix".into(),
        author: "sam".into(),
    };
    let before = f.counts().await;
    let copy = request.clone();
    assert!(
        f.writer
            .write(RetrySafety::Idempotent, move |tx| {
                register_completion_action(tx, copy, &Hooks::default())
            })
            .await
            .unwrap()
    );
    assert_eq!(f.counts().await, before);
    f.manual("work", json!({"ready":true}), false).await;
    let project = f.context.project;
    let newer = f
        .writer
        .write(RetrySafety::Idempotent, move |tx| {
            completion_target(tx, project, &id("work"))
        })
        .await
        .unwrap()
        .unwrap();
    let request = RegisterCompletionAction {
        target: newer,
        ..request
    };
    assert!(matches!(
        f.writer
            .write(RetrySafety::Idempotent, move |tx| {
                register_completion_action(tx, request, &Hooks::default())
            })
            .await
            .unwrap_err(),
        PublicError::Conflict { .. }
    ));
}
#[tokio::test]
async fn concurrent_explicit_retry_and_rejected_completion_advance_work_once() {
    let (f, land, _) = setup(false).await;
    let retry_context = f.context.clone();
    let complete_context = f.context.clone();
    let request = retry_request(f.context.project, &["work"], Some("explicit"));
    let identity = land.identity;
    let (retry, completion) = tokio::join!(
        f.writer
            .write(RetrySafety::NonIdempotent, move |tx| plans::step_retry(
                tx,
                &retry_context,
                request,
                &mut Hooks::default()
            )),
        f.writer.write(RetrySafety::Idempotent, move |tx| complete(
            tx,
            &complete_context,
            Complete {
                identity,
                completion_id: "race".into(),
                kind: rejected(),
                outputs: JsonMap::default(),
                processes_gone: true,
                submission_version: None
            },
            &mut Hooks::default()
        ))
    );
    let result = completion.unwrap().unwrap();
    match result.action.unwrap() {
        CompletionActionOutcome::Applied(_) => assert!(retry.is_err()),
        CompletionActionOutcome::Conflict(_) => assert!(retry.is_ok()),
        CompletionActionOutcome::Discarded => panic!("discarded"),
    };
    let work: i64 = f
        .reads
        .snapshot(|c| {
            Ok(c.query_row(
                "SELECT work_generation FROM steps WHERE step_id='work'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(work, 2);
    assert_eq!(f.counts().await.3, 1);
    assert_eq!(f.state().await.status(&id("land")), StepStatus::Failed);
}
#[tokio::test]
async fn completion_storage_failure_rolls_back_terminal_state_and_can_replay_journal() {
    let (f, land, _) = setup(true).await;
    let before = f.counts().await;
    let context = f.context.clone();
    let identity = land.identity.clone();
    assert!(
        f.writer
            .write(RetrySafety::Idempotent, move |tx| complete(
                tx,
                &context,
                Complete {
                    identity,
                    completion_id: "storage-fail".into(),
                    kind: rejected(),
                    outputs: JsonMap::default(),
                    processes_gone: true,
                    submission_version: None
                },
                &mut Hooks {
                    storage_fail: true,
                    ..Hooks::default()
                }
            ))
            .await
            .is_err()
    );
    assert_eq!(f.counts().await, before);
    assert_eq!(f.state().await.status(&id("land")), StepStatus::Running);
    let result = f.finish(&land, rejected(), json!({})).await;
    assert!(matches!(
        result.action,
        Some(CompletionActionOutcome::Applied(_))
    ));
}

#[tokio::test]
async fn output_validation_failure_discards_the_registered_action() {
    let f = Fixture::new(
        json!({"steps":{"work":{"run":"empty"},"land":{"run":"int","in":{"value":{"default":1}}}}}),
    )
    .await;
    f.manual("work", json!({}), false).await;
    let land = f.reserve("land", json!({"value":1})).await;
    let project = f.context.project;
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
                    message: "fix".into(),
                    author: "sam".into(),
                },
                &Hooks::default(),
            )
        })
        .await
        .unwrap();
    let outcome = f
        .finish(&land, CompletionKind::Succeeded, json!({"value":"bad"}))
        .await;
    assert_eq!(outcome.status, StepStatus::Failed);
    assert!(matches!(outcome.error, Some(PublicError::Invalid { .. })));
    assert_eq!(outcome.action, Some(CompletionActionOutcome::Discarded));
    assert_eq!(f.state().await.status(&id("work")), StepStatus::Succeeded);
    assert_eq!(f.counts().await.3, 0);
}
#[tokio::test]
async fn identical_registration_replays_without_revalidating_policy_or_changing_author() {
    let (f, land, target) = setup(false).await;
    let before = f.counts().await;
    let request = RegisterCompletionAction {
        project: f.context.project,
        run: land.identity.run,
        target,
        message: "please fix".into(),
        author: "new caller".into(),
    };
    assert!(
        f.writer
            .write(RetrySafety::Idempotent, move |tx| {
                register_completion_action(
                    tx,
                    request,
                    &Hooks {
                        refuse: true,
                        ..Hooks::default()
                    },
                )
            })
            .await
            .unwrap()
    );
    assert_eq!(f.counts().await, before);
    let author: String = f
        .reads
        .snapshot(|c| {
            Ok(c.query_row(
                "SELECT json_extract(completion_action,'$.author') FROM runs WHERE step_id='land'",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(author, "sam");
}

#[tokio::test]
async fn frozen_completion_persists_action_conflict_when_current_plan_is_invalid() {
    let (f, land, target) = setup(true).await;
    let admitted = f.context.clone();
    let identity = land.identity.clone();
    let outcome = f
        .writer
        .write(RetrySafety::Idempotent, move |tx| {
            complete_frozen(
                tx,
                CompletionContext {
                    admitted: &admitted,
                    current: Err("sibling signature no longer declares its consumed output"),
                },
                Complete {
                    completion_id: identity.run.to_string(),
                    identity,
                    kind: rejected(),
                    outputs: map(json!({})),
                    processes_gone: true,
                    submission_version: None,
                },
                &mut Hooks::default(),
            )
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.status, StepStatus::Failed);
    let Some(CompletionActionOutcome::Conflict(conflict)) = &outcome.action else {
        panic!("missing persisted conflict")
    };
    assert_eq!(conflict.expected, target);
    assert!(conflict.message.contains("sibling signature"));
    let counts = f.counts().await;
    assert_eq!(f.finish(&land, rejected(), json!({})).await, outcome);
    assert_eq!(f.counts().await, counts);
    assert_eq!(f.state().await.status(&id("work")), StepStatus::Succeeded);
    assert_eq!(f.state().await.status(&id("land")), StepStatus::Failed);
}
