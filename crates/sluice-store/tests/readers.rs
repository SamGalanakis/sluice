#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use rusqlite::params;
use sluice_model::{
    commands::{MarkRead, Message, MessageVerb, MessageView},
    error::PublicError,
    events::{Event, UnitStep},
    ids::{MessageId, ProjectId, RecordSeq, StepId, UnitName, WorkGeneration},
};
use sluice_store::{ReadPool, RetrySafety, Writer, messages::*};

async fn setup() -> (ScratchHome, Writer, ReadPool, ProjectId) {
    let h = ScratchHome::new().unwrap();
    assert!(h.root().exists());
    assert_eq!(ScratchHome::validate(h.path()).unwrap(), h.path());
    let w = Writer::open(h.path()).unwrap();
    let p = ProjectId::new();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "INSERT INTO projects(project_id,name,created_at) VALUES (?1,'p','now')",
            [p.to_string()],
        )?;
        tx.changed(Some(p), "projects");
        Ok(())
    })
    .await
    .unwrap();
    let r = ReadPool::open(h.path(), 2).unwrap();
    (h, w, r, p)
}
/// A message row and record as any release may have stored it (free threads and
/// senders, a question with no recipient): the readers must take them all.
async fn post(
    w: &Writer,
    p: ProjectId,
    thread: &str,
    from: &str,
    to: Option<&str>,
    needs_reply: bool,
    reply: Option<MessageId>,
) -> Message {
    let (thread, from, to) = (thread.to_owned(), from.to_owned(), to.map(str::to_owned));
    w.write(RetrySafety::NonIdempotent, move |tx| {
        let mut m = Message {
            id: MessageId(0),
            verb: match (reply, needs_reply) {
                (Some(_), _) => MessageVerb::Reply,
                (None, true) => MessageVerb::Ask,
                (None, false) => MessageVerb::Say,
            },
            from,
            to,
            thread,
            body: "hello".into(),
            title: None,
            ui: None,
            input: None,
            data: None,
            run: None,
            at: String::new(),
            to_message: reply,
            answer: None,
            state: None,
            answered_by: None,
        };
        let record = tx.append_record(Some(p), Event::Message(Box::new(m.clone())))?;
        m.id = MessageId(record.seq.0);
        m.at = record.at;
        tx.sql().execute(
            "UPDATE records SET thread=?1,payload=?2 WHERE seq=?3",
            params![m.thread, serde_json::to_string(&Event::Message(Box::new(m.clone())))?, m.id.0],
        )?;
        tx.sql().execute("INSERT INTO messages(id,project_id,thread,\"from\",\"to\",body,needs_reply,reply_to,at) VALUES (?1,?2,?3,?4,?5,'hello',?6,?7,?8)",params![m.id.0,p.to_string(),m.thread,m.from,m.to,needs_reply,reply.map(|r| r.0),m.at])?;
        if let Some(parent) = reply.filter(|_| !needs_reply) {
            tx.sql().execute("UPDATE messages SET resolved_by=?1 WHERE id=?2 AND needs_reply=1 AND resolved_by IS NULL",params![m.id.0,parent.0])?;
        }
        tx.changed(Some(p), "messages");
        Ok(m)
    })
    .await
    .unwrap()
}
async fn view(r: &ReadPool, p: ProjectId, v: MessageView, thread: Option<&str>) -> Vec<Message> {
    let thread = thread.map(str::to_owned);
    r.snapshot(move |c| messages(c, p, v, thread.as_deref(), None, "owner"))
        .await
        .unwrap()
}
async fn read(w: &Writer, p: ProjectId, thread: &str, watermark: MessageId) -> MessageId {
    let req = MarkRead {
        project: p,
        identity: "owner".into(),
        thread: thread.into(),
        through: watermark,
    };
    w.write(RetrySafety::Idempotent, move |tx| mark_read(tx, req))
        .await
        .unwrap()
}

