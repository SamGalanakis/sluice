#[path = "fixtures/install_support.rs"]
mod support;
use serde_json::{Value, json};
use sluice::install::Installation;
use sluice_model::{commands::CommandReply, error::PublicError};
use std::{
    path::Path,
    sync::{Arc, Barrier},
    time::Duration,
};

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
fn p7_install_select_requires_fence_and_publishes_release_and_home_as_one_generation() {
    let gate = support::Gate::new();
    let install = Installation::at(gate.install.clone()).unwrap();
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
#[test]
fn p7_install_fence_waits_for_decisions_and_survives_home_restoration() {
    let gate = support::Gate::new();
    let install = Installation::at(gate.install.clone()).unwrap();
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
#[test]
fn p7_install_concurrent_admissions_are_ordered_against_the_fence_generation() {
    let gate = support::Gate::new();
    let install = Installation::at(gate.install.clone()).unwrap();
    let barrier = Arc::new(Barrier::new(17));
    let threads: Vec<_> = (0..16)
        .map(|_| {
            let install = install.clone();
            let barrier = barrier.clone();
            let home = gate.home.clone();
            std::thread::spawn(move || {
                barrier.wait();
                match install.admission_guard(&home, false) {
                    Ok(guard) => {
                        std::thread::sleep(Duration::from_millis(10));
                        Some(guard.generation)
                    }
                    Err(PublicError::Busy { .. }) => None,
                    Err(e) => panic!("{e}"),
                }
            })
        })
        .collect();
    barrier.wait();
    let fenced = install.fence("race".into()).unwrap();
    for thread in threads {
        if let Some(generation) = thread.join().unwrap() {
            assert!(generation < fenced.generation);
        }
    }
    assert!(install.admission_guard(&gate.home, false).is_err());
}
#[test]
fn p7_install_malformed_fence_fails_closed_and_control_dir_cannot_be_inside_home() {
    let gate = support::Gate::new();
    let install = Installation::at(gate.install.clone()).unwrap();
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
#[test]
fn p7_install_paused_and_old_socket_clients_refuse_fenced_admission_and_dead_broker_stays_dead() {
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
}
#[test]
fn p7_install_maintenance_boot_serves_edits_retry_and_status_without_admission() {
    let mut gate = support::Gate::new();
    assert!(gate.cli(&["install", "fence", "cutover"]).status.success());
    gate.boot(Path::new(env!("CARGO_BIN_EXE_sluice")), true);
    let CommandReply::Project(project) = gate.rpc(json!({"command":"project_create","args":{"name":"p","description":"","icon":null,"resources":{},"author":"test"}})).unwrap() else { panic!("project") };
    let selector = json!({"kind":"id","value":project.project_id});
    // Set the importer's durable home mode through the writer while stopped.
    gate.stop();
    let db = rusqlite::Connection::open(gate.home.join("sluice.db")).unwrap();
    db.execute("UPDATE maintenance SET mode='cutover'", [])
        .unwrap();
    drop(db);
    gate.boot(Path::new(env!("CARGO_BIN_EXE_sluice")), true);
    gate.rpc(json!({"command":"plan_patch","args":{"project":selector,"rev":1,"ops":[{"op":"add","path":"/steps/work","value":{"run":"fixture.echo","in":{"value":{"default":1}}}}],"start":true,"dry_run":false,"author":"test","reason":"maintenance edit"}})).unwrap();
    gate.rpc(json!({"command":"step_set_output","args":{"project":selector,"step":"work","outputs":{"value":1},"force":true,"reason":"fixture terminal result","author":"test"}})).unwrap();
    gate.rpc(json!({"command":"step_retry","args":{"project":selector,"selection":{"steps":["work"],"tags":null},"message":"maintenance feedback","author":"test","reason":"cutover"}})).unwrap();
    assert!(gate.rpc(json!({"command":"status","args":{"project":selector,"selection":{"steps":null,"tags":null}}})).is_ok());
    assert!(matches!(gate.rpc(call()), Err(PublicError::Busy { .. })));
    // A manifest marker makes this a Rust selection for the ordered release API.
    let release = fake_release(gate.root.path(), "release");
    std::fs::write(release.join("manifest.json"), b"{}").unwrap();
    Installation::at(gate.install.clone())
        .unwrap()
        .select(&release, &gate.home)
        .unwrap();
    let runtime = sluice_runtime::coordinator::executor().unwrap();
    let status = runtime
        .block_on(sluice::install::release_cutover(
            Installation::at(gate.install.clone()).unwrap(),
        ))
        .unwrap();
    assert!(status.fence.is_none());
    let db = rusqlite::Connection::open(gate.home.join("sluice.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT mode FROM maintenance", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "normal"
    );
}

#[tokio::test(flavor = "multi_thread")]
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
#[ignore = "P7 real concurrent direct calls and guardian barriers; scratch delegated services only"]
fn p7_install_direct_clients_race_fence_without_new_payload_continuations() {
    let mut gate = support::Gate::new();
    gate.boot(Path::new(env!("CARGO_BIN_EXE_sluice")), false);
    let clients: Vec<_> = (0..8)
        .map(|_| {
            let home = gate.home.clone();
            std::thread::spawn(move || support::rpc(&home, call()))
        })
        .collect();
    let fence = gate.cli(&["install", "fence", "live direct race"]);
    assert!(fence.status.success(), "{:?}", fence);
    let launches = || {
        let mut launches: Vec<_> = std::fs::read_dir(gate.home.join("runs"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path().join("launch.json"))
            .filter(|p| p.exists())
            .collect();
        launches.sort();
        launches
    };
    let before = launches();
    let mut accepted = 0;
    for client in clients {
        match client.join().unwrap() {
            Ok(_) => accepted += 1,
            Err(PublicError::Busy { .. }) => (),
            Err(e) => panic!("{e}"),
        }
    }
    for _ in 0..8 {
        assert!(matches!(gate.rpc(call()), Err(PublicError::Busy { .. })));
    }
    // Give services accepted just before fence entry time to reach the barrier.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        launches(),
        before,
        "payload continuation after fence publication"
    );
    let sql = rusqlite::Connection::open(gate.home.join("sluice.db")).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM calls", [], |r| r.get::<_, usize>(0))
            .unwrap(),
        accepted
    );
    // Existing completion/status paths remain callable during maintenance.
    assert!(gate.rpc(json!({"command":"projects_list"})).is_ok());
}
