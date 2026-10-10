#[allow(dead_code)]
#[path = "../../../tests/support/home.rs"]
mod home;
#[allow(dead_code)]
#[path = "support/plan_rows.rs"]
mod plan_rows;
mod support {
    use super::home::ScratchHome;
    use rusqlite::params;
    use serde_json::{Value, json};
    use sluice_model::error::PublicError;
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
        pub _home: ScratchHome,
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
                _home: home,
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
use rusqlite::params;
use serde_json::{Value, json};
use sluice_model::{
    commands::*,
    error::PublicError,
    ids::{AttemptId, ProjectSelector, RunId},
};
use sluice_store::{
    RetrySafety,
    plans::{self},
};
use support::*;

#[tokio::test]
async fn failed_composition_rolls_back_edit_state_archive_and_records() {
    let f =
        Fixture::new(json!({"steps":{"a":{"run":"core.external","outputs":{"n":"int"}}}})).await;
    f.manual("a", json!({"n":3}), false).await;
    let project = f.context.project;
    let before = f.counts().await;
    let error = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            crate::plan_rows::commit_document(
                tx,
                project,
                &map(json!({"steps":{}})),
                Some(&plan(json!({"steps":{}}))),
                None,
            )?;
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

/// Who cancelled or retried a run, and why, is kept on the run (`runs.stopped`), so the page
/// still says it once the log has trimmed the record: a retry on the step's latest run only, a
/// cancel on the run it stops.
#[tokio::test]
async fn cancel_and_retry_keep_who_and_why_on_the_run() {
    let f = Fixture::new(json!({"steps":{"a":{"run":"empty"}}})).await;
    f.manual("a", json!({}), false).await;
    let project = f.context.project;
    let runs: Vec<String> = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let mut runs = vec![];
            for (n, at) in ["2026-10-01T00:00:00Z", "2026-10-01T01:00:00Z"]
                .into_iter()
                .enumerate()
            {
                let (attempt, run) = (AttemptId::new(), RunId::new());
                let phase = if n == 0 { "terminal" } else { "executing" };
                tx.sql().execute(
                    "INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at) VALUES (?1,?2,'a',1,1,?3,'{}','hash',?4)",
                    params![attempt.to_string(), project.to_string(), phase, at],
                )?;
                tx.sql().execute(
                    "INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at,started_at) VALUES (?1,?2,?3,'a',1,1,?4,?4)",
                    params![run.to_string(), project.to_string(), attempt.to_string(), at],
                )?;
                runs.push(run.to_string());
            }
            tx.changed(Some(project), "status");
            Ok(runs)
        })
        .await
        .unwrap();
    let reads = &f.reads;
    let stopped = |run: String| async move {
        reads
            .snapshot(move |c| {
                Ok(c.query_row(
                    "SELECT coalesce(stopped,'{}') FROM runs WHERE run_id=?1",
                    [run],
                    |r| r.get::<_, String>(0),
                )?)
            })
            .await
            .unwrap()
    };
    let context = f.context.clone();
    let request = retry_request(project, &["a"], None);
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::step_retry(tx, &context, request, &mut Hooks::default())
        })
        .await
        .unwrap();
    assert_eq!(stopped(runs[0].clone()).await, "{}");
    let kept: Value = serde_json::from_str(&stopped(runs[1].clone()).await).unwrap();
    assert_eq!(kept["retry"]["author"], "sam");
    assert_eq!(kept["retry"]["reason"], "retry");
    assert!(
        kept["retry"]["at"]
            .as_str()
            .is_some_and(|a| a.starts_with("20"))
    );
    // the step runs again; the owner cancels it
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET status='running' WHERE project_id=?1 AND step_id='a'",
                [project.to_string()],
            )?;
            tx.changed(Some(project), "status");
            Ok(())
        })
        .await
        .unwrap();
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::step_cancel(
                tx,
                &context,
                StepCancel {
                    expected_rev: None,
                    project: ProjectSelector::Id(project),
                    selection: StepSelection {
                        steps: Some(vec![id("a")]),
                        tags: None,
                    },
                    reason: "switch to the new brief".into(),
                    author: Some("owner".into()),
                },
            )
        })
        .await
        .unwrap();
    let kept: Value = serde_json::from_str(&stopped(runs[1].clone()).await).unwrap();
    assert_eq!(kept["cancel"]["author"], "owner");
    assert_eq!(kept["cancel"]["reason"], "switch to the new brief");
    assert_eq!(
        kept["retry"]["author"], "sam",
        "a cancel keeps the retry beside it"
    );
}

async fn cancel_external(f: &Fixture) {
    let context = f.context.clone();
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::step_cancel(
                tx,
                &context,
                StepCancel {
                    expected_rev: None,
                    project: ProjectSelector::Id(context.project),
                    selection: StepSelection {
                        steps: Some(vec![id("a")]),
                        tags: None,
                    },
                    reason: "cancel".into(),
                    author: Some("owner".into()),
                },
            )
        })
        .await
        .unwrap();
}

async fn dismissal(f: &Fixture) -> bool {
    let project = f.context.project;
    f.reads
        .snapshot(move |sql| Ok(sluice_store::messages::dismissed(sql, project)?.contains("a")))
        .await
        .unwrap()
}

