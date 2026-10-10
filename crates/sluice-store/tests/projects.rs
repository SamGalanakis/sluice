#[allow(dead_code)]
#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use serde_json::{Value, json};
use sluice_model::{
    error::PublicError,
    ids::{ProjectId, ProjectSelector},
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
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
