//! Real kernel failures must preserve the guardian and a neighboring run.
use sluice_model::{
    Limits, RunLimits, RunUsage,
    ids::{HomeId, InvocationId, RunId},
};
use sluice_process::{
    cgroup::{Cgroup, RunCgroups},
    identity::OwnedProcess,
    signals::{StopPolicy, stop_invocation},
    spawn::PreparedLaunch,
    systemd::{ServiceCommand, StartOutcome, TransientService, WorkloadSlice, enforce_run_limits},
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

fn capped() -> RunLimits {
    RunLimits {
        memory_max_bytes: Some(64 << 20),
        cpu_millis: Some(250),
        tasks: Some(32),
    }
}
/// What a build run gets by default: a memory cap, no CPU or task cap.
fn build() -> RunLimits {
    RunLimits {
        memory_max_bytes: Some(256 << 20),
        cpu_millis: None,
        tasks: None,
    }
}

/// Bugs: a run's pids.max of 512 made a Buck2 sync fail with EAGAIN (an unlimited-tasks run
/// must get `max` and start more than 512 threads); a task-limit hit went unseen (it must show
/// in the run's counters from pids.events); and raising the home total refused to apply while
/// runs were live.
#[tokio::test]
async fn resource_limits_share_sibling_budget_and_isolate_neighboring_run() {
    let home = tempfile::tempdir().unwrap();
    let totals = Limits {
        memory_bytes: Some(1 << 30),
        cpu_millis: Some(2000),
        tasks: Some(2048),
    };
    let home_id = HomeId::new();
    let slice = WorkloadSlice::new(home_id, totals).unwrap();
    let mut units = Units(Vec::new(), slice.name().to_owned());
    for mode in ["survivor", "victim", "threads"] {
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
            if mode == "threads" { build() } else { capped() },
            256 << 20,
        ));
        units.0.push(TransientService::for_test(RunId::new()));
        let outcome = units.0.last_mut().unwrap().start_once(&spec).await.unwrap();
        assert!(
            matches!(outcome, StartOutcome::Confirmed { .. }),
            "{mode}: {outcome:?}"
        );
        if mode == "threads" {
            fs::write(home.path().join("threads/confirmed"), b"confirmed").unwrap();
        }
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
    assert!(
        usage.pids_limit_hits > 0,
        "a fork past pids.max must count in pids.events: {usage:?}"
    );
    wait_file(&home.path().join("threads/evidence.json")).await;
    let threads: serde_json::Value =
        serde_json::from_slice(&fs::read(home.path().join("threads/evidence.json")).unwrap())
            .unwrap();
    assert_eq!(threads["pids_max"], "max", "{threads}");
    assert_eq!(threads["service_pids_max"], "max", "{threads}");
    assert!(
        threads["cpu_max"].as_str().unwrap().starts_with("max "),
        "{threads}"
    );
    assert_eq!(
        threads["memory_max"],
        (256u64 << 20).to_string(),
        "{threads}"
    );
    assert!(threads["threads"].as_u64().unwrap() > 512, "{threads}");
    assert_eq!(threads["usage"]["pids_limit_hits"], 0, "{threads}");
    assert!(units.0[0].query().await.unwrap().main_pid.is_some());
    slice.ensure().await.unwrap();
    // A changed total applies to the populated slice; live runs keep their own caps.
    let changed = WorkloadSlice::new(
        home_id,
        Limits {
            memory_bytes: Some(2 << 30),
            cpu_millis: None,
            ..totals
        },
    )
    .unwrap();
    changed.ensure().await.unwrap();
    let group = String::from_utf8(
        Command::new("/usr/bin/systemctl")
            .args([
                "--user",
                "show",
                "--property=ControlGroup",
                "--value",
                slice.name(),
            ])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let group = Path::new("/sys/fs/cgroup").join(group.trim().trim_start_matches('/'));
    assert_eq!(
        fs::read_to_string(group.join("memory.max")).unwrap().trim(),
        (2u64 << 30).to_string()
    );
    assert!(
        fs::read_to_string(group.join("cpu.max"))
            .unwrap()
            .starts_with("max ")
    );
    fs::write(home.path().join("survivor/finish"), b"done").unwrap();
    wait_file(&home.path().join("survivor/evidence.json")).await;
    let survivor: RunUsage =
        serde_json::from_slice(&fs::read(home.path().join("survivor/evidence.json")).unwrap())
            .unwrap();
    assert_eq!(survivor.oom_kills, 0);
}

/// Bug: runs started with memory.max=max and nobody noticed until the host was reclaiming
/// memory (incident 2026-10-19). A run's limits are read back from systemd and the kernel and
/// must match its frozen budget; a cap lost after start is applied again.
#[tokio::test]
async fn a_runs_limits_are_read_back_and_a_lost_cap_is_applied_again() {
    let slice = WorkloadSlice::new(HomeId::new(), Limits::UNLIMITED).unwrap();
    let mut units = Units(
        vec![TransientService::for_test(RunId::new())],
        slice.name().to_owned(),
    );
    let mut spec = ServiceCommand::new("/usr/bin/sleep");
    spec.args = vec!["30".into()];
    spec.workload_slice = Some(slice);
    spec.run_budget = Some((build(), 256 << 20));
    assert!(matches!(
        units.0[0].start_once(&spec).await.unwrap(),
        StartOutcome::Confirmed { .. }
    ));
    let unit = units.0[0].name().to_owned();
    let cap = ((256u64 << 20) + (256 << 20)).to_string();
    let applied = enforce_run_limits(&unit, build(), 256 << 20).await.unwrap();
    assert_eq!(
        (
            applied.memory_max.as_str(),
            applied.cgroup_memory_max.as_str()
        ),
        (cap.as_str(), cap.as_str())
    );
    assert_eq!(
        (applied.tasks_max.as_str(), applied.cgroup_pids_max.as_str()),
        ("infinity", "max")
    );
    assert!(applied.cgroup_cpu_max.starts_with("max "), "{applied:?}");
    // A frozen budget the service does not have is a mismatch, named.
    assert!(
        applied
            .mismatches(sluice_process::cgroup::service_limits(capped(), 256 << 20).unwrap())
            .iter()
            .any(|m| m.starts_with("MemoryMax is")),
        "{applied:?}"
    );
    // The cap is lost at runtime, as by hand: it is put back and read back again.
    assert!(
        Command::new("/usr/bin/systemctl")
            .args([
                "--user",
                "set-property",
                "--runtime",
                &unit,
                "MemoryMax=infinity"
            ])
            .status()
            .unwrap()
            .success()
    );
    let repaired = enforce_run_limits(&unit, build(), 256 << 20).await.unwrap();
    assert_eq!(repaired.cgroup_memory_max, cap);
}

#[test]
#[ignore = "internal resource service worker"]
fn resource_worker() {
    if let Some(nonce) = std::env::var_os("SLUICE_TEST_RESOURCE_NONCE") {
        let timeout = std::env::var_os("SLUICE_TEST_RESOURCE_TIMEOUT").unwrap();
        let marker = std::env::var_os("SLUICE_TEST_RESOURCE_MARKER").unwrap();
        sluice_process::launcher::payload_exec_main(vec![nonce, timeout, marker], |args| {
            let script = match std::env::var("SLUICE_TEST_RESOURCE_MODE").as_deref() {
                // Start more threads than the old 512-task default allowed, then exit.
                Ok("threads") => {
                    "import os, pathlib, sys, threading\nos.setsid()\np=pathlib.Path(sys.argv[1])\nev=threading.Event()\nts=[threading.Thread(target=ev.wait) for _ in range(600)]\nfor t in ts: t.start()\n(p.parent/'threads').write_text(str(len(ts)))\nev.set()\nfor t in ts: t.join()\np.write_text(str(os.getpid()))\nwhile not (p.parent/'finish').exists(): pass"
                }
                // A victim first forks past its task limit (EAGAIN), then frees those tasks.
                Ok("victim") => {
                    "import os, pathlib, sys, threading\nos.setsid()\np=pathlib.Path(sys.argv[1])\nev=threading.Event()\nts=[]\ntry:\n  for _ in range(64):\n    t=threading.Thread(target=ev.wait); t.start(); ts.append(t)\nexcept RuntimeError: pass\nev.set()\nfor t in ts: t.join()\np.write_text(str(os.getpid()))\nwhile not (p.parent/'allocate').exists(): pass\nx=bytearray(40*1024*1024)\nwhile True: pass"
                }
                _ => {
                    "import os, pathlib, sys\nos.setsid()\np=pathlib.Path(sys.argv[1])\np.write_text(str(os.getpid()))\nwhile not (p.parent/'allocate').exists(): pass\nx=bytearray(40*1024*1024)\nwhile True: pass"
                }
            };
            let status = Command::new("/usr/bin/python3")
                .args(["-c", script])
                .arg(&args[0])
                .status()?;
            Ok(status.code().unwrap_or(1))
        });
    }
    let Some(dir) = std::env::var_os("SLUICE_TEST_RESOURCE_DIR").map(std::path::PathBuf::from)
    else {
        return;
    };
    let mode = std::env::var("SLUICE_TEST_RESOURCE_MODE").unwrap();
    let victim = mode == "victim";
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut guardian = OwnedProcess::capture(std::process::id()).unwrap();
        let service = Cgroup::open_service(&guardian.identity().cgroup).unwrap();
        let limits = if mode == "threads" { build() } else { capped() };
        let service_path = service.filesystem_path();
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
        if mode == "threads" {
            let payload_dir = groups.payload().filesystem_path();
            let read = |path: std::path::PathBuf| fs::read_to_string(path).unwrap().trim().to_owned();
            fs::write(dir.join("finish"), b"done").unwrap();
            groups.payload().wait_populated(false, Duration::from_secs(3)).await.unwrap();
            for mut payload in payloads { payload.wait().unwrap(); }
            let evidence = serde_json::json!({
                "pids_max": read(payload_dir.join("pids.max")),
                "cpu_max": read(payload_dir.join("cpu.max")),
                "memory_max": read(payload_dir.join("memory.max")),
                "service_pids_max": read(service_path.join("pids.max")),
                "threads": read(dir.join("threads")).parse::<u64>().unwrap(),
                "usage": groups.payload().resource_usage().unwrap(),
            });
            fs::write(dir.join("evidence.json"), serde_json::to_vec(&evidence).unwrap()).unwrap();
            // Stay live until the parent has observed the transient service's start.
            wait_file(&dir.join("confirmed")).await;
            return;
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
