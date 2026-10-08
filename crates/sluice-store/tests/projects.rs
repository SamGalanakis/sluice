#[allow(dead_code)]
#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use serde_json::{Value, json};
use sluice_model::{
    error::PublicError,
    ids::{ProjectId, ProjectSelector, Revision},
};
use sluice_store::{
    ReadPool, RetrySafety, StoreError, Writer,
    projects::{
        self, CreateProject, EmptyPlanInitializer, NoResourceSettings, Project, ResourceSettings,
        UpdateProject,
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
        .map(|outcome| outcome.rev)
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

async fn write_doc(
    writer: &Writer,
    id: ProjectId,
    markdown: &str,
) -> Result<projects::DocChange, PublicError> {
    let request = projects::WriteBoardDoc {
        markdown: markdown.into(),
        expected_rev: None,
        reason: None,
        author: "orch".into(),
    };
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::board_doc_write(tx, &selector(id), request)
        })
        .await
}

/// The board's document is columns added to an existing home, never a table: every pinned
/// release counts the home's tables and refuses a 24th. The retired slots' column and view
/// stay for the releases that still read them.
#[tokio::test]
async fn a_home_from_before_the_document_gains_its_columns_when_its_writer_opens() {
    let home = ScratchHome::new().unwrap();
    let id = {
        let writer = Writer::open(home.path()).unwrap();
        let id = create(&writer, "old").await.project_id;
        writer.shutdown().await.unwrap();
        id
    };
    {
        let c = rusqlite::Connection::open(home.path().join("sluice.db")).unwrap();
        c.execute_batch(
            "ALTER TABLE projects DROP COLUMN board_doc; ALTER TABLE projects DROP COLUMN board_doc_rev;
             ALTER TABLE projects DROP COLUMN board_doc_at; ALTER TABLE projects DROP COLUMN board_doc_author;",
        )
        .unwrap();
    }
    assert!(matches!(
        ReadPool::open(home.path(), 1).map(|_| ()),
        Err(StoreError::InvalidDatabase(_))
    ));
    let writer = Writer::open(home.path()).unwrap();
    set_board(&writer, id, Some("root = Doc()"), Some(0))
        .await
        .unwrap();
    let change = write_doc(&writer, id, "## Phase\nGreen.").await.unwrap();
    assert_eq!((change.rev, change.changed), (Revision(1), true));
    let c = rusqlite::Connection::open(home.path().join("sluice.db")).unwrap();
    let tables: i64 = c
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 23);
    let row: (String, i64, String) = c
        .query_row(
            "SELECT board_doc, board_doc_rev, board_doc_author FROM projects",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(row, ("## Phase\nGreen.".into(), 1, "orch".into()));
    let slots: i64 = c
        .query_row("SELECT count(*) FROM board_slots", [], |r| r.get(0))
        .unwrap();
    assert_eq!(slots, 0, "the retired view still reads");
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
