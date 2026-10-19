//! What a named session's `next` sees (SPEC §8.1, "What a session sees"): its own units'
//! records and the messages to it (scope `mine`, its default), everyone's with scope `all`,
//! unnamed callers as before; each session resumes from its own cursor; and how `sluice next`
//! prints a long question and a settled unit's outcome.
use super::stored_messages;
use sluice_model::{
    commands::{NextResult, Scope, Settles, StepStatus},
    error::PublicError,
    events::{Event, Record, UnitStep},
    ids::{ProjectId, RecordSeq, RunId, StepId, WorkGeneration},
    rpc::JsonMap,
};
use sluice_runtime::watch::*;
use sluice_store::{ReadPool, RetrySafety, Writer, messages};
use std::{path::PathBuf, time::Duration};
use stored_messages::{Stored, stored};

struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("sluice-test-scope-{}", RunId::new()));
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

/// Project `p`: unit `a` (steps `a-work`, `a-rm`) owned by `@a`, unit `b` (step `b-work`,
/// tagged `arc:x`) owned by `@b`, unit `u` (step `u-work`) unowned; sessions `@a` and `@b`
/// registered; no fallback.
async fn setup() -> (Home, Writer, ReadPool, ProjectId) {
    let h = Home::new();
    let w = Writer::open(&h.0).unwrap();
    let r = ReadPool::open(&h.0, 2).unwrap();
    let p = ProjectId::new();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        let id = p.to_string();
        tx.sql().execute("INSERT INTO projects(project_id,name,created_at) VALUES (?1,'p','now')", [&id])?;
        tx.sql().execute("INSERT INTO plans(project_id,rev,root_order) VALUES (?1,1,'[\"steps\"]')", [&id])?;
        tx.sql().execute(
            "INSERT INTO steps(project_id,step_id,position,declaration,unit,paused,run,priority,owner) VALUES
             (?1,'a-work',0,'{\"run\":\"x\",\"tags\":[\"unit:a\"]}','a','false','x',0,'@a'),
             (?1,'a-rm',1,'{\"run\":\"x\",\"tags\":[\"unit:a\"]}','a','false','x',0,'@a'),
             (?1,'b-work',2,'{\"run\":\"x\",\"tags\":[\"unit:b\",\"arc:x\"]}','b','false','x',0,'@b'),
             (?1,'u-work',3,'{\"run\":\"x\",\"tags\":[\"unit:u\"]}','u','false','x',0,NULL)",
            [&id],
        )?;
        tx.sql().execute(
            "INSERT INTO readers(project_id,identity,stream,thread,cursor) VALUES (?1,'@a','orchestrator','',0),(?1,'@b','orchestrator','',0)",
            [&id],
        )?;
        tx.changed(Some(p), "plan");
        Ok(())
    })
    .await
    .unwrap();
    (h, w, r, p)
}
fn step(id: &str, status: StepStatus, held: bool) -> UnitStep {
    UnitStep {
        id: StepId::new(id).unwrap(),
        status,
        held,
        outputs: Some(JsonMap::default()),
        omitted: vec![],
    }
}
async fn append(w: &Writer, p: ProjectId, event: Event) -> Record {
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.append_record(Some(p), event)
    })
    .await
    .unwrap()
}
async fn settled(w: &Writer, p: ProjectId, unit: &str, steps: Vec<UnitStep>) -> Record {
    append(
        w,
        p,
        Event::UnitSettled {
            unit: unit.parse().unwrap(),
            work: WorkGeneration(1),
            steps,
        },
    )
    .await
}
async fn failed(w: &Writer, p: ProjectId, id: &str, error: PublicError) -> Record {
    append(
        w,
        p,
        Event::StepStatus {
            step: StepId::new(id).unwrap(),
            from: Some(StepStatus::Running),
            to: StepStatus::Failed,
            error: Some(error),
            run_ids: vec![],
            needs: JsonMap::default(),
        },
    )
    .await
}
async fn ask(w: &Writer, p: ProjectId, from: &str, to: &str, body: &str) -> Record {
    let m = stored(
        w,
        p,
        Stored {
            thread: "t",
            from,
            to: Some(to),
            body,
            question: true,
            ..Stored::default()
        },
    )
    .await;
    Record {
        seq: RecordSeq(m.id.0),
        at: m.at.clone(),
        project: Some(p),
        event: Event::Message(Box::new(m)),
    }
}
/// One `next` from seq 0 that returns as soon as it has looked (with every waking record of
/// the page it read).
fn from_start(p: ProjectId, me: Option<&str>, scope: Option<Scope>) -> NextOptions {
    NextOptions {
        projects: vec![p],
        since_seq: Some(RecordSeq(0)),
        me: me.map(Into::into),
        scope,
        timeout: Some(Duration::ZERO),
        settle: Duration::from_millis(1),
        settle_max: Duration::ZERO,
        ..NextOptions::default()
    }
}
/// Every waking record of one pass over the whole log.
async fn woken(w: &Writer, r: &ReadPool, options: NextOptions) -> Vec<i64> {
    let mut seen = vec![];
    let mut options = options;
    loop {
        let batch = next(w, r, options.clone()).await.unwrap();
        if batch.records.is_empty() {
            return seen;
        }
        seen.extend(batch.records.iter().map(|r| r.seq.0));
        options.since_seq = Some(batch.last_seq);
    }
}

