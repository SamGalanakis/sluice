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
                "agentish" => (
                    json!({"cwd":"string?"}),
                    json!({"session":"string?","git":"Any?"}),
                    true,
                ),
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

/// A plan whose `work` runs an agent-like open fn that declares `summary`.
async fn agentish() -> Fixture {
    Fixture::new(json!({"steps":{"work":{"run":"agentish","outputs":{"summary":"string"}}}})).await
}
async fn submit(f: &Fixture, run: &Reservation, outputs: serde_json::Value) {
    let request = StepSubmit {
        project: f.context.project,
        step: id("work"),
        run: run.identity.run,
        outputs: map(outputs),
        author: Some("agent".into()),
    };
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            step_submit(tx, request)
        })
        .await
        .unwrap()
        .unwrap();
}
async fn note(f: &Fixture, run: Option<&Reservation>) -> sluice_model::attempt::AttemptNote {
    let project = f.context.project;
    let current = run.map(|r| r.identity.run);
    f.reads
        .snapshot(move |sql| attempt_note(sql, project, &id("work"), current))
        .await
        .unwrap()
}
async fn finish_with(
    f: &Fixture,
    run: &Reservation,
    kind: CompletionKind,
    outputs: serde_json::Value,
) -> CompletionResult {
    let context = f.context.clone();
    let identity = run.identity.clone();
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            let version: Option<i64> = rusqlite::OptionalExtension::optional(tx.sql().query_row(
                "SELECT version FROM submissions WHERE run_id=?1",
                [identity.run.to_string()],
                |r| r.get(0),
            ))?;
            complete(
                tx,
                &context,
                Complete {
                    completion_id: identity.run.to_string(),
                    identity,
                    kind,
                    outputs: map(outputs),
                    processes_gone: true,
                    submission_version: version.map(|v| v as u64),
                },
                &mut Hooks::default(),
            )
        })
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn a_submitted_run_is_finishing_and_a_settled_one_succeeds_with_the_recorded_outputs() {
    let f = agentish().await;
    let run = f.reserve("work", json!({})).await;
    f.start(&run).await;
    let project = f.context.project;
    let finishing = || async {
        f.reads
            .snapshot(move |sql| finishing(sql, project))
            .await
            .unwrap()
    };
    assert!(finishing().await.is_empty());
    let identity = run.identity.clone();
    let outputs = map(json!({"summary":"fixed","session":"s-3"}));
    let settled = outputs.clone();
    // Not finishing until it has submitted: refused, nothing written.
    let refused = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            settle(
                tx,
                &identity,
                &settled,
                "owner".into(),
                "old release".into(),
            )
        })
        .await;
    assert!(
        matches!(&refused, Err(PublicError::Conflict { message, .. }) if message.contains("no longer finishing")),
        "{refused:?}"
    );
    submit(&f, &run, json!({"summary":"fixed"})).await;
    let found = finishing().await;
    let entry = &found[&id("work")];
    assert_eq!(entry.release, "fixture");
    assert!(entry.submission_seq.is_some() && !entry.since.is_empty());
    let identity = run.identity.clone();
    let settled = outputs.clone();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            settle(
                tx,
                &identity,
                &settled,
                "owner".into(),
                "old release".into(),
            )
        })
        .await
        .unwrap();
    let result = finish_with(
        &f,
        &run,
        CompletionKind::Cancelled {
            message: "cancel intent".into(),
        },
        json!({}),
    )
    .await;
    assert_eq!(result.status, StepStatus::Succeeded);
    assert_eq!(result.error, None);
    assert_eq!(result.outputs, outputs);
    let state = f.state().await;
    assert_eq!(state.status(&id("work")), StepStatus::Succeeded);
    assert!(finishing().await.is_empty());
    f.retry(&["work"], None).await;
    let next = f.reserve("work", json!({})).await;
    let note = note(&f, Some(&next)).await;
    assert!(
        note.text()
            .contains("was settled on its submission by owner (old release)."),
        "{}",
        note.text()
    );
}

