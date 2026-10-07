#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use rusqlite::params;
use sluice_model::{
    commands::{Message, MessageVerb, RecordPage},
    error::PublicError,
    events::Event,
    ids::{MessageId, ProjectId, RecordSeq, ResultId, Revision, RunId},
};
use sluice_store::{
    ReadPool, RetrySafety, WriteTransaction, Writer, messages::message, records::*,
};

async fn setup() -> (ScratchHome, Writer, ReadPool, ProjectId, ProjectId) {
    let home = ScratchHome::new().unwrap();
    assert!(home.root().exists());
    assert_eq!(ScratchHome::validate(home.path()).unwrap(), home.path());
    let w = Writer::open(home.path()).unwrap();
    let p = ProjectId::new();
    let q = ProjectId::new();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        for (id, name) in [(p, "p"), (q, "q")] {
            tx.sql().execute(
                "INSERT INTO projects(project_id,name,created_at) VALUES (?1,?2,'now')",
                params![id.to_string(), name],
            )?;
        }
        tx.changed(None, "projects");
        Ok(())
    })
    .await
    .unwrap();
    let r = ReadPool::open(home.path(), 2).unwrap();
    (home, w, r, p, q)
}
fn event() -> Event {
    Event::ProjectUpdate {
        fields: vec!["description".into()],
        reason: None,
        author: "test".into(),
    }
}
/// A note on any thread, as messages stored before threads were derived may be: the
/// log's thread filters must take any name.
fn note_on(
    tx: &mut WriteTransaction<'_>,
    p: ProjectId,
    thread: &str,
) -> sluice_store::Result<Message> {
    let mut m = Message {
        id: MessageId(0),
        verb: MessageVerb::Say,
        from: "orchestrator".into(),
        to: Some("owner".into()),
        thread: thread.into(),
        body: "hello".into(),
        title: None,
        ui: None,
        input: None,
        data: None,
        run: None,
        at: String::new(),
        to_message: None,
        answer: None,
        state: None,
        answered_by: None,
    };
    let record = tx.append_record(Some(p), Event::Message(Box::new(m.clone())))?;
    m.id = MessageId(record.seq.0);
    m.at = record.at;
    tx.sql().execute(
        "UPDATE records SET thread=?1,payload=?2 WHERE seq=?3",
        params![
            thread,
            serde_json::to_string(&Event::Message(Box::new(m.clone())))?,
            m.id.0
        ],
    )?;
    tx.sql().execute("INSERT INTO messages(id,project_id,thread,\"from\",\"to\",body,at) VALUES (?1,?2,?3,'orchestrator','owner','hello',?4)",params![m.id.0,p.to_string(),thread,m.at])?;
    Ok(m)
}
async fn append(w: &Writer, p: Option<ProjectId>, count: usize) -> Vec<RecordSeq> {
    w.write(RetrySafety::NonIdempotent, move |tx| {
        (0..count)
            .map(|_| tx.append_record(p, event()).map(|r| r.seq))
            .collect()
    })
    .await
    .unwrap()
}
async fn read(r: &ReadPool, p: Option<ProjectId>, filter: RecordFilter) -> RecordPage {
    r.snapshot(move |c| read_records(c, p, &filter)?.into_page())
        .await
        .unwrap()
}

#[tokio::test]
async fn global_sequence_has_gaps_and_never_reuses_trimmed_committed_ids() {
    let (_h, w, r, p, q) = setup().await;
    let a = append(&w, Some(p), 1).await[0];
    let b = append(&w, None, 1).await[0];
    let c = append(&w, Some(q), 1).await[0];
    let d = append(&w, Some(p), 3).await;
    assert!(a < b && b < c && c < d[0]);
    assert_eq!(
        read(&r, None, RecordFilter::default()).await.records[0].seq,
        b
    );
    w.write(RetrySafety::NonIdempotent, move |tx| {
        trim_to(tx, Some(p), 3, 1)
    })
    .await
    .unwrap();
    let next = append(&w, Some(p), 1).await[0];
    assert!(next > d[2]);
    assert_eq!(
        read(&r, Some(q), RecordFilter::default()).await.records[0].seq,
        c
    );
}

