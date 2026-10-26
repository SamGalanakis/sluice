use super::store_plan_rows as plan_rows;
use super::stored_messages;
use indexmap::IndexMap;
use serde_json::json;
use sluice_model::{
    commands::{NextResult, Settles, StepStatus},
    error::PublicError,
    events::{Event, Record, UnitStep},
    ids::{ProjectId, RecordSeq, Revision, RunId, UnitName, WorkGeneration},
    plan::{FnSignature, Plan, compile_rows},
    plan_rows::PlanRows,
    rpc::JsonMap,
    types::Type,
};
use sluice_runtime::watch::*;
use sluice_store::{ReadPool, RetrySafety, Writer, messages, records};
use std::{path::PathBuf, time::Duration};
use stored_messages::{Stored, stored};
/// The home's path, and the scratch directory holding it and the installation directory a
/// development binary keeps beside it; both go with the scratch directory.
struct Home(
    PathBuf,
    #[allow(dead_code, reason = "owns the scratch directory until the home drops")]
    crate::home::ScratchHome,
);
impl Home {
    fn new() -> Self {
        let scratch = crate::home::ScratchHome::new().unwrap();
        Self(scratch.path().to_path_buf(), scratch)
    }
}
async fn setup() -> (Home, Writer, ReadPool, ProjectId) {
    let h = Home::new();
    let w = Writer::open(&h.0).unwrap();
    let r = ReadPool::open(&h.0, 2).unwrap();
    let p = project(&w, "p").await;
    (h, w, r, p)
}
/// A project with an empty plan at rev 1, and no record of either: these tests count records.
async fn project(w: &Writer, name: &str) -> ProjectId {
    let p = ProjectId::new();
    let name = name.to_string();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "INSERT INTO projects(project_id,name,created_at) VALUES (?1,?2,'now')",
            (p.to_string(), name),
        )?;
        tx.sql().execute(
            "INSERT INTO plans(project_id,rev,root_order) VALUES (?1,1,'[\"steps\"]')",
            [p.to_string()],
        )?;
        tx.changed(Some(p), "projects");
        Ok(())
    })
    .await
    .unwrap();
    p
}
/// A plan document compiled from its rows, and the document, to commit as the project's plan.
fn compile(doc: &[u8], signatures: &IndexMap<String, FnSignature>) -> (JsonMap, Plan) {
    let doc: JsonMap = serde_json::from_slice(doc).unwrap();
    let rows = PlanRows::from_document(&doc, None).unwrap();
    (doc, compile_rows(&rows, signatures).unwrap())
}
fn options(p: ProjectId) -> NextOptions {
    NextOptions {
        projects: vec![p],
        since_seq: Some(RecordSeq(0)),
        timeout: Some(Duration::ZERO),
        settle: Duration::ZERO,
        ..NextOptions::default()
    }
}
async fn post(
    w: &Writer,
    p: ProjectId,
    body: &str,
    from: &str,
    to: Option<&str>,
    question: bool,
    reply: Option<sluice_model::ids::MessageId>,
) -> sluice_model::commands::Message {
    stored(
        w,
        p,
        Stored {
            thread: "t",
            from,
            to,
            body,
            question,
            reply,
            ..Stored::default()
        },
    )
    .await
}
async fn settled(w: &Writer, p: ProjectId, name: &str) -> Record {
    let unit: UnitName = name.parse().unwrap();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.append_record(
            Some(p),
            Event::UnitSettled {
                unit,
                work: WorkGeneration(1),
                steps: vec![],
            },
        )
    })
    .await
    .unwrap()
}
async fn singleton_settles_once_per_rest_without_open_exception(
    w: &Writer,
    r: &ReadPool,
    p: ProjectId,
) {
    let signatures = IndexMap::from([(
        "test.fn".into(),
        FnSignature {
            outputs: IndexMap::from([("value".into(), Type::String)]),
            open: false,
            ..FnSignature::default()
        },
    )]);
    let (doc, plan) = compile(br#"{"steps":{"one":{"run":"test.fn"}}}"#, &signatures);
    w.write(RetrySafety::NonIdempotent, move |tx| {
        plan_rows::commit_document(tx, p, &doc, None, None)?;
        tx.sql().execute(
            "UPDATE steps SET status='succeeded',outputs=?2 WHERE project_id=?1",
            (p.to_string(), json!({"value":"old"}).to_string()),
        )?;
        tx.changed(Some(p), "status");
        Ok(())
    })
    .await
    .unwrap();
    let copy = plan.clone();
    let events = w
        .write(RetrySafety::NonIdempotent, move |tx| {
            record_settlements(tx, p, Revision(2), &copy)
        })
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    let copy = plan.clone();
    assert!(
        w.write(RetrySafety::NonIdempotent, move |tx| record_settlements(
            tx,
            p,
            Revision(2),
            &copy
        ))
        .await
        .unwrap()
        .is_empty()
    );
    let batch = next(w, r, options(p)).await.unwrap();
    assert_eq!(batch.records.len(), 1);
    assert!(
        matches!(&batch.records[0].event,Event::UnitSettled{unit,steps,..}if unit.as_str()=="one"&&steps[0].outputs.as_ref().unwrap().0["value"].as_value()=="old")
    );
    let copy = plan.clone();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "UPDATE steps SET work_generation=work_generation+1 WHERE project_id=?1",
            [p.to_string()],
        )?;
        record_settlements(tx, p, Revision(2), &copy)?;
        Ok(())
    })
    .await
    .unwrap();
    let more = next(
        w,
        r,
        NextOptions {
            since_seq: Some(batch.last_seq),
            ..options(p)
        },
    )
    .await
    .unwrap();
    assert_eq!(more.records.len(), 1);
}
async fn owner_answer_to_worker_wakes_and_notes_and_own_questions_do_not(
    w: &Writer,
    r: &ReadPool,
    p: ProjectId,
) {
    let q = post(
        w,
        p,
        "worker asks owner",
        "worker",
        Some("owner"),
        true,
        None,
    )
    .await;
    post(w, p, "decision", "worker", Some("owner"), false, None).await;
    post(w, p, "my own question", "@lead", None, true, None).await;
    // The session @lead waits: its own question does not wake it.
    let options = |p| NextOptions {
        me: Some("@lead".into()),
        scope: Some(sluice_model::commands::Scope::All),
        ..options(p)
    };
    let silent = next(w, r, options(p)).await.unwrap();
    assert!(silent.timed_out);
    assert_eq!(silent.records.len(), 0);
    assert_eq!(silent.notes.len(), 1);
    let answer = post(w, p, "yes", "owner", None, false, Some(q.id)).await;
    let wake = next(
        w,
        r,
        NextOptions {
            since_seq: Some(silent.last_seq),
            ..options(p)
        },
    )
    .await
    .unwrap();
    assert_eq!(wake.records[0].seq.0, answer.id.0);
}
async fn notes_are_messages_first_and_whole_and_cursor_stops_at_first_wake(
    w: &Writer,
    r: &ReadPool,
    p: ProjectId,
) {
    post(
        w,
        p,
        "all\nwhole\nlines",
        "worker",
        Some("owner"),
        false,
        None,
    )
    .await;
    let first = settled(w, p, "one").await;
    let second = post(w, p, "question", "worker", None, true, None).await;
    let batch = next(w, r, options(p)).await.unwrap();
    assert_eq!(batch.last_seq, first.seq);
    assert_eq!(batch.notes.len(), 1);
    let text = render(&batch, Settles::None, 1, false, &Cancels::default()).unwrap();
    assert!(text.starts_with("SAY"));
    assert!(text.contains("all\n  whole\n  lines"));
    assert!(!text.contains("question"));
    let more = next(
        w,
        r,
        NextOptions {
            since_seq: Some(batch.last_seq),
            ..options(p)
        },
    )
    .await
    .unwrap();
    assert_eq!(more.last_seq.0, second.id.0);
    assert_eq!(more.records.len(), 1);
}
async fn failures_stale_and_skipped_wake_success_alone_does_not(
    w: &Writer,
    r: &ReadPool,
    p: ProjectId,
) {
    for to in [
        StepStatus::Succeeded,
        StepStatus::Failed,
        StepStatus::Stale,
        StepStatus::Skipped,
    ] {
        let copy = to.clone();
        let rec = w
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.append_record(
                    Some(p),
                    Event::StepStatus {
                        step: "x".parse().unwrap(),
                        from: Some(StepStatus::Running),
                        to: copy,
                        error: Some(PublicError::FnFailure {
                            message: "first\nlast boom".into(),
                        }),
                        run_ids: vec![],
                        needs: JsonMap::default(),
                    },
                )
            })
            .await
            .unwrap();
        let batch = next(
            w,
            r,
            NextOptions {
                since_seq: Some(RecordSeq(rec.seq.0 - 1)),
                ..options(p)
            },
        )
        .await
        .unwrap();
        assert_eq!(batch.timed_out, to == StepStatus::Succeeded);
        if to == StepStatus::Failed {
            assert!(
                line(&batch.records[0], Settles::Full, 600, &Cancels::default())
                    .unwrap()
                    .ends_with("last boom")
            );
        }
    }
}
#[tokio::test]
async fn cursor_expiry_recovers_without_losing_durable_messages() {
    let (_h, w, r, p) = setup().await;
    let question = post(&w, p, "question", "worker", None, true, None).await;
    let rec = settled(&w, p, "one").await;
    w.write(RetrySafety::NonIdempotent, move |tx| {
        records::trim_to(tx, Some(p), 1, 1)?;
        Ok(())
    })
    .await
    .unwrap();
    let expired = next(&w, &r, options(p)).await.unwrap_err();
    assert!(
        matches!(expired,PublicError::CursorExpired{message}if message.contains("earliest=")&&message.contains("latest=")&&message.contains("resnapshot status/messages"))
    );
    assert!(
        r.snapshot(move |sql| messages::message(sql, p, question.id))
            .await
            .is_ok()
    );
    let recovered = next(
        &w,
        &r,
        NextOptions {
            since_seq: Some(RecordSeq(rec.seq.0 - 1)),
            ..options(p)
        },
    )
    .await
    .unwrap();
    assert_eq!(recovered.last_seq, rec.seq);
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn multiple_project_pagination_never_skips_a_record() {
    let (_h, w, r, p) = setup().await;
    let q = project(&w, "q").await;
    w.write(RetrySafety::NonIdempotent, move |tx| {
        for (p, name) in [(p, "p"), (q, "q")] {
            for _ in 0..450 {
                tx.append_record(
                    Some(p),
                    Event::UnitSettled {
                        unit: name.parse().unwrap(),
                        work: WorkGeneration(1),
                        steps: vec![],
                    },
                )?;
            }
        }
        Ok(())
    })
    .await
    .unwrap();
    let batch = next(
        &w,
        &r,
        NextOptions {
            projects: vec![p, q],
            all: true,
            settle: Duration::from_millis(5),
            settle_max: Duration::from_secs(2),
            ..options(p)
        },
    )
    .await
    .unwrap();
    assert_eq!(batch.records.len(), 900);
    assert!(
        batch
            .records
            .windows(2)
            .all(|w| w[0].seq.0 + 1 == w[1].seq.0)
    );
    w.shutdown().await.unwrap();
}
/// Steps a manual clock until the spawned `next` returns, committing a wake
/// before each step when `wake` is set, and returns the result with the number
/// of wakes committed. A step is taken only once `next` has returned or is
/// parked on the clock, so store reads and writes always finish on virtual
/// time. The step cap turns a call that never returns into a failure.
async fn drive(
    task: &mut tokio::task::JoinHandle<Result<NextResult, PublicError>>,
    clock: &ManualClock,
    step: Duration,
    wake: Option<(&Writer, ProjectId)>,
) -> (NextResult, usize) {
    let mut woken = 0;
    for _ in 0..10_000 {
        if !task.is_finished()
            && let Some((w, p)) = wake
        {
            settled(w, p, "u").await;
            woken += 1;
        }
        tokio::select! {
            result = &mut *task => return (result.unwrap().unwrap(), woken),
            () = clock.parked() => clock.advance(step),
        }
    }
    panic!("next did not return");
}
#[tokio::test]
async fn settle_batches_wakes_and_absolute_max_cuts_continuous_wakes() {
    let (_h, w, r, p) = setup().await;
    let clock = ManualClock::new();
    let day = Duration::from_secs(86_400);
    let options_for = |since: RecordSeq, settle: Duration, settle_max: Duration| NextOptions {
        since_seq: Some(since),
        timeout: Some(day),
        settle,
        settle_max,
        clock: Clock::Manual(clock.clone()),
        ..options(p)
    };
    let spawn = |options: NextOptions| {
        let (writer, reads) = (w.clone(), r.clone());
        tokio::spawn(async move { next(&writer, &reads, options).await })
    };
    // Settle batches: wakes at t=0,0,20,35,35 with a 40ms settle. A wake is
    // stamped when next reads it, never before its commit, so the earliest
    // possible deadline is 40ms and all five commit before the clock reaches it.
    // The clock then jumps to 75ms, the deadline of a batch that read the fourth
    // wake at 35ms, while a read begun before the fifth commit may be pending:
    // that read must not end the batch without the fifth.
    let mut task = spawn(options_for(RecordSeq(0), Duration::from_millis(40), day));
    settled(&w, p, "u").await;
    settled(&w, p, "u").await;
    clock.advance(Duration::from_millis(20));
    settled(&w, p, "u").await;
    clock.advance(Duration::from_millis(15));
    settled(&w, p, "u").await;
    let fifth = settled(&w, p, "u").await;
    clock.advance(Duration::from_millis(40));
    let (batch, _) = drive(&mut task, &clock, Duration::from_millis(10), None).await;
    assert!(!batch.timed_out);
    assert_eq!(batch.records.len(), 5);
    assert_eq!(batch.last_seq, fifth.seq);
    // Max cuts continuous wakes: a wake every 30ms keeps a one-day settle open
    // for as long as the producer runs (at most 300s of clock), so only the
    // 70ms absolute maximum can end the call.
    let mut task = spawn(options_for(batch.last_seq, day, Duration::from_millis(70)));
    let (cut, woken) = drive(&mut task, &clock, Duration::from_millis(30), Some((&w, p))).await;
    assert!(!cut.timed_out);
    assert!(!cut.records.is_empty() && cut.records.len() <= woken);
    let mut task = spawn(NextOptions {
        timeout: Some(Duration::ZERO),
        ..options_for(
            cut.last_seq,
            Duration::from_millis(5),
            Duration::from_secs(1),
        )
    });
    let (more, _) = drive(&mut task, &clock, Duration::from_millis(10), None).await;
    assert_eq!(cut.records.len() + more.records.len(), woken);
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn read_progress_is_monotonic_separate_from_owner_and_alerts_deduplicate_every_settlement() {
    let (_h, w, r, p) = setup().await;
    let mut opts = options(p);
    opts.unread_alert_min = Some(1);
    next(&w, &r, opts).await.unwrap();
    let one = settled(&w, p, "one").await;
    let two = settled(&w, p, "two").await;
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "UPDATE readers SET heartbeat_at='2000-01-01T00:00:00Z' WHERE project_id=?1",
            [p.to_string()],
        )?;
        tx.sql().execute(
            "UPDATE records SET at='2000-01-01T00:00:00Z' WHERE seq IN (?1,?2)",
            (one.seq.0, two.seq.0),
        )?;
        tx.changed(Some(p), "readers");
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(unread_alerts(&w, 1.0).await.unwrap().len(), 2);
    assert!(unread_alerts(&w, 1.0).await.unwrap().is_empty());
    let position = r
        .snapshot(move |sql| {
            let orchestrator =
                messages::reader(sql, p, "orchestrator", messages::ORCHESTRATOR_STREAM, "")?;
            let owner = messages::reader(sql, p, "owner", messages::OWNER_STREAM, "")?;
            Ok((orchestrator, owner))
        })
        .await
        .unwrap();
    assert_eq!(position.0.cursor, RecordSeq(0));
    assert_eq!(position.1.cursor, RecordSeq(0));
    w.shutdown().await.unwrap();
}

#[test]
fn compact_rendering_modes_keep_json_whole_and_unicode_cut_safe() {
    let outputs:JsonMap=serde_json::from_value(json!({"landed":true,"summary":"  résumé\nsecond","evidence":"x".repeat(100),"items":[1,2],"sha":"abc"})).unwrap();
    let record = Record {
        seq: RecordSeq(3),
        at: "now".into(),
        project: None,
        event: Event::UnitSettled {
            unit: "u".parse().unwrap(),
            work: WorkGeneration(1),
            steps: vec![UnitStep {
                id: "u-work".parse().unwrap(),
                status: StepStatus::Succeeded,
                held: false,
                outputs: Some(outputs),
                omitted: vec![],
            }],
        },
    };
    let result = NextResult {
        suppressed: None,
        records: vec![record],
        notes: vec![],
        last_seq: RecordSeq(3),
        timed_out: false,
    };
    let short = render(&result, Settles::Short, 600, false, &Cancels::default()).unwrap();
    assert!(short.contains("work.summary: résumé"));
    assert!(short.contains("(+ work.evidence, work.items:"));
    let none = render(&result, Settles::None, 600, false, &Cancels::default()).unwrap();
    assert_eq!(
        none,
        "UNIT u settled · succeeded: work succeeded\n\nseq 3\n"
    );
    let full = render(&result, Settles::Full, 3, false, &Cancels::default()).unwrap();
    assert!(full.contains("rés…"));
    let json = render(&result, Settles::None, 1, true, &Cancels::default()).unwrap();
    assert!(json.contains(&"x".repeat(100)));
    assert!(render(&result, Settles::Full, 0, false, &Cancels::default()).is_err());
}
#[test]
fn cursor_files_roundtrip_and_invalid_files_fail() {
    let h = Home::new();
    let path = h.0.join("cursor");
    assert_eq!(load_cursor(&path).unwrap(), None);
    save_cursor(&path, RecordSeq(42)).unwrap();
    assert_eq!(load_cursor(&path).unwrap(), Some(RecordSeq(42)));
    std::fs::write(&path, "-1").unwrap();
    assert!(load_cursor(&path).is_err());
    assert!(save_cursor(&path, RecordSeq(-2)).is_err());
    assert_eq!(std::fs::read_dir(&h.0).unwrap().count(), 1);
}

async fn queued_work_is_startable_but_paused_external_and_failed_gates_can_settle(
    w: &Writer,
    r: &ReadPool,
    p: ProjectId,
) {
    let signatures = IndexMap::from([
        ("test.fn".into(), FnSignature::default()),
        (
            "core.external".into(),
            FnSignature {
                open: true,
                ..FnSignature::default()
            },
        ),
    ]);
    let (doc, plan) = compile(br#"{"steps":{"ready":{"run":"test.fn","needs":{"lane":1}},"paused":{"run":"test.fn","paused":true},"ext":{"run":"core.external"},"failed":{"run":"test.fn"},"gate":{"run":"test.fn","after":["failed"]}}}"#, &signatures);
    w.write(RetrySafety::NonIdempotent, move |tx| {
        plan_rows::commit_document(tx, p, &doc, None, None)?;
        tx.sql().execute(
            "UPDATE steps SET status='failed' WHERE project_id=?1 AND step_id='failed'",
            [p.to_string()],
        )?;
        tx.changed(Some(p), "status");
        Ok(())
    })
    .await
    .unwrap();
    let emitted = w
        .write(RetrySafety::NonIdempotent, move |tx| {
            record_settlements(tx, p, Revision(2), &plan)
        })
        .await
        .unwrap();
    assert_eq!(emitted.len(), 4);
    assert!(
        !emitted
            .iter()
            .any(|r| matches!(&r.event,Event::UnitSettled{unit,..}if unit.as_str()=="ready"))
    );
    let names: Vec<_> = emitted
        .iter()
        .filter_map(|r| {
            if let Event::UnitSettled { unit, .. } = &r.event {
                Some(unit.as_str())
            } else {
                None
            }
        })
        .collect();
    assert!(names.contains(&"paused") && names.contains(&"gate"));
    let batch = next(
        w,
        r,
        NextOptions {
            settle: Duration::from_millis(1),
            settle_max: Duration::from_secs(1),
            ..options(p)
        },
    )
    .await
    .unwrap();
    assert_eq!(batch.records.len(), 4);
}
async fn delivery_with_running_cleanup_does_not_settle_and_certificate_survives_trim(
    w: &Writer,
    _r: &ReadPool,
    p: ProjectId,
) {
    let signatures = IndexMap::from([("test.fn".into(), FnSignature::default())]);
    let (doc, plan) = compile(br#"{"steps":{"u-work":{"run":"test.fn","tags":["unit:u","exit"]},"u-clean":{"run":"test.fn","tags":["unit:u"],"after":["u-work"]}}}"#, &signatures);
    w.write(RetrySafety::NonIdempotent,move|tx|{plan_rows::commit_document(tx,p,&doc,None,None)?;tx.sql().execute("UPDATE steps SET status=CASE WHEN step_id='u-work' THEN 'succeeded' ELSE 'running' END WHERE project_id=?1",[p.to_string()])?;tx.changed(Some(p),"status");Ok(())}).await.unwrap();
    let copy = plan.clone();
    assert!(
        w.write(RetrySafety::NonIdempotent, move |tx| record_settlements(
            tx,
            p,
            Revision(2),
            &copy
        ))
        .await
        .unwrap()
        .is_empty()
    );
    let copy = plan.clone();
    assert_eq!(
        w.write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET status='succeeded' WHERE project_id=?1",
                [p.to_string()],
            )?;
            record_settlements(tx, p, Revision(2), &copy)
        })
        .await
        .unwrap()
        .len(),
        1
    );
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.append_record(Some(p), Event::RunOrphan { run: RunId::new() })?;
        records::trim_to(tx, Some(p), 1, 1)?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        w.write(RetrySafety::NonIdempotent, move |tx| record_settlements(
            tx,
            p,
            Revision(2),
            &plan
        ))
        .await
        .unwrap()
        .is_empty()
    );
}
async fn timeout_starts_from_now_and_all_includes_own_notes_and_nonwaking_records(
    w: &Writer,
    r: &ReadPool,
    p: ProjectId,
) {
    settled(w, p, "before").await;
    let timed = next(
        w,
        r,
        NextOptions {
            since_seq: None,
            ..options(p)
        },
    )
    .await
    .unwrap();
    assert!(timed.timed_out);
    assert!(timed.records.is_empty());
    post(w, p, "mine", "orchestrator", None, false, None).await;
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.append_record(Some(p), Event::RunOrphan { run: RunId::new() })
    })
    .await
    .unwrap();
    let all = next(
        w,
        r,
        NextOptions {
            since_seq: Some(timed.last_seq),
            all: true,
            settle: Duration::from_millis(1),
            settle_max: Duration::from_secs(1),
            ..options(p)
        },
    )
    .await
    .unwrap();
    assert_eq!(all.records.len(), 2);
}
#[tokio::test]
async fn upkeep_asks_about_unread_settlements_only_with_unread_alert_min_and_trims_to_log_max() {
    let (h, w, r, p) = setup().await;
    next(&w, &r, options(p)).await.unwrap();
    let one = settled(&w, p, "one").await;
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "UPDATE readers SET heartbeat_at='2000-01-01T00:00:00Z' WHERE project_id=?1",
            [p.to_string()],
        )?;
        tx.sql().execute(
            "UPDATE records SET at='2000-01-01T00:00:00Z' WHERE seq=?1",
            [one.seq.0],
        )?;
        tx.changed(Some(p), "readers");
        Ok(())
    })
    .await
    .unwrap();
    let alerts = || {
        r.snapshot(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM messages WHERE \"from\"='sluice' AND \"to\"='owner'",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
    };
    let config = |value: serde_json::Value| {
        std::fs::write(h.0.join("config.json"), value.to_string()).unwrap()
    };
    sluice_runtime::scheduler::upkeep(&w, &h.0, vec![p])
        .await
        .unwrap();
    assert_eq!(alerts().await.unwrap(), 0, "no unread_alert_min, no alert");
    config(json!({"unread_alert_min": 1}));
    for _ in 0..2 {
        sluice_runtime::scheduler::upkeep(&w, &h.0, vec![p])
            .await
            .unwrap();
        assert_eq!(alerts().await.unwrap(), 1);
    }
    config(json!({"log_max": 20}));
    for i in 0..40 {
        post(&w, p, &format!("note {i}"), "worker", None, false, None).await;
    }
    let count = || {
        r.snapshot(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM records WHERE project_id=?1",
                [p.to_string()],
                |r| r.get::<_, i64>(0),
            )?)
        })
    };
    assert!(count().await.unwrap() > 40);
    sluice_runtime::scheduler::upkeep(&w, &h.0, vec![p])
        .await
        .unwrap();
    assert_eq!(count().await.unwrap(), 18, "trimmed to 90% of log_max");
    w.shutdown().await.unwrap();
}

