use clap::Parser;
use sluice::{
    cli::{Cli, Mode},
    modes,
};
use sluice_model::error::PublicError;
use sluice_process::host::guard_scratch_home;
use std::{path::PathBuf, process::ExitCode};

fn dispatch() -> Result<(), PublicError> {
    sluice::install::early_dispatch()?;
    sluice::release::early_dispatch()?;
    // The home is SLUICE_HOME, else the one the installation selects.
    let home = match std::env::var_os("SLUICE_HOME") {
        Some(home) => Some(PathBuf::from(home)),
        None => sluice::install::Installation::configured()?.selected_home()?,
    };
    if let Some(home) = &home {
        guard_scratch_home(home)?;
        modes::payload_exec::early_dispatch(home.clone());
    }
    modes::agent::early_dispatch()?;
    let cli = Cli::parse();
    modes::agent::structured(&cli.mode)?;
    let home = match home {
        Some(home) => home,
        // Installation control never touches a home, and selects the first one.
        None if matches!(cli.mode, Mode::Install { .. }) => PathBuf::new(),
        None => {
            return Err(PublicError::BadRequest {
                message: "no Sluice home: set SLUICE_HOME, or select one with `sluice install \
                          select <release> <home>` (the installation is SLUICE_INSTALL_DIR, \
                          default ~/.local/share/sluice/install)"
                    .into(),
            });
        }
    };
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
