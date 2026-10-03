use clap::Parser;
use sluice::cli::{Cli, Mode};
use sluice_model::error::PublicError;
use sluice_process::host::{OWNER_HOME, guard_scratch_home};
use std::{path::PathBuf, process::ExitCode};

fn dispatch() -> Result<(), PublicError> {
    let home = std::env::var_os("SLUICE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(OWNER_HOME));
    guard_scratch_home(&home)?;
    let cli = Cli::parse();
    if let Mode::ImportPythonHome { src, dst } = &cli.mode {
        guard_scratch_home(src)?;
        guard_scratch_home(dst)?;
    }
    Err(PublicError::not_implemented(cli.mode.name()))
}
fn main() -> ExitCode {
    match dispatch() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", sluice::error_json(&error));
            ExitCode::FAILURE
        }
    }
}
