//! Fake fn/engine entry point reserved for later acceptance units.
use clap::Parser;
use sluice_model::error::PublicError;
use std::{path::PathBuf, process::ExitCode};

#[derive(Parser)]
struct Fixture {
    #[arg(value_enum)]
    kind: Kind,
}
#[derive(Clone, clap::ValueEnum)]
enum Kind {
    Fn,
    Codex,
    Claude,
    Devin,
}
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
    let _fixture = Fixture::parse();
    eprintln!(
        "{}",
        sluice::error_json(&PublicError::not_implemented("fake fn/engine"))
    );
    ExitCode::FAILURE
}