#[tokio::test]
async fn reading_one_thread_does_not_hide_older_unread_note_in_another() {
    let (_h, w, r, p) = setup().await;
    let old = post(&w, p, "a", "worker", Some("owner"), false, None).await;
    let newer = post(&w, p, "b", "worker", Some("owner"), false, None).await;
    read(&w, p, "b", newer.id).await;
    assert_eq!(
        view(&r, p, MessageView::Inbox, None)
            .await
            .iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        vec![old.id]
    );
    assert_eq!(view(&r, p, MessageView::History, None).await.len(), 2);
}
#[tokio::test]
async fn owner_reads_and_orchestrator_positions_never_move_each_other() {
    let (_h, w, r, p) = setup().await;
    let m = post(&w, p, "a", "worker", Some("owner"), false, None).await;
    w.write(RetrySafety::Idempotent, move |tx| {
        orchestrator_read(tx, p, "owner", RecordSeq(99), Some(30))
    })
    .await
    .unwrap();
    read(&w, p, "a", m.id).await;
    let (owner, orch) = r
        .snapshot(move |c| {
            Ok((
                reader(c, p, "owner", OWNER_STREAM, "a")?,
                reader(c, p, "owner", ORCHESTRATOR_STREAM, "")?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(owner.cursor.0, m.id.0);
    assert_eq!(orch.cursor, RecordSeq(99));
    assert!(owner.heartbeat_at.is_none());
    assert!(orch.heartbeat_at.is_some());
    w.write(RetrySafety::Idempotent, move |tx| {
        orchestrator_read(tx, p, "owner", RecordSeq(150), None)
    })
    .await
    .unwrap();
    let owner = r
        .snapshot(move |c| reader(c, p, "owner", OWNER_STREAM, "a"))
        .await
        .unwrap();
    assert_eq!(owner.cursor.0, m.id.0);
}
#[tokio::test]
async fn read_watermarks_are_monotonic_idempotent_and_clamped_to_displayed_thread() {
    let (_h, w, r, p) = setup().await;
    let a = post(&w, p, "a", "worker", Some("owner"), false, None).await;
    let b = post(&w, p, "b", "worker", Some("owner"), false, None).await;
    assert_eq!(read(&w, p, "a", b.id).await, a.id);
    assert_eq!(read(&w, p, "a", MessageId(0)).await, a.id);
    assert_eq!(read(&w, p, "a", a.id).await, a.id);
    let later = post(&w, p, "a", "worker", Some("owner"), false, None).await;
    assert!(
        view(&r, p, MessageView::Inbox, None)
            .await
            .iter()
            .any(|m| m.id == later.id)
    );
    assert_eq!(read(&w, p, "missing", MessageId(9999)).await, MessageId(0));
    let future = post(&w, p, "missing", "worker", Some("owner"), false, None).await;
    assert!(
        view(&r, p, MessageView::Inbox, None)
            .await
            .iter()
            .any(|m| m.id == future.id)
    );
}
#[tokio::test]
async fn inbox_questions_lead_grouped_unread_notes_and_reading_never_hides_open_questions() {
    let (_h, w, r, p) = setup().await;
    let z = post(&w, p, "z", "worker", Some("owner"), false, None).await;
    let a = post(&w, p, "a", "worker", Some("owner"), false, None).await;
    let q = post(&w, p, "q", "worker", Some("owner"), true, None).await;
    let other = post(&w, p, "other", "worker", Some("orchestrator"), true, None).await;
    assert_eq!(
        view(&r, p, MessageView::Inbox, None)
            .await
            .iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        vec![q.id, a.id, z.id]
    );
    read(&w, p, "q", q.id).await;
    assert_eq!(view(&r, p, MessageView::Inbox, None).await[0].id, q.id);
    assert_eq!(
        view(&r, p, MessageView::Questions, None)
            .await
            .iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        vec![q.id, other.id]
    );
    post(&w, p, "q", "owner", None, false, Some(q.id)).await;
    assert_eq!(
        view(&r, p, MessageView::Questions, None)
            .await
            .iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        vec![other.id]
    );
}
#[tokio::test]
async fn history_includes_full_owner_participation_and_thread_view_is_complete() {
    let (_h, w, r, p) = setup().await;
    let first = post(&w, p, "arc", "a", Some("b"), false, None).await;
    post(&w, p, "arc", "b", Some("owner"), false, None).await;
    post(&w, p, "arc", "owner", Some("a"), false, None).await;
    post(&w, p, "private", "a", Some("b"), false, None).await;
    post(&w, p, "owner-started", "owner", None, false, None).await;
    let history = view(&r, p, MessageView::History, None).await;
    assert_eq!(history.len(), 4);
    assert!(history.iter().any(|m| m.id == first.id));
    assert_eq!(view(&r, p, MessageView::Thread, Some("arc")).await.len(), 3);
    let since = r
        .snapshot(move |c| {
            messages(
                c,
                p,
                MessageView::Thread,
                Some("arc"),
                Some(first.id),
                "owner",
            )
        })
        .await
        .unwrap();
    assert_eq!(since.len(), 2);
    assert!(matches!(
        r.snapshot(move |c| messages(c, p, MessageView::Thread, None, None, "owner"))
            .await,
        Err(sluice_store::StoreError::Public(
            PublicError::Invalid { .. }
        ))
    ));
}
#[tokio::test]
async fn orchestrator_readers_are_keyed_by_identity_project_and_survive_rename() {
    let (_h, w, r, p) = setup().await;
    let q = ProjectId::new();
    w.write(RetrySafety::Idempotent, move |tx| {
        tx.sql().execute(
            "INSERT INTO projects(project_id,name,created_at) VALUES (?1,'q','now')",
            [q.to_string()],
        )?;
        orchestrator_read(tx, p, "lead", RecordSeq(10), Some(30))?;
        orchestrator_read(tx, p, "other", RecordSeq(3), None)?;
        orchestrator_read(tx, q, "lead", RecordSeq(7), None)?;
        orchestrator_read(tx, p, "lead", RecordSeq(2), Some(10))?;
        tx.sql().execute(
            "UPDATE projects SET name='renamed' WHERE project_id=?1",
            [p.to_string()],
        )?;
        tx.changed(Some(p), "projects");
        Ok(())
    })
    .await
    .unwrap();
    let positions = r
        .snapshot(move |c| {
            Ok((
                reader(c, p, "lead", ORCHESTRATOR_STREAM, "")?,
                reader(c, p, "other", ORCHESTRATOR_STREAM, "")?,
                reader(c, q, "lead", ORCHESTRATOR_STREAM, "")?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(positions.0.cursor, RecordSeq(10));
    assert_eq!(positions.0.unread_alert_min, Some(10));
    assert_eq!(positions.1.cursor, RecordSeq(3));
    assert_eq!(positions.2.cursor, RecordSeq(7));
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql()
            .execute("DELETE FROM projects WHERE project_id=?1", [p.to_string()])?;
        tx.changed(None, "projects");
        Ok(())
    })
    .await
    .unwrap();
    let count: i64 = r
        .snapshot(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM readers WHERE project_id=?1",
                [p.to_string()],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(count, 0);
}
#[tokio::test]
async fn next_wakes_on_addressed_questions_and_worker_answers_but_holds_notes_and_own_messages() {
    let (_h, w, r, p) = setup().await;
    let question = post(&w, p, "t", "worker", Some("worker2"), true, None).await;
    let answer = post(&w, p, "t", "owner", None, false, Some(question.id)).await;
    let note = post(&w, p, "n", "worker", Some("orchestrator"), false, None).await;
    let addressed = post(&w, p, "q", "worker", Some("orchestrator"), true, None).await;
    let broadcast = post(&w, p, "all", "worker", None, true, None).await;
    let mine = post(&w, p, "mine", "orchestrator", None, true, None).await;
    let flags = r
        .snapshot(move |c| {
            [question, answer, note, addressed, broadcast, mine]
                .iter()
                .map(|m| message_wakes(c, p, m, "orchestrator"))
                .collect::<sluice_store::Result<Vec<_>>>()
        })
        .await
        .unwrap();
    assert_eq!(flags, vec![false, true, false, true, true, false]);
}
#[tokio::test]
async fn nobody_reading_alert_is_reserved_once_and_threshold_changes_do_not_repeat_it() {
    let (_h, w, r, p) = setup().await;
    w.write(RetrySafety::Idempotent, move |tx| {
        orchestrator_read(tx, p, "lead", RecordSeq(0), Some(30))
    })
    .await
    .unwrap();
    let seq = w
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.append_record(
                Some(p),
                Event::UnitSettled {
                    unit: UnitName::new("u").unwrap(),
                    work: WorkGeneration(1),
                    steps: vec![UnitStep {
                        id: StepId::new("work").unwrap(),
                        status: sluice_model::commands::StepStatus::Failed,
                        held: false,
                        outputs: None,
                        omitted: vec![],
                    }],
                },
            )
        })
        .await
        .unwrap()
        .seq;
    let now = time::OffsetDateTime::now_utc();
    assert!(
        w.write(RetrySafety::NonIdempotent, move |tx| unread_alert(
            tx, p, "lead", now
        ))
        .await
        .unwrap()
        .is_none()
    );
    let later = now + time::Duration::hours(1);
    let alert = w
        .write(RetrySafety::NonIdempotent, move |tx| {
            unread_alert(tx, p, "lead", later)
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alert.data.unwrap().as_value()["unread_record"], seq.0);
    w.write(RetrySafety::Idempotent, move |tx| {
        orchestrator_read(tx, p, "lead", RecordSeq(0), Some(5))
    })
    .await
    .unwrap();
    assert!(
        w.write(RetrySafety::NonIdempotent, move |tx| unread_alert(
            tx, p, "lead", later
        ))
        .await
        .unwrap()
        .is_none()
    );
    assert_eq!(view(&r, p, MessageView::Inbox, None).await.len(), 1);
    let count: i64 = r
        .snapshot(|c| {
            Ok(
                c.query_row("SELECT count(*) FROM notification_attempts", [], |r| {
                    r.get(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(count, 1);
    w.write(RetrySafety::Idempotent, move |tx| {
        orchestrator_read(tx, p, "lead", RecordSeq(alert.id.0 + 1), Some(5))
    })
    .await
    .unwrap();
    assert!(
        w.write(RetrySafety::NonIdempotent, move |tx| unread_alert(
            tx, p, "lead", later
        ))
        .await
        .unwrap()
        .is_none()
    );
}
#[tokio::test]
async fn alerts_require_configured_stale_reader_waking_record_and_unarchived_project() {
    let (_h, w, _r, p) = setup().await;
    let now = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    post(&w, p, "note", "worker", Some("lead"), false, None).await;
    assert!(
        w.write(RetrySafety::NonIdempotent, move |tx| unread_alert(
            tx, p, "lead", now
        ))
        .await
        .unwrap()
        .is_none()
    );
    w.write(RetrySafety::Idempotent, move |tx| {
        orchestrator_read(tx, p, "lead", RecordSeq(0), Some(30))
    })
    .await
    .unwrap();
    assert!(
        w.write(RetrySafety::NonIdempotent, move |tx| unread_alert(
            tx, p, "lead", now
        ))
        .await
        .unwrap()
        .is_none()
    );
    post(&w, p, "q", "worker", Some("lead"), true, None).await;
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "UPDATE projects SET archived=1 WHERE project_id=?1",
            params![p.to_string()],
        )?;
        tx.changed(Some(p), "projects");
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        w.write(RetrySafety::NonIdempotent, move |tx| unread_alert(
            tx, p, "lead", now
        ))
        .await
        .unwrap()
        .is_none()
    );
}
