//! Real kernel failures must preserve the guardian and a neighboring run.
use sluice_model::{
    RunLimits, RunUsage,
    ids::{HomeId, InvocationId, RunId},
};
use sluice_process::{
    cgroup::{Cgroup, RunCgroups},
    identity::OwnedProcess,
    signals::{StopPolicy, stop_invocation},
    spawn::PreparedLaunch,
    systemd::{ServiceCommand, StartOutcome, TransientService, WorkloadSlice},
};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command, time::Duration};
use tokio_util::sync::CancellationToken;

struct Units(Vec<TransientService>, String);
impl Drop for Units {
    fn drop(&mut self) {
        for unit in &self.0 {
            let _ = Command::new("/usr/bin/timeout")
                .args(["5", "/usr/bin/systemctl", "--user", "stop", unit.name()])
                .status();
        }
        for action in ["stop", "revert"] {
            let _ = Command::new("/usr/bin/timeout")
                .args(["5", "/usr/bin/systemctl", "--user", action, &self.1])
                .status();
        }
    }
}
async fn wait_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.metadata().is_ok_and(|metadata| metadata.len() > 0) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker did not publish evidence");
}

#[tokio::test]
async fn resource_limits_share_sibling_budget_and_isolate_neighboring_run() {
    let home = tempfile::tempdir().unwrap();
    let totals = RunLimits {
        memory_bytes: 1 << 30,
        cpu_millis: 2000,
        tasks: 256,
    };
    let home_id = HomeId::new();
    let slice = WorkloadSlice::new(home_id, totals).unwrap();
    let mut units = Units(Vec::new(), slice.name().to_owned());
    for mode in ["survivor", "victim"] {
        let dir = home.path().join(mode);
        fs::create_dir(&dir).unwrap();
        let mut spec = ServiceCommand::new(std::env::current_exe().unwrap());
        spec.args = [
            "--exact",
            "limits::resource_worker",
            "--ignored",
            "--nocapture",
        ]
        .map(Into::into)
        .into();
        spec.env.insert("SLUICE_TEST".into(), "1".into());
        spec.env
            .insert("SLUICE_TEST_RESOURCE_DIR".into(), dir.into_os_string());
        spec.env
            .insert("SLUICE_TEST_RESOURCE_MODE".into(), mode.into());
        spec.workload_slice = Some(slice.clone());
        spec.run_budget = Some((
            RunLimits {
                memory_bytes: 64 << 20,
                cpu_millis: 250,
                tasks: 32,
            },
            256 << 20,
        ));
        units.0.push(TransientService::for_test(RunId::new()));
        assert!(matches!(
            units.0.last_mut().unwrap().start_once(&spec).await.unwrap(),
            StartOutcome::Confirmed { .. }
        ));
    }
    wait_file(&home.path().join("survivor/ready")).await;
    wait_file(&home.path().join("victim/evidence.json")).await;
    let usage: RunUsage =
        serde_json::from_slice(&fs::read(home.path().join("victim/evidence.json")).unwrap())
            .unwrap();
    assert!(
        usage.oom_kills >= 2,
        "grouped OOM must stop both sibling payloads: {usage:?}"
    );
    assert!(usage.memory_peak_bytes >= 64 << 20);
    assert!(
        usage.memory_peak_bytes < 80 << 20,
        "memory runaway escaped its cap: {usage:?}"
    );
    assert!(
        usage.cpu_throttled_usec > 0,
        "busy payload must be throttled: {usage:?}"
    );
    assert!(units.0[0].query().await.unwrap().main_pid.is_some());
    slice.ensure().await.unwrap();
    let changed = WorkloadSlice::new(
        home_id,
        RunLimits {
            memory_bytes: 512 << 20,
            ..totals
        },
    )
    .unwrap();
    assert_eq!(
        changed.ensure().await.unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    fs::write(home.path().join("survivor/finish"), b"done").unwrap();
    wait_file(&home.path().join("survivor/evidence.json")).await;
    let survivor: RunUsage =
        serde_json::from_slice(&fs::read(home.path().join("survivor/evidence.json")).unwrap())
            .unwrap();
    assert_eq!(survivor.oom_kills, 0);
}

#[test]
#[ignore = "internal resource service worker"]
fn resource_worker() {
    if let Some(nonce) = std::env::var_os("SLUICE_TEST_RESOURCE_NONCE") {
        let timeout = std::env::var_os("SLUICE_TEST_RESOURCE_TIMEOUT").unwrap();
        let marker = std::env::var_os("SLUICE_TEST_RESOURCE_MARKER").unwrap();
        sluice_process::launcher::payload_exec_main(vec![nonce, timeout, marker], |args| {
            let status = Command::new("/usr/bin/python3")
                .args(["-c", "import os, pathlib, sys\nos.setsid()\np=pathlib.Path(sys.argv[1])\np.write_text(str(os.getpid()))\nwhile not (p.parent/'allocate').exists(): pass\nx=bytearray(40*1024*1024)\nwhile True: pass"])
                .arg(&args[0]).status()?;
            Ok(status.code().unwrap_or(1))
        });
    }
    let Some(dir) = std::env::var_os("SLUICE_TEST_RESOURCE_DIR").map(std::path::PathBuf::from)
    else {
        return;
    };
    let victim = std::env::var("SLUICE_TEST_RESOURCE_MODE").unwrap() == "victim";
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut guardian = OwnedProcess::capture(std::process::id()).unwrap();
        let service = Cgroup::open_service(&guardian.identity().cgroup).unwrap();
        let limits = RunLimits { memory_bytes: 64 << 20, cpu_millis: 250, tasks: 32 };
        let groups = RunCgroups::create(service, &mut guardian, limits, 256 << 20).unwrap();
        let launcher = dir.join("launcher.sh");
        fs::write(&launcher, "#!/bin/sh\nexport SLUICE_TEST_RESOURCE_NONCE=\"$2\" SLUICE_TEST_RESOURCE_TIMEOUT=\"$3\" SLUICE_TEST_RESOURCE_MARKER=\"$4\"\nexec \"$SLUICE_TEST_RESOURCE_BINARY\" --exact limits::resource_worker --ignored --nocapture\n").unwrap();
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
        let mut payloads = Vec::new();
        for index in 0..if victim { 2 } else { 1 } {
            let marker = dir.join(format!("payload-{index}"));
            let leaf = groups.invocation(InvocationId::new()).unwrap();
            let mut command = Command::new(&launcher);
            command.env("SLUICE_TEST_RESOURCE_BINARY", std::env::current_exe().unwrap());
            let prepared = PreparedLaunch::spawn(&mut command, &[marker.as_os_str().to_owned()], Duration::from_secs(3)).unwrap();
            let payload = prepared.continue_in(&leaf, &CancellationToken::new(), |_| Ok(())).unwrap();
            wait_file(&marker).await;
            let pid: u32 = fs::read_to_string(marker).unwrap().parse().unwrap();
            assert_ne!(pid, payload.process().identity().pid, "allocation must happen in an inherited descendant");
            let detached = OwnedProcess::capture(pid).unwrap();
            assert_eq!(detached.identity().cgroup, leaf.path());
            payloads.push(payload);
        }
        if victim {
            tokio::time::timeout(Duration::from_secs(3), async {
                while groups.payload().resource_usage().unwrap().cpu_throttled_usec == 0 {
                    tokio::task::yield_now().await;
                }
            }).await.unwrap();
            fs::write(dir.join("allocate"), b"go").unwrap();
            groups.payload().wait_populated(false, Duration::from_secs(3)).await.unwrap();
        } else {
            fs::write(dir.join("ready"), b"ready").unwrap();
            wait_file(&dir.join("finish")).await;
            stop_invocation(groups.payload(), StopPolicy::default()).await.unwrap();
        }
        for mut payload in payloads { payload.wait().unwrap(); }
        assert!(groups.control.populated().unwrap(), "guardian died with payload");
        assert!(groups.payload_empty().unwrap());
        let usage = groups.payload().resource_usage().unwrap();
        fs::write(dir.join("evidence.json"), serde_json::to_vec(&usage).unwrap()).unwrap();
    });
}
