//! Real-host gate. All services and sockets are disposable and test-owned.
use rustix::{
    fd::OwnedFd,
    process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal},
};
use serde::Serialize;
use sluice_process::{host::HostCheck, tmux::ApprovedTmux};
use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize)]
struct Identity {
    role: String,
    pid: u32,
    start_time: u64,
    cgroup: String,
    session: i32,
}
struct Tracked {
    identity: Identity,
    pidfd: OwnedFd,
}
impl Tracked {
    fn record(role: &str, pid: u32) -> io::Result<Self> {
        let before = identity(role, pid)?;
        let pidfd = pidfd_open(
            Pid::from_raw(pid as i32).ok_or_else(|| io::Error::other("invalid pid"))?,
            PidfdFlags::empty(),
        )?;
        let after = identity(role, pid)?;
        if before.start_time != after.start_time {
            return Err(io::Error::other("pid changed while opening pidfd"));
        }
        Ok(Self {
            identity: after,
            pidfd,
        })
    }
    fn alive(&self) -> bool {
        procfs::process::Process::new(self.identity.pid as i32)
            .and_then(|p| p.stat())
            .is_ok_and(|s| {
                s.starttime == self.identity.start_time && s.state != 'Z' && s.state != 'X'
            })
    }
    fn sample(&self) -> io::Result<Identity> {
        let sample = identity(&self.identity.role, self.identity.pid)?;
        if sample.start_time != self.identity.start_time {
            return Err(io::Error::other("owned process identity changed"));
        }
        Ok(sample)
    }
}
fn identity(role: &str, pid: u32) -> io::Result<Identity> {
    let process = procfs::process::Process::new(pid as i32).map_err(io::Error::other)?;
    let stat = process.stat().map_err(io::Error::other)?;
    let cgroup = process
        .cgroups()
        .map_err(io::Error::other)?
        .0
        .into_iter()
        .find(|g| g.hierarchy == 0)
        .ok_or_else(|| io::Error::other("no v2 cgroup"))?
        .pathname;
    Ok(Identity {
        role: role.into(),
        pid,
        start_time: stat.starttime,
        cgroup,
        session: stat.session,
    })
}

#[derive(Serialize)]
struct Evidence {
    binary: PathBuf,
    version: String,
    unit: String,
    service_cgroup: String,
    observation_delay_ms: u128,
    immediate: Vec<Identity>,
    delayed: Vec<Identity>,
    migration_observed: bool,
    alive_after_unit_stop: Vec<Identity>,
    remaining_after_owned_cleanup: Vec<Identity>,
}

struct Fixture {
    scratch: tempfile::TempDir,
    unit: String,
    tracked: BTreeMap<String, Tracked>,
}
impl Fixture {
    fn new() -> io::Result<Self> {
        let scratch = tempfile::Builder::new().prefix("sluice-test-").tempdir()?;
        fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o700))?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        Ok(Self {
            scratch,
            unit: format!(
                "sluice-test-containment-{}-{nanos}.service",
                std::process::id()
            ),
            tracked: BTreeMap::new(),
        })
    }
    fn collect(&mut self) -> io::Result<()> {
        for role in ["worker", "server", "pane", "sleep", "detached"] {
            if !self.tracked.contains_key(role)
                && let Ok(text) =
                    fs::read_to_string(self.scratch.path().join(format!("{role}.pid")))
            {
                let pid = text.trim().parse().map_err(io::Error::other)?;
                self.tracked
                    .insert(role.into(), Tracked::record(role, pid)?);
            }
        }
        Ok(())
    }
    fn ready(&self) -> bool {
        if self.tracked.len() != 5 || !self.scratch.path().join("ready").exists() {
            return false;
        }
        // PID files precede exec/setsid. Do not call fork-time identities ready.
        let sleeping = |role: &str| {
            procfs::process::Process::new(self.tracked[role].identity.pid as i32)
                .and_then(|p| p.stat())
                .is_ok_and(|s| s.comm == "sleep")
        };
        sleeping("sleep")
            && sleeping("detached")
            && self.tracked["detached"]
                .sample()
                .is_ok_and(|s| s.session == s.pid as i32)
    }
    fn sample(&self) -> io::Result<Vec<Identity>> {
        self.tracked.values().map(Tracked::sample).collect()
    }
    fn alive(&self) -> Vec<Identity> {
        self.tracked
            .values()
            .filter(|p| p.alive())
            .map(|p| p.sample().unwrap_or_else(|_| p.identity.clone()))
            .collect()
    }
    fn stop(&self) -> io::Result<()> {
        let result = systemctl("stop", &self.unit);
        let show = systemctl_property("ActiveState", &self.unit)?;
        if !matches!(show.trim(), "inactive" | "failed") {
            return Err(io::Error::other(format!(
                "unit still active: {show}; stop={result:?}"
            )));
        }
        let _ = systemctl("reset-failed", &self.unit);
        Ok(())
    }
    fn cleanup_owned(&self) {
        for tracked in self.tracked.values() {
            // Stable descriptors, including the host tmux's escaped pane.
            let _ = pidfd_send_signal(&tracked.pidfd, Signal::KILL);
        }
    }
    fn wait_dead(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.alive().is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Also collect files written immediately before a startup failure.
        let _ = self.collect();
        let _ = self.stop();
        self.cleanup_owned();
        self.wait_dead();
        if !self.alive().is_empty() {
            eprintln!(
                "owned fixture processes survived cleanup: {:?}",
                self.alive()
            );
        }
    }
}