/// Two named sessions each wake only on their own units' settles and failures and the
/// questions to them; an unowned unit with no fallback is everyone's, and once the project
/// has a fallback only the fallback's; scope all sees everything, and an unnamed `next` is
/// as before (every settle, a question to any session).
#[tokio::test]
async fn each_session_sees_its_own_units_and_questions() {
    let (_h, w, r, p) = setup().await;
    let a = settled(
        &w,
        p,
        "a",
        vec![step("a-work", StepStatus::Succeeded, false)],
    )
    .await;
    let b = settled(
        &w,
        p,
        "b",
        vec![step("b-work", StepStatus::Succeeded, false)],
    )
    .await;
    let u = settled(
        &w,
        p,
        "u",
        vec![step("u-work", StepStatus::Succeeded, false)],
    )
    .await;
    let b_failed = failed(
        &w,
        p,
        "b-work",
        PublicError::FnFailure {
            message: "boom".into(),
        },
    )
    .await;
    let to_a = ask(&w, p, "a-work", "@a", "which schema?").await;
    let to_b = ask(&w, p, "b-work", "@b", "which branch?").await;
    let (a, b, u, b_failed, to_a, to_b) = (
        a.seq.0,
        b.seq.0,
        u.seq.0,
        b_failed.seq.0,
        to_a.seq.0,
        to_b.seq.0,
    );

    assert_eq!(
        woken(&w, &r, from_start(p, Some("@a"), None)).await,
        [a, u, to_a]
    );
    assert_eq!(
        woken(&w, &r, from_start(p, Some("@b"), None)).await,
        [b, u, b_failed, to_b]
    );
    // Everyone's, named or not.
    let all = vec![a, b, u, b_failed, to_a, to_b];
    assert_eq!(
        woken(&w, &r, from_start(p, Some("@a"), Some(Scope::All))).await,
        all
    );
    assert_eq!(woken(&w, &r, from_start(p, None, None)).await, all);
    // Scope mine needs a name.
    let refused = next(&w, &r, from_start(p, None, Some(Scope::Mine)))
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("scope mine"), "{refused}");
    // Units and tags narrow either scope; messages to the reader stay.
    assert_eq!(
        woken(
            &w,
            &r,
            NextOptions {
                tags: vec!["arc:x".into()],
                ..from_start(p, Some("@a"), Some(Scope::All))
            }
        )
        .await,
        [b, b_failed, to_a, to_b]
    );
    assert_eq!(
        woken(
            &w,
            &r,
            NextOptions {
                units: vec!["b*".into()],
                ..from_start(p, Some("@b"), None)
            }
        )
        .await,
        [b, b_failed, to_b]
    );

    // With a fallback, the unowned unit is the fallback's alone, and its change wakes @a.
    let update = append(
        &w,
        p,
        Event::ProjectUpdate {
            fields: vec!["fallback".into()],
            reason: None,
            author: "@b".into(),
        },
    )
    .await
    .seq
    .0;
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "UPDATE projects SET fallback='@b' WHERE project_id=?1",
            [p.to_string()],
        )?;
        tx.changed(Some(p), "projects");
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        woken(&w, &r, from_start(p, Some("@a"), None)).await,
        [a, to_a, update]
    );
    assert_eq!(
        woken(&w, &r, from_start(p, Some("@b"), None)).await,
        [b, u, b_failed, to_b]
    );
}

