use indexmap::IndexMap;
use serde_json::json;
use sluice_model::{
    commands::{MessagePost, NextResult, Settles, StepStatus},
    error::PublicError,
    events::{Event, Record, UnitStep},
    ids::{ProjectId, RecordSeq, RunId, UnitName, WorkGeneration},
    plan::{FnSignature, Plan},
    rpc::JsonMap,
    types::Type,
};
use sluice_runtime::watch::*;
use sluice_store::{ReadPool, RetrySafety, Writer, messages, records};
use std::{path::PathBuf, time::Duration};
struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("sluice-test-p3-04-{}", RunId::new()));
        let p = sluice_process::host::guard_scratch_home(&p).unwrap();
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn setup() -> (Home, Writer, ReadPool, ProjectId) {
    let h = Home::new();
    let w = Writer::open(&h.0).unwrap();
    let r = ReadPool::open(&h.0, 2).unwrap();
    let p = project(&w, "p").await;
    (h, w, r, p)
}
async fn project(w: &Writer, name: &str) -> ProjectId {
    let p = ProjectId::new();
    let name = name.to_string();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "INSERT INTO projects(project_id,name,created_at) VALUES (?1,?2,'now')",
            (p.to_string(), name),
        )?;
        tx.changed(Some(p), "projects");
        Ok(())
    })
    .await
    .unwrap();
    p
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
    let post:MessagePost=serde_json::from_value(json!({"project":{"kind":"id","value":p},"body":body,"from":from,"to":to,"needs_reply":question,"reply_to":reply})).unwrap();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        messages::message_post(tx, post, &messages::NoPlanInputs)
    })
    .await
    .unwrap()
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
#[tokio::test]
async fn singleton_settles_once_per_generation_without_open_exception() {
    let (_h, w, r, p) = setup().await;
    let signatures = IndexMap::from([(
        "test.fn".into(),
        FnSignature {
            outputs: IndexMap::from([("value".into(), Type::String)]),
            open: false,
            ..FnSignature::default()
        },
    )]);
    let plan = Plan::parse_json(br#"{"steps":{"one":{"run":"test.fn"}}}"#, &signatures).unwrap();
    let copy = plan.clone();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        sluice_store::plans::initialize_plan(tx, p, &copy)?;
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
            record_settlements(tx, p, &copy)
        })
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    let copy = plan.clone();
    assert!(
        w.write(RetrySafety::NonIdempotent, move |tx| record_settlements(
            tx, p, &copy
        ))
        .await
        .unwrap()
        .is_empty()
    );
    let batch = next(&w, &r, options(p)).await.unwrap();
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
        record_settlements(tx, p, &copy)?;
        Ok(())
    })
    .await
    .unwrap();
    let more = next(
        &w,
        &r,
        NextOptions {
            since_seq: Some(batch.last_seq),
            ..options(p)
        },
    )
    .await
    .unwrap();
    assert_eq!(more.records.len(), 1);
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn owner_answer_to_worker_wakes_and_notes_own_questions_and_clarifications_do_not() {
    let (_h, w, r, p) = setup().await;
    let q = post(
        &w,
        p,
        "worker asks owner",
        "worker",
        Some("owner"),
        true,
        None,
    )
    .await;
    post(&w, p, "decision", "worker", Some("owner"), false, None).await;
    post(&w, p, "my own question", "orchestrator", None, true, None).await;
    let silent = next(&w, &r, options(p)).await.unwrap();
    assert!(silent.timed_out);
    assert_eq!(silent.records.len(), 0);
    assert_eq!(silent.notes.len(), 1);
    post(&w, p, "clarify", "owner", Some("worker"), true, Some(q.id)).await;
    let silent2 = next(
        &w,
        &r,
        NextOptions {
            since_seq: Some(silent.last_seq),
            ..options(p)
        },
    )
    .await
    .unwrap();
    assert!(silent2.timed_out);
    let answer = post(&w, p, "yes", "owner", None, false, Some(q.id)).await;
    let wake = next(
        &w,
        &r,
        NextOptions {
            since_seq: Some(silent2.last_seq),
            ..options(p)
        },
    )
    .await
    .unwrap();
    assert_eq!(wake.records[0].seq.0, answer.id.0);
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn notes_are_messages_first_and_whole_and_cursor_stops_at_first_wake() {
    let (_h, w, r, p) = setup().await;
    post(
        &w,
        p,
        "all\nwhole\nlines",
        "worker",
        Some("owner"),
        false,
        None,
    )
    .await;
    let first = settled(&w, p, "one").await;
    let second = post(&w, p, "question", "worker", None, true, None).await;
    let batch = next(&w, &r, options(p)).await.unwrap();
    assert_eq!(batch.last_seq, first.seq);
    assert_eq!(batch.notes.len(), 1);
    let text = render(&batch, Settles::None, 1, false).unwrap();
    assert!(text.starts_with("NOTE"));
    assert!(text.contains("all\n  whole\n  lines"));
    assert!(!text.contains("question"));
    let more = next(
        &w,
        &r,
        NextOptions {
            since_seq: Some(batch.last_seq),
            ..options(p)
        },
    )
    .await
    .unwrap();
    assert_eq!(more.last_seq.0, second.id.0);
    assert_eq!(more.records.len(), 1);
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn failures_stale_and_skipped_wake_success_alone_does_not() {
    let (_h, w, r, p) = setup().await;
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
            &w,
            &r,
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
                line(&batch.records[0], Settles::Full, 600)
                    .unwrap()
                    .ends_with("last boom")
            );
        }
    }
    w.shutdown().await.unwrap();
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
/// of wakes committed. Steps are virtual; each also yields a real millisecond so
/// the writer and read-pool threads can answer. The step cap only turns a stuck
/// call into a failure instead of a hang.
async fn drive(
    task: &mut tokio::task::JoinHandle<Result<NextResult, PublicError>>,
    clock: &ManualClock,
    step: Duration,
    wake: Option<(&Writer, ProjectId)>,
) -> (NextResult, usize) {
    let mut woken = 0;
    for _ in 0..10_000 {
        if task.is_finished() {
            break;
        }
        if let Some((w, p)) = wake {
            settled(w, p, "u").await;
            woken += 1;
        }
        clock.advance(step);
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(task.is_finished(), "next did not return");
    (task.await.unwrap().unwrap(), woken)
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
    let mut task = spawn(options_for(RecordSeq(0), Duration::from_millis(40), day));
    settled(&w, p, "u").await;
    settled(&w, p, "u").await;
    clock.advance(Duration::from_millis(20));
    settled(&w, p, "u").await;
    clock.advance(Duration::from_millis(15));
    settled(&w, p, "u").await;
    settled(&w, p, "u").await;
    let (batch, _) = drive(&mut task, &clock, Duration::from_millis(10), None).await;
    assert!(!batch.timed_out);
    assert_eq!(batch.records.len(), 5);
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
    assert_eq!(unread_alerts(&w).await.unwrap().len(), 2);
    assert!(unread_alerts(&w).await.unwrap().is_empty());
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
#[tokio::test]
async fn rename_keeps_reader_cursor_and_historical_unit_payload() {
    let (_h, w, r, p) = setup().await;
    let historical = settled(&w, p, "old").await;
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "UPDATE projects SET name='new' WHERE project_id=?1",
            [p.to_string()],
        )?;
        tx.changed(Some(p), "projects");
        Ok(())
    })
    .await
    .unwrap();
    let batch = next(&w, &r, options(p)).await.unwrap();
    assert_eq!(batch.records[0], historical);
    let pos = r
        .snapshot(move |sql| {
            messages::reader(sql, p, "orchestrator", messages::ORCHESTRATOR_STREAM, "")
        })
        .await
        .unwrap();
    assert_eq!(pos.cursor, batch.last_seq);
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
        records: vec![record],
        notes: vec![],
        last_seq: RecordSeq(3),
        timed_out: false,
    };
    let short = render(&result, Settles::Short, 600, false).unwrap();
    assert!(short.contains("work.summary: résumé"));
    assert!(short.contains("(+ work.evidence, work.items:"));
    let none = render(&result, Settles::None, 600, false).unwrap();
    assert_eq!(none, "UNIT u settled: work succeeded\nseq 3\n");
    let full = render(&result, Settles::Full, 3, false).unwrap();
    assert!(full.contains("rés…"));
    let json = render(&result, Settles::None, 1, true).unwrap();
    assert!(json.contains(&"x".repeat(100)));
    assert!(render(&result, Settles::Full, 0, false).is_err());
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

