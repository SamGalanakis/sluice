use crate::home;
use home::ScratchHome;
use sluice_model::ids::{InvocationId, ProjectId};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    artifacts::{self, Bundle, BundleScope},
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use std::{collections::BTreeMap, fs, os::unix::fs::symlink};

async fn setup() -> (ScratchHome, Writer, ReadPool, ProjectId) {
    let home = ScratchHome::new().unwrap();
    assert_eq!(ScratchHome::validate(home.path()).unwrap(), home.path());
    let writer = Writer::open(home.path()).unwrap();
    let reads = ReadPool::open(home.path(), 2).unwrap();
    let id = writer
        .write(RetrySafety::NonIdempotent, |tx| {
            Ok(projects::project_create(
                tx,
                CreateProject {
                    name: "p".parse().unwrap(),
                    description: "".into(),
                    icon: None,
                    resources: None,
                    limits: None,
                    author: "test".into(),
                },
                &EmptyPlanInitializer,
                &NoResourceSettings,
            )?
            .project_id)
        })
        .await
        .unwrap();
    (home, writer, reads, id)
}
fn bundle(value: &str) -> Bundle {
    Bundle::new(BTreeMap::from([
        ("fn.json".into(), b"{}".to_vec()),
        ("main.py".into(), value.as_bytes().to_vec()),
        ("helpers/sibling.py".into(), b"frozen helper".to_vec()),
    ]))
    .unwrap()
}
async fn stage(writer: &Writer, scope: BundleScope, value: &str) -> InvocationId {
    let b = bundle(value);
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            artifacts::stage_generation(tx, scope, b)
        })
        .await
        .unwrap()
}
async fn state(reads: &ReadPool, id: InvocationId) -> artifacts::ArtifactJob {
    reads
        .snapshot(move |c| artifacts::job(c, id))
        .await
        .unwrap()
}

async fn raw_cleanup(writer: &Writer, project: ProjectId, path: String) -> InvocationId {
    let id = InvocationId::new();
    writer.write(RetrySafety::NonIdempotent,move|tx|{tx.sql().execute("INSERT INTO artifact_jobs(job_id,project_id,kind,generation,path,state,created_at) VALUES (?1,?2,'cleanup',(SELECT coalesce(max(generation),0)+1 FROM artifact_jobs WHERE path=?3 AND kind='cleanup'),?3,'pending','now')",[id.to_string(),project.to_string(),path])?;tx.changed(Some(project),"artifacts");Ok(())}).await.unwrap();
    id
}