#[tokio::test]
async fn trimming_keeps_messages_calls_results_and_authored_edits_queryable() {
    let (_h, w, r, p, _q) = setup().await;
    let id=w.write(RetrySafety::NonIdempotent,move |tx|{
        let m=note_on(tx, p, "t")?;
        tx.sql().execute("INSERT INTO calls(call_id,project_id,fn,status,inputs,outputs,created_at,finished_at) VALUES (?1,?2,'test','succeeded','{}','{}','now','now')",params![RunId::new().to_string(),p.to_string()])?;
        tx.sql().execute("INSERT INTO step_results(result_id,project_id,step_id,generation,declaration,status,outputs,recorded_at,removed_at) VALUES (?1,?2,'removed',1,'{}','succeeded','{}','now','now')",params![ResultId::new().to_string(),p.to_string()])?;
        let edit=tx.append_record(Some(p),Event::PlanEdit {rev:Revision(1),author:"editor".into(),reason:"test".into(),ops:vec![]})?;
        tx.sql().execute("INSERT INTO plan_edits(project_id,rev,seq,at,author,reason,ops) VALUES (?1,1,?2,?3,'editor','test','[]')",params![p.to_string(),edit.seq.0,edit.at])?;Ok(m.id)
    }).await.unwrap();
    append(&w, Some(p), 12).await;
    w.write(RetrySafety::NonIdempotent, move |tx| {
        trim_to(tx, Some(p), 10, 9)
    })
    .await
    .unwrap();
    let (m, counts) = r
        .snapshot(move |c| {
            let counts = ["calls", "step_results", "plan_edits"]
                .iter()
                .map(|table| {
                    c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
                        r.get::<_, i64>(0)
                    })
                })
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok((message(c, p, id)?, counts))
        })
        .await
        .unwrap();
    assert_eq!(m.id, id);
    assert_eq!(counts, vec![1, 1, 1]);
    let ids = read(&r, Some(p), RecordFilter::default()).await.records;
    assert!(!ids.iter().any(|r| r.seq.0 == id.0));
}
#[tokio::test]
async fn trim_rollback_preserves_rows_floor_and_change_notifications() {
    let (_h, w, r, p, _q) = setup().await;
    let ids = append(&w, Some(p), 11).await;
    let watch = w.subscribe();
    let result = w
        .write(
            RetrySafety::NonIdempotent,
            move |tx| -> sluice_store::Result<()> {
                trim_to(tx, Some(p), 10, 9)?;
                Err(PublicError::Conflict {
                    message: "rollback".into(),
                    current_rev: None,
                }
                .into())
            },
        )
        .await;
    assert!(result.is_err());
    assert!(!watch.has_changed().unwrap());
    let page = read(
        &r,
        Some(p),
        RecordFilter {
            since: Some(RecordSeq(0)),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(page.records.len(), 11);
    assert_eq!(page.records[0].seq, ids[0]);
}
#[tokio::test]
async fn invalid_filters_and_future_payload_versions_fail_explicitly() {
    let (_h, w, r, p, _q) = setup().await;
    append(&w, Some(p), 1).await;
    for filter in [
        RecordFilter {
            kinds: vec!["inbox".into()],
            ..Default::default()
        },
        RecordFilter {
            limit: 0,
            ..Default::default()
        },
        RecordFilter {
            since: Some(RecordSeq(-1)),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            r.snapshot(move |c| read_records(c, Some(p), &filter)).await,
            Err(sluice_store::StoreError::Public(
                PublicError::BadRequest { .. }
            ))
        ));
    }
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "UPDATE records SET payload_version=2 WHERE project_id=?1",
            [p.to_string()],
        )?;
        tx.changed(Some(p), "log");
        Ok(())
    })
    .await
    .unwrap();
    assert!(matches!(
        r.snapshot(move |c| read_records(c, Some(p), &RecordFilter::default()))
            .await,
        Err(sluice_store::StoreError::InvalidDatabase(_))
    ));
}