fn bounded(command: Command) -> io::Result<Output> {
    // Wrap an argv vector, never a shell string. timeout bounds cleanup as well.
    let mut timeout = Command::new("/usr/bin/timeout");
    timeout
        .args(["--kill-after=1s", "10s"])
        .arg(command.get_program())
        .args(command.get_args());
    if let Some(dir) = command.get_current_dir() {
        timeout.current_dir(dir);
    }
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => {
                timeout.env(key, value);
            }
            None => {
                timeout.env_remove(key);
            }
        }
    }
    let output = timeout.stdin(Stdio::null()).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{timeout:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output)
}
fn systemctl(operation: &str, unit: &str) -> io::Result<Output> {
    let mut command = Command::new("/usr/bin/systemctl");
    command.args(["--user", operation, unit]);
    bounded(command)
}
fn systemctl_property(property: &str, unit: &str) -> io::Result<String> {
    let mut command = Command::new("/usr/bin/systemctl");
    command.args(["--user", "show", "--value", "--property", property, unit]);
    Ok(String::from_utf8_lossy(&bounded(command)?.stdout)
        .trim()
        .into())
}
fn under(group: &str, root: &str) -> bool {
    group == root
        || group
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}
fn default_prefix() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/private-tmux")
}
fn evidence_dir() -> PathBuf {
    std::env::var_os("SLUICE_CONTAINMENT_EVIDENCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/p3-00-evidence")
        })
}

