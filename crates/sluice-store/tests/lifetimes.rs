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
use sluice_model::{commands::*, error::PublicError, ids::*, rpc::JsonMap};
use sluice_store::{RetrySafety, attempts::*, plans};
use support::*;

#[tokio::test]
async fn remove_reintroduce_resets_generation_even_for_never_run_steps() {
    let doc = json!({"steps":{"a":{"run":"empty"}},"inputs":{"n":"int"}});
    let mut f = Fixture::new(doc.clone()).await;
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::set_input(
                tx,
                &context,
                "n",
                json!(4).try_into().unwrap(),
                "sam".into(),
                "value".into(),
            )
        })
        .await
        .unwrap();
    let first: i64 = f
        .reads
        .snapshot(|c| Ok(c.query_row("SELECT generation FROM steps", [], |r| r.get(0))?))
        .await
        .unwrap();
    f.apply(json!({"steps":{}})).await;
    f.apply(doc).await;
    let next: i64 = f
        .reads
        .snapshot(|c| Ok(c.query_row("SELECT generation FROM steps", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert!(next > first);
    assert!(f.state().await.inputs.0.is_empty());
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Pending);
}
#[tokio::test]
async fn reused_id_in_single_patch_keeps_generation_state_and_result() {
    let f = Fixture::new(json!({"steps":{"a":{"run":"empty"}}})).await;
    let original = f.manual("a", json!({}), false).await;
    let mut prepared = edit(
        &f.context,
        json!({"steps":{"a":{"run":"empty","tags":["new"]}}}),
    );
    prepared.ops = vec![
        PatchOperation::Remove {
            path: "/steps/a".into(),
        },
        PatchOperation::Add {
            path: "/steps/a".into(),
            value: json!({"run":"empty","tags":["new"]}).try_into().unwrap(),
        },
    ];
    let project = f.context.project;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::apply_edit(tx, project, prepared)
        })
        .await
        .unwrap();
    let (generation, result): (i64, String) = f
        .reads
        .snapshot(|c| {
            Ok(
                c.query_row("SELECT generation,result_id FROM steps", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(generation, 1);
    assert_eq!(result, original.to_string());
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Succeeded);
}
#[tokio::test]
async fn reservation_assigns_messages_but_only_started_advances_cursor() {
    let f = Fixture::new(json!({"steps":{"a":{"run":"empty"}}})).await;
    f.manual("a", json!({}), false).await;
    f.retry(&["a"], Some("first message")).await;
    let r = f.reserve("a", json!({})).await;
    assert!(r.messages.through > 0);
    let cursor: i64 = f
        .reads
        .snapshot(|c| Ok(c.query_row("SELECT delivery_cursor FROM steps", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(cursor, 0);
    let identity = r.identity.clone();
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            assert!(spawn_attempted(tx, &identity)?);
            assert!(!spawn_attempted(tx, &identity)?);
            assert!(claim(tx, &identity, guardian(identity.run))?);
            assert!(!claim(tx, &identity, guardian(identity.run))?);
            Ok(())
        })
        .await
        .unwrap();
    let cursor: i64 = f
        .reads
        .snapshot(|c| Ok(c.query_row("SELECT delivery_cursor FROM steps", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(cursor, 0);
    let identity = r.identity;
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            assert!(started(tx, &identity, &mut Hooks::default())?);
            assert!(!started(tx, &identity, &mut Hooks::default())?);
            Ok(())
        })
        .await
        .unwrap();
    let cursor: i64 = f
        .reads
        .snapshot(|c| Ok(c.query_row("SELECT delivery_cursor FROM steps", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(cursor, r.messages.through);
}
#[tokio::test]
async fn failed_before_start_keeps_messages_for_retry_and_records_predecessor() {
    let f = Fixture::new(json!({"steps":{"a":{"run":"empty"}}})).await;
    f.manual("a", json!({}), false).await;
    f.retry(&["a"], Some("message")).await;
    let r = f.reserve("a", json!({})).await;
    f.finish(
        &r,
        CompletionKind::Lost {
            message: "before start".into(),
        },
        json!({}),
    )
    .await;
    f.retry(&["a"], None).await;
    let next = f.reserve("a", json!({})).await;
    assert_eq!(next.prev_run, Some(r.identity.run));
    assert_eq!(next.messages, r.messages);
}
#[tokio::test]
async fn reserve_replay_is_read_only_and_changed_request_conflicts() {
    let f = Fixture::new(json!({"steps":{"a":{"run":"echo","in":{"value":{"default":1}}}}})).await;
    let request = f.request("a", json!({"value":1}), -1, None).await;
    let copy = request.clone();
    let context = f.context.clone();
    let r = f
        .writer
        .write(RetrySafety::Idempotent, move |tx| {
            reserve(tx, &context, copy, &mut Hooks::default())
        })
        .await
        .unwrap();
    let before = f.counts().await;
    let context = f.context.clone();
    let copy = request.clone();
    let replay = f
        .writer
        .write(RetrySafety::Idempotent, move |tx| {
            reserve(tx, &context, copy, &mut Hooks::default())
        })
        .await
        .unwrap();
    assert_eq!(r, replay);
    assert_eq!(f.counts().await, before);
    let context = f.context.clone();
    let bad = Reserve {
        inputs: map(json!({"value":2})),
        ..request
    };
    assert!(matches!(
        f.writer
            .write(RetrySafety::Idempotent, move |tx| reserve(
                tx,
                &context,
                bad,
                &mut Hooks::default()
            ))
            .await
            .unwrap_err(),
        PublicError::Conflict { .. }
    ));
}
#[tokio::test]
async fn stale_generation_and_attempt_callbacks_are_noops() {
    let f = Fixture::new(json!({"steps":{"a":{"run":"empty"}}})).await;
    let r = f.reserve("a", json!({})).await;
    let mut wrong = r.identity.clone();
    wrong.generation = StepGeneration(99);
    let before = f.counts().await;
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            assert!(!claim(tx, &wrong, guardian(wrong.run))?);
            assert!(!started(tx, &wrong, &mut Hooks::default())?);
            assert!(!cancel(tx, &wrong, "sam".into(), "cancel".into())?);
            assert!(
                complete(
                    tx,
                    &context,
                    Complete {
                        identity: wrong,
                        completion_id: "stale".into(),
                        kind: CompletionKind::Succeeded,
                        outputs: JsonMap::default(),
                        processes_gone: true,
                        submission_version: None
                    },
                    &mut Hooks::default()
                )?
                .is_none()
            );
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(f.counts().await, before);
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Running);
}
#[tokio::test]
async fn cleanup_and_cancel_intent_guard_claim_start_and_success() {
    let f = Fixture::new(json!({"steps":{"a":{"run":"empty"}}})).await;
    let r = f.reserve("a", json!({})).await;
    let context = f.context.clone();
    let identity = r.identity.clone();
    let before = f.counts().await;
    assert!(
        f.writer
            .write(RetrySafety::Idempotent, move |tx| complete(
                tx,
                &context,
                Complete {
                    identity,
                    completion_id: "unclean".into(),
                    kind: CompletionKind::Succeeded,
                    outputs: JsonMap::default(),
                    processes_gone: false,
                    submission_version: None
                },
                &mut Hooks::default()
            ))
            .await
            .is_err()
    );
    assert_eq!(f.counts().await, before);
    let identity = r.identity.clone();
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            assert!(cancel(tx, &identity, "sam".into(), "stop".into())?);
            assert!(!claim(tx, &identity, guardian(identity.run))?);
            assert!(!started(tx, &identity, &mut Hooks::default())?);
            Ok(())
        })
        .await
        .unwrap();
    let outcome = f.finish(&r, CompletionKind::Succeeded, json!({})).await;
    assert!(matches!(outcome.error, Some(PublicError::Cancelled { .. })));
    assert_eq!(outcome.status, StepStatus::Failed);
}
#[tokio::test]
async fn lost_replay_changes_no_records_and_old_callbacks_cannot_touch_new_work() {
    let f = Fixture::new(json!({"steps":{"a":{"run":"empty"}}})).await;
    let r = f.reserve("a", json!({})).await;
    let context = f.context.clone();
    let identity = r.identity.clone();
    let outcome = f
        .writer
        .write(RetrySafety::Idempotent, move |tx| {
            lost(
                tx,
                &context,
                identity,
                "lost-id".into(),
                true,
                &mut Hooks::default(),
            )
        })
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        outcome.error,
        Some(PublicError::ProcessLost { .. })
    ));
    f.retry(&["a"], None).await;
    let next = f.reserve("a", json!({})).await;
    let before = f.counts().await;
    let context = f.context.clone();
    let identity = r.identity;
    let replay = f
        .writer
        .write(RetrySafety::Idempotent, move |tx| {
            assert!(!claim(tx, &identity, guardian(identity.run))?);
            assert!(!started(tx, &identity, &mut Hooks::default())?);
            lost(
                tx,
                &context,
                identity,
                "lost-id".into(),
                true,
                &mut Hooks::default(),
            )
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replay, outcome);
    assert_eq!(f.counts().await, before);
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Running);
    assert_ne!(next.identity.work, WorkGeneration(1));
}
#[tokio::test]
async fn failed_scatter_keeps_good_item_and_unstarted_item_keeps_assigned_range() {
    let f = Fixture::new(
        json!({"steps":{"a":{"run":"int","in":{"value":{"default":[1,2]}},"scatter":"value"}}}),
    )
    .await;
    f.fail_projection("a").await;
    f.retry(&["a"], Some("batch message")).await;
    let context = f.context.clone();
    let r0 = f.request("a", json!({"value":1}), 0, Some(2)).await;
    let r1 = f.request("a", json!({"value":2}), 1, Some(2)).await;
    let (r0, r1) = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            Ok((
                reserve(tx, &context, r0, &mut Hooks::default())?,
                reserve(tx, &context, r1, &mut Hooks::default())?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(r0.messages, r1.messages);
    f.start(&r0).await;
    f.finish(&r0, CompletionKind::Succeeded, json!({"value":1}))
        .await;
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Running);
    f.finish(
        &r1,
        CompletionKind::Lost {
            message: "no launch".into(),
        },
        json!({}),
    )
    .await;
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Failed);
    f.retry(&["a"], Some("new feedback")).await;
    let context = f.context.clone();
    let request = f.request("a", json!({"value":2}), 1, Some(2)).await;
    let next = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            reserve(tx, &context, request, &mut Hooks::default())
        })
        .await
        .unwrap();
    assert_eq!(next.messages.after, r1.messages.after);
    assert!(next.messages.through > r1.messages.through);
    let run = next.identity.run;
    let (delivered, done): (i64, i64) = f
        .reads
        .snapshot(move |c| {
            Ok((
                c.query_row(
                    "SELECT count(*) FROM message_deliveries WHERE run_id=?1",
                    [run.to_string()],
                    |r| r.get(0),
                )?,
                c.query_row("SELECT done FROM steps", [], |r| r.get(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(delivered, 2);
    assert_eq!(done, 1);
    assert_eq!(next.prev_run, Some(r1.identity.run));
    f.finish(&next, CompletionKind::Succeeded, json!({"value":2}))
        .await;
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Succeeded);
    assert_eq!(
        f.state().await.steps[&id("a")].outputs,
        map(json!({"value":[1,2]}))
    );
}
#[tokio::test]
async fn scatter_reorder_forbids_predecessor_resume_and_clears_kept_items() {
    let mut f = Fixture::new(
        json!({"steps":{"a":{"run":"int","in":{"value":{"default":[1,2]}},"scatter":"value"}}}),
    )
    .await;
    let context = f.context.clone();
    let q0 = f.request("a", json!({"value":1}), 0, Some(2)).await;
    let q1 = f.request("a", json!({"value":2}), 1, Some(2)).await;
    let (r0, r1) = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            Ok((
                reserve(tx, &context, q0, &mut Hooks::default())?,
                reserve(tx, &context, q1, &mut Hooks::default())?,
            ))
        })
        .await
        .unwrap();
    f.finish(&r0, CompletionKind::Succeeded, json!({"value":1}))
        .await;
    f.finish(
        &r1,
        CompletionKind::Failed(PublicError::FnFailure {
            message: "bad".into(),
        }),
        json!({}),
    )
    .await;
    f.retry(&["a"], None).await;
    f.apply(
        json!({"steps":{"a":{"run":"int","in":{"value":{"default":[2,1]}},"scatter":"value"}}}),
    )
    .await;
    let context = f.context.clone();
    let request = f.request("a", json!({"value":2}), 0, Some(2)).await;
    let next = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            reserve(tx, &context, request, &mut Hooks::default())
        })
        .await
        .unwrap();
    assert_eq!(next.prev_run, None);
}

#[tokio::test]
async fn scatter_run_ids_follow_numeric_item_order_above_ten_items() {
    let values: Vec<i64> = (0..12).collect();
    let f = Fixture::new(
        json!({"steps":{"a":{"run":"int","in":{"value":{"default":values}},"scatter":"value"}}}),
    )
    .await;
    let mut requests = vec![];
    for i in [0, 10, 11, 1, 2, 3, 4, 5, 6, 7, 8, 9] {
        requests.push(f.request("a", json!({"value":i}), i, Some(12)).await);
    }
    let context = f.context.clone();
    let expected = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let mut runs = vec![];
            for request in requests {
                let index = request.item_index;
                runs.push((
                    index,
                    reserve(tx, &context, request, &mut Hooks::default())?
                        .identity
                        .run,
                ));
            }
            runs.sort_by_key(|(index, _)| *index);
            Ok(runs.into_iter().map(|(_, run)| run).collect::<Vec<_>>())
        })
        .await
        .unwrap();
    let actual: Vec<RunId> = f
        .reads
        .snapshot(|c| {
            let value: String = c.query_row("SELECT run_ids FROM steps", [], |r| r.get(0))?;
            Ok(serde_json::from_str(&value)?)
        })
        .await
        .unwrap();
    assert_eq!(actual, expected);
}
