//! The `sluice-test-*` user units a test home can own, and their teardown.
//! Depends only on sluice-model and serde_json so every test crate can include it.
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

/// Drop guard for a home outside [`ScratchHome`]: stops its units on teardown,
/// panics included.
pub struct HomeUnits(pub PathBuf);
impl Drop for HomeUnits {
    fn drop(&mut self) {
        stop_home_units(&self.0);
    }
}

/// The `sluice-test-coordinator-<digest>` unit a CLI command auto-starts for `home`.
pub fn coordinator_unit(home: &Path) -> String {
    let digest =
        sluice_model::hash::ExecutionProvenance::fingerprint(home.as_os_str().as_encoded_bytes())
            .to_string();
    format!("sluice-test-coordinator-{}.service", &digest[..16])
}

/// The run directories under `home/runs`, by run id.
fn runs(home: &Path) -> Vec<(String, PathBuf)> {
    let Ok(runs) = std::fs::read_dir(home.join("runs")) else {
        return Vec::new();
    };
    runs.flatten()
        .filter_map(|entry| {
            let run = entry.file_name().to_str()?.to_owned();
            run.parse::<sluice_model::ids::RunId>().ok()?;
            Some((run, entry.path()))
        })
        .collect()
}

/// Every unit `home` can own: its auto-started coordinator and one guardian unit
/// per run directory under `home/runs`.
pub fn home_units(home: &Path) -> Vec<String> {
    let mut units = vec![coordinator_unit(home)];
    units.extend(
        runs(home)
            .into_iter()
            .map(|(run, _)| format!("sluice-test-{run}.service")),
    );
    units
}

/// The workload slice `home`'s runs were launched into, named by the home id their launch
/// records carry: `sluice-test-workloads-<home>.slice`. `None` when no run was launched.
pub fn home_slice(home: &Path) -> Option<String> {
    runs(home).into_iter().find_map(|(_, dir)| {
        let launch: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("runtime.json")).ok()?).ok()?;
        let id = launch.pointer("/identity/home")?.as_str()?;
        Some(format!(
            "sluice-test-workloads-{}.slice",
            id.replace('-', "_")
        ))
    })
}

/// Teardown for a test home: stop its units ([`stop_units`]), then its workload slice, which
/// stops anything still in it, and remove the slice's settings and lock.
pub fn stop_home_units(home: &Path) {
    stop_units(&home_units(home));
    stop_home_slice(home);
}

/// Stops `home`'s workload slice, once its units are stopped ([`stop_home_units`]).
pub fn stop_home_slice(home: &Path) {
    if let Some(slice) = home_slice(home) {
        stop_slice(&slice);
    }
}

/// `systemctl --user`, with the user manager found the way Sluice finds it: a scrubbed
/// environment (a test run inside a Sluice run's unit) has no XDG_RUNTIME_DIR, and without
/// it every call fails, so a teardown that ignored the failure left its units running.
fn systemctl(args: &[&str]) -> Output {
    let mut command = Command::new("/usr/bin/systemctl");
    command.arg("--user").args(args);
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        command.env("XDG_RUNTIME_DIR", runtime_dir());
    }
    command.output().expect("run systemctl")
}
/// Whether the user manager has `unit` active (or activating).
pub fn unit_active(unit: &str) -> bool {
    systemctl(&["is-active", "--quiet", unit]).status.success()
}
fn runtime_dir() -> PathBuf {
    let uid = std::fs::metadata("/proc/self")
        .expect("read /proc/self")
        .uid();
    PathBuf::from(format!("/run/user/{uid}"))
}

/// Stops and reset-fails exactly these `sluice-test-*` units, then asserts none of
/// them is still active (unless the test is already panicking, where a second
/// panic would abort the teardown of everything else).
pub fn stop_units(units: &[String]) {
    if units.is_empty() {
        return;
    }
    for unit in units {
        assert!(unit.starts_with("sluice-test-"), "refusing to stop {unit}");
    }
    let names: Vec<&str> = units.iter().map(String::as_str).collect();
    for operation in ["stop", "reset-failed"] {
        systemctl(&[&[operation][..], &names].concat());
    }
    assert_stopped(units);
}

/// Stops a home's workload slice (and anything still in it), drops its runtime settings and
/// removes its lock file, then asserts it is gone.
fn stop_slice(slice: &str) {
    assert!(
        slice.starts_with("sluice-test-workloads-"),
        "refusing to stop {slice}"
    );
    for operation in ["stop", "revert"] {
        systemctl(&[operation, slice]);
    }
    let _ = std::fs::remove_file(runtime_dir().join(format!("{slice}.lock")));
    assert_stopped(&[slice.to_owned()]);
}

fn assert_stopped(units: &[String]) {
    let names: Vec<&str> = units.iter().map(String::as_str).collect();
    let states = systemctl(&[&["is-active"][..], &names].concat());
    let states: Vec<String> = String::from_utf8_lossy(&states.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    if std::thread::panicking() {
        return;
    }
    // One state per unit, or the manager was never reached and nothing was stopped.
    assert_eq!(
        states.len(),
        units.len(),
        "could not read the state of {units:?}: {states:?}"
    );
    let left: Vec<String> = units
        .iter()
        .zip(&states)
        .filter(|(_, state)| !matches!(state.as_str(), "inactive" | "failed" | "unknown"))
        .map(|(unit, state)| format!("{unit} ({state})"))
        .collect();
    assert!(left.is_empty(), "test units left running: {left:?}");
}