#[tokio::test]
async fn worker_refuses_a_different_home_even_with_valid_job_paths() {
    let (home, writer, reads, project) = setup().await;
    let (other, other_writer, _other_reads, _) = setup().await;
    let id = stage(&writer, BundleScope::Project(project), "private").await;
    assert!(artifacts::execute(&writer, other.path(), id).await.is_err());
    assert_eq!(state(&reads, id).await.state, "pending");
    assert!(!other.path().join(state(&reads, id).await.path).exists());
    let identity: String = reads
        .snapshot(|c| Ok(c.query_row("SELECT home_id FROM home_meta", [], |r| r.get(0))?))
        .await
        .unwrap();
    other_writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql()
                .execute("UPDATE home_meta SET home_id=?1", [identity])?;
            tx.changed(None, "home");
            Ok(())
        })
        .await
        .unwrap();
    // A restored copy may share logical HomeId; it still cannot redirect a worker.
    assert!(artifacts::execute(&writer, other.path(), id).await.is_err());
    artifacts::execute(&writer, home.path(), id).await.unwrap();
    writer.shutdown().await.unwrap();
    other_writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn generation_publication_and_crash_recovery_keep_frozen_files() {
    let (home, writer, reads, project) = setup().await;
    let scope = BundleScope::Project(project);
    let id = stage(&writer, scope, "first").await;
    assert!(
        reads
            .snapshot(move |c| artifacts::published_generation(c, scope))
            .await
            .unwrap()
            .is_none()
    );
    let pending = state(&reads, id).await;
    assert_eq!(pending.state, "pending");
    artifacts::execute(&writer, home.path(), id).await.unwrap();
    let published = reads
        .snapshot(move |c| artifacts::published_generation(c, scope))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(published.job_id, id);
    assert_eq!(published.state, "done");
    assert_eq!(
        fs::read(home.path().join(&published.path).join("helpers/sibling.py")).unwrap(),
        b"frozen helper"
    );
    assert_eq!(
        fs::read(home.path().join(&published.path).join("main.py")).unwrap(),
        b"first"
    );
    assert!(published.manifest["fingerprint"].as_str().unwrap().len() == 64);
    let next = stage(&writer, scope, "second").await;
    assert_eq!(
        reads
            .snapshot(move |c| artifacts::published_generation(c, scope))
            .await
            .unwrap()
            .unwrap()
            .job_id,
        id
    );
    artifacts::execute(&writer, home.path(), next)
        .await
        .unwrap();
    assert_eq!(
        fs::read(home.path().join(published.path).join("main.py")).unwrap(),
        b"first"
    );

    let partial = stage(&writer, BundleScope::Project(project), "complete").await;
    let pending = state(&reads, partial).await;
    let parent = home
        .path()
        .join(&pending.path)
        .parent()
        .unwrap()
        .to_path_buf();
    let staging = parent.join(format!(".stage-{partial}"));
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("main.py"), b"part").unwrap();
    writer
        .write(RetrySafety::Idempotent, move |tx| {
            tx.sql().execute(
                "UPDATE artifact_jobs SET state='running' WHERE job_id=?1",
                [partial.to_string()],
            )?;
            tx.changed(Some(project), "artifacts");
            Ok(())
        })
        .await
        .unwrap();

    let partial_path = pending.path.clone();
    let renamed = stage(&writer, BundleScope::Project(project), "complete").await;
    let pending = state(&reads, renamed).await;
    let final_dir = home.path().join(&pending.path);
    fs::create_dir_all(final_dir.join("helpers")).unwrap();
    for (name, bytes) in bundle("complete").files() {
        fs::write(final_dir.join(name), bytes).unwrap();
    }
    writer
        .write(RetrySafety::Idempotent, move |tx| {
            tx.sql().execute(
                "UPDATE artifact_jobs SET state='running' WHERE job_id=?1",
                [renamed.to_string()],
            )?;
            tx.changed(Some(project), "artifacts");
            Ok(())
        })
        .await
        .unwrap();

    writer.shutdown().await.unwrap();
    let writer = Writer::open(home.path()).unwrap();
    artifacts::recover(&writer, home.path()).await.unwrap();
    assert_eq!(state(&reads, partial).await.state, "done");
    assert_eq!(
        fs::read(home.path().join(partial_path).join("main.py")).unwrap(),
        b"complete"
    );
    assert!(!staging.exists());
    assert_eq!(state(&reads, renamed).await.state, "done");
    assert_eq!(fs::read(final_dir.join("main.py")).unwrap(), b"complete");
    let bad = stage(&writer, BundleScope::Project(project), "frozen").await;
    let dest = home.path().join(state(&reads, bad).await.path);
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("fn.json"), b"{}").unwrap();
    fs::write(dest.join("main.py"), b"tampered").unwrap();
    let good = stage(&writer, BundleScope::Home, "good").await;
    assert!(artifacts::recover(&writer, home.path()).await.is_err());
    assert_eq!(state(&reads, bad).await.state, "pending");
    assert_eq!(state(&reads, good).await.state, "done");
    assert_eq!(fs::read(dest.join("main.py")).unwrap(), b"tampered");
    fs::remove_dir_all(dest).unwrap();
    artifacts::recover(&writer, home.path()).await.unwrap();
    assert_eq!(state(&reads, bad).await.state, "done");

    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn artifact_cleanup_refuses_escape_and_recovers_interrupted_deletion() {
    let (home, writer, reads, project) = setup().await;
    {
        let exterior = home.root().join("outside");
        fs::create_dir(&exterior).unwrap();
        fs::write(exterior.join("keep"), b"untouched").unwrap();
        for path in [
            exterior.to_str().unwrap().to_string(),
            "../outside".into(),
            "projects/p".into(),
            format!("projects/{project}/../../outside"),
            format!("projects//{project}"),
            format!("projects/{project}/."),
        ] {
            assert!(artifacts::validate_cleanup_path(&path).is_err());
            let id = raw_cleanup(&writer, project, path).await;
            assert!(artifacts::execute(&writer, home.path(), id).await.is_err());
            assert_eq!(state(&reads, id).await.state, "pending");
            assert_eq!(fs::read(exterior.join("keep")).unwrap(), b"untouched");
        }
    }
    {
        let exterior = home.root().join("outside");
        let external_project = exterior.join(project.to_string());
        fs::create_dir(&external_project).unwrap();
        fs::write(external_project.join("keep"), b"untouched").unwrap();
        symlink(&exterior, home.path().join("projects")).unwrap();
        let cleanup = raw_cleanup(&writer, project, format!("projects/{project}")).await;
        assert!(
            artifacts::execute(&writer, home.path(), cleanup)
                .await
                .is_err()
        );
        let generation = stage(&writer, BundleScope::Project(project), "danger").await;
        assert!(
            artifacts::execute(&writer, home.path(), generation)
                .await
                .is_err()
        );
        assert_eq!(
            fs::read(external_project.join("keep")).unwrap(),
            b"untouched"
        );
        assert!(!external_project.join("generations").exists());

        fs::remove_file(home.path().join("projects")).unwrap();
    }
    {
        let exterior = home.root().join("outside");
        fs::create_dir(home.path().join("projects")).unwrap();
        symlink(&exterior, home.path().join(format!("projects/{project}"))).unwrap();
        let cleanup = raw_cleanup(&writer, project, format!("projects/{project}")).await;
        artifacts::execute(&writer, home.path(), cleanup)
            .await
            .unwrap();
        assert_eq!(state(&reads, cleanup).await.state, "done");
        assert_eq!(fs::read(exterior.join("keep")).unwrap(), b"untouched");
        assert!(!home.path().join(format!("projects/{project}")).exists());
    }
    {
        for path in [
            "",
            "../secret",
            "/absolute",
            "a/./b",
            "a//b",
            "a/../b",
            "nul\0file",
        ] {
            assert!(Bundle::new(BTreeMap::from([(path.into(), vec![0])])).is_err());
        }
        assert!(
            Bundle::new(BTreeMap::from([
                ("a".into(), vec![0]),
                ("a/b".into(), vec![0])
            ]))
            .is_err()
        );
    }
    {
        let id = raw_cleanup(&writer, project, format!("projects/{project}")).await;
        let trash = home.path().join(format!("trash/{id}"));
        fs::create_dir_all(&trash).unwrap();
        fs::write(trash.join("secret"), b"old").unwrap();
        artifacts::execute(&writer, home.path(), id).await.unwrap();
        assert!(!trash.exists());
        assert_eq!(state(&reads, id).await.state, "done");
    }
    writer.shutdown().await.unwrap();
}
