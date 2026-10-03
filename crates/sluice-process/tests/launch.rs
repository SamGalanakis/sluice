//! Launch boundary tests use p0's fixture binary and disposable user services.
use sluice_model::ids::{InvocationId, RunId};
use sluice_process::{
    cgroup::{Cgroup, RunCgroups},
    identity::OwnedProcess,
    signals::{StopPolicy, stop_invocation, stop_run},
    spawn::PreparedLaunch,
    systemd::{ServiceCommand, StartOutcome, TransientService},
};
use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

fn fixture_binary() -> PathBuf {
    std::env::var_os("SLUICE_FIXTURE_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("fixture")
        })
}
fn command(home: &Path) -> Command {
    let mut command = Command::new(fixture_binary());
    command.env("SLUICE_HOME", home);
    command
}
fn marker_args(home: &Path) -> Vec<OsString> {
    vec!["write-marker".into(), home.join("marker").into_os_string()]
}

#[test]
fn post_exec_spawn_returns_and_drop_never_dispatches() {
    let home = tempfile::tempdir().unwrap();
    let began = Instant::now();
    let prepared = PreparedLaunch::spawn(
        &mut command(home.path()),
        &marker_args(home.path()),
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "pre-exec stop would deadlock spawn"
    );
    let identity = prepared.process().identity().clone();
    let process = OwnedProcess::open(&identity).unwrap();
    thread::sleep(Duration::from_millis(50));
    assert!(!home.path().join("marker").exists());
    drop(prepared);
    assert!(process.exited().unwrap());
    assert!(!identity.matches_current().unwrap());
    assert!(!home.path().join("marker").exists());
}

#[test]
fn pidfd_rejects_stale_generation_and_remains_dead_after_reap() {
    let child = Command::new("/usr/bin/sleep").arg("10").spawn().unwrap();
    let process = OwnedProcess::capture(child.id()).unwrap();
    let mut child = OwnedChild { child, process };
    let process = &child.process;
    let mut stale = process.identity().clone();
    stale.start_time += 1;
    assert!(OwnedProcess::open(&stale).is_err());
    stale = process.identity().clone();
    stale.boot_id = "00000000-0000-0000-0000-000000000000".into();
    assert!(OwnedProcess::open(&stale).is_err());
    assert!(child.child.try_wait().unwrap().is_none());
    process.signal(rustix::process::Signal::KILL).unwrap();
    child.child.wait().unwrap();
    assert!(process.exited().unwrap());
    process.signal(rustix::process::Signal::TERM).unwrap();
}

#[test]
fn launcher_timeout_never_dispatches() {
    let home = tempfile::tempdir().unwrap();
    let prepared = PreparedLaunch::spawn(
        &mut command(home.path()),
        &marker_args(home.path()),
        Duration::from_millis(150),
    )
    .unwrap();
    thread::sleep(Duration::from_millis(250));
    assert!(prepared.process().exited().unwrap());
    assert!(!home.path().join("marker").exists());
}

