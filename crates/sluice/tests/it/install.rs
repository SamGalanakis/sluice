use crate::install_support as support;
use serde_json::{Value, json};
use sluice::install::Installation;
use sluice_model::{commands::CommandReply, error::PublicError};
use std::{path::Path, time::Duration};

fn fake_release(root: &Path, name: &str) -> std::path::PathBuf {
    let release = root.join(name);
    std::fs::create_dir_all(release.join("bin")).unwrap();
    std::fs::write(release.join("bin/sluice"), b"immutable fixture").unwrap();
    release
}
fn call() -> Value {
    json!({"command":"fn_call","args":{"name":"fixture.echo","inputs":{"value":7},"project":null,"direct":true,"wait_seconds":0,"author":"install-test"}})
}

#[test]
fn fenced_socket_clients_dead_broker_and_maintenance_edits() {
    let mut gate = support::Gate::new();
    gate.boot(Path::new(env!("CARGO_BIN_EXE_sluice")), false);
    let mut old =
        std::os::unix::net::UnixStream::connect(gate.home.join("coordinator.sock")).unwrap();
    let pid = gate.children[0].id().to_string();
    assert!(
        std::process::Command::new("/bin/kill")
            .args(["-STOP", &pid])
            .status()
            .unwrap()
            .success()
    );
    let clients: Vec<_> = (0..8)
        .map(|_| {
            let mut stream =
                std::os::unix::net::UnixStream::connect(gate.home.join("coordinator.sock"))
                    .unwrap();
            std::thread::spawn(move || support::send(&mut stream, call()))
        })
        .collect();
    let fence = gate.cli(&["install", "fence", "paused socket race"]);
    assert!(fence.status.success(), "{:?}", fence);
    assert!(
        std::process::Command::new("/bin/kill")
            .args(["-CONT", &pid])
            .status()
            .unwrap()
            .success()
    );
    for client in clients {
        assert!(matches!(
            client.join().unwrap(),
            Err(PublicError::Busy { .. })
        ));
    }
    assert!(matches!(
        support::send(&mut old, call()),
        Err(PublicError::Busy { .. })
    ));
    for _ in 0..8 {
        assert!(matches!(gate.rpc(call()), Err(PublicError::Busy { .. })));
    }
    gate.stop();
    for command in [
        vec!["coordinator"],
        vec![
            "tool",
            "fn_call",
            r#"{"name":"fixture.echo","inputs":{"value":7},"direct":true}"#,
        ],
    ] {
        let output = gate.cli(&command);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("maintenance"));
    }
    let callback = gate.cli(&[
        "tool",
        "submission",
        r#"{"run":"019a2b3c-4d5e-7f01-8234-56789abcdef0"}"#,
    ]);
    assert!(!callback.status.success());
    assert!(String::from_utf8_lossy(&callback.stderr).contains("maintenance"));
    assert!(!gate.home.join("runs").exists());

    {
        assert!(
            gate.cli(&["install", "fence", "maintenance"])
                .status
                .success()
        );
        gate.boot(Path::new(env!("CARGO_BIN_EXE_sluice")), true);
        let CommandReply::Project(project) = gate.rpc(json!({"command":"project_create","args":{"name":"p","description":"","icon":null,"resources":{},"author":"test"}})).unwrap() else { panic!("project") };
        let selector = json!({"kind":"id","value":project.project_id});
        gate.rpc(json!({"command":"plan_edit","args":{"project":selector,"rev":1,"ops":[{"op":"step.add","step":"work","spec":{"run":"fixture.echo","in":{"value":{"default":1}}}}],"start":true,"dry_run":false,"author":"test","reason":"maintenance edit"}})).unwrap();
        gate.rpc(json!({"command":"step_set_output","args":{"project":selector,"step":"work","outputs":{"value":1},"force":true,"reason":"fixture terminal result","author":"test"}})).unwrap();
        gate.rpc(json!({"command":"step_retry","args":{"project":selector,"selection":{"steps":["work"],"tags":null},"message":"maintenance feedback","author":"@test","reason":"maintenance"}})).unwrap();
        assert!(gate.rpc(json!({"command":"status","args":{"project":selector,"selection":{"steps":null,"tags":null}}})).is_ok());
        assert!(matches!(gate.rpc(call()), Err(PublicError::Busy { .. })));
        // A manifest marker makes this a Rust selection for the ordered release API.
        let release = fake_release(gate.root.path(), "release");
        std::fs::write(release.join("manifest.json"), b"{}").unwrap();
        Installation::at(gate.install.clone())
            .unwrap()
            .select(&release, &gate.home)
            .unwrap();
        let status = Installation::at(gate.install.clone())
            .unwrap()
            .unfence_checked(|selected| {
                assert_eq!(selected, std::fs::canonicalize(&release).unwrap());
                Ok(())
            })
            .unwrap();
        assert!(status.fence.is_none());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn p7_install_disconnected_writer_client_retains_guard_until_commit() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("h");
    let writer = sluice_store::Writer::open(&home).unwrap();
    let install = Installation::for_home(&home).unwrap();
    let (ready, started) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let copy = writer.clone();
    let client = tokio::spawn(async move {
        sluice::install::admission_write(
            &copy,
            sluice_store::RetrySafety::NonIdempotent,
            move |tx| {
                ready.send(()).unwrap();
                wait.recv().unwrap();
                tx.sql()
                    .execute("UPDATE maintenance SET revision=revision+1", [])?;
                tx.changed(None, "maintenance");
                Ok(())
            },
        )
        .await
    });
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    client.abort();
    let (finished, fenced) = std::sync::mpsc::channel();
    let fence = std::thread::spawn(move || {
        finished
            .send(install.fence("disconnected client".into()).unwrap())
            .unwrap()
    });
    assert!(fenced.recv_timeout(Duration::from_millis(100)).is_err());
    release.send(()).unwrap();
    fenced.recv_timeout(Duration::from_secs(5)).unwrap();
    fence.join().unwrap();
    writer.shutdown().await.unwrap();
    let sql = rusqlite::Connection::open(home.join("sluice.db")).unwrap();
    assert_eq!(
        sql.query_row("SELECT revision FROM maintenance", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn installation_generations_fencing_and_corrupt_control() {
    let gate = support::Gate::new();
    let install = Installation::at(gate.install.clone()).unwrap();
    {
        let release = fake_release(gate.root.path(), "release");
        assert!(
            install
                .select(&release, &gate.home)
                .unwrap_err()
                .to_string()
                .contains("requires")
        );
        let fence = install.fence("test switch".into()).unwrap();
        let selected = install.select(&release, &gate.home).unwrap();
        let selection = selected.selection.unwrap();
        assert!(selection.generation > fence.generation);
        assert_eq!(selection.release_path, release);
        assert_eq!(selection.home_path, gate.home);
        assert!(install.admission_guard(&gate.home, false).is_err());
        assert!(install.admission_guard(&gate.home, true).is_ok());
        install.unfence().unwrap();
        assert!(
            install
                .admission_guard(&gate.root.path().join("old-home"), false)
                .is_err()
        );
        assert!(install.admission_guard(&gate.home, false).is_ok());
    }
    {
        let guard = install.admission_guard(&gate.home, false).unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        let copy = install.clone();
        let task =
            std::thread::spawn(move || sent.send(copy.fence("rollback".into()).unwrap()).unwrap());
        assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
        drop(guard);
        let fenced = received.recv_timeout(Duration::from_secs(5)).unwrap();
        task.join().unwrap();
        std::fs::remove_dir_all(&gate.home).unwrap();
        std::fs::create_dir(&gate.home).unwrap();
        assert_eq!(install.status().unwrap().fence, fenced.fence);
        assert!(install.admission_guard(&gate.home, false).is_err());
    }
    {
        install.fence("test".into()).unwrap();
        std::fs::write(gate.install.join("fence.json"), b"broken").unwrap();
        assert!(install.admission_guard(&gate.home, false).is_err());
        let inside = Installation::at(gate.home.join("install")).unwrap();
        inside.fence("test".into()).unwrap();
        assert!(
            inside
                .select(&fake_release(gate.root.path(), "r"), &gate.home)
                .is_err()
        );
    }
}
