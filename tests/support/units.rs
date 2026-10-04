//! The `sluice-test-*` user units a test home can own, and their teardown.
//! Depends only on sluice-model so every test crate can include it.
use std::{
    path::{Path, PathBuf},
    process::Command,
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

/// Every unit `home` can own: its auto-started coordinator and one guardian unit
/// per run directory under `home/runs`.
pub fn home_units(home: &Path) -> Vec<String> {
    let mut units = vec![coordinator_unit(home)];
    if let Ok(runs) = std::fs::read_dir(home.join("runs")) {
        for entry in runs.flatten() {
            if let Some(run) = entry.file_name().to_str()
                && run.parse::<sluice_model::ids::RunId>().is_ok()
            {
                units.push(format!("sluice-test-{run}.service"));
            }
        }
    }
    units
}

/// Teardown for a test home: stop its units ([`stop_units`]).
pub fn stop_home_units(home: &Path) {
    stop_units(&home_units(home));
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
    for operation in ["stop", "reset-failed"] {
        let _ = Command::new("/usr/bin/systemctl")
            .args(["--user", operation])
            .args(units)
            .output();
    }
    let Ok(states) = Command::new("/usr/bin/systemctl")
        .args(["--user", "is-active"])
        .args(units)
        .output()
    else {
        return;
    };
    let left: Vec<String> = units
        .iter()
        .zip(String::from_utf8_lossy(&states.stdout).lines())
        .filter(|(_, state)| !matches!(*state, "inactive" | "failed" | "unknown"))
        .map(|(unit, state)| format!("{unit} ({state})"))
        .collect();
    if !std::thread::panicking() {
        assert!(left.is_empty(), "test units left running: {left:?}");
    }
}