/// A named session without a since resumes after its own last batch, kept in its own readers
/// row: two sessions never share or move each other's position, and an unnamed one starts at
/// the top as before.
#[tokio::test]
async fn each_session_resumes_from_its_own_cursor() {
    let (_h, w, r, p) = setup().await;
    // The log has a past: a session's first wait starts after it.
    settled(&w, p, "u", vec![step("u-work", StepStatus::Pending, true)]).await;
    let resume = |me: Option<&str>| NextOptions {
        projects: vec![p],
        since_seq: None,
        me: me.map(Into::into),
        timeout: Some(Duration::ZERO),
        settle: Duration::from_millis(1),
        settle_max: Duration::ZERO,
        ..NextOptions::default()
    };
    // Both start now: nothing yet.
    assert!(
        next(&w, &r, resume(Some("@a")))
            .await
            .unwrap()
            .records
            .is_empty()
    );
    assert!(
        next(&w, &r, resume(Some("@b")))
            .await
            .unwrap()
            .records
            .is_empty()
    );
    let first = settled(
        &w,
        p,
        "u",
        vec![step("u-work", StepStatus::Succeeded, false)],
    )
    .await;
    // @a takes it; @b's position is its own, so it still gets it next.
    let got = next(&w, &r, resume(Some("@a"))).await.unwrap();
    assert_eq!(got.records, std::slice::from_ref(&first));
    assert!(
        next(&w, &r, resume(Some("@a")))
            .await
            .unwrap()
            .records
            .is_empty()
    );
    let second = settled(&w, p, "u", vec![step("u-work", StepStatus::Failed, false)]).await;
    let got = next(&w, &r, resume(Some("@b"))).await.unwrap();
    assert_eq!(got.records, [first, second.clone()]);
    let got = next(&w, &r, resume(Some("@a"))).await.unwrap();
    assert_eq!(got.records, std::slice::from_ref(&second));
    let position = |me: &'static str| {
        r.snapshot(move |sql| messages::reader(sql, p, me, messages::ORCHESTRATOR_STREAM, ""))
    };
    assert_eq!(position("@a").await.unwrap().cursor, second.seq);
    assert_eq!(position("@b").await.unwrap().cursor, second.seq);
    // Unnamed: from the top, whatever any session read.
    assert!(next(&w, &r, resume(None)).await.unwrap().records.is_empty());
}