/// `work` runs an agent-like open fn and declares outputs; `next` reads one of them, `gated`
/// waits on another, and `many` scatters.
async fn rolling() -> Fixture {
    Fixture::new(json!({"steps":{
        "work":{"run":"agentish","outputs":{"summary":"string","red":"int","green":"boolean?"}},
        "next":{"run":"echo","in":{"value":{"source":"work/summary"}}},
        "gated":{"run":"empty","after":["work/green"]},
        "many":{"run":"echo","scatter":"value","in":{"value":{"default":[1,2]}}}}}))
    .await
}
async fn progress(
    f: &Fixture,
    step: &str,
    run: sluice_model::ids::RunId,
    outputs: serde_json::Value,
) -> Result<serde_json::Value, PublicError> {
    let request = StepProgress {
        project: sluice_model::ids::ProjectSelector::Id(f.context.project),
        step: id(step),
        run,
        outputs: map(outputs),
    };
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            step_progress(tx, request)
        })
        .await
}
async fn read(f: &Fixture) -> Option<Progress> {
    let project = f.context.project;
    f.reads
        .snapshot(move |c| read_progress(c, project, "work"))
        .await
        .unwrap()
}
fn invalid_errors(result: Result<serde_json::Value, PublicError>) -> Vec<String> {
    match result {
        Err(PublicError::Invalid { errors, .. }) => errors,
        other => panic!("expected invalid, got {other:?}"),
    }
}

#[tokio::test]
async fn progress_is_checked_against_the_steps_outputs_and_comes_from_its_current_run() {
    let f = rolling().await;
    let stranger = sluice_model::ids::RunId::new();
    // Not running yet: refused.
    let refused = progress(&f, "work", stranger, json!({"red":1})).await;
    assert!(
        matches!(&refused, Err(PublicError::Conflict { message, .. }) if message.contains("not running")),
        "{refused:?}"
    );
    let run = f.reserve("work", json!({})).await;
    f.start(&run).await;
    let current = run.identity.run;
    // A value of the wrong type, an unknown field and nothing at all are invalid.
    let errors = invalid_errors(progress(&f, "work", current, json!({"red":"three"})).await);
    assert!(
        errors.iter().any(|e| e.contains("outputs.red")),
        "{errors:?}"
    );
    let errors = invalid_errors(progress(&f, "work", current, json!({"nope":1,"red":2})).await);
    assert_eq!(errors, ["outputs.nope: not an output of step work"]);
    invalid_errors(progress(&f, "work", current, json!({})).await);
    // Another run is not the step's current one.
    let wrong = progress(&f, "work", stranger, json!({"red":1})).await;
    assert!(
        matches!(&wrong, Err(PublicError::Conflict { message, .. }) if message.contains("not the current run")),
        "{wrong:?}"
    );
    // A step not in the plan.
    assert!(matches!(
        progress(&f, "ghost", current, json!({"red":1})).await,
        Err(PublicError::NotFound { .. })
    ));
    // Nothing was stored by the refusals; the fn's own outputs are fields too.
    assert_eq!(read(&f).await, None);
    let set = progress(&f, "work", current, json!({"session":"s-1"}))
        .await
        .unwrap();
    assert_eq!(set["progress"], json!({"session":"s-1"}));
    // A scattered step's items do not publish progress.
    let request = f.request("many", json!({"value":1}), 0, Some(2)).await;
    let item_run = request.run;
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            reserve(tx, &context, request, &mut Hooks::default())
        })
        .await
        .unwrap();
    let errors = invalid_errors(progress(&f, "many", item_run, json!({"value":[1]})).await);
    assert!(errors[0].contains("scattered"), "{errors:?}");
}