async fn run_fixture(expect_migration: bool) -> io::Result<Evidence> {
    let prefix = std::env::var_os("SLUICE_PRIVATE_TMUX_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(default_prefix);
    let binary = if expect_migration {
        PathBuf::from("/usr/bin/tmux")
    } else {
        ApprovedTmux::load(&prefix).await?.binary().to_owned()
    };
    let mut version = Command::new(&binary);
    version.arg("-V");
    let version = String::from_utf8_lossy(&bounded(version)?.stdout)
        .trim()
        .to_owned();
    if expect_migration && version != "tmux 3.4" {
        return Err(io::Error::other(format!(
            "red-side gate requires host tmux 3.4, got {version}"
        )));
    }
    let mut fixture = Fixture::new()?;
    // The pane owns two children, one detached into a new session. argv is fixed;
    // no paths, unit names or user data are interpolated into the shell script.
    fs::write(
        fixture.scratch.path().join("pane.sh"),
        "#!/bin/sh\necho \"$$\" > pane.pid\n/usr/bin/sleep 120 &\necho \"$!\" > sleep.pid\n/usr/bin/setsid /usr/bin/sleep 120 &\necho \"$!\" > detached.pid\nwait\n",
    )?;
    let mut start = Command::new("/usr/bin/systemd-run");
    start
        .args([
            "--user",
            "--quiet",
            "--collect",
            "--service-type=exec",
            "--unit",
        ])
        .arg(&fixture.unit)
        .args([
            "--property=Delegate=yes",
            "--property=KillMode=control-group",
            "--property=Restart=no",
            "--property=RuntimeMaxSec=60s",
            "--property=TimeoutStopSec=3s",
            "--working-directory",
        ])
        .arg(fixture.scratch.path())
        .arg("--setenv")
        .arg(format!(
            "SLUICE_TEST_WORKER_DIR={}",
            fixture.scratch.path().display()
        ))
        .arg("--setenv")
        .arg(format!("SLUICE_TEST_TMUX_BINARY={}", binary.display()))
        .arg("--setenv")
        .arg(format!("SLUICE_TEST_EXPECT_MIGRATION={expect_migration}"))
        .arg("--setenv")
        .arg(format!("SLUICE_PRIVATE_TMUX_PREFIX={}", prefix.display()))
        .arg("--")
        .arg(std::env::current_exe()?)
        .args(["--exact", "service_worker", "--ignored", "--nocapture"]);
    bounded(start)?;
    let group = systemctl_property("ControlGroup", &fixture.unit)?;
    if !group.ends_with(&fixture.unit) {
        return Err(io::Error::other("no unique service cgroup"));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        fixture.collect()?;
        if fixture.ready() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if !fixture.ready() {
        return Err(io::Error::other(
            "fixture not ready with all five identities",
        ));
    }
    let immediate = fixture.sample()?;
    assert_ne!(
        fixture.tracked["detached"].identity.session, fixture.tracked["pane"].identity.session,
        "setsid child must have a different session identity"
    );
    let observed = Instant::now();
    thread::sleep(Duration::from_millis(3200));
    let delayed = fixture.sample()?;
    let observation_delay_ms = observed.elapsed().as_millis();
    let migration_observed = delayed.iter().any(|pid| !under(&pid.cgroup, &group));
    fixture.stop()?;
    // Give systemd and the private server's children time to reap after stop.
    let stop_deadline = Instant::now() + Duration::from_millis(500);
    while !fixture.alive().is_empty() && Instant::now() < stop_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let alive_after_unit_stop = fixture.alive();
    fixture.cleanup_owned();
    fixture.wait_dead();
    Ok(Evidence {
        binary,
        version,
        unit: fixture.unit.clone(),
        service_cgroup: group,
        observation_delay_ms,
        immediate,
        delayed,
        migration_observed,
        alive_after_unit_stop,
        remaining_after_owned_cleanup: fixture.alive(),
    })
}

#[tokio::test]
#[ignore = "real systemd user manager, cgroup delegation and private tmux build required"]
async fn containment_private_tmux() {
    let prefix = std::env::var_os("SLUICE_PRIVATE_TMUX_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(default_prefix);
    let report = HostCheck::new(&prefix).run().await;
    let dir = evidence_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("host-report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    assert!(report.ready, "host prerequisites: {report:#?}");
    let evidence = run_fixture(false).await.unwrap();
    fs::write(
        dir.join("containment-private.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
    println!("{}", serde_json::to_string_pretty(&evidence).unwrap());
    assert!(evidence.observation_delay_ms >= 3000);
    assert!(
        evidence
            .immediate
            .iter()
            .chain(&evidence.delayed)
            .all(|p| under(&p.cgroup, &evidence.service_cgroup))
    );
    assert!(!evidence.migration_observed);
    assert!(
        evidence.alive_after_unit_stop.is_empty(),
        "unit stop leaked private tmux work"
    );
    assert!(evidence.remaining_after_owned_cleanup.is_empty());
}

#[tokio::test]
#[ignore = "red-side gate requires the host's scope-migrating /usr/bin/tmux 3.4"]
async fn containment_host_tmux_detects_migration() {
    let evidence = run_fixture(true).await.unwrap();
    let dir = evidence_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("containment-host.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
    println!("{}", serde_json::to_string_pretty(&evidence).unwrap());
    assert!(evidence.observation_delay_ms >= 3000);
    assert!(
        evidence.migration_observed,
        "fixture failed to detect host tmux migration"
    );
    assert!(
        evidence
            .delayed
            .iter()
            .any(|p| p.role == "pane" && !under(&p.cgroup, &evidence.service_cgroup))
    );
    assert!(evidence.remaining_after_owned_cleanup.is_empty());
}

// Invoked as this test binary's sole test inside the delegated service. The
// normal --ignored invocation does nothing without its explicit worker env.
#[test]
#[ignore = "internal service worker for containment fixtures"]
fn service_worker() {
    let Some(dir) = std::env::var_os("SLUICE_TEST_WORKER_DIR").map(PathBuf::from) else {
        return;
    };
    assert_eq!(
        fs::canonicalize(std::env::current_dir().unwrap()).unwrap(),
        fs::canonicalize(&dir).unwrap()
    );
    fs::write(dir.join("worker.pid"), std::process::id().to_string()).unwrap();
    let binary = PathBuf::from(std::env::var_os("SLUICE_TEST_TMUX_BINARY").unwrap());
    let host = std::env::var("SLUICE_TEST_EXPECT_MIGRATION").unwrap() == "true";
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let approved = if host {
        assert_eq!(binary, Path::new("/usr/bin/tmux"));
        None
    } else {
        let artifact = runtime
            .block_on(ApprovedTmux::load(&PathBuf::from(
                std::env::var_os("SLUICE_PRIVATE_TMUX_PREFIX").unwrap(),
            )))
            .unwrap();
        assert_eq!(artifact.binary(), binary);
        Some(artifact)
    };
    let command = |foreground: bool| {
        if let Some(tmux) = &approved {
            if foreground {
                tmux.server_command(&dir, None).unwrap()
            } else {
                tmux.client_command(&dir).unwrap()
            }
        } else {
            let mut command = Command::new(&binary);
            command
                .current_dir(&dir)
                .args(["-S", "tmux.sock", "-f", "/dev/null"])
                .env_remove("TMUX")
                .env_remove("TMUX_PANE");
            if foreground {
                command.arg("-D");
            }
            command
        }
    };
    let mut start = command(true);
    start
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut server = start.spawn().unwrap();
    fs::write(dir.join("server.pid"), server.id().to_string()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !dir.join("tmux.sock").exists() && Instant::now() < deadline {
        assert!(
            server.try_wait().unwrap().is_none(),
            "foreground server exited"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        dir.join("tmux.sock").exists(),
        "private socket never appeared"
    );
    let mut pane = command(false);
    pane.args([
        "new-session",
        "-d",
        "-s",
        "containment-fixture",
        "--",
        "/bin/sh",
        "pane.sh",
    ]);
    bounded(pane).unwrap();
    fs::write(dir.join("ready"), b"ready").unwrap();
    // systemd owns termination. Nothing sends keys or addresses another server.
    loop {
        assert!(server.try_wait().unwrap().is_none());
        thread::sleep(Duration::from_millis(100));
    }
}