/// A long question is shown whole up to `BODY_WHOLE` characters; past it, its first paragraph
/// and every paragraph that asks, each whole (never cut mid-sentence), and where to read the
/// rest. The `#id` sits on the ASK line with the title, and the batch's seq stands apart.
#[tokio::test]
async fn a_long_question_keeps_its_first_paragraph_and_its_questions_whole() {
    let (_h, w, r, p) = setup().await;
    let short = "Which schema?\nv2 or v3.";
    let filler = "The migration touched many tables. ".repeat(80);
    let long = format!(
        "We hit a wall on the cutover: the old rows carry two owners.\nIt only shows on the copy.\n\n{filler}\n\nQuestion: keep the first owner or the last?\n\n{filler}\n\nShould I also drop the backup when it passes?"
    );
    assert!(long.chars().count() > BODY_WHOLE);
    ask(&w, p, "a-work", "@a", short).await;
    let asked = ask(&w, p, "a-work", "@a", &long).await;
    let batch = next(&w, &r, from_start(p, Some("@a"), None)).await.unwrap();
    let text = render(&batch, Settles::None, 600, false, &Cancels::default()).unwrap();
    assert!(
        text.contains("-> @a: Which schema?\n  v2 or v3.\nASK"),
        "{text}"
    );
    let id = asked.seq.0;
    let line = text
        .lines()
        .find(|l| l.starts_with(&format!("ASK #{id} ")))
        .unwrap();
    assert!(line.ends_with("-> @a: We hit a wall on the cutover: the old rows carry two owners."));
    assert!(text.contains("\n  It only shows on the copy.\n  \n  Question: keep the first owner or the last?\n  \n  Should I also drop the backup when it passes?\n  \n  ["));
    assert!(!text.contains("The migration touched"));
    let shown = body_shown(&long, id, "t");
    let left = long.chars().count() - shown.rsplit_once("\n\n[").unwrap().0.chars().count();
    assert!(
        text.contains(&format!(
            "[{left} more characters of #{id} not shown: messages(view=\"thread\", thread=\"t\") reads it whole]"
        )),
        "{text}"
    );
    // The cursor stands apart, after a blank line.
    assert!(
        text.ends_with(&format!("\n\nseq {}\n", batch.last_seq.0)),
        "{text}"
    );
    // A title leads its question's line; the body follows whole.
    let titled = NextResult {
        suppressed: None,
        records: vec![Record {
            event: match asked.event.clone() {
                Event::Message(mut m) => {
                    m.title = Some("Owner rows".into());
                    m.body = short.into();
                    Event::Message(m)
                }
                _ => unreachable!(),
            },
            ..asked
        }],
        notes: vec![],
        last_seq: RecordSeq(id),
        timed_out: false,
    };
    let text = render(&titled, Settles::None, 600, false, &Cancels::default()).unwrap();
    assert!(
        text.starts_with(&format!(
            "ASK #{id} t a-work -> @a: Owner rows\n  Which schema?\n  v2 or v3.\n"
        )),
        "{text}"
    );
}

/// A settled unit's line says how it ended right after `settled`: a failure or a cancel is
/// never read as a success, and a mix is counted.
#[tokio::test]
async fn a_settled_line_names_how_the_unit_ended() {
    let (_h, w, r, p) = setup().await;
    failed(
        &w,
        p,
        "a-work",
        PublicError::FnFailure {
            message: "work failed".into(),
        },
    )
    .await;
    let failed_unit = settled(
        &w,
        p,
        "a",
        vec![
            step("a-work", StepStatus::Failed, false),
            step("a-rm", StepStatus::Pending, true),
        ],
    )
    .await;
    failed(
        &w,
        p,
        "b-work",
        PublicError::Cancelled {
            message: "cancelled: superseded".into(),
        },
    )
    .await;
    let cancelled_unit = settled(&w, p, "b", vec![step("b-work", StepStatus::Failed, false)]).await;
    let fine = settled(
        &w,
        p,
        "u",
        vec![step("u-work", StepStatus::Succeeded, false)],
    )
    .await;
    let result = NextResult {
        suppressed: None,
        records: vec![failed_unit, cancelled_unit, fine],
        notes: vec![],
        last_seq: RecordSeq(0),
        timed_out: false,
    };
    let cancels = {
        let result = result.clone();
        r.snapshot(move |sql| Cancels::read(sql, &result))
            .await
            .unwrap()
    };
    let text = render(&result, Settles::None, 600, false, &cancels).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[..3],
        [
            "UNIT a settled · failed: work failed · rm pending (held)",
            "UNIT b settled · cancelled: work cancelled",
            "UNIT u settled · succeeded: work succeeded",
        ]
    );
    // Counts when it is more than one, or mixed.
    let steps = [
        step("x-1", StepStatus::Failed, false),
        step("x-2", StepStatus::Failed, false),
        step("x-3", StepStatus::Succeeded, false),
    ];
    assert_eq!(outcome(&steps, |_| false), "2 failed");
    assert_eq!(
        outcome(&steps, |s| s.id.as_str() == "x-2"),
        "1 failed, 1 cancelled"
    );
    assert_eq!(
        outcome(&[step("x-1", StepStatus::Stale, false)], |_| false),
        "stale"
    );
}