#[tokio::test]
async fn progress_merges_and_feeds_no_reader_or_gate_and_writes_no_record() {
    let f = rolling().await;
    let run = f.reserve("work", json!({})).await;
    f.start(&run).await;
    let current = run.identity.run;
    let before = f.counts().await;
    let first = progress(&f, "work", current, json!({"summary":"half","red":3}))
        .await
        .unwrap();
    let second = progress(&f, "work", current, json!({"red":2,"green":true}))
        .await
        .unwrap();
    assert_eq!(
        second["progress"],
        json!({"summary":"half","red":2,"green":true})
    );
    assert_eq!(second["run"], json!(current));
    assert!(first["at"].is_string() && second["at"].is_string());
    // No record: nothing that waits on the log wakes for it.
    assert_eq!(f.counts().await, before);
    // Never final: the step has no outputs, its reader and its gate still wait.
    let state = f.state().await;
    assert_eq!(state.status(&id("work")), StepStatus::Running);
    assert!(state.steps[&id("work")].outputs.0.is_empty());
    let plan = &f.context.plan;
    for reader in ["next", "gated"] {
        assert_eq!(state.status(&id(reader)), StepStatus::Pending);
        let decision = sluice_model::gates::evaluate_step(plan, &state, &plan.steps()[&id(reader)]);
        assert!(
            !matches!(decision, sluice_model::gates::GateDecision::Ready),
            "{reader}: {decision:?}"
        );
    }
    let shown = read(&f).await.unwrap();
    assert!(shown.live && !shown.superseded);
    assert_eq!(shown.fresher("red"), Some(&json!(2)));
    assert_eq!(shown.run.as_deref(), Some(current.to_string().as_str()));
    // The query tool reads it from steps.
    let rows = sluice_store::query::query(
        f.home.path(),
        "SELECT json_extract(progress, '$.red'), progress_at IS NOT NULL, progress_run FROM steps WHERE project_id = ? AND step_id = 'work'",
        Some(&[rusqlite::types::Value::Text(f.context.project.to_string())]),
        None,
    )
    .unwrap();
    assert_eq!(
        rows.rows()[0],
        [
            sluice_store::query::QueryCell::Integer(2),
            sluice_store::query::QueryCell::Integer(1),
            sluice_store::query::QueryCell::Text(current.to_string())
        ]
    );
}

#[tokio::test]
async fn progress_outlives_its_run_and_clears_when_the_next_run_starts() {
    let f = rolling().await;
    let run = f.reserve("work", json!({})).await;
    f.start(&run).await;
    progress(&f, "work", run.identity.run, json!({"red":5}))
        .await
        .unwrap();
    finish_with(
        &f,
        &run,
        CompletionKind::Succeeded,
        json!({"summary":"done","red":0}),
    )
    .await;
    // Kept after its run, but the outputs that came after it are the fresher values.
    let kept = read(&f).await.unwrap();
    assert!(!kept.live && kept.superseded);
    assert_eq!(kept.outputs["red"], json!(5));
    assert_eq!(kept.fresher("red"), None);
    // The finished run may not publish any more.
    let late = progress(&f, "work", run.identity.run, json!({"red":6})).await;
    assert!(
        matches!(late, Err(PublicError::Conflict { .. })),
        "{late:?}"
    );
    f.retry(&["work"], None).await;
    // Still there while the retried step waits to start ...
    assert!(read(&f).await.is_some());
    // ... and gone once its next run starts.
    let next = f.reserve("work", json!({})).await;
    assert_eq!(read(&f).await, None);
    f.start(&next).await;
    // The new run's progress starts afresh: nothing of the old run's merges in.
    let set = progress(&f, "work", next.identity.run, json!({"green":false}))
        .await
        .unwrap();
    assert_eq!(set["progress"], json!({"green":false}));
    // A run that fails leaves its progress as the freshest values it has.
    finish_with(
        &f,
        &next,
        CompletionKind::Failed(PublicError::FnFailure {
            message: "suite crashed".into(),
        }),
        json!({}),
    )
    .await;
    let failed = read(&f).await.unwrap();
    assert!(!failed.live && !failed.superseded);
    assert_eq!(failed.fresher("green"), Some(&json!(false)));
}
