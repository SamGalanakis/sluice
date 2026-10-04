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
async fn creation_initializes_empty_plan_authored_edit_and_id_directory_job() {
    let (home, writer, reads) = setup().await;
    let p = create(&writer, "p").await;
    let id = p.project_id;
    assert_eq!(p.settings_rev, Revision(1));
    let (doc, rev, edits) = reads
        .snapshot(move |c| {
            Ok((
                c.query_row(
                    "SELECT doc FROM plans WHERE project_id=?1",
                    [id.to_string()],
                    |r| r.get::<_, String>(0),
                )?,
                c.query_row(
                    "SELECT rev FROM plans WHERE project_id=?1",
                    [id.to_string()],
                    |r| r.get::<_, i64>(0),
                )?,
                c.query_row(
                    "SELECT count(*) FROM plan_edits WHERE project_id=?1",
                    [id.to_string()],
                    |r| r.get::<_, i64>(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&doc).unwrap(),
        json!({"steps":{}})
    );
    assert_eq!((rev, edits), (1, 1));
    assert_eq!(records(&reads, Some(id)).await[0]["author"], "sam");
    assert!(!home.path().join(format!("projects/{id}")).exists());
    assert_eq!(artifacts::recover(&writer, home.path()).await.unwrap(), 1);
    assert!(home.path().join(format!("projects/{id}/fns")).is_dir());
    writer.shutdown().await.unwrap();
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

#[tokio::test]
async fn duplicate_names_and_stale_revisions_have_no_partial_effect() {
    let (_home, writer, reads) = setup().await;
    let p = create(&writer, "p").await;
    create(&writer, "q").await;
    let id = p.project_id;
    let before = snapshot(&reads, id).await;
    for request in [
        UpdateProject {
            new_name: Some("q".parse().unwrap()),
            description: Some("changed".into()),
            icon: Some(Icon::image(b"GIF89a".to_vec()).unwrap()),
            paused: Some(true),
            author: "owner".into(),
            ..Default::default()
        },
        UpdateProject {
            new_name: Some("r".parse().unwrap()),
            expected_settings_rev: Some(Revision(9)),
            description: Some("changed".into()),
            ..Default::default()
        },
    ] {
        assert!(matches!(
            update(&writer, id, request).await,
            Err(PublicError::Conflict { .. })
        ));
        assert_eq!(snapshot(&reads, id).await, before);
    }
    let duplicate = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            projects::project_create(
                tx,
                CreateProject {
                    name: "p".parse().unwrap(),
                    description: String::new(),
                    icon: None,
                    resources: None,
                    author: "owner".into(),
                },
                &EmptyPlanInitializer,
                &NoResourceSettings,
            )
        })
        .await;
    assert!(matches!(duplicate, Err(PublicError::Conflict { .. })));
    assert_eq!(snapshot(&reads, id).await, before);
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

#[tokio::test]
async fn changed_settings_have_authors_reasons_and_noops_do_not_increment_revision() {
    let (_home, writer, reads) = setup().await;
    let p = create(&writer, "p").await;
    let id = p.project_id;
    let request = UpdateProject {
        description: Some("new".into()),
        icon: Some(Icon::text(" 🌊 ").unwrap()),
        paused: Some(true),
        archived: Some(true),
        reason: Some("deploy".into()),
        author: "orch".into(),
        ..Default::default()
    };
    let updated = update(&writer, id, request.clone()).await.unwrap();
    assert_eq!(updated.settings_rev, Revision(2));
    let before = snapshot(&reads, id).await;
    assert_eq!(update(&writer, id, request).await.unwrap(), updated);
    assert_eq!(snapshot(&reads, id).await, before);
    let got = records(&reads, Some(id)).await;
    assert_eq!(
        &got[1..],
        &[
            json!({"kind":"project.pause","paused":true,"reason":"deploy","author":"orch"}),
            json!({"kind":"project.archive","archived":true,"reason":"deploy","author":"orch"}),
            json!({"kind":"project.update","fields":["description","icon"],"reason":"deploy","author":"orch"})
        ]
    );
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn image_import_replace_text_clear_and_generation_urls() {
    let (home, writer, reads) = setup().await;
    let p = create(&writer, "p").await;
    let id = p.project_id;
    let source = home.root().join("source.svg");
    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>";
    std::fs::write(&source, svg).unwrap();
    let icon = Icon::from_argument(source.to_str().unwrap(), home.root()).unwrap();
    std::fs::remove_file(source).unwrap();
    let p = update(
        &writer,
        id,
        UpdateProject {
            icon: Some(icon),
            author: "sam".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let url = p.icon.as_ref().unwrap().url(id).unwrap();
    assert!(url.starts_with(&format!("/projects/id/{id}/icon?generation=")));
    let (media, bytes, hash) = reads
        .snapshot(move |c| projects::icon_image(c, &selector(id), 1))
        .await
        .unwrap();
    assert_eq!(
        (media.as_str(), bytes.as_slice(), hash),
        ("image/svg+xml", svg.as_slice(), artifacts::fingerprint(svg))
    );
    artifacts::recover(&writer, home.path()).await.unwrap();
    assert_eq!(
        std::fs::read(home.path().join(format!("projects/{id}/icons/1/image"))).unwrap(),
        svg
    );
    let unchanged = update(
        &writer,
        id,
        UpdateProject {
            description: Some("other".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(unchanged.icon.unwrap().url(id).unwrap(), url);
    let png = b"\x89PNG\r\n\x1a\n";
    let p = update(
        &writer,
        id,
        UpdateProject {
            icon: Some(Icon::image(png.to_vec()).unwrap()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_ne!(p.icon.unwrap().url(id).unwrap(), url);
    assert!(
        reads
            .snapshot(move |c| projects::icon_image(c, &selector(id), 1))
            .await
            .is_err()
    );
    update(
        &writer,
        id,
        UpdateProject {
            icon: Some(Icon::text("🌊").unwrap()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        reads
            .snapshot(move |c| projects::icon_image(c, &selector(id), 3))
            .await
            .is_err()
    );
    let p = update(
        &writer,
        id,
        UpdateProject {
            icon: Some(Icon::text("").unwrap()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(p.icon, None);
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
async fn delete_requires_archive_exact_confirmation_and_current_revision() {
    let (_home, writer, reads) = setup().await;
    let p = create(&writer, "p").await;
    assert!(matches!(
        delete(&writer, &p).await,
        Err(PublicError::BadRequest { .. })
    ));
    let p = archive(&writer, p.project_id).await;
    let before = snapshot(&reads, p.project_id).await;
    for (name, revision) in [("wrong", p.settings_rev), ("p", Revision(1))] {
        let id = p.project_id;
        let request = DeleteProject {
            confirm_name: name.into(),
            expected_settings_rev: revision,
            author: "sam".into(),
        };
        assert!(
            writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    projects::project_delete(tx, &selector(id), request, &StoredWorkOnly)
                })
                .await
                .is_err()
        );
        assert_eq!(snapshot(&reads, id).await, before);
    }
    let old = p.clone();
    let renamed = update(
        &writer,
        p.project_id,
        UpdateProject {
            new_name: Some("renamed".parse().unwrap()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        delete(&writer, &old).await,
        Err(PublicError::Conflict { .. })
    ));
    delete(&writer, &renamed).await.unwrap();
    assert!(matches!(
        reads
            .snapshot(move |c| projects::resolve(c, &selector(renamed.project_id)))
            .await,
        Err(StoreError::Public(PublicError::NotFound { .. }))
    ));
    writer.shutdown().await.unwrap();
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
async fn coordinator_live_guard_can_refuse_deletion() {
    struct LiveGuardian;
    impl projects::DeletionGuard for LiveGuardian {
        fn check(&self, _: ProjectId) -> sluice_store::Result<()> {
            Err(PublicError::BadRequest {
                message: "guardian still live".into(),
            }
            .into())
        }
    }
    let (_home, writer, reads) = setup().await;
    let p = create(&writer, "p").await;
    let p = archive(&writer, p.project_id).await;
    let id = p.project_id;
    let request = DeleteProject {
        confirm_name: "p".into(),
        expected_settings_rev: p.settings_rev,
        author: "sam".into(),
    };
    assert!(
        writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                projects::project_delete(tx, &selector(id), request, &LiveGuardian)
            })
            .await
            .is_err()
    );
    reads
        .snapshot(move |c| projects::resolve(c, &selector(id)))
        .await
        .unwrap();
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
async fn a_board_is_set_cleared_fenced_by_its_rev_and_recorded_without_its_program() {
    let (_home, writer, reads) = setup().await;
    let p = create(&writer, "board").await;
    let id = p.project_id;
    assert_eq!((p.board.as_deref(), p.board_rev), (None, Revision(0)));
    let program = "root = Stack([units, failed])\nunits = Units([\"running\"])\nfailed = Metric(\"Failed\", \"SELECT count(*) FROM steps WHERE project_id = ? AND status = 'failed'\")";
    assert_eq!(
        set_board(&writer, id, Some(program), Some(0))
            .await
            .unwrap(),
        Revision(1)
    );
    let (p, records, _) = snapshot(&reads, id).await;
    assert_eq!(
        (p.board.as_deref(), p.board_rev),
        (Some(program), Revision(1))
    );
    // The same program again changes nothing.
    assert_eq!(
        set_board(&writer, id, Some(program), None).await.unwrap(),
        Revision(1)
    );
    // A stale rev is a conflict naming the current one, and nothing changes.
    let stale = set_board(&writer, id, Some("root = Text(\"x\")"), Some(0)).await;
    assert!(
        matches!(
            stale,
            Err(PublicError::Conflict {
                current_rev: Some(Revision(1)),
                ..
            })
        ),
        "{stale:?}"
    );
    // A program that does not check is invalid, every bad line listed, and nothing changes.
    let bad = set_board(
        &writer,
        id,
        Some("root = Stack([a])\nnot a statement\na = Chart(\"pie\", \"SELECT 1\")"),
        None,
    )
    .await;
    let Err(PublicError::Invalid { errors, .. }) = bad else {
        panic!("{bad:?}")
    };
    assert!(
        errors.iter().any(|e| e.starts_with("line 2: ")),
        "{errors:?}"
    );
    assert!(
        errors.iter().any(|e| e.starts_with("line 3: Chart: kind")),
        "{errors:?}"
    );
    // Clearing.
    assert_eq!(
        set_board(&writer, id, None, Some(1)).await.unwrap(),
        Revision(2)
    );
    let (cleared, after, _) = snapshot(&reads, id).await;
    assert_eq!((cleared.board, cleared.board_rev), (None, Revision(2)));
    let boards: Vec<&Value> = after
        .iter()
        .filter(|r| r["kind"] == "project.board")
        .collect();
    assert_eq!(
        boards,
        [
            &json!({"kind":"project.board","rev":1,"cleared":false,"reason":"lane overview","author":"orch"}),
            &json!({"kind":"project.board","rev":2,"cleared":true,"reason":"lane overview","author":"orch"}),
        ]
    );
    assert_eq!(
        records
            .iter()
            .filter(|r| r["kind"] == "project.board")
            .count(),
        1
    );
    // The settings revision is the settings', not the board's.
    assert_eq!(cleared.settings_rev, p.settings_rev);
    let listed = reads.snapshot(projects::list).await.unwrap();
    assert_eq!(listed[0].board_rev, Revision(2));
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
