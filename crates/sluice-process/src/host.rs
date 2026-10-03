//! Host prerequisites. Failed checks block execution; no weaker cleanup fallback exists.
use crate::tmux::{ApprovedTmux, checked_output, invalid};
use serde::{Deserialize, Serialize};
use sluice_model::error::PublicError;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Prerequisite {
    CgroupV2,
    SystemdUserManager,
    Delegation,
    PidfdOpen,
    ProcBootId,
    ApprovedTmux,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CheckResult {
    pub prerequisite: Prerequisite,
    pub passed: bool,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DelegationProof {
    pub unit: String,
    pub cgroup: String,
    pub enabled_controller: String,
    pub child_group_created: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HostReport {
    pub ready: bool,
    pub checks: Vec<CheckResult>,
    pub boot_id: Option<String>,
    pub delegation: Option<DelegationProof>,
}

pub struct HostCheck {
    tmux_prefix: PathBuf,
}

impl HostCheck {
    pub fn new(tmux_prefix: impl Into<PathBuf>) -> Self {
        Self {
            tmux_prefix: tmux_prefix.into(),
        }
    }

    /// Run all six real probes, including a disposable delegated user service.
    pub async fn run(&self) -> HostReport {
        let mut checks = Vec::new();
        checks.push(outcome(
            Prerequisite::CgroupV2,
            check_cgroup_v2(),
            "Boot Linux with unified cgroup v2 mounted read-write at /sys/fs/cgroup.",
        ));
        let mut manager = Command::new("/usr/bin/systemctl");
        manager.args(["--user", "show", "--property=Version", "--value"]);
        let manager = checked_output(manager).await.and_then(|output| {
            if output.stdout.iter().all(u8::is_ascii_whitespace) {
                Err(invalid("user manager returned no version"))
            } else {
                Ok(format!(
                    "Reachable systemd user manager {}",
                    String::from_utf8_lossy(&output.stdout).trim()
                ))
            }
        });
        let manager_ok = manager.is_ok();
        checks.push(outcome(Prerequisite::SystemdUserManager, manager,
            "Run in a login with a systemd user manager and its user bus; check systemctl --user show."));
        let proof = if manager_ok {
            check_delegation().await
        } else {
            Err(io::Error::other(
                "delegation requires a reachable systemd user manager",
            ))
        };
        let delegation = match proof {
            Ok(proof) => {
                checks.push(outcome(Prerequisite::Delegation,
                    Ok(format!("Delegate=yes permitted writable subtree_control, +{} and a child group in {}. Probe unit stopped.", proof.enabled_controller, proof.cgroup)), ""));
                Some(proof)
            }
            Err(error) => {
                checks.push(outcome(Prerequisite::Delegation, Err(error),
                    "Enable user-service cgroup delegation of the pids controller; check user@UID.service Delegate and cgroup permissions."));
                None
            }
        };
        let pidfd = rustix::process::pidfd_open(
            rustix::process::getpid(),
            rustix::process::PidfdFlags::empty(),
        )
        .map(|_| "pidfd_open is available for this user.".into())
        .map_err(io::Error::from);
        checks.push(outcome(
            Prerequisite::PidfdOpen,
            pidfd,
            "Use Linux 5.3 or newer and permit pidfd_open in the host's syscall policy.",
        ));
        let boot =
            fs::read_to_string("/proc/sys/kernel/random/boot_id").and_then(|s| parse_boot_id(&s));
        let boot_id = boot.as_ref().ok().cloned();
        checks.push(outcome(
            Prerequisite::ProcBootId,
            boot.map(|id| format!("Readable /proc boot id {id}.")),
            "Mount procfs with readable /proc/sys/kernel/random/boot_id.",
        ));
        let tmux = ApprovedTmux::load(&self.tmux_prefix).await.map(|artifact|
            format!("Approved tmux {} at {}; libevent statically linked and dynamic ncurses/tinfo resolved.",
                artifact.manifest().version, artifact.binary().display()));
        checks.push(outcome(
            Prerequisite::ApprovedTmux,
            tmux,
            &format!(
                "Build the release artifact with scripts/build-private-tmux {} and rerun doctor.",
                self.tmux_prefix.display()
            ),
        ));
        HostReport {
            ready: checks.iter().all(|check| check.passed),
            checks,
            boot_id,
            delegation,
        }
    }
}

fn outcome(prerequisite: Prerequisite, result: io::Result<String>, action: &str) -> CheckResult {
    match result {
        Ok(message) => CheckResult {
            prerequisite,
            passed: true,
            message,
        },
        Err(error) => CheckResult {
            prerequisite,
            passed: false,
            message: format!("{error}. {action}"),
        },
    }
}

fn check_cgroup_v2() -> io::Result<String> {
    // Linux CGROUP2_SUPER_MAGIC, from include/uapi/linux/magic.h.
    if rustix::fs::statfs("/sys/fs/cgroup")?.f_type != 0x6367_7270 {
        return Err(invalid("/sys/fs/cgroup is not a cgroup v2 filesystem"));
    }
    fs::read_to_string("/sys/fs/cgroup/cgroup.controllers")?;
    Ok("cgroup v2 is mounted at /sys/fs/cgroup.".into())
}

fn parse_boot_id(text: &str) -> io::Result<String> {
    let text = text.trim();
    if text.len() != 36
        || !text.bytes().enumerate().all(|(i, byte)| {
            if [8, 13, 18, 23].contains(&i) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return Err(invalid("/proc boot id is not a UUID"));
    }
    Ok(text.into())
}

async fn check_delegation() -> io::Result<DelegationProof> {
    let mut unit = ProbeUnit::new()?;
    let result = async {
        let mut start = Command::new("/usr/bin/systemd-run");
        start
            .args([
                "--user",
                "--quiet",
                "--collect",
                "--service-type=exec",
                "--unit",
            ])
            .arg(&unit.name)
            .args([
                "--property=Delegate=yes",
                "--property=KillMode=control-group",
                "--property=Restart=no",
                "--property=RuntimeMaxSec=30s",
                "--property=TimeoutStopSec=3s",
                "--",
                "/usr/bin/sleep",
                "30",
            ]);
        checked_output(start).await?;
        let mut show = Command::new("/usr/bin/systemctl");
        show.args([
            "--user",
            "show",
            "--property=ControlGroup",
            "--property=MainPID",
        ])
        .arg(&unit.name);
        let output = checked_output(show).await?;
        let show = String::from_utf8_lossy(&output.stdout);
        let group = show
            .lines()
            .find_map(|s| s.strip_prefix("ControlGroup="))
            .filter(|s| s.ends_with(&unit.name))
            .ok_or_else(|| invalid("probe has no matching cgroup"))?;
        let pid: u32 = show
            .lines()
            .find_map(|s| s.strip_prefix("MainPID="))
            .ok_or_else(|| invalid("probe has no MainPID"))?
            .parse()
            .map_err(|_| invalid("invalid probe MainPID"))?;
        let root = cgroup_path(group)?;
        let process = procfs::process::Process::new(pid as i32).map_err(io::Error::other)?;
        let groups = process.cgroups().map_err(io::Error::other)?;
        if !groups
            .0
            .iter()
            .any(|entry| entry.hierarchy == 0 && entry.pathname == group)
        {
            return Err(invalid("probe MainPID is outside its service cgroup"));
        }
        let mut subtree = OpenOptions::new()
            .write(true)
            .open(root.join("cgroup.subtree_control"))?;
        let controllers = fs::read_to_string(root.join("cgroup.controllers"))?;
        if !controllers.split_whitespace().any(|s| s == "pids") {
            return Err(invalid(
                "pids controller is not delegated to the user service",
            ));
        }
        let control = root.join("control");
        fs::create_dir(&control)?;
        fs::write(control.join("cgroup.procs"), pid.to_string())?;
        subtree.write_all(b"+pids")?;
        let payload = root.join("payload");
        fs::create_dir(&payload)?;
        if !fs::read_to_string(payload.join("cgroup.controllers"))?
            .split_whitespace()
            .any(|s| s == "pids")
        {
            return Err(invalid(
                "child group did not inherit the enabled pids controller",
            ));
        }
        // Open the child control too: directory creation alone is insufficient.
        OpenOptions::new()
            .write(true)
            .open(payload.join("cgroup.subtree_control"))?;
        Ok(DelegationProof {
            unit: unit.name.clone(),
            cgroup: group.into(),
            enabled_controller: "pids".into(),
            child_group_created: true,
        })
    }
    .await;
    // Reconcile this unique unit even when start timed out or reported an error.
    let cleanup = unit.stop().await;
    match (result, cleanup) {
        (Ok(proof), Ok(())) => Ok(proof),
        (Err(error), Ok(())) => Err(error),
        (result, Err(cleanup)) => Err(io::Error::other(format!(
            "delegation result {result:?}; cleanup failed: {cleanup}"
        ))),
    }
}

fn cgroup_path(group: &str) -> io::Result<PathBuf> {
    let relative = group
        .strip_prefix('/')
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("invalid service cgroup path"))?;
    if Path::new(relative)
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(invalid("invalid service cgroup path components"));
    }
    Ok(Path::new("/sys/fs/cgroup").join(relative))
}

struct ProbeUnit {
    name: String,
    stopped: bool,
}
impl ProbeUnit {
    fn new() -> io::Result<Self> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        Ok(Self {
            name: format!(
                "sluice-test-doctor-{}-{time}-{}.service",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ),
            stopped: false,
        })
    }
    async fn stop(&mut self) -> io::Result<()> {
        let mut stop = Command::new("/usr/bin/systemctl");
        stop.args(["--user", "stop"]).arg(&self.name);
        // A failed start may already have been collected. Verify absence below.
        let _ = checked_output(stop).await;
        let mut show = Command::new("/usr/bin/systemctl");
        show.args(["--user", "show", "--property=ActiveState", "--value"])
            .arg(&self.name);
        let output = checked_output(show).await?;
        let state = String::from_utf8_lossy(&output.stdout);
        if !matches!(state.trim(), "inactive" | "failed") {
            return Err(io::Error::other(format!(
                "{} still {}",
                self.name,
                state.trim()
            )));
        }
        let mut reset = Command::new("/usr/bin/systemctl");
        reset.args(["--user", "reset-failed"]).arg(&self.name);
        let _ = checked_output(reset).await;
        self.stopped = true;
        Ok(())
    }
}
impl Drop for ProbeUnit {
    fn drop(&mut self) {
        if !self.stopped {
            // Drop cannot await. Keep the cancellation/panic path bounded too.
            for operation in ["stop", "reset-failed"] {
                let _ = Command::new("/usr/bin/timeout")
                    .args([
                        "--kill-after=1s",
                        "8s",
                        "/usr/bin/systemctl",
                        "--user",
                        operation,
                    ])
                    .arg(&self.name)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
}

pub const OWNER_HOME: &str = "/home/sam/.sluice";

/// Resolve existing symlinks and normalize a possibly nonexistent suffix, in path order.
pub fn resolve_path(path: &Path) -> Result<PathBuf, PublicError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(storage)?.join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::RootDir => resolved.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(part) => {
                resolved.push(part);
                match fs::symlink_metadata(&resolved) {
                    Ok(_) => {
                        resolved = fs::canonicalize(&resolved).map_err(storage)?;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(storage(e)),
                }
            }
            Component::Prefix(_) => {
                return Err(PublicError::BadRequest {
                    message: "unsupported path prefix".into(),
                });
            }
        }
    }
    Ok(resolved)
}
fn storage(error: std::io::Error) -> PublicError {
    PublicError::Storage {
        message: error.to_string(),
    }
}

/// Reject the protected home and every descendant before creating or opening writable state.
pub fn guard_home(path: &Path, owner_home: &Path) -> Result<PathBuf, PublicError> {
    let resolved = resolve_path(path)?;
    let protected = resolve_path(owner_home)?;
    if resolved.starts_with(protected) {
        return Err(PublicError::BadRequest {
            message: "refusing the owner's real SLUICE_HOME or a path under it".into(),
        });
    }
    Ok(resolved)
}

pub fn guard_scratch_home(path: &Path) -> Result<PathBuf, PublicError> {
    let resolved = guard_home(path, Path::new(OWNER_HOME))?;
    if let Some(home) = std::env::var_os("HOME") {
        guard_home(&resolved, &PathBuf::from(home).join(".sluice"))?;
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refuses_unsafe_cgroup_paths_and_invalid_boot_identity() {
        for group in [
            "",
            "/",
            "relative",
            "/../user.slice",
            "/user.slice/../../etc",
        ] {
            assert!(cgroup_path(group).is_err(), "{group}");
        }
        assert_eq!(
            cgroup_path("/user.slice/probe.service").unwrap(),
            Path::new("/sys/fs/cgroup/user.slice/probe.service")
        );
        assert!(parse_boot_id("01234567-89ab-cdef-0123-456789abcdef\n").is_ok());
        for boot in ["", "abc", "01234567-89ab-cdef-0123-456789abcdeg"] {
            assert!(parse_boot_id(boot).is_err());
        }
    }
}
