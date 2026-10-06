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
use sluice_model::{commands::StepStatus, error::PublicError};
use sluice_store::{RetrySafety, plans};
use support::*;

#[tokio::test]
async fn removed_manual_typed_result_survives_projection_and_feed_deletion() {
    let mut f=Fixture::new(json!({"steps":{"a":{"run":"core.external","outputs":{"word":"string","report":{"type":"record","fields":{"ok":"boolean","notes":"string[]"}}},"tags":["unit:lane"]}}})).await;
    let expected = json!({"word":"héllo","report":{"ok":true,"notes":["a","b"]}});
    f.manual("a", expected.clone(), false).await;
    f.apply(json!({"steps":{}})).await;
    let value:serde_json::Value=f.reads.snapshot(|c| {let (outputs,manual,unit,status):(String,bool,String,String)=c.query_row("SELECT outputs,manual,unit,status FROM outcomes",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;Ok(json!({"outputs":serde_json::from_str::<serde_json::Value>(&outputs)?,"manual":manual,"unit":unit,"status":status}))}).await.unwrap();
    assert_eq!(
        value,
        json!({"outputs":expected,"manual":true,"unit":"lane","status":"succeeded"})
    );
    f.writer
        .write(RetrySafety::NonIdempotent, |tx| {
            tx.sql().execute("DELETE FROM records", [])?;
            tx.changed(None, "log");
            Ok(())
        })
        .await
        .unwrap();
    let rows: i64 = f
        .reads
        .snapshot(|c| Ok(c.query_row("SELECT count(*) FROM outcomes", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(rows, 1);
    assert!(f.state().await.steps.is_empty());
}
#[tokio::test]
async fn removed_result_uses_recorded_unit_and_declaration_after_retagging() {
    let mut f = Fixture::new(
        json!({"steps":{"a":{"run":"core.external","outputs":{"n":"int"},"tags":["unit:old"]}}}),
    )
    .await;
    f.manual("a", json!({"n":1}), false).await;
    f.apply(
        json!({"steps":{"a":{"run":"core.external","outputs":{"n":"int"},"tags":["unit:new"]}}}),
    )
    .await;
    f.apply(json!({"steps":{}})).await;
    let (unit, declaration): (String, String) = f
        .reads
        .snapshot(|c| {
            Ok(
                c.query_row("SELECT unit,declaration FROM outcomes", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(unit, "old");
    assert!(declaration.contains("unit:old"));
}
#[tokio::test]
async fn terminal_snapshots_are_immutable_and_staleness_has_a_new_identity() {
    let f = Fixture::new(
        json!({"inputs":{"n":"Any"},"steps":{"a":{"run":"echo","in":{"value":{"source":"n"}}}}}),
    )
    .await;
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::set_input(
                tx,
                &context,
                "n",
                json!(1).try_into().unwrap(),
                "sam".into(),
                "value".into(),
            )
        })
        .await
        .unwrap();
    let original = f.manual("a", json!({"value":1}), false).await;
    let error = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE step_results SET outputs='{}' WHERE result_id=?1",
                [original.to_string()],
            )?;
            tx.changed(None, "log");
            Ok(())
        })
        .await
        .unwrap_err();
    assert!(matches!(error, PublicError::Invalid { .. }));
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::set_input(
                tx,
                &context,
                "n",
                json!(2).try_into().unwrap(),
                "sam".into(),
                "change".into(),
            )
        })
        .await
        .unwrap();
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Stale);
    let ids: (String, String) = f
        .reads
        .snapshot(move |c| {
            Ok((
                c.query_row("SELECT result_id FROM steps", [], |r| r.get(0))?,
                c.query_row(
                    "SELECT status FROM step_results WHERE result_id=?1",
                    [original.to_string()],
                    |r| r.get(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_ne!(ids.0, original.to_string());
    assert_eq!(ids.1, "succeeded");
}
#[tokio::test]
async fn failed_skipped_stale_are_archived_and_never_run_pending_is_not() {
    let mut f=Fixture::new(json!({"inputs":{"n":"Any","enabled":"boolean"},"steps":{"failed":{"run":"empty"},"stale":{"run":"echo","in":{"value":{"source":"n"}}},"skipped":{"run":"empty","after":["enabled"]},"pending":{"run":"empty"}}})).await;
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::set_input(
                tx,
                &context,
                "enabled",
                json!(false).try_into().unwrap(),
                "sam".into(),
                "skip".into(),
            )?;
            plans::set_input(
                tx,
                &context,
                "n",
                json!(1).try_into().unwrap(),
                "sam".into(),
                "input".into(),
            )
        })
        .await
        .unwrap();
    f.manual("stale", json!({"value":1}), false).await;
    f.fail_projection("failed").await;
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::set_input(
                tx,
                &context,
                "n",
                json!(2).try_into().unwrap(),
                "sam".into(),
                "stale".into(),
            )
        })
        .await
        .unwrap();
    f.apply(json!({"steps":{}})).await;
    let statuses: Vec<(String, String)> = f
        .reads
        .snapshot(|c| {
            let mut q = c.prepare("SELECT step_id,status FROM outcomes ORDER BY step_id")?;
            Ok(q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
        .unwrap();
    assert_eq!(
        statuses,
        vec![
            ("failed".into(), "failed".into()),
            ("skipped".into(), "skipped".into()),
            ("stale".into(), "stale".into())
        ]
    );
}
#[tokio::test]
async fn file_binding_path_changes_staleness_but_frozen_bytes_are_only_provenance() {
    let mut f =
        Fixture::new(json!({"steps":{"a":{"run":"file","in":{"brief":{"file":"/tmp/brief-a"}}}}}))
            .await;
    let request = f
        .request("a", json!({"brief":"first bytes"}), -1, None)
        .await;
    let context = f.context.clone();
    let r = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            sluice_store::attempts::reserve(tx, &context, request, &mut Hooks::default())
        })
        .await
        .unwrap();
    f.finish(
        &r,
        sluice_store::attempts::CompletionKind::Succeeded,
        json!({"value":1}),
    )
    .await;
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            plans::reconcile(tx, &context)
        })
        .await
        .unwrap();
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Succeeded);
    f.retry(&["a"], None).await;
    let r = f
        .reserve("a", json!({"brief":"new bytes at same path"}))
        .await;
    f.finish(
        &r,
        sluice_store::attempts::CompletionKind::Succeeded,
        json!({"value":1}),
    )
    .await;
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Succeeded);
    f.apply(json!({"steps":{"a":{"run":"file","in":{"brief":{"file":"/tmp/brief-b"}}}}}))
        .await;
    assert_eq!(f.state().await.status(&id("a")), StepStatus::Stale);
}
#[tokio::test]
async fn whole_patch_removal_archives_every_result_but_keeps_reused_id() {
    let mut f = Fixture::new(
        json!({"steps":{"a":{"run":"empty"},"b":{"run":"empty"},"c":{"run":"empty"}}}),
    )
    .await;
    for s in ["a", "b", "c"] {
        f.manual(s, json!({}), false).await;
    }
    f.apply(json!({"steps":{"c":{"run":"empty"},"new":{"run":"empty"}}}))
        .await;
    assert_eq!(f.state().await.status(&id("c")), StepStatus::Succeeded);
    assert_eq!(f.state().await.status(&id("new")), StepStatus::Pending);
    let gone: Vec<String> = f
        .reads
        .snapshot(|c| {
            let mut q = c.prepare("SELECT step_id FROM outcomes ORDER BY step_id")?;
            Ok(q.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
        .unwrap();
    assert_eq!(gone, vec!["a", "b"]);
}

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

#[tokio::test]
async fn typed_result_read_retains_the_skip_reason_after_removal() {
    let mut f = Fixture::new(
        json!({"inputs":{"enabled":"boolean"},"steps":{"a":{"run":"empty","after":["enabled"]}}}),
    )
    .await;
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::set_input(
                tx,
                &context,
                "enabled",
                json!(false).try_into().unwrap(),
                "sam".into(),
                "skip".into(),
            )
        })
        .await
        .unwrap();
    f.apply(json!({"steps":{}})).await;
    let result = f
        .reads
        .snapshot(|c| {
            let id: String = c.query_row("SELECT result_id FROM outcomes", [], |r| r.get(0))?;
            plans::read_result(c, id.parse().unwrap())
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.status, StepStatus::Skipped);
    assert!(result.removed_at.is_some());
    let Some(plans::ResultError::Skipped(reasons)) = result.error else {
        panic!("lost skip reasons")
    };
    assert_eq!(
        reasons.iter().map(ToString::to_string).collect::<Vec<_>>(),
        vec!["enabled is false"]
    );
}