/// Sets each step's status (a `+` before its name also moves its work generation, as a retry
/// does) and records settlements in the same transaction: the units that settled, in order.
async fn rest_after(
    w: &Writer,
    p: ProjectId,
    plan: &Plan,
    statuses: &[(&str, &str)],
) -> Vec<String> {
    let plan = plan.clone();
    let statuses: Vec<(String, String)> = statuses
        .iter()
        .map(|(step, status)| (step.to_string(), status.to_string()))
        .collect();
    let emitted = w
        .write(RetrySafety::NonIdempotent, move |tx| {
            for (step, status) in &statuses {
                let (step, retry) = match step.strip_prefix('+') {
                    Some(step) => (step, 1),
                    None => (step.as_str(), 0),
                };
                tx.sql().execute(
                    "UPDATE steps SET status=?3,work_generation=work_generation+?4 WHERE project_id=?1 AND step_id=?2",
                    (p.to_string(), step, status, retry),
                )?;
            }
            tx.changed(Some(p), "status");
            record_settlements(tx, p, Revision(2), &plan)
        })
        .await
        .unwrap();
    emitted
        .iter()
        .map(|r| match &r.event {
            Event::UnitSettled { unit, .. } => unit.to_string(),
            other => panic!("not a settlement: {other:?}"),
        })
        .collect()
}
/// A committed plan: `a` outside unit `y`, whose work waits on `a`, and unit `z`, whose close
/// is paused.
async fn gated(w: &Writer, p: ProjectId) -> Plan {
    let signatures = IndexMap::from([("test.fn".into(), FnSignature::default())]);
    let (doc, plan) = compile(
        br#"{"steps":{"a":{"run":"test.fn"},
        "y-fork":{"run":"test.fn","tags":["unit:y"]},
        "y-work":{"run":"test.fn","tags":["unit:y","exit"],"after":["a","y-fork"]},
        "y-rm":{"run":"test.fn","tags":["unit:y"],"after":["y-work"]},
        "z-work":{"run":"test.fn","tags":["unit:z"]},
        "z-close":{"run":"test.fn","tags":["unit:z","exit"],"after":["z-work"],"paused":true},
        "z-rm":{"run":"test.fn","tags":["unit:z"],"after":["z-close"]}}}"#,
        &signatures,
    );
    w.write(RetrySafety::NonIdempotent, move |tx| {
        plan_rows::commit_document(tx, p, &doc, None, None)
    })
    .await
    .unwrap();
    plan
}

