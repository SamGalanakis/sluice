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
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(std::ffi::OsStr::new("payload-exec")) {
        sluice_process::launcher::payload_exec_main(args.collect(), |args| {
            match args.first().and_then(|s| s.to_str()) {
                Some("write-marker") if args.len() == 2 => {
                    std::fs::write(&args[1], b"dispatched")?;
                    Ok(0)
                }
                Some("exec") if args.len() >= 2 => {
                    let mut command = std::process::Command::new(&args[1]);
                    command.args(&args[2..]).stdin(std::process::Stdio::null());
                    sluice_process::launcher::exec_payload(&mut command)
                }
                _ => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "unknown fixture payload",
                )),
            }
        });
    }
    let _fixture = Fixture::parse();
    eprintln!(
        "{}",
        sluice::error_json(&PublicError::not_implemented("fake fn/engine"))
    );
    ExitCode::FAILURE
}
