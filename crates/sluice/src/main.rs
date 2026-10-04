use clap::Parser;
use sluice::{cli::Cli, modes};
use sluice_model::error::PublicError;
use sluice_process::host::{OWNER_HOME, guard_scratch_home};
use std::{path::PathBuf, process::ExitCode};

fn dispatch() -> Result<(), PublicError> {
    sluice::install::early_dispatch()?;
    sluice::release::early_dispatch()?;
    if let Some(result) = modes::import_python_home::early_dispatch() {
        return result;
    }
    let home = std::env::var_os("SLUICE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(OWNER_HOME));
    guard_scratch_home(&home)?;
    modes::payload_exec::early_dispatch(home.clone());
    modes::agent::early_dispatch()?;
    let cli = Cli::parse();
    modes::agent::structured(&cli.mode)?;
    let runtime = sluice_runtime::coordinator::executor().map_err(|e| PublicError::Storage {
        message: e.to_string(),
    })?;
    runtime.block_on(modes::run(cli.mode, home))
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
