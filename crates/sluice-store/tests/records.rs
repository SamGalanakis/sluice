#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use rusqlite::params;
use serde_json::json;
use sluice_model::{
    commands::{MessagePost, RecordPage},
    error::PublicError,
    events::Event,
    ids::{ProjectId, RecordSeq, ResultId, Revision, RunId},
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    messages::{NoPlanInputs, message, message_post},
    records::*,
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
fn post(p: ProjectId, thread: &str) -> MessagePost {
    serde_json::from_value(
        json!({"project":{"kind":"id","value":p},"body":"hello","thread":thread,"from":"worker"}),
    )
    .unwrap()
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
async fn filters_groups_threads_since_tail_and_forward_pagination_share_a_snapshot_cursor() {
    let (_h, w, r, p, q) = setup().await;
    let mut ids = vec![];
    for thread in ["a", "b", "a", "b", "a", "b"] {
        ids.push(
            w.write(RetrySafety::NonIdempotent, move |tx| {
                message_post(tx, post(p, thread), &NoPlanInputs)
            })
            .await
            .unwrap()
            .id
            .0,
        );
        append(&w, Some(q), 1).await;
    }
    let top = append(&w, Some(p), 1).await[0];
    let a = read(
        &r,
        Some(p),
        RecordFilter {
            threads: vec!["a".into()],
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        a.records.iter().map(|r| r.seq.0).collect::<Vec<_>>(),
        vec![ids[0], ids[2], ids[4]]
    );
    assert_eq!(a.last_seq, top);
    let mixed = read(
        &r,
        Some(p),
        RecordFilter {
            kinds: vec!["message".into(), "project".into()],
            threads: vec!["b".into()],
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        mixed.records.iter().map(|r| r.seq.0).collect::<Vec<_>>(),
        vec![ids[1], ids[3], ids[5], top.0]
    );
    let tail = read(
        &r,
        Some(p),
        RecordFilter {
            limit: 2,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        tail.records.iter().map(|r| r.seq.0).collect::<Vec<_>>(),
        vec![ids[5], top.0]
    );
    let page = read(
        &r,
        Some(p),
        RecordFilter {
            since: Some(RecordSeq(ids[1])),
            kinds: vec!["message".into()],
            limit: 2,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(page.last_seq.0, ids[3]);
    let rest = read(
        &r,
        Some(p),
        RecordFilter {
            since: Some(page.last_seq),
            kinds: vec!["message".into()],
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        rest.records.iter().map(|r| r.seq.0).collect::<Vec<_>>(),
        vec![ids[4], ids[5]]
    );
    assert_eq!(rest.last_seq, top);
    let future = append(&w, Some(q), 1).await[0];
    assert_eq!(
        read(
            &r,
            Some(p),
            RecordFilter {
                since: Some(future),
                ..Default::default()
            }
        )
        .await
        .last_seq,
        future
    );
}
#[tokio::test]
async fn expired_cursors_return_typed_bounds_and_other_projects_do_not_expire() {
    let (_h, w, r, p, q) = setup().await;
    let untouched = append(&w, Some(q), 1).await[0];
    let ids = append(&w, Some(p), 11).await;
    assert_eq!(
        w.write(RetrySafety::NonIdempotent, move |tx| trim_to(
            tx,
            Some(p),
            10,
            9
        ))
        .await
        .unwrap(),
        2
    );
    let expired = r
        .snapshot(move |c| {
            read_records(
                c,
                Some(p),
                &RecordFilter {
                    since: Some(RecordSeq(0)),
                    kinds: vec!["message".into()],
                    ..Default::default()
                },
            )
        })
        .await
        .unwrap();
    assert_eq!(
        expired,
        RecordRead::CursorExpired {
            earliest: ids[2],
            latest: ids[10]
        }
    );
    let error = expired.into_page().unwrap_err().into_public(true);
    assert!(matches!(error, PublicError::CursorExpired { .. }));
    assert!(error.to_string().contains("resnapshot"));
    let at_floor = read(
        &r,
        Some(p),
        RecordFilter {
            since: Some(ids[1]),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(at_floor.records.len(), 9);
    let still = read(
        &r,
        Some(q),
        RecordFilter {
            since: Some(RecordSeq(0)),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(still.records[0].seq, untouched);
}
#[tokio::test]
async fn gaps_without_a_trim_are_valid_cursors() {
    let (_h, w, r, p, q) = setup().await;
    let other = append(&w, Some(q), 5).await[4];
    let own = append(&w, Some(p), 1).await[0];
    let page = read(
        &r,
        Some(p),
        RecordFilter {
            since: Some(RecordSeq(0)),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(page.records[0].seq, own);
    assert!(own > other);
    assert_eq!(
        read(
            &r,
            Some(p),
            RecordFilter {
                since: Some(other),
                ..Default::default()
            }
        )
        .await
        .records[0]
            .seq,
        own
    );
}
#[tokio::test]
async fn default_retention_trims_only_after_10000_and_keeps_9000() {
    let (_h, w, r, p, _q) = setup().await;
    append(&w, Some(p), RETENTION_MAX).await;
    assert_eq!(
        w.write(RetrySafety::NonIdempotent, move |tx| trim_records(
            tx,
            Some(p)
        ))
        .await
        .unwrap(),
        0
    );
    append(&w, Some(p), 1).await;
    assert_eq!(
        w.write(RetrySafety::NonIdempotent, move |tx| trim_records(
            tx,
            Some(p)
        ))
        .await
        .unwrap(),
        1001
    );
    let count: i64 = r
        .snapshot(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM records WHERE project_id=?1",
                [p.to_string()],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(count, 9000);
}
#[tokio::test]
async fn trimming_keeps_messages_calls_results_and_authored_edits_queryable() {
    let (_h, w, r, p, _q) = setup().await;
    let id=w.write(RetrySafety::NonIdempotent,move |tx|{
        let m=message_post(tx,post(p,"t"),&NoPlanInputs)?;
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
#[tokio::test]
async fn empty_feed_and_filtered_empty_page_preserve_the_high_watermark() {
    let (_h, w, r, p, _q) = setup().await;
    let page = read(&r, Some(p), RecordFilter::default()).await;
    assert!(page.records.is_empty());
    assert_eq!(page.last_seq, RecordSeq(0));
    let seq = append(&w, Some(p), 1).await[0];
    let filtered = read(
        &r,
        Some(p),
        RecordFilter {
            kinds: vec!["call".into()],
            ..Default::default()
        },
    )
    .await;
    assert!(filtered.records.is_empty());
    assert_eq!(filtered.last_seq, seq);
}
#[tokio::test]
async fn filter_values_are_bound_and_literal_thread_names_round_trip() {
    let (_h, w, r, p, _q) = setup().await;
    let name = "x' OR 1=1 -- %_*";
    let m = w
        .write(RetrySafety::NonIdempotent, move |tx| {
            message_post(tx, post(p, name), &NoPlanInputs)
        })
        .await
        .unwrap();
    let page = read(
        &r,
        Some(p),
        RecordFilter {
            threads: vec![name.into()],
            ..Default::default()
        },
    )
    .await;
    assert_eq!(page.records[0].seq.0, m.id.0);
    let empty = read(
        &r,
        Some(p),
        RecordFilter {
            threads: vec!["other".into()],
            ..Default::default()
        },
    )
    .await;
    assert!(empty.records.is_empty());
}
