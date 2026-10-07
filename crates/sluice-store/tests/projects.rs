#[allow(dead_code)]
#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use serde_json::{Value, json};
use sluice_model::{
    error::PublicError,
    ids::{AttemptId, ProjectId, ProjectSelector, Revision, RunId},
};
use sluice_store::{
    ReadPool, RetrySafety, StoreError, Writer, artifacts,
    projects::{
        self, CreateProject, DeleteProject, EmptyPlanInitializer, Icon, NoResourceSettings,
        Project, ResourceSettings, StoredWorkOnly, UpdateProject,
    },
};

fn selector(id: ProjectId) -> ProjectSelector {
    ProjectSelector::Id(id)
}
async fn setup() -> (ScratchHome, Writer, ReadPool) {
    let home = ScratchHome::new().unwrap();
    assert_eq!(ScratchHome::validate(home.path()).unwrap(), home.path());
    let writer = Writer::open(home.path()).unwrap();
    let reads = ReadPool::open(home.path(), 2).unwrap();
    (home, writer, reads)
}
async fn create(writer: &Writer, name: &str) -> Project {
    let request = CreateProject {
        name: name.parse().unwrap(),
        description: "description".into(),
        icon: None,
        resources: None,
        author: "sam".into(),
    };
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::project_create(tx, request, &EmptyPlanInitializer, &NoResourceSettings)
        })
        .await
        .unwrap()
}
async fn update(
    writer: &Writer,
    id: ProjectId,
    request: UpdateProject,
) -> Result<Project, PublicError> {
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::project_update(tx, &selector(id), request, &NoResourceSettings)
        })
        .await
}
async fn archive(writer: &Writer, id: ProjectId) -> Project {
    update(
        writer,
        id,
        UpdateProject {
            archived: Some(true),
            author: "sam".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}
async fn delete(writer: &Writer, p: &Project) -> Result<projects::DeletedProject, PublicError> {
    let request = DeleteProject {
        confirm_name: p.name.to_string(),
        expected_settings_rev: p.settings_rev,
        author: "sam".into(),
    };
    let id = p.project_id;
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::project_delete(tx, &selector(id), request, &StoredWorkOnly)
        })
        .await
}
async fn records(reads: &ReadPool, id: Option<ProjectId>) -> Vec<Value> {
    reads
        .snapshot(move |c| {
            let mut stmt =
                c.prepare("SELECT payload FROM records WHERE project_id IS ?1 ORDER BY seq")?;
            let rows = stmt
                .query_map([id.map(|p| p.to_string())], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows.into_iter()
                .map(|r| Ok(serde_json::from_str(&r)?))
                .collect()
        })
        .await
        .unwrap()
}
async fn snapshot(reads: &ReadPool, id: ProjectId) -> (Project, Vec<Value>, i64) {
    let p = reads
        .snapshot(move |c| projects::resolve(c, &selector(id)))
        .await
        .unwrap();
    let r = records(reads, Some(id)).await;
    let jobs = reads
        .snapshot(|c| Ok(c.query_row("SELECT count(*) FROM artifact_jobs", [], |r| r.get(0))?))
        .await
        .unwrap();
    (p, r, jobs)
}

#[tokio::test]
async fn rename_keeps_cursors_readers_sessions_and_foreign_keys() {
    let (home, writer, reads) = setup().await;
    let p = create(&writer, "p").await;
    let id = p.project_id;
    let run = RunId::new();
    let attempt = AttemptId::new();
    writer.write(RetrySafety::NonIdempotent,move|tx| {
        tx.sql().execute("INSERT INTO readers(project_id,identity,stream,cursor) VALUES (?1,'owner','next',42)",[id.to_string()])?;
        tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,phase,request,inputs_hash,created_at) VALUES (?1,?2,'executing','{}','hash','now')",[attempt.to_string(),id.to_string()])?;
        tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,created_at) VALUES (?1,?2,?3,'now')",[run.to_string(),id.to_string(),attempt.to_string()])?;
        tx.sql().execute("INSERT INTO sessions(run_id,project_id,engine,cwd,session_id,recorded_at) VALUES (?1,?2,'codex','/repo','session','now')",[run.to_string(),id.to_string()])?;
        tx.changed(Some(id),"messages"); Ok(())
    }).await.unwrap();
    artifacts::recover(&writer, home.path()).await.unwrap();
    let keys = vec![sluice_store::ChangeKey::new(Some(id), "settings")];
    let mut subscription = reads.subscribe(&writer, keys).await.unwrap();
    let renamed = update(
        &writer,
        id,
        UpdateProject {
            new_name: Some("new".parse().unwrap()),
            expected_settings_rev: Some(Revision(1)),
            author: "owner".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(renamed.project_id, id);
    assert_eq!(renamed.settings_rev, Revision(2));
    subscription.wait().await.unwrap();
    let (cursor, run_owner, session_owner, plan_owner) = reads
        .snapshot(move |c| {
            Ok((
                c.query_row("SELECT cursor FROM readers", [], |r| r.get::<_, i64>(0))?,
                c.query_row("SELECT project_id FROM runs", [], |r| r.get::<_, String>(0))?,
                c.query_row("SELECT project_id FROM sessions", [], |r| {
                    r.get::<_, String>(0)
                })?,
                c.query_row("SELECT project_id FROM plans", [], |r| {
                    r.get::<_, String>(0)
                })?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(cursor, 42);
    assert_eq!(
        (run_owner, session_owner, plan_owner),
        (id.to_string(), id.to_string(), id.to_string())
    );
    assert!(home.path().join(format!("projects/{id}")).is_dir());
    let record = records(&reads, Some(id)).await.pop().unwrap();
    assert_eq!(
        record,
        json!({"kind":"project.rename","old_name":"p","new_name":"new","author":"owner"})
    );
    let stale = reads
        .snapshot(|c| projects::resolve(c, &"p".parse().unwrap()))
        .await
        .unwrap_err();
    assert!(matches!(
        stale,
        StoreError::Public(PublicError::NotFound { .. })
    ));
    let callback = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_update(
                tx,
                &"p".parse().unwrap(),
                UpdateProject {
                    description: Some("wrong".into()),
                    ..Default::default()
                },
                &NoResourceSettings,
            )
        })
        .await
        .unwrap_err();
    assert!(matches!(callback, PublicError::NotFound { .. }));
    writer.shutdown().await.unwrap();
}

/// A bare project id selects by id, so neither creation nor rename may take a UUID as a name.
#[tokio::test]
async fn names_that_look_like_project_ids_are_refused() {
    let (_home, writer, reads) = setup().await;
    let p = create(&writer, "p").await;
    let id = p.project_id;
    let before = snapshot(&reads, id).await;
    let v7 = ProjectId::new().to_string();
    let v4 = "0d4f1f3e-2b6c-4a5e-9f1d-3c2b1a0f9e8d".to_owned();
    let simple = v7.replace('-', "");
    for name in [v7, v4, simple] {
        let refused = writer
            .write(RetrySafety::NonIdempotent, {
                let name = name.clone();
                move |tx| {
                    projects::project_create(
                        tx,
                        CreateProject {
                            name: name.parse().unwrap(),
                            description: String::new(),
                            icon: None,
                            resources: None,
                            author: "owner".into(),
                        },
                        &EmptyPlanInitializer,
                        &NoResourceSettings,
                    )
                }
            })
            .await;
        assert!(
            matches!(&refused, Err(PublicError::BadRequest { message }) if message.contains("looks like a project id")),
            "{name}: {refused:?}"
        );
        let renamed = update(
            &writer,
            id,
            UpdateProject {
                new_name: Some(name.parse().unwrap()),
                description: Some("changed".into()),
                author: "owner".into(),
                ..Default::default()
            },
        )
        .await;
        assert!(
            matches!(renamed, Err(PublicError::BadRequest { .. })),
            "{name}"
        );
        assert_eq!(snapshot(&reads, id).await, before);
    }
    let count: i64 = reads
        .snapshot(|c| Ok(c.query_row("SELECT count(*) FROM projects", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(count, 1);
    // Ordinary names that merely contain hex digits and hyphens stay valid.
    create(&writer, "deadbeef-cafe").await;
    writer.shutdown().await.unwrap();
}

#[test]
fn invalid_icons_are_refused_and_all_supported_formats_are_sniffed() {
    assert!(Icon::text(&"x".repeat(17)).is_err());
    assert!(Icon::text("a\nb").is_err());
    assert!(Icon::image(b"not an image".to_vec()).is_err());
    assert!(Icon::image(vec![0; projects::ICON_MAX + 1]).is_err());
    let home = ScratchHome::new().unwrap();
    assert!(Icon::from_argument("/no/such/icon.png", home.path()).is_err());
    assert!(Icon::from_argument("~/no-such-icon.png", home.path()).is_err());
    for bytes in [
        b"GIF87a".as_slice(),
        b"GIF89a",
        b"\xff\xd8\xff",
        b"RIFF1234WEBP",
        b"<?xml version=\"1.0\"?><svg/>",
        b"\xef\xbb\xbf<svg/>",
    ] {
        Icon::image(bytes.to_vec()).unwrap();
    }
}

#[tokio::test]
async fn deletion_refuses_running_steps_queued_calls_direct_calls_attempts_and_guardians() {
    for kind in [
        "step",
        "pending-call",
        "running-call",
        "direct",
        "pending-direct",
        "attempt",
        "guardian",
    ] {
        let (_home, writer, reads) = setup().await;
        let p = create(&writer, "p").await;
        let p = archive(&writer, p.project_id).await;
        let id = p.project_id;
        writer.write(RetrySafety::NonIdempotent,move|tx| {
            if kind=="step" { tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,status) VALUES (?1,'a',0,'{}','running')",[id.to_string()])?; }
            else if kind.contains("call") || kind.contains("direct") { tx.sql().execute("INSERT INTO calls(call_id,project_id,fn,status,inputs,direct,created_at) VALUES (?1,?2,'test',?3,'{}',?4,'now')",rusqlite::params![RunId::new().to_string(),id.to_string(),if kind.starts_with("pending"){"pending"}else{"running"},kind.contains("direct")])?; }
            else { let a=AttemptId::new(); tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,phase,request,inputs_hash,created_at) VALUES (?1,?2,?3,'{}','hash','now')",[a.to_string(),id.to_string(),if kind=="attempt"{"reserved".into()}else{"terminal".into()}])?; if kind=="guardian" { tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,guardian_pid,created_at) VALUES (?1,?2,?3,123,'now')",[RunId::new().to_string(),id.to_string(),a.to_string()])?; } }
            tx.changed(Some(id),"status"); Ok(())
        }).await.unwrap();
        let before = snapshot(&reads, id).await;
        assert!(
            matches!(
                delete(&writer, &p).await,
                Err(PublicError::BadRequest { .. })
            ),
            "{kind}"
        );
        assert_eq!(snapshot(&reads, id).await, before);
        writer.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn deleting_and_recreating_name_keeps_new_files_and_drain_ownership_separate() {
    let (home, writer, reads) = setup().await;
    let p = create(&writer, "p").await;
    let old = p.project_id;
    artifacts::recover(&writer, home.path()).await.unwrap();
    std::fs::write(home.path().join(format!("projects/{old}/.env")), b"secret").unwrap();
    let p = archive(&writer, old).await;
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE maintenance SET paused_projects=?1",
                [json!([old.to_string()]).to_string()],
            )?;
            tx.changed(None, "maintenance");
            Ok(())
        })
        .await
        .unwrap();
    delete(&writer, &p).await.unwrap();
    let replacement = create(&writer, "p").await;
    assert_ne!(replacement.project_id, old);
    artifacts::recover(&writer, home.path()).await.unwrap();
    let dir = home
        .path()
        .join(format!("projects/{}", replacement.project_id));
    std::fs::write(dir.join(".env"), b"new").unwrap();
    artifacts::recover(&writer, home.path()).await.unwrap();
    assert_eq!(std::fs::read(dir.join(".env")).unwrap(), b"new");
    assert!(!home.path().join(format!("projects/{old}")).exists());
    assert!(records(&reads, Some(old)).await.is_empty());
    assert_eq!(
        records(&reads, None).await[0],
        json!({"kind":"project.delete","project_id":old.to_string(),"name":"p","author":"sam"})
    );
    let ownership: String = reads
        .snapshot(|c| Ok(c.query_row("SELECT paused_projects FROM maintenance", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(ownership, "[]");
    writer.shutdown().await.unwrap();
}

struct RejectResources;
impl ResourceSettings for RejectResources {
    fn set_resources(
        &self,
        tx: &mut sluice_store::WriteTransaction<'_>,
        id: ProjectId,
        _: &Value,
    ) -> sluice_store::Result<bool> {
        tx.sql().execute("INSERT INTO resources(scope,project_id,name,declaration,capacity) VALUES (?1,?1,'cpu','2',2)",[id.to_string()])?;
        Err(PublicError::BadRequest {
            message: "invalid resource function".into(),
        }
        .into())
    }
}
#[tokio::test]
async fn resource_adapter_failure_rolls_back_rename_and_creation() {
    let (_home, writer, reads) = setup().await;
    let p = create(&writer, "p").await;
    let id = p.project_id;
    let before = snapshot(&reads, id).await;
    let error = writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::project_update(
                tx,
                &selector(id),
                UpdateProject {
                    new_name: Some("q".parse().unwrap()),
                    resources: Some(json!({"cpu":2})),
                    ..Default::default()
                },
                &RejectResources,
            )
        })
        .await;
    assert!(matches!(error, Err(PublicError::BadRequest { .. })));
    assert_eq!(snapshot(&reads, id).await, before);
    let result = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "q".parse().unwrap(),
                    description: "".into(),
                    icon: None,
                    resources: Some(json!({"cpu":2})),
                    author: "sam".into(),
                },
                &EmptyPlanInitializer,
                &RejectResources,
            )
        })
        .await;
    assert!(result.is_err());
    let counts = reads
        .snapshot(|c| {
            Ok((
                c.query_row("SELECT count(*) FROM projects", [], |r| r.get::<_, i64>(0))?,
                c.query_row("SELECT count(*) FROM resources", [], |r| r.get::<_, i64>(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(counts, (1, 0));
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn owned_rows_and_global_run_directories_are_removed_but_shared_engine_home_is_retained() {
    let (home, writer, reads) = setup().await;
    let p = create(&writer, "p").await;
    let q = create(&writer, "q").await;
    let id = p.project_id;
    let other = q.project_id;
    let engine = format!("engine-homes/{}", sluice_model::ids::InvocationId::new());
    let private = engine.clone();
    let run = RunId::new();
    let run2 = RunId::new();
    writer.write(RetrySafety::NonIdempotent,move|tx| {
        for (owner,r) in [(id,run),(other,run2)] {let a=AttemptId::new();tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,phase,request,inputs_hash,created_at) VALUES (?1,?2,'terminal','{}','hash','now')",[a.to_string(),owner.to_string()])?;tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,created_at,finished_at) VALUES (?1,?2,?3,'now','done')",[r.to_string(),owner.to_string(),a.to_string()])?;tx.sql().execute("INSERT INTO sessions(run_id,project_id,engine,cwd,session_id,metadata,recorded_at) VALUES (?1,?2,'codex','/repo',?1,?3,'now')",[r.to_string(),owner.to_string(),json!({"private_home":private}).to_string()])?;}
        tx.sql().execute("INSERT INTO calls(call_id,run_id,project_id,fn,status,inputs,direct,created_at) VALUES (?1,?1,?2,'test','succeeded','{}',1,'now')",[run.to_string(),id.to_string()])?;
        tx.sql().execute("INSERT INTO submissions(run_id,project_id,outputs,at) VALUES (?1,?2,'{}','now')",[run.to_string(),id.to_string()])?;
        tx.sql().execute("INSERT INTO resources(scope,project_id,name,declaration) VALUES (?1,?1,'cpu','2')",[id.to_string()])?;
        tx.sql().execute("INSERT INTO leases(project_id,run_id,request_id,scope,resource,amount,state,created_at) VALUES (?1,?2,'request',?1,'cpu',1,'released','now')",[id.to_string(),run.to_string()])?;
        tx.sql().execute("INSERT INTO messages(id,project_id,thread,\"from\",body,at) VALUES (900,?1,'t','owner','hi','now')",[id.to_string()])?;
        tx.sql().execute("INSERT INTO readers(project_id,identity,stream,cursor) VALUES (?1,'owner','next',900)",[id.to_string()])?;
        tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration) VALUES (?1,'a',0,'{}')",[id.to_string()])?;
        tx.sql().execute("INSERT INTO messages(id,project_id,thread,\"from\",body,reply_to,at) VALUES (901,?1,'t','owner','reply',900,'now')",[id.to_string()])?;
        tx.sql().execute("UPDATE messages SET resolved_by=901 WHERE id=900",[])?;
        tx.sql().execute("INSERT INTO notification_attempts(project_id,message_id,attempt_id,outcome,reserved_at) VALUES (?1,900,'notify','reserved','now')",[id.to_string()])?;
        tx.sql().execute("INSERT INTO question_attachments(project_id,message_id,run_id,step_id,generation,title,attached_at,detached_at) VALUES (?1,900,?2,'a',1,'q','now','done')",[id.to_string(),run.to_string()])?;
        tx.sql().execute("INSERT INTO message_deliveries(project_id,run_id,message_id,assigned_at) VALUES (?1,?2,900,'now')",[id.to_string(),run.to_string()])?;
        tx.sql().execute("INSERT INTO inputs(project_id,name,position,declaration,value) VALUES (?1,'n',0,'\"int\"','1')",[id.to_string()])?;
        let result=sluice_model::ids::ResultId::new().to_string();
        tx.sql().execute("INSERT INTO step_results(result_id,project_id,step_id,generation,declaration,status,recorded_at) VALUES (?1,?2,'a',1,'{}','succeeded','now')",[result.clone(),id.to_string()])?;
        tx.sql().execute("UPDATE steps SET result_id=?2 WHERE project_id=?1",[id.to_string(),result])?;

        tx.changed(Some(id),"status");Ok(())
    }).await.unwrap();
    std::fs::create_dir_all(home.path().join(format!("runs/{run}"))).unwrap();
    std::fs::create_dir_all(home.path().join(&engine)).unwrap();
    std::fs::write(home.path().join(&engine).join("session"), b"shared").unwrap();
    let p = archive(&writer, id).await;
    delete(&writer, &p).await.unwrap();
    artifacts::recover(&writer, home.path()).await.unwrap();
    assert!(!home.path().join(format!("runs/{run}")).exists());
    assert!(home.path().join(&engine).is_dir());
    reads
        .snapshot(move |c| {
            for table in [
                "plans",
                "steps",
                "plan_edits",
                "inputs",
                "step_results",
                "attempts",
                "notification_attempts",
                "question_attachments",
                "message_deliveries",
                "runs",
                "calls",
                "sessions",
                "submissions",
                "leases",
                "resources",
                "messages",
                "readers",
                "records",
            ] {
                assert_eq!(
                    c.query_row(
                        &format!("SELECT count(*) FROM {table} WHERE project_id=?1"),
                        [id.to_string()],
                        |r| r.get::<_, i64>(0)
                    )?,
                    0,
                    "{table}"
                );
            }
            Ok(())
        })
        .await
        .unwrap();
    let q = archive(&writer, other).await;
    delete(&writer, &q).await.unwrap();
    artifacts::recover(&writer, home.path()).await.unwrap();
    assert!(!home.path().join(engine).exists());
    writer.shutdown().await.unwrap();
}

async fn set_board(
    writer: &Writer,
    id: ProjectId,
    program: Option<&str>,
    expected_rev: Option<u64>,
) -> Result<Revision, PublicError> {
    let request = projects::SetBoard {
        program: program.map(Into::into),
        expected_rev: expected_rev.map(Revision),
        reason: Some("lane overview".into()),
        author: "orch".into(),
    };
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::board_set(tx, &selector(id), request)
        })
        .await
}

#[tokio::test]
async fn a_home_from_before_boards_gains_them_when_its_writer_opens() {
    let home = ScratchHome::new().unwrap();
    let id = {
        let writer = Writer::open(home.path()).unwrap();
        create(&writer, "old").await.project_id
    };
    // Drop the board columns, as a release before boards left the home.
    {
        let c = rusqlite::Connection::open(home.path().join("sluice.db")).unwrap();
        c.execute_batch(
            "ALTER TABLE projects DROP COLUMN board_rev; ALTER TABLE projects DROP COLUMN board;",
        )
        .unwrap();
    }
    assert!(matches!(
        ReadPool::open(home.path(), 1).map(|_| ()),
        Err(StoreError::InvalidDatabase(_))
    ));
    let writer = Writer::open(home.path()).unwrap();
    let reads = ReadPool::open(home.path(), 1).unwrap();
    let p = reads
        .snapshot(move |c| projects::resolve(c, &selector(id)))
        .await
        .unwrap();
    assert_eq!((p.board, p.board_rev), (None, Revision(0)));
    assert_eq!(
        set_board(&writer, id, Some("root = Text(\"hello\")"), Some(0))
            .await
            .unwrap(),
        Revision(1)
    );
}

#[tokio::test]
async fn a_home_from_before_progress_gains_its_columns_when_its_writer_opens() {
    let home = ScratchHome::new().unwrap();
    {
        let writer = Writer::open(home.path()).unwrap();
        create(&writer, "old").await;
    }
    // Drop the progress columns, as a release before step_progress left the home.
    {
        let c = rusqlite::Connection::open(home.path().join("sluice.db")).unwrap();
        c.execute_batch(
            "ALTER TABLE steps DROP COLUMN progress_run; ALTER TABLE steps DROP COLUMN progress_at; ALTER TABLE steps DROP COLUMN progress;",
        )
        .unwrap();
    }
    // Readers refuse it until the writer has added them; the schema version stays 1.
    assert!(matches!(
        ReadPool::open(home.path(), 1).map(|_| ()),
        Err(StoreError::InvalidDatabase(_))
    ));
    let _writer = Writer::open(home.path()).unwrap();
    let c = rusqlite::Connection::open(home.path().join("sluice.db")).unwrap();
    let columns: Vec<String> = c
        .prepare(
            "SELECT name FROM pragma_table_info('steps') WHERE name LIKE 'progress%' ORDER BY cid",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(columns, ["progress", "progress_at", "progress_run"]);
    let version: i64 = c
        .query_row("SELECT schema_version FROM home_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 1);
    ReadPool::open(home.path(), 1).unwrap();
}

/// The board briefly shipped as schema 2, which a run's pinned CLI from before it refuses: the
/// writer marks such a home 1 again and keeps its boards.
#[tokio::test]
async fn a_home_marked_with_the_interim_board_schema_is_marked_one_again() {
    let home = ScratchHome::new().unwrap();
    let id = {
        let writer = Writer::open(home.path()).unwrap();
        let id = create(&writer, "interim").await.project_id;
        set_board(&writer, id, Some("root = Text(\"kept\")"), Some(0))
            .await
            .unwrap();
        writer.shutdown().await.unwrap();
        id
    };
    let version = |c: &rusqlite::Connection| -> (i64, i64) {
        (
            c.query_row("SELECT schema_version FROM home_meta", [], |r| r.get(0))
                .unwrap(),
            c.pragma_query_value(None, "user_version", |r| r.get(0))
                .unwrap(),
        )
    };
    {
        let c = rusqlite::Connection::open(home.path().join("sluice.db")).unwrap();
        c.execute_batch("UPDATE home_meta SET schema_version=2; PRAGMA user_version=2;")
            .unwrap();
    }
    assert!(matches!(
        ReadPool::open(home.path(), 1).map(|_| ()),
        Err(StoreError::UnsupportedSchema { found: 2 })
    ));
    let _writer = Writer::open(home.path()).unwrap();
    let c = rusqlite::Connection::open(home.path().join("sluice.db")).unwrap();
    assert_eq!(version(&c), (1, 1));
    let reads = ReadPool::open(home.path(), 1).unwrap();
    let p = reads
        .snapshot(move |c| projects::resolve(c, &selector(id)))
        .await
        .unwrap();
    assert_eq!(
        (p.board.as_deref(), p.board_rev),
        (Some("root = Text(\"kept\")"), Revision(1))
    );
}

async fn set_slot(
    writer: &Writer,
    id: ProjectId,
    key: &str,
    markdown: Option<&str>,
) -> Result<projects::SlotChange, PublicError> {
    let request = projects::SetBoardSlot {
        key: key.into(),
        markdown: markdown.map(Into::into),
        author: "orch".into(),
    };
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::board_slot_set(tx, &selector(id), request)
        })
        .await
}
async fn slots(reads: &ReadPool, id: ProjectId) -> Vec<projects::BoardSlot> {
    reads
        .snapshot(move |c| projects::board_slots(c, id))
        .await
        .unwrap()
}
fn invalid_message(result: Result<projects::SlotChange, PublicError>) -> String {
    match result {
        Err(PublicError::Invalid { message, errors }) => {
            assert_eq!(errors, std::slice::from_ref(&message));
            message
        }
        other => panic!("{other:?}"),
    }
}

/// A board slot is set and cleared with no revision, each change a `project.update` record
/// naming the slot (a kind every pinned release reads), and the settings revision untouched.
#[tokio::test]
async fn a_board_slot_is_set_and_cleared_without_a_revision_and_recorded() {
    let (_home, writer, reads) = setup().await;
    let p = create(&writer, "slots").await;
    let id = p.project_id;
    let set = set_slot(&writer, id, "phase", Some("**Green** soon"))
        .await
        .unwrap();
    assert!(set.changed);
    let slot = set.slot.unwrap();
    assert_eq!(
        (
            slot.key.as_str(),
            slot.markdown.as_str(),
            slot.author.as_str()
        ),
        ("phase", "**Green** soon", "orch")
    );
    assert_eq!(slots(&reads, id).await, std::slice::from_ref(&slot));
    // The same markdown again changes nothing: no record, the same time.
    let again = set_slot(&writer, id, "phase", Some("**Green** soon"))
        .await
        .unwrap();
    assert_eq!((again.changed, again.slot), (false, Some(slot.clone())));
    set_slot(&writer, id, "needs-sam", Some("- one\n- two"))
        .await
        .unwrap();
    let keys: Vec<String> = slots(&reads, id).await.into_iter().map(|s| s.key).collect();
    assert_eq!(keys, ["needs-sam", "phase"]);
    // "" and null both clear; clearing a slot that is not set changes nothing.
    let cleared = set_slot(&writer, id, "phase", Some("")).await.unwrap();
    assert_eq!((cleared.changed, cleared.slot), (true, None));
    assert!(!set_slot(&writer, id, "phase", None).await.unwrap().changed);
    assert!(
        set_slot(&writer, id, "needs-sam", None)
            .await
            .unwrap()
            .changed
    );
    assert!(slots(&reads, id).await.is_empty());
    let stored: Option<String> = reads
        .snapshot(move |c| {
            Ok(c.query_row(
                "SELECT board_slots FROM projects WHERE project_id=?1",
                [id.to_string()],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(stored, None, "no slots left is no value");
    let (after, records, _) = snapshot(&reads, id).await;
    let updates: Vec<&Value> = records
        .iter()
        .filter(|r| r["kind"] == "project.update")
        .collect();
    assert_eq!(
        updates,
        [
            &json!({"kind":"project.update","fields":["board_slot:phase"],"reason":null,"author":"orch"}),
            &json!({"kind":"project.update","fields":["board_slot:needs-sam"],"reason":null,"author":"orch"}),
            &json!({"kind":"project.update","fields":["board_slot:phase"],"reason":"cleared","author":"orch"}),
            &json!({"kind":"project.update","fields":["board_slot:needs-sam"],"reason":"cleared","author":"orch"}),
        ]
    );
    assert_eq!(after.settings_rev, p.settings_rev);
    assert_eq!(after.board_rev, Revision(0));
}

#[tokio::test]
async fn a_board_slot_refuses_a_bad_key_long_markdown_and_too_many_slots() {
    let (_home, writer, reads) = setup().await;
    let id = create(&writer, "limits").await.project_id;
    for key in ["", "Phase", "-lead", "a b", "a/b", &"k".repeat(65)] {
        let message = invalid_message(set_slot(&writer, id, key, Some("x")).await);
        assert!(message.starts_with("key: "), "{key}: {message}");
    }
    for key in ["a", "0", "phase.main", "needs_sam-2", &"k".repeat(64)] {
        set_slot(&writer, id, key, Some("x")).await.unwrap();
    }
    let long = "x".repeat(16 * 1024 + 1);
    let message = invalid_message(set_slot(&writer, id, "long", Some(&long)).await);
    assert!(message.contains("over the slot's 16 KiB"), "{message}");
    set_slot(&writer, id, "long", Some(&long[1..]))
        .await
        .unwrap();
    for n in slots(&reads, id).await.len()..64 {
        set_slot(&writer, id, &format!("s{n}"), Some("x"))
            .await
            .unwrap();
    }
    assert_eq!(slots(&reads, id).await.len(), 64);
    let message = invalid_message(set_slot(&writer, id, "one-more", Some("x")).await);
    assert!(message.contains("64 slots"), "{message}");
    // A slot already there still changes, and a clear makes room.
    set_slot(&writer, id, "a", Some("y")).await.unwrap();
    set_slot(&writer, id, "a", None).await.unwrap();
    set_slot(&writer, id, "one-more", Some("x")).await.unwrap();
    // All of a project's slots together stay under 256 KiB.
    for n in 0..64 {
        set_slot(&writer, id, &format!("s{n}"), None).await.unwrap();
    }
    let mut refused = None;
    for n in 0..20 {
        if let Err(e) = set_slot(&writer, id, &format!("big{n}"), Some(&long[1..])).await {
            refused = Some(e);
            break;
        }
    }
    let Some(PublicError::Invalid { message, .. }) = refused else {
        panic!("{refused:?}")
    };
    assert!(message.contains("256 KiB together"), "{message}");
    // An unknown project is not_found.
    let missing = set_slot(&writer, ProjectId::new(), "a", Some("x")).await;
    assert!(
        matches!(missing, Err(PublicError::NotFound { .. })),
        "{missing:?}"
    );
}

/// Slots are a column and a view added to an existing home, never a table: every pinned
/// release counts the home's tables and refuses a 24th.
#[tokio::test]
async fn a_home_from_before_slots_gains_them_when_its_writer_opens() {
    let home = ScratchHome::new().unwrap();
    let id = {
        let writer = Writer::open(home.path()).unwrap();
        let id = create(&writer, "old").await.project_id;
        writer.shutdown().await.unwrap();
        id
    };
    {
        let c = rusqlite::Connection::open(home.path().join("sluice.db")).unwrap();
        c.execute_batch("DROP VIEW board_slots; ALTER TABLE projects DROP COLUMN board_slots;")
            .unwrap();
    }
    assert!(matches!(
        ReadPool::open(home.path(), 1).map(|_| ()),
        Err(StoreError::InvalidDatabase(_))
    ));
    let writer = Writer::open(home.path()).unwrap();
    set_slot(&writer, id, "phase", Some("hello")).await.unwrap();
    let c = rusqlite::Connection::open(home.path().join("sluice.db")).unwrap();
    let tables: i64 = c
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 23);
    let row: (String, String, String) = c
        .query_row(
            "SELECT project_id, key, markdown FROM board_slots",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(row, (id.to_string(), "phase".into(), "hello".into()));
}

#[tokio::test]
async fn a_home_from_before_retiring_gains_its_columns_when_its_writer_opens() {
    let home = ScratchHome::new().unwrap();
    {
        let writer = Writer::open(home.path()).unwrap();
        create(&writer, "old").await;
    }
    // Drop the retiring columns, as a release before them left the home.
    {
        let c = rusqlite::Connection::open(home.path().join("sluice.db")).unwrap();
        c.execute_batch(
            "ALTER TABLE projects DROP COLUMN prune_keep; ALTER TABLE projects DROP COLUMN prune_done_after;",
        )
        .unwrap();
    }
    assert!(matches!(
        ReadPool::open(home.path(), 1).map(|_| ()),
        Err(StoreError::InvalidDatabase(_))
    ));
    let _writer = Writer::open(home.path()).unwrap();
    let reads = ReadPool::open(home.path(), 1).unwrap();
    let p = reads
        .snapshot(|c| projects::resolve(c, &ProjectSelector::Name("old".parse().unwrap())))
        .await
        .unwrap();
    assert_eq!((p.prune_done_after, p.prune_keep), (None, vec![]));
}