#[tokio::test]
async fn dismissal_survives_record_retention_until_retry_and_new_cancel() {
    let f = Fixture::new(json!({"steps":{"a":{"run":"core.external"}}})).await;
    cancel_external(&f).await;
    let project = f.context.project;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute("DELETE FROM records", [])?;
            sluice_store::messages::dismiss(
                tx,
                StepDismiss {
                    project,
                    step: id("a"),
                    dismissed: true,
                },
            )
        })
        .await
        .unwrap();
    assert!(dismissal(&f).await);
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            for _ in 0..3 {
                tx.append_record(
                    Some(project),
                    sluice_model::events::Event::StepCancel {
                        step: id("unrelated"),
                        author: "owner".into(),
                        reason: "other work".into(),
                    },
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        dismissal(&f).await,
        "unrelated records must not expire a dismissal"
    );
    assert_eq!(
        f.reads
            .snapshot(move |sql| sluice_store::messages::dismissed_lately(sql, project, 10.0))
            .await
            .unwrap()
            .len(),
        1
    );
    let context = f.context.clone();
    let request = retry_request(project, &["a"], None);
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::step_retry(tx, &context, request, &mut Hooks::default())
        })
        .await
        .unwrap();
    assert!(!dismissal(&f).await);
    cancel_external(&f).await;
    assert!(
        !dismissal(&f).await,
        "a new cancel needs its own dismissal even without a run"
    );
}

#[tokio::test]
async fn legacy_dismissal_is_rewritten_without_refreshing_undo_time() {
    let mut f =
        Fixture::new(json!({"steps":{"a":{"run":"core.external"},"b":{"run":"core.external"},"c":{"run":"core.external"},"d":{"run":"core.external"}}}))
            .await;
    cancel_external(&f).await;
    let project = f.context.project;
    f.writer.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute("DELETE FROM records", [])?;
        tx.sql().execute("INSERT INTO readers(project_id,identity,stream,thread,cursor,heartbeat_at) VALUES (?1,'owner','dismissed','a',226915,'2000-01-01T00:00:00Z'),(?1,'owner','dismissed','b',226915,'2000-01-01T00:00:00Z')", [project.to_string()])?;
        for (step, error) in [
            ("c", PublicError::AgentFailure { kind: "Cancelled".into(), message: "stop".into(), session: None }),
            ("d", PublicError::FnFailure { message: "cancelled: stop".into() }),
        ] {
            tx.sql().execute("UPDATE steps SET status='failed',error=?2 WHERE step_id=?1", params![step, serde_json::to_string(&error)?])?;
            tx.sql().execute("INSERT INTO readers(project_id,identity,stream,thread,cursor,heartbeat_at) VALUES (?1,'owner','dismissed',?2,1,'2000-01-01T00:00:00Z')", params![project.to_string(), step])?;
        }
        tx.changed(Some(project), "status");
        Ok(())
    }).await.unwrap();
    f.writer.shutdown().await.unwrap();
    f.writer = sluice_store::Writer::open(f._home.path()).unwrap();
    assert!(
        dismissal(&f).await,
        "a legacy mark holds for the current cancel"
    );
    f.reads.snapshot(move |sql| {
        let row: (i64, Option<i64>, String) = sql.query_row("SELECT cursor,unread_alert_min,heartbeat_at FROM readers WHERE thread='a' AND stream='dismissed'", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert_eq!(row, (1, Some(1), "2000-01-01T00:00:00Z".into()));
        assert_eq!(sql.query_row("SELECT count(*) FROM readers WHERE thread='b' AND stream='dismissed'", [], |r| r.get::<_,i64>(0))?, 0);
        assert_eq!(sluice_store::messages::dismissed(sql, project)?, ["a".into(), "c".into(), "d".into()].into_iter().collect());
        assert!(sluice_store::messages::dismissed_lately(sql, project, 10.0)?.is_empty());
        Ok(())
    }).await.unwrap();
    let context = f.context.clone();
    let request = retry_request(project, &["a"], None);
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::step_retry(tx, &context, request, &mut Hooks::default())
        })
        .await
        .unwrap();
    cancel_external(&f).await;
    assert!(!dismissal(&f).await);
    f.writer.shutdown().await.unwrap();
    f.writer = sluice_store::Writer::open(f._home.path()).unwrap();
    assert!(
        !dismissal(&f).await,
        "startup must not rebind an already migrated mark"
    );
}

#[tokio::test]
async fn dismissal_does_not_hide_a_failure_or_follow_a_recreated_step() {
    let doc = json!({"steps":{"a":{"run":"core.external"}}});
    let mut f = Fixture::new(doc.clone()).await;
    cancel_external(&f).await;
    let project = f.context.project;
    f.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            sluice_store::messages::dismiss(
                tx,
                StepDismiss {
                    project,
                    step: id("a"),
                    dismissed: true,
                },
            )
        })
        .await
        .unwrap();
    f.fail_projection("a").await;
    assert!(
        !dismissal(&f).await,
        "a non-cancel failure cannot be hidden by an old mark"
    );
    f.apply(json!({"steps":{}})).await;
    f.apply(doc).await;
    cancel_external(&f).await;
    assert!(
        !dismissal(&f).await,
        "the same step id with a new generation needs a new mark"
    );
}
