use clap::Parser;
use sluice::cli::{Cli, Mode};
use sluice_model::{RuntimeApi, error::PublicError, rpc::decode_json};
use sluice_process::host::{OWNER_HOME, guard_scratch_home};
use std::{path::PathBuf, process::ExitCode};
mod import_python_home;

fn dispatch() -> Result<(), PublicError> {
    if let Some(result) = import_python_home::dispatch_import_python_home() {
        return result;
    }
    let home = std::env::var_os("SLUICE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(OWNER_HOME));
    guard_scratch_home(&home)?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().and_then(|s| s.to_str()) == Some("payload-exec")
        && args
            .get(1)
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.len() == 64)
    {
        sluice_process::launcher::payload_exec_main(args[1..].to_vec(), |args| {
            if args.first().and_then(|s| s.to_str()) != Some("runtime") {
                return Err(std::io::Error::other("unknown payload dispatcher"));
            }
            let runtime = sluice_runtime::coordinator::executor()?;
            runtime.block_on(sluice_runtime::execution::payload_entry(home))
        });
    }
    let cli = Cli::parse();
    if let Mode::ImportPythonHome { src, dst } = &cli.mode {
        guard_scratch_home(src)?;
        guard_scratch_home(dst)?;
    }
    let runtime = sluice_runtime::coordinator::executor().map_err(|e| PublicError::Storage {
        message: e.to_string(),
    })?;
    runtime.block_on(async move {
        match cli.mode {
            Mode::Coordinator { .. } => sluice_runtime::client::run_home(home, false).await,
            Mode::Serve { no_runner, .. } => {
                sluice_runtime::client::run_home(home, !no_runner).await
            }
            Mode::Loop => {
                let client = sluice_runtime::client::ensure_coordinator(
                    &home,
                    &std::env::current_exe().map_err(|e| PublicError::Storage {
                        message: e.to_string(),
                    })?,
                )
                .await?;
                let _lease = client.acquire_scheduler().await?;
                sluice_runtime::client::wait_for_signal().await
            }
            Mode::Guardian(args) => {
                sluice_runtime::execution::guardian_entry(home, args.run, args.attempt, args.socket)
                    .await
            }
            Mode::Tool { name, json } => {
                let value = json.as_deref().unwrap_or("{}");
                let request = if name == "rpc" {
                    decode_json(value.as_bytes())?
                } else {
                    let args: sluice_model::rpc::JsonValue = decode_json(value.as_bytes())?;
                    let mut object = serde_json::json!({"command":name});
                    if args.as_value() != &serde_json::json!({}) {
                        object["args"] = args.into_value();
                    }
                    decode_json(&serde_json::to_vec(&object).expect("JSON"))?
                };
                let client = sluice_runtime::client::ensure_coordinator(
                    &home,
                    &std::env::current_exe().map_err(|e| PublicError::Storage {
                        message: e.to_string(),
                    })?,
                )
                .await?;
                println!(
                    "{}",
                    serde_json::to_string(&client.command(request).await?).expect("reply")
                );
                Ok(())
            }
            mode => Err(PublicError::not_implemented(mode.name())),
        }
    })
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