#[tokio::test]
async fn queued_work_is_startable_but_paused_external_and_failed_gates_can_settle() {
    let (_h, w, r, p) = setup().await;
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
    let plan=Plan::parse_json(br#"{"steps":{"ready":{"run":"test.fn","needs":{"lane":1}},"paused":{"run":"test.fn","paused":true},"ext":{"run":"core.external"},"failed":{"run":"test.fn"},"gate":{"run":"test.fn","after":["failed"]}}}"#,&signatures).unwrap();
    let copy = plan.clone();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        sluice_store::plans::initialize_plan(tx, p, &copy)?;
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
            record_settlements(tx, p, &plan)
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
        &w,
        &r,
        NextOptions {
            settle: Duration::from_millis(1),
            settle_max: Duration::from_secs(1),
            ..options(p)
        },
    )
    .await
    .unwrap();
    assert_eq!(batch.records.len(), 4);
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn delivery_with_running_cleanup_does_not_settle_and_certificate_survives_trim() {
    let (_h, w, _r, p) = setup().await;
    let signatures = IndexMap::from([("test.fn".into(), FnSignature::default())]);
    let plan=Plan::parse_json(br#"{"steps":{"u-work":{"run":"test.fn","tags":["unit:u","exit"]},"u-clean":{"run":"test.fn","tags":["unit:u"],"after":["u-work"]}}}"#,&signatures).unwrap();
    let copy = plan.clone();
    w.write(RetrySafety::NonIdempotent,move|tx|{sluice_store::plans::initialize_plan(tx,p,&copy)?;tx.sql().execute("UPDATE steps SET status=CASE WHEN step_id='u-work' THEN 'succeeded' ELSE 'running' END WHERE project_id=?1",[p.to_string()])?;tx.changed(Some(p),"status");Ok(())}).await.unwrap();
    let copy = plan.clone();
    assert!(
        w.write(RetrySafety::NonIdempotent, move |tx| record_settlements(
            tx, p, &copy
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
            record_settlements(tx, p, &copy)
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
            tx, p, &plan
        ))
        .await
        .unwrap()
        .is_empty()
    );
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn timeout_starts_from_now_and_all_includes_own_notes_and_nonwaking_records() {
    let (_h, w, r, p) = setup().await;
    settled(&w, p, "before").await;
    let timed = next(
        &w,
        &r,
        NextOptions {
            since_seq: None,
            ..options(p)
        },
    )
    .await
    .unwrap();
    assert!(timed.timed_out);
    assert!(timed.records.is_empty());
    post(&w, p, "mine", "orchestrator", None, false, None).await;
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.append_record(Some(p), Event::RunOrphan { run: RunId::new() })
    })
    .await
    .unwrap();
    let all = next(
        &w,
        &r,
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
    w.shutdown().await.unwrap();
}