struct UnitGuard(TransientService);
impl Drop for UnitGuard {
    fn drop(&mut self) {
        for operation in ["stop", "reset-failed"] {
            let _ = Command::new("/usr/bin/timeout")
                .args([
                    "--kill-after=1s",
                    "10s",
                    "/usr/bin/systemctl",
                    "--user",
                    operation,
                ])
                .arg(self.0.name())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

#[tokio::test]
#[ignore = "requires delegated systemd user services and compiled fixture binary"]
async fn launch_containment_and_cancellation_gate() {
    let home = tempfile::tempdir().unwrap();
    let mut guard = UnitGuard(TransientService::for_test(RunId::new()));
    let mut spec = ServiceCommand::new(std::env::current_exe().unwrap());
    spec.args = ["--exact", "launch_worker", "--ignored", "--nocapture"]
        .map(OsString::from)
        .to_vec();
    spec.cwd = Some(home.path().into());
    spec.env.insert(
        "SLUICE_TEST_LAUNCH_DIR".into(),
        home.path().as_os_str().to_owned(),
    );
    spec.env.insert(
        "SLUICE_FIXTURE_BINARY".into(),
        fixture_binary().into_os_string(),
    );
    let outcome = guard.0.start_once(&spec).await.unwrap();
    let StartOutcome::Confirmed { state, .. } = outcome else {
        panic!("{outcome:?}")
    };
    let group = Cgroup::open_service(state.cgroup.as_deref().unwrap()).unwrap();
    assert!(matches!(
        guard.0.start_once(&spec).await.unwrap(),
        StartOutcome::Confirmed {
            reconciled: true,
            ..
        }
    ));
    let deadline = Instant::now() + Duration::from_secs(15);
    while !home.path().join("done").exists() {
        assert!(
            Instant::now() < deadline,
            "worker timed out: {:?}",
            guard.0.query().await.unwrap()
        );
        let state = guard.0.query().await.unwrap();
        assert!(!state.stopped(), "worker failed: {state:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    println!(
        "{}",
        fs::read_to_string(home.path().join("launch-evidence.json")).unwrap()
    );
    let evidence_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/p3-01-evidence");
    fs::create_dir_all(&evidence_dir).unwrap();
    fs::copy(
        home.path().join("launch-evidence.json"),
        evidence_dir.join("launch.json"),
    )
    .unwrap();
    let proof = stop_run(&guard.0, &group, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(proof.cgroup(), group.path());
    assert!(!group.populated().unwrap());
}

#[test]
#[ignore = "internal launch service worker"]
fn launch_worker() {
    let Some(home) = std::env::var_os("SLUICE_TEST_LAUNCH_DIR").map(PathBuf::from) else {
        return;
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let mut guardian = OwnedProcess::capture(std::process::id()).unwrap();
        let service = Cgroup::open_service(&guardian.identity().cgroup).unwrap();
        let groups = RunCgroups::create(service, &mut guardian).unwrap();
        assert_eq!(guardian.identity().cgroup, groups.control.path());
        let duplicate = InvocationId::new();
        let empty = groups.invocation(duplicate).unwrap();
        assert!(
            groups.invocation(duplicate).is_err(),
            "invocation names cannot be reused"
        );
        assert!(!empty.populated().unwrap());
        assert_eq!(
            groups
                .control
                .wait_populated(false, Duration::from_millis(10))
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        let mut evidence = Vec::new();
        for case in [
            "normal",
            "cancel-before",
            "cancel-record",
            "record-failed",
            "killed-before",
        ] {
            let leaf = groups.invocation(InvocationId::new()).unwrap();
            let marker = home.join("marker");
            let _ = fs::remove_file(&marker);
            let prepared = PreparedLaunch::spawn(
                &mut command(&home),
                &marker_args(&home),
                Duration::from_secs(5),
            )
            .unwrap();
            assert_eq!(prepared.process().identity().cgroup, groups.control.path());
            let before = prepared.process().identity().clone();
            let owned = OwnedProcess::open(&before).unwrap();
            let cancel = CancellationToken::new();
            if case == "cancel-before" {
                cancel.cancel();
            }
            if case == "killed-before" {
                prepared
                    .process()
                    .signal(rustix::process::Signal::KILL)
                    .unwrap();
                drop(prepared);
            } else {
                let result = prepared.continue_in(&leaf, &cancel, |identity| {
                    assert!(!marker.exists());
                    assert_eq!(identity.cgroup, leaf.path());
                    evidence.push(identity.clone());
                    if case == "cancel-record" {
                        cancel.cancel();
                    }
                    if case == "record-failed" {
                        return Err(io::Error::other("injected record failure"));
                    }
                    Ok(())
                });
                if case == "normal" {
                    let mut running = result.unwrap();
                    assert_eq!(running.started().identity.cgroup, leaf.path());
                    assert!(running.wait().unwrap().success());
                    assert!(marker.exists());
                } else {
                    assert!(result.is_err());
                    assert!(!marker.exists());
                }
            }
            assert!(owned.exited().unwrap());
            let proof = stop_invocation(&leaf, StopPolicy::default()).await.unwrap();
            assert_eq!(proof.cgroup(), leaf.path());
            assert!(!leaf.populated().unwrap());
            let dead_group = &evidence;
            assert!(dead_group.iter().all(|id| !id.matches_current().unwrap()));
        }
        fs::write(
            home.join("launch-evidence.json"),
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();
        fs::write(home.join("done"), b"done").unwrap();
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}

struct OwnedChild {
    child: std::process::Child,
    process: OwnedProcess,
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.process.signal(rustix::process::Signal::KILL);
        let _ = self.child.wait();
    }
}

#[test]
fn wrong_grant_nonce_cannot_dispatch() {
    use std::{
        io::{Read, Write},
        os::{fd::OwnedFd, unix::net::UnixStream},
    };
    let home = tempfile::tempdir().unwrap();
    let (mut parent, socket) = UnixStream::pair().unwrap();
    parent
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let child = command(home.path())
        .args(["payload-exec", &"a".repeat(64), "2000", "write-marker"])
        .arg(home.path().join("marker"))
        .stdin(Stdio::from(OwnedFd::from(socket)))
        .spawn()
        .unwrap();
    let process = OwnedProcess::capture(child.id()).unwrap();
    let mut child = OwnedChild { child, process };
    let mut length = [0; 4];
    parent.read_exact(&mut length).unwrap();
    let length = u32::from_be_bytes(length) as usize;
    assert!(length < 16 * 1024);
    let mut hello = vec![0; length];
    parent.read_exact(&mut hello).unwrap();
    let hello: serde_json::Value = serde_json::from_slice(&hello).unwrap();
    assert_eq!(hello["identity"]["pid"], child.child.id());
    let grant = serde_json::to_vec(
        &serde_json::json!({"nonce":"b".repeat(64),"identity":hello["identity"]}),
    )
    .unwrap();
    parent
        .write_all(&(grant.len() as u32).to_be_bytes())
        .unwrap();
    parent.write_all(&grant).unwrap();
    assert_eq!(child.child.wait().unwrap().code(), Some(126));
    assert!(child.process.exited().unwrap());
    assert!(!home.path().join("marker").exists());
}
