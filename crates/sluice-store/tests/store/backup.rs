use crate::home;
use home::ScratchHome;
use rusqlite::Connection;
use sluice_model::{error::PublicError, ids::ProjectId};
use sluice_store::{
    RetrySafety, StoreError, Writer,
    backup::{self, AdmissionPaused},
};
use std::{fs, path::Path, sync::Arc};

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

#[cfg(unix)]
#[tokio::test]
async fn backup_and_restore_preserve_ownership_and_refuse_unsafe_destinations() {
    let (home, writer, _) = setup().await;
    {
        let dst = home.root().join("case0-export");
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
    }
    {
        let src = home.root().join("case1-backup.db");
        backup::backup(home.path(), &src).unwrap();
        let dst = home.root().join("case1-restored");
        fs::create_dir(&dst).unwrap();
        fs::write(dst.join("hidden"), b"keep").unwrap();
        assert!(backup::restore_into_fresh_home(&src, &dst).is_err());
        assert_eq!(fs::read(dst.join("hidden")).unwrap(), b"keep");
        assert!(!dst.join("sluice.db").exists());
        assert!(backup::restore_into_fresh_home(&src, home.path()).is_err());
    }
    {
        let missing = home.root().join("case2-missing.db");
        let dst = home.root().join("case2-restored");
        assert!(backup::restore_into_fresh_home(&missing, &dst).is_err());
        assert!(!missing.exists());
        assert!(!dst.exists());
        let invalid = home.root().join("case2-invalid.db");
        fs::write(&invalid, b"not SQLite").unwrap();
        assert!(backup::restore_into_fresh_home(&invalid, &dst).is_err());
        assert!(!dst.exists());
        let wrong = home.root().join("case2-foreign.db");
        let c = Connection::open(&wrong).unwrap();
        c.execute_batch(
            "CREATE TABLE home_meta(home_id TEXT); INSERT INTO home_meta VALUES ('foreign')",
        )
        .unwrap();
        drop(c);
        assert!(backup::restore_into_fresh_home(&wrong, &dst).is_err());
        assert!(!dst.exists());
    }
    {
        let src = home.root().join("case3-backup.db");
        backup::backup(home.path(), &src).unwrap();
        let c = Connection::open(&src).unwrap();
        c.pragma_update(None, "user_version", 99).unwrap();
        drop(c);
        let dst = home.root().join("case3-restored");
        assert!(matches!(
            backup::restore_into_fresh_home(&src, &dst),
            Err(StoreError::UnsupportedSchema { found: 99 })
        ));
        assert!(!dst.exists());
        fs::create_dir(&dst).unwrap();
        assert!(backup::restore_into_fresh_home(&src, &dst).is_err());
        assert_eq!(fs::read_dir(&dst).unwrap().count(), 0);
    }
    {
        let paused = pause(&writer, "operator", 1).await;
        let nested = home.path().join("nested");
        assert!(backup::export_home(home.path(), &nested, &paused).is_err());
        assert!(backup::restore_into_fresh_home(home.path(), &nested).is_err());
        assert!(!nested.exists());
        fs::write(home.path().join(".sluice-copy-incomplete"), b"interrupted").unwrap();
        let dst = home.root().join("case4-export");
        assert!(backup::export_home(home.path(), &dst, &paused).is_err());
        assert!(!dst.exists());
        let dst = home.root().join("case4-restored");
        assert!(backup::restore_into_fresh_home(home.path(), &dst).is_err());
        assert!(!dst.exists());

        fs::remove_file(home.path().join(".sluice-copy-incomplete")).unwrap();
    }
    {
        use std::os::unix::fs::symlink;
        let paused = pause(&writer, "operator", 1).await;
        let outside = home.root().join("case5-outside");
        fs::write(&outside, b"keep").unwrap();
        symlink(&outside, home.path().join("escape")).unwrap();
        let dst = home.root().join("case5-export");
        assert!(backup::export_home(home.path(), &dst, &paused).is_err());
        assert!(!dst.exists());
        let db = home.root().join("case5-backup.db");
        backup::backup(home.path(), &db).unwrap();
        let link = home.root().join("case5-db-link");
        symlink(&db, &link).unwrap();
        assert!(backup::restore_into_fresh_home(&link, &dst).is_err());
        let destination_link = home.root().join("case5-dst-link");
        symlink(&outside, &destination_link).unwrap();
        assert!(backup::backup(home.path(), &destination_link).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"keep");

        fs::remove_file(home.path().join("escape")).unwrap();
    }
    {
        let src = home.root().join("case6-backup.db");
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
    }
    writer.shutdown().await.unwrap();
}
