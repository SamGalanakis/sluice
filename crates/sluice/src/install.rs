//! Public executable-facing installation API.
pub use sluice_runtime::install::*;

/// The shell-free entry passes this internal prefix before any CLI parsing.
pub fn early_dispatch() -> Result<(), sluice_model::error::PublicError> {
    use std::{os::unix::process::CommandExt, process::Command};
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--installation-entry")) {
        return Ok(());
    }
    let dir = args
        .next()
        .ok_or_else(|| sluice_model::error::PublicError::BadRequest {
            message: "installation entry needs its control directory".into(),
        })?;
    let installation = Installation::at(dir.into())?;
    let status = installation.status()?;
    let selection =
        status
            .selection
            .ok_or_else(|| sluice_model::error::PublicError::BadRequest {
                message: "installation has no selection".into(),
            })?;
    let error = Command::new(selection.release_path.join("bin/sluice"))
        .args(args)
        .env("SLUICE_HOME", selection.home_path)
        .env("SLUICE_INSTALL_DIR", installation.dir)
        .exec();
    Err(sluice_model::error::PublicError::Storage {
        message: error.to_string(),
    })
}
