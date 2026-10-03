//! Scratch-only fixture entry point. Per-mode handlers live in fixture/.
use sluice_model::error::PublicError;
use std::{path::PathBuf, process::ExitCode};
#[path = "fixture/mod.rs"]
mod modes;
fn main() -> ExitCode {
    let result = std::env::var_os("SLUICE_HOME")
        .ok_or_else(|| PublicError::BadRequest {
            message: "fixture requires scratch SLUICE_HOME".into(),
        })
        .and_then(|home| sluice_process::host::guard_scratch_home(&PathBuf::from(home)));
    if let Err(error) = result {
        eprintln!("{}", sluice::error_json(&error));
        return ExitCode::FAILURE;
    }
    modes::run()
}
