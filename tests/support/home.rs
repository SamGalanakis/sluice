use sluice_model::error::PublicError;
use sluice_process::host::guard_scratch_home;
use std::path::{Path, PathBuf};

#[allow(dead_code)]
#[path = "units.rs"]
pub mod units;

/// Owns the scratch directory; does not mutate process-global SLUICE_HOME.
pub struct ScratchHome {
    temp: tempfile::TempDir,
    home: PathBuf,
}
impl ScratchHome {
    pub fn new() -> Result<Self, PublicError> {
        let root = guard_scratch_home(&std::env::temp_dir())?;
        let temp = tempfile::tempdir_in(root).map_err(|e| PublicError::Storage {
            message: e.to_string(),
        })?;
        let home = guard_scratch_home(&temp.path().join("home"))?;
        std::fs::create_dir(&home).map_err(|e| PublicError::Storage {
            message: e.to_string(),
        })?;
        Ok(Self { temp, home })
    }
    pub fn path(&self) -> &Path {
        &self.home
    }
    pub fn root(&self) -> &Path {
        self.temp.path()
    }
    #[allow(dead_code)]
    pub fn validate(path: &Path) -> Result<PathBuf, PublicError> {
        guard_scratch_home(path)
    }
}
impl Drop for ScratchHome {
    fn drop(&mut self) {
        // Only homes something served from can own a unit; skip the systemctl
        // round trips for the many store-level homes that never did.
        if self.home.join("coordinator.sock").exists() || self.home.join("runs").exists() {
            units::stop_home_units(&self.home);
        }
    }
}
