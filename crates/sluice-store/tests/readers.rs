#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use rusqlite::params;
use sluice_model::{
    commands::{MarkRead, Message, MessageVerb, MessageView},
    events::Event,
    ids::{MessageId, ProjectId, RecordSeq},
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
