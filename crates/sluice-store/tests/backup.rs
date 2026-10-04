#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use rusqlite::Connection;
use sluice_model::{error::PublicError, ids::ProjectId};
use sluice_store::{
    RetrySafety, StoreError, Writer,
    backup::{self, AdmissionPaused},
};
use std::{
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

async fn setup() -> (ScratchHome, Writer, ProjectId) {
    let home = ScratchHome::new().unwrap();
    assert_eq!(ScratchHome::validate(home.path()).unwrap(), home.path());
    let writer = Writer::open(home.path()).unwrap();
    let project = ProjectId::new();
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "INSERT INTO projects(project_id,name,created_at) VALUES (?1,'p','now')",
                [project.to_string()],
            )?;
            tx.changed(Some(project), "projects");
            Ok(())
        })
        .await
        .unwrap();
    (home, writer, project)
}
fn scalar(path: &Path, sql: &str) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}
async fn pause(writer: &Writer, owner: &str, revision: i64) -> AdmissionPaused {
    let owner = owner.to_owned();
    let stored = owner.clone();
    writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "UPDATE maintenance SET mode='drain',owner=?1,revision=?2 WHERE singleton=1",
                (&stored, revision),
            )?;
            tx.changed(None, "maintenance");
            Ok(())
        })
        .await
        .unwrap();
    AdmissionPaused { owner, revision }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn online_backup_during_commits_is_coherent_and_restores_an_isolated_home() {
    let (home, writer, project) = setup().await;
    writer.write(RetrySafety::NonIdempotent,move |tx| {
        let payload = serde_json::json!({"kind":"message","body":"x".repeat(2000)}).to_string();
        for _ in 0..1000 {
            tx.sql().execute("INSERT INTO records(project_id,at,kind,payload,thread) VALUES (?1,'now','message',?2,'seed')",(project.to_string(),&payload))?;
        }
        tx.changed(Some(project),"log"); Ok(())
    }).await.unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let committed = Arc::new(AtomicU64::new(0));
    let background = writer.clone();
    let stop_writer = stop.clone();
    let count_writer = committed.clone();
    let task = tokio::spawn(async move {
        while !stop_writer.load(Ordering::Relaxed) {
            background.write(RetrySafety::NonIdempotent,move |tx| {
                for body in ["one","two"] {
                    let payload = serde_json::json!({"kind":"message","body":body}).to_string();
                    tx.sql().execute("INSERT INTO records(project_id,at,kind,payload,thread) VALUES (?1,'now','message',?2,'pair')",(project.to_string(),payload))?;
                }
                tx.changed(Some(project),"log"); Ok(())
            }).await.unwrap();
            count_writer.fetch_add(1, Ordering::Relaxed);
        }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while committed.load(Ordering::Relaxed) < 5 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let before = committed.load(Ordering::Relaxed);
    for n in 0..3 {
        let source = home.path().to_owned();
        let dst = home.root().join(format!("backup{n}.db"));
        let target = dst.clone();
        let info = tokio::task::spawn_blocking(move || backup::backup(&source, &target))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(info.path, dst);
        assert_eq!(info.bytes, fs::metadata(&dst).unwrap().len());
        let c = Connection::open(&dst).unwrap();
        assert_eq!(
            c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM records WHERE thread='seed'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1000
        );
        let pairs = c
            .query_row(
                "SELECT count(*) FROM records WHERE thread='pair'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap();
        assert!(pairs > 0 && pairs % 2 == 0);
        assert_eq!(
            c.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            sluice_store::schema::SCHEMA_VERSION
        );
        assert_eq!(
            c.query_row("SELECT name FROM projects", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "p"
        );
        drop(c);
        let restored = ScratchHome::new().unwrap();
        backup::restore_into_fresh_home(&dst, restored.path()).unwrap();
        let restored_db = restored.path().join("sluice.db");
        assert_eq!(
            scalar(
                &restored_db,
                "SELECT count(*) FROM records WHERE thread='pair'"
            ),
            pairs
        );
        let restored_writer = Writer::open(restored.path()).unwrap();
        restored_writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql()
                    .execute("UPDATE projects SET description='restored only'", [])?;
                tx.changed(Some(project), "projects");
                Ok(())
            })
            .await
            .unwrap();
        restored_writer.shutdown().await.unwrap();
        assert_eq!(
            scalar(
                &home.path().join("sluice.db"),
                "SELECT count(*) FROM projects WHERE description=''"
            ),
            1
        );
        assert!(!home.root().join(format!("backup{n}.db-wal")).exists());
        assert!(!home.root().join(format!("backup{n}.db-shm")).exists());
    }
    stop.store(true, Ordering::Relaxed);
    task.await.unwrap();
    assert!(
        committed.load(Ordering::Relaxed) > before,
        "writer continued committing through online copies"
    );
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn backup_preserves_existing_destinations_and_source_and_sidecars() {
    let (home, writer, _) = setup().await;
    let dst = home.root().join("keep.db");
    fs::write(&dst, b"keep me").unwrap();
    for target in [&dst, home.path(), &home.path().join("sluice.db")] {
        assert!(matches!(
            backup::backup(home.path(), target),
            Err(StoreError::Public(PublicError::BadRequest { .. }))
        ));
    }
    assert_eq!(fs::read(&dst).unwrap(), b"keep me");
    assert_eq!(
        scalar(
            &home.path().join("sluice.db"),
            "SELECT count(*) FROM projects"
        ),
        1
    );
    let reserved = home.root().join("sidecar.db");
    fs::write(
        home.root().join("sidecar.db-wal"),
        b"another owner's sidecar",
    )
    .unwrap();
    assert!(backup::backup(home.path(), &reserved).is_err());
    assert!(!reserved.exists());
    assert_eq!(
        fs::read(home.root().join("sidecar.db-wal")).unwrap(),
        b"another owner's sidecar"
    );
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn export_requires_active_matching_maintenance_ownership() {
    let (home, writer, _) = setup().await;
    let dst = home.root().join("export");
    let mut claim = AdmissionPaused {
        owner: "operator".into(),
        revision: 0,
    };
    assert!(matches!(
        backup::export_home(home.path(), &dst, &claim),
        Err(StoreError::Public(PublicError::Busy { .. }))
    ));
    assert!(!dst.exists());
    claim = pause(&writer, "operator", 7).await;
    for bad in [
        AdmissionPaused {
            owner: "other".into(),
            revision: 7,
        },
        AdmissionPaused {
            owner: "operator".into(),
            revision: 6,
        },
        AdmissionPaused {
            owner: String::new(),
            revision: 7,
        },
    ] {
        assert!(matches!(
            backup::export_home(home.path(), &dst, &bad),
            Err(StoreError::Public(PublicError::Busy { .. }))
        ));
        assert!(!dst.exists());
    }
    backup::export_home(home.path(), &dst, &claim).unwrap();
    assert_eq!(
        scalar(&dst.join("sluice.db"), "SELECT revision FROM maintenance"),
        7
    );
    assert!(!dst.join(".sluice-copy-incomplete").exists());
    assert!(backup::export_home(home.path(), &dst, &claim).is_err());
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn full_export_and_restore_preserve_generations_artifacts_permissions_and_identity() {
    let (home, writer, project) = setup().await;
    let paused = pause(&writer, "export-owner", 1).await;
    let files = [
        "home.json".to_owned(),
        "fns/add/generations/1/main.py".into(),
        "fns/add/generations/1/helper.py".into(),
        "fns/add/generations/2/main.py".into(),
        format!("projects/{project}/fns/pinned/1/main.py"),
        format!("projects/{project}/.env"),
        "recipes/build.json".into(),
        "runs/run1/fn/main.py".into(),
        "runs/run1/stderr.txt".into(),
        "releases/release1/runtime".into(),
        "engine-homes/session1/checkpoint".into(),
    ];
    for path in &files {
        let path = home.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!(
                "fixture for {}",
                path.file_name().unwrap().to_str().unwrap()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    fs::create_dir(home.path().join("locks")).unwrap();
    fs::write(home.path().join("locks/transient"), b"skip").unwrap();
    #[cfg(unix)]
    let _endpoint =
        std::os::unix::net::UnixListener::bind(home.path().join("runs/run1/socket")).unwrap();
    let exported = home.root().join("export");
    backup::export_home(home.path(), &exported, &paused).unwrap();
    assert!(!exported.join("coordinator.lock").exists());
    assert!(!exported.join("locks").exists());
    assert!(!exported.join("runs/run1/socket").exists());
    let restored = home.root().join("restored");
    backup::restore_into_fresh_home(&exported, &restored).unwrap();
    for path in files {
        assert_eq!(
            fs::read(restored.join(&path)).unwrap(),
            fs::read(home.path().join(&path)).unwrap(),
            "{path}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(restored.join(&path))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    let source = Connection::open(home.path().join("sluice.db")).unwrap();
    let target = Connection::open(restored.join("sluice.db")).unwrap();
    assert_eq!(
        source
            .query_row("SELECT home_id FROM home_meta", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        target
            .query_row("SELECT home_id FROM home_meta", [], |r| r
                .get::<_, String>(0))
            .unwrap()
    );
    assert_eq!(
        target
            .query_row("SELECT owner FROM maintenance", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "export-owner"
    );
    drop(target);
    drop(source);
    let restored_writer = Writer::open(&restored).unwrap();
    restored_writer.shutdown().await.unwrap();
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn restore_refuses_any_nonempty_destination_without_touching_it() {
    let (home, writer, _) = setup().await;
    let src = home.root().join("backup.db");
    backup::backup(home.path(), &src).unwrap();
    let dst = home.root().join("restored");
    fs::create_dir(&dst).unwrap();
    fs::write(dst.join("hidden"), b"keep").unwrap();
    assert!(backup::restore_into_fresh_home(&src, &dst).is_err());
    assert_eq!(fs::read(dst.join("hidden")).unwrap(), b"keep");
    assert!(!dst.join("sluice.db").exists());
    assert!(backup::restore_into_fresh_home(&src, home.path()).is_err());
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_restore_leaves_a_fresh_destination_and_does_not_create_a_missing_source() {
    let (home, writer, _) = setup().await;
    let missing = home.root().join("missing.db");
    let dst = home.root().join("restored");
    assert!(backup::restore_into_fresh_home(&missing, &dst).is_err());
    assert!(!missing.exists());
    assert!(!dst.exists());
    let invalid = home.root().join("invalid.db");
    fs::write(&invalid, b"not SQLite").unwrap();
    assert!(backup::restore_into_fresh_home(&invalid, &dst).is_err());
    assert!(!dst.exists());
    let wrong = home.root().join("foreign.db");
    let c = Connection::open(&wrong).unwrap();
    c.execute_batch(
        "CREATE TABLE home_meta(home_id TEXT); INSERT INTO home_meta VALUES ('foreign')",
    )
    .unwrap();
    drop(c);
    assert!(backup::restore_into_fresh_home(&wrong, &dst).is_err());
    assert!(!dst.exists());
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn foreign_schema_is_rejected_after_copy_and_partial_destination_is_cleaned() {
    let (home, writer, _) = setup().await;
    let src = home.root().join("backup.db");
    backup::backup(home.path(), &src).unwrap();
    let c = Connection::open(&src).unwrap();
    c.pragma_update(None, "user_version", 99).unwrap();
    drop(c);
    let dst = home.root().join("restored");
    assert!(matches!(
        backup::restore_into_fresh_home(&src, &dst),
        Err(StoreError::UnsupportedSchema { found: 99 })
    ));
    assert!(!dst.exists());
    fs::create_dir(&dst).unwrap();
    assert!(backup::restore_into_fresh_home(&src, &dst).is_err());
    assert_eq!(fs::read_dir(&dst).unwrap().count(), 0);
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn exports_and_restores_refuse_overlapping_trees_and_incomplete_sources() {
    let (home, writer, _) = setup().await;
    let paused = pause(&writer, "operator", 1).await;
    let nested = home.path().join("nested");
    assert!(backup::export_home(home.path(), &nested, &paused).is_err());
    assert!(backup::restore_into_fresh_home(home.path(), &nested).is_err());
    assert!(!nested.exists());
    fs::write(home.path().join(".sluice-copy-incomplete"), b"interrupted").unwrap();
    let dst = home.root().join("export");
    assert!(backup::export_home(home.path(), &dst, &paused).is_err());
    assert!(!dst.exists());
    let dst = home.root().join("restored");
    assert!(backup::restore_into_fresh_home(home.path(), &dst).is_err());
    assert!(!dst.exists());
    writer.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn symlinks_cannot_escape_an_export_or_overwrite_a_restore_target() {
    use std::os::unix::fs::symlink;
    let (home, writer, _) = setup().await;
    let paused = pause(&writer, "operator", 1).await;
    let outside = home.root().join("outside");
    fs::write(&outside, b"keep").unwrap();
    symlink(&outside, home.path().join("escape")).unwrap();
    let dst = home.root().join("export");
    assert!(backup::export_home(home.path(), &dst, &paused).is_err());
    assert!(!dst.exists());
    let db = home.root().join("backup.db");
    backup::backup(home.path(), &db).unwrap();
    let link = home.root().join("db-link");
    symlink(&db, &link).unwrap();
    assert!(backup::restore_into_fresh_home(&link, &dst).is_err());
    let destination_link = home.root().join("dst-link");
    symlink(&outside, &destination_link).unwrap();
    assert!(backup::backup(home.path(), &destination_link).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"keep");
    writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_restores_reserve_one_destination_without_removing_the_winner() {
    let (home, writer, _) = setup().await;
    let src = home.root().join("backup.db");
    backup::backup(home.path(), &src).unwrap();
    for existing in [false, true] {
        let dst = home.root().join(format!("shared-{existing}"));
        if existing {
            fs::create_dir(&dst).unwrap();
        }
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let barrier = barrier.clone();
                let src = src.clone();
                let dst = dst.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    backup::restore_into_fresh_home(&src, &dst)
                })
            })
            .collect();
        let successes = tasks
            .into_iter()
            .map(|task| task.join().unwrap())
            .filter(Result::is_ok)
            .count();
        assert_eq!(successes, 1);
        assert_eq!(
            scalar(&dst.join("sluice.db"), "SELECT count(*) FROM projects"),
            1
        );
        assert!(!dst.join(".sluice-copy-incomplete").exists());
    }
    writer.shutdown().await.unwrap();
}