async fn a_unit_settles_again_each_time_it_comes_back_to_rest(
    w: &Writer,
    _r: &ReadPool,
    p: ProjectId,
) {
    let plan = gated(w, p).await;
    let rest =
        |statuses: &'static [(&'static str, &'static str)]| rest_after(w, p, &plan, statuses);
    // `a` fails: `y` is held behind it.
    assert_eq!(
        rest(&[("y-fork", "succeeded"), ("a", "failed")]).await,
        ["a", "y"]
    );
    // `a` is retried; `y`'s steps are untouched, and it is not at rest while `a` can run.
    assert_eq!(rest(&[("+a", "pending")]).await, Vec::<String>::new());
    assert_eq!(rest(&[("a", "running")]).await, Vec::<String>::new());
    assert_eq!(
        rest(&[("a", "succeeded"), ("y-work", "running")]).await,
        ["a"]
    );
    assert_eq!(rest(&[("y-work", "failed")]).await, ["y"]);
    // `y`'s own work is retried and then finishes, cleanup and all.
    assert_eq!(rest(&[("+y-work", "running")]).await, Vec::<String>::new());
    assert_eq!(
        rest(&[("y-work", "succeeded"), ("y-rm", "succeeded")]).await,
        ["y"]
    );
    // `z` rests on its paused close; its output set by hand, it runs to the end and settles
    // again.
    assert_eq!(rest(&[("z-work", "succeeded")]).await, ["z"]);
    assert_eq!(
        rest(&[("z-close", "succeeded")]).await,
        Vec::<String>::new()
    );
    assert_eq!(rest(&[("z-rm", "succeeded")]).await, ["z"]);
    // Nothing moved: nothing settles.
    assert_eq!(rest(&[]).await, Vec::<String>::new());
}
async fn settlements_recorded_before_statuses_upgrade_without_a_record(
    w: &Writer,
    _r: &ReadPool,
    p: ProjectId,
) {
    let plan = gated(w, p).await;
    // Before the upgrade: `y` settled early, held on `a` while it ran, and `z` settled on its
    // paused close. Their certificates carry no statuses.
    let copy = plan.clone();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "UPDATE steps SET status=CASE step_id WHEN 'a' THEN 'running' WHEN 'y-fork' THEN 'succeeded' WHEN 'z-work' THEN 'succeeded' ELSE 'pending' END WHERE project_id=?1",
            [p.to_string()],
        )?;
        tx.changed(Some(p), "status");
        let mut settlements = json!({});
        for unit in ["y", "z"] {
            let mut signature = vec![];
            for id in &copy.units()[&unit.parse::<UnitName>().unwrap()].steps {
                let (generation, work): (i64, i64) = tx.sql().query_row(
                    "SELECT generation,work_generation FROM steps WHERE project_id=?1 AND step_id=?2",
                    (p.to_string(), id.as_str()),
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                signature.push(json!([id, generation, work]));
            }
            settlements[format!("{p}/{unit}")] = json!({"signature": signature, "work": 1});
        }
        tx.sql().execute(
            "UPDATE maintenance SET settings=json_set(settings,'$.settlements',json(?1)) WHERE singleton=1",
            [settlements.to_string()],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let settlements = || {
        w.write(RetrySafety::NonIdempotent, move |tx| {
            let raw: String = tx.sql().query_row(
                "SELECT settings FROM maintenance WHERE singleton=1",
                [],
                |r| r.get(0),
            )?;
            Ok(serde_json::from_str::<serde_json::Value>(&raw)?["settlements"].clone())
        })
    };
    // The first reconcile after the upgrade writes no record: `z` is at rest as it was, and
    // `y` waits on `a`, still running, so its next rest settles.
    let rest =
        |statuses: &'static [(&'static str, &'static str)]| rest_after(w, p, &plan, statuses);
    assert_eq!(rest(&[]).await, Vec::<String>::new());
    let stored = settlements().await.unwrap();
    assert_eq!(rest(&[]).await, Vec::<String>::new());
    // `y` really finishes, and says so; `z` stays quiet.
    assert_eq!(rest(&[("a", "succeeded")]).await, ["a"]);
    assert_eq!(
        rest(&[("y-work", "succeeded"), ("y-rm", "succeeded")]).await,
        ["y"]
    );
    // Both certificates were upgraded in place at the first reconcile, their stamps unchanged:
    // `z` with its statuses, `y` with none.
    let z = &stored[format!("{p}/z")];
    assert_eq!(z["statuses"], json!(["succeeded", "pending", "pending"]));
    assert_eq!(z["signature"].as_array().unwrap().len(), 3);
    assert_eq!(z["signature"][0].as_array().unwrap().len(), 3);
    assert_eq!(stored[format!("{p}/y")]["statuses"], json!([]));
    assert_eq!(stored[format!("{p}/y")]["work"], 1);
}

#[tokio::test]
async fn messages_statuses_and_full_feed() {
    let (_h, w, r, mut p) = setup().await;
    owner_answer_to_worker_wakes_and_notes_and_own_questions_do_not(&w, &r, p).await;
    p = project(&w, "case-1").await;
    notes_are_messages_first_and_whole_and_cursor_stops_at_first_wake(&w, &r, p).await;
    p = project(&w, "case-2").await;
    failures_stale_and_skipped_wake_success_alone_does_not(&w, &r, p).await;
    p = project(&w, "case-3").await;
    timeout_starts_from_now_and_all_includes_own_notes_and_nonwaking_records(&w, &r, p).await;
    w.shutdown().await.unwrap();
}

#[tokio::test]
async fn settlement_and_retry_certificates() {
    let (_h, w, r, mut p) = setup().await;
    singleton_settles_once_per_rest_without_open_exception(&w, &r, p).await;
    p = project(&w, "case-1").await;
    queued_work_is_startable_but_paused_external_and_failed_gates_can_settle(&w, &r, p).await;
    p = project(&w, "case-2").await;
    delivery_with_running_cleanup_does_not_settle_and_certificate_survives_trim(&w, &r, p).await;
    p = project(&w, "case-3").await;
    a_unit_settles_again_each_time_it_comes_back_to_rest(&w, &r, p).await;
    p = project(&w, "case-4").await;
    settlements_recorded_before_statuses_upgrade_without_a_record(&w, &r, p).await;
    w.shutdown().await.unwrap();
}

/// Status chatter used to wake an orchestrator's grep loop. A subscription counts it,
/// holds the digest until due, and returns every pending ask immediately, even past a page.
#[tokio::test]
async fn subscription_suppresses_chatter_and_asks_bypass_the_digest() {
    use sluice_model::commands::{StepSays, SubscriptionSet, SubscriptionWake};
    use sluice_runtime::subscriptions;
    let (_h, w, r, p) = setup().await;
    // Use an actual stored plan step so the policy can distinguish workers from sessions.
    let (doc, _) = compile(
        br#"{"steps":{"worker":{"run":"test.fn"}}}"#,
        &IndexMap::from([("test.fn".into(), FnSignature::default())]),
    );
    w.write(RetrySafety::NonIdempotent, move |tx| {
        plan_rows::commit_document(tx, p, &doc, None, None)?;
        Ok(())
    })
    .await
    .unwrap();
    let config = SubscriptionSet {
        include_routine: false,
        name: "lanes".into(),
        projects: vec![format!("id:{p}").parse().unwrap()],
        wake: vec![SubscriptionWake::Ask, SubscriptionWake::SayToMe],
        say_from_steps: StepSays::AsksOnly,
        digest: Some("30m".into()),
        me: "@orch".into(),
    };
    subscriptions::set(&w, config.clone()).await.unwrap();
    let clock = ManualClock::new();
    let task = {
        let (w, r, clock) = (w.clone(), r.clone(), clock.clone());
        tokio::spawn(async move {
            subscriptions::next(
                &w,
                &r,
                "lanes".into(),
                Duration::from_secs(3600),
                Clock::Manual(clock),
                None,
            )
            .await
        })
    };
    clock.parked().await;
    // A batch larger than the log page proves next drains all matching events before returning.
    w.write(RetrySafety::NonIdempotent, move |tx| {
        for _ in 0..205 {
            stored_messages::store(
                tx,
                p,
                &Stored {
                    from: "worker",
                    to: Some("@orch"),
                    body: "still waiting",
                    ..Stored::default()
                },
            )?;
        }
        for body in ["decision one", "decision two"] {
            stored_messages::store(
                tx,
                p,
                &Stored {
                    from: "worker",
                    to: Some("@orch"),
                    body,
                    question: true,
                    ..Stored::default()
                },
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    let got = task.await.unwrap().unwrap();
    assert_eq!(got.records.len(), 2);
    assert_eq!(got.suppressed.unwrap()["say"], 205);
    assert!(!got.timed_out);
    // No fake time passed: the ask did not wait for the 30 minute digest.
    post(&w, p, "queued", "worker", Some("@orch"), false, None).await;
    let wait = || {
        let (w, r, clock) = (w.clone(), r.clone(), clock.clone());
        tokio::spawn(async move {
            subscriptions::next(
                &w,
                &r,
                "lanes".into(),
                Duration::from_secs(3600),
                Clock::Manual(clock),
                None,
            )
            .await
        })
    };
    let interrupted = wait();
    clock.parked().await;
    assert!(
        !interrupted.is_finished(),
        "chatter must not wake the reader"
    );
    interrupted.abort();
    assert!(interrupted.await.unwrap_err().is_cancelled());
    assert_eq!(
        subscriptions::get(&r, "lanes".into()).await.unwrap()["cursor"],
        json!(got.last_seq)
    );
    let second = wait();
    clock.parked().await;
    clock.advance(Duration::from_secs(1800));
    let got = second.await.unwrap().unwrap();
    assert!(got.records.is_empty());
    assert!(!got.timed_out, "digest returned before the timeout");
    assert_eq!(got.suppressed.unwrap()["say"], 1);
    // An explicit question requests the worker's update; asks-only allows its say.
    post(&w, p, "status please", "@orch", Some("worker"), true, None).await;
    post(&w, p, "still waiting", "worker", Some("@orch"), false, None).await;
    let asked = subscriptions::next(
        &w,
        &r,
        "lanes".into(),
        Duration::ZERO,
        Clock::Manual(clock.clone()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(asked.records.len(), 1);
    assert_eq!(asked.suppressed.unwrap()["ask"], 1);
    // An opt-in permits unsolicited says; none suppresses even the requested says.
    subscriptions::set(
        &w,
        SubscriptionSet {
            say_from_steps: StepSays::None,
            ..config.clone()
        },
    )
    .await
    .unwrap();
    post(&w, p, "queued", "worker", Some("@orch"), false, None).await;
    let none = subscriptions::next(
        &w,
        &r,
        "lanes".into(),
        Duration::ZERO,
        Clock::Manual(clock.clone()),
        None,
    )
    .await
    .unwrap();
    assert!(none.records.is_empty());
    assert_eq!(none.suppressed.unwrap()["say"], 1);
    subscriptions::set(
        &w,
        SubscriptionSet {
            say_from_steps: StepSays::All,
            ..config
        },
    )
    .await
    .unwrap();
    post(&w, p, "queued", "worker", Some("@orch"), false, None).await;
    let all = subscriptions::next(
        &w,
        &r,
        "lanes".into(),
        Duration::ZERO,
        Clock::Manual(clock),
        None,
    )
    .await
    .unwrap();
    assert_eq!(all.records.len(), 1);
    subscriptions::remove(&w, "lanes".into()).await.unwrap();
    w.shutdown().await.unwrap();
}

/// Quota and execution-limit wakes must be selectable without subscribing to every failure.
#[tokio::test]
async fn subscription_wake_kinds_select_typed_events() {
    use sluice_model::commands::{StepSays, SubscriptionSet, SubscriptionWake as Wake};
    use sluice_runtime::subscriptions;
    let (_h, w, r, p) = setup().await;
    let cases = [
        ("failed", Wake::Failed, 3),
        ("quota", Wake::Quota, 1),
        ("limit", Wake::Limit, 1),
        ("stale", Wake::Stale, 1),
        ("settled", Wake::Settled, 1),
        ("reply", Wake::Reply, 1),
    ];
    let question = post(&w, p, "question", "@orch", Some("worker"), true, None).await;
    for (name, wake, _) in &cases {
        subscriptions::set(
            &w,
            SubscriptionSet {
                include_routine: false,
                name: (*name).into(),
                projects: vec![format!("id:{p}").parse().unwrap()],
                wake: vec![*wake],
                say_from_steps: StepSays::None,
                digest: None,
                me: "@orch".into(),
            },
        )
        .await
        .unwrap();
    }
    w.write(RetrySafety::NonIdempotent, move |tx| {
        for (to, error) in [
            (
                StepStatus::Failed,
                Some(PublicError::FnFailure {
                    message: "bad command".into(),
                }),
            ),
            (
                StepStatus::Failed,
                Some(PublicError::AgentFailure {
                    retry_at: None,
                    kind: "QuotaExhausted".into(),
                    message: "reset tomorrow".into(),
                    session: None,
                }),
            ),
            (
                StepStatus::Failed,
                Some(PublicError::ResourceExhausted {
                    resource: sluice_model::limits::ExhaustedResource::Memory,
                    limit: 1024,
                    peak: Some(2048),
                    message: "memory cap".into(),
                }),
            ),
            (StepStatus::Stale, None),
        ] {
            tx.append_record(
                Some(p),
                Event::StepStatus {
                    step: "worker".parse().unwrap(),
                    from: Some(StepStatus::Running),
                    to,
                    error,
                    run_ids: vec![],
                    needs: JsonMap::default(),
                },
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    settled(&w, p, "worker").await;
    post(
        &w,
        p,
        "answer",
        "worker",
        Some("@orch"),
        false,
        Some(question.id),
    )
    .await;
    for (name, _, expected) in cases {
        let got = subscriptions::next(&w, &r, name.into(), Duration::ZERO, Clock::default(), None)
            .await
            .unwrap();
        assert_eq!(got.records.len(), expected, "{name}: {got:?}");
        assert!(!got.timed_out);
        let empty =
            subscriptions::next(&w, &r, name.into(), Duration::ZERO, Clock::default(), None)
                .await
                .unwrap();
        assert!(
            empty.records.is_empty(),
            "{name} repeated a delivered record"
        );
    }
    w.shutdown().await.unwrap();
}

/// Previously a wait froze its project list and never heard the first message in B.
/// Independent cursors also prevent replaying A when B's cursor trails it.
#[tokio::test]
async fn all_projects_discover_an_inbox_and_resume_each_project() {
    let (_h, w, r, a) = setup().await;
    let b = ProjectId::new();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "UPDATE projects SET fallback='@lead' WHERE project_id=?1",
            [a.to_string()],
        )?;
        tx.sql().execute(
            "INSERT INTO projects(project_id,name,created_at) VALUES (?1,'b','now')",
            [b.to_string()],
        )?;
        tx.changed(Some(b), "projects");
        Ok(())
    })
    .await
    .unwrap();
    let options = NextOptions {
        me: Some("@lead".into()),
        timeout: Some(Duration::from_secs(3600)),
        ..NextOptions::default()
    };
    let clock = ManualClock::new();
    let wait = next(
        &w,
        &r,
        NextOptions {
            clock: Clock::Manual(clock.clone()),
            ..options.clone()
        },
    );
    tokio::pin!(wait);
    tokio::select! {
        result = &mut wait => panic!("wait returned before B addressed us: {result:?}"),
        _ = clock.parked() => {}
    }
    stored(
        &w,
        b,
        Stored {
            from: "@worker",
            to: Some("@lead"),
            body: "first B",
            question: true,
            ..Stored::default()
        },
    )
    .await;
    let first = wait.await.unwrap();
    assert_eq!(first.records.len(), 1);
    assert_eq!(first.records[0].project, Some(b));
    for (project, body) in [(a, "A together"), (b, "B together")] {
        stored(
            &w,
            project,
            Stored {
                from: "@worker",
                to: Some("@lead"),
                body,
                question: true,
                ..Stored::default()
            },
        )
        .await;
    }
    let both = next(&w, &r, options.clone()).await.unwrap();
    assert_eq!(
        both.records.iter().map(|r| r.project).collect::<Vec<_>>(),
        vec![Some(a), Some(b)]
    );
    let empty = next(
        &w,
        &r,
        NextOptions {
            timeout: Some(Duration::ZERO),
            ..options
        },
    )
    .await
    .unwrap();
    assert!(empty.timed_out);
    assert!(empty.records.is_empty());
    w.shutdown().await.unwrap();
}

/// Stored all-project subscriptions used to require a static nonempty list. A new
/// inbox must join a sleeping wait, and durable project cursors must survive reopen.
#[tokio::test]
async fn all_project_subscription_discovers_projects_and_survives_restart() {
    use sluice_model::commands::{StepSays, SubscriptionSet, SubscriptionWake};
    use sluice_runtime::subscriptions;
    let (h, w, r, a) = setup().await;
    subscriptions::set(
        &w,
        SubscriptionSet {
            name: "all".into(),
            projects: vec![],
            me: "@lead".into(),
            wake: vec![SubscriptionWake::Ask],
            include_routine: false,
            say_from_steps: StepSays::All,
            digest: None,
        },
    )
    .await
    .unwrap();
    let clock = ManualClock::new();
    let wait = subscriptions::next(
        &w,
        &r,
        "all".into(),
        Duration::from_secs(3600),
        Clock::Manual(clock.clone()),
        None,
    );
    tokio::pin!(wait);
    tokio::select! {
        result = &mut wait => panic!("subscription returned without work: {result:?}"),
        _ = clock.parked() => {}
    }
    let b = ProjectId::new();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "INSERT INTO projects(project_id,name,created_at) VALUES (?1,'b','now')",
            [b.to_string()],
        )?;
        tx.changed(Some(b), "projects");
        Ok(())
    })
    .await
    .unwrap();
    stored(
        &w,
        b,
        Stored {
            from: "@worker",
            to: Some("@lead"),
            body: "new inbox",
            question: true,
            ..Stored::default()
        },
    )
    .await;
    let first = wait.await.unwrap();
    assert_eq!(first.records.len(), 1);
    assert_eq!(first.records[0].project, Some(b));
    w.shutdown().await.unwrap();
    let w = Writer::open(&h.0).unwrap();
    let r = ReadPool::open(&h.0, 2).unwrap();
    for (project, body) in [(a, "A after restart"), (b, "B after restart")] {
        stored(
            &w,
            project,
            Stored {
                from: "@worker",
                to: Some("@lead"),
                body,
                question: true,
                ..Stored::default()
            },
        )
        .await;
    }
    let both = subscriptions::next(&w, &r, "all".into(), Duration::ZERO, Clock::default(), None)
        .await
        .unwrap();
    assert_eq!(
        both.records.iter().map(|r| r.project).collect::<Vec<_>>(),
        vec![Some(a), Some(b)]
    );
    let stored = subscriptions::get(&r, "all".into()).await.unwrap();
    assert_eq!(stored["cursors"].as_object().unwrap().len(), 2);
    let empty = subscriptions::next(&w, &r, "all".into(), Duration::ZERO, Clock::default(), None)
        .await
        .unwrap();
    assert!(empty.timed_out);
    assert!(empty.records.is_empty());
    w.shutdown().await.unwrap();
}
