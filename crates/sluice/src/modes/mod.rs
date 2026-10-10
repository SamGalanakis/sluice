//! Executable mode registration shared with the foundation checks.
use crate::cli::Mode;
use sluice_model::error::PublicError;
use std::{future::Future, path::PathBuf, pin::Pin};
pub mod agent;
pub mod coordinator;
pub mod guardian;
pub mod home;
pub mod install;
pub mod mcp;
pub mod payload_exec;
pub mod scheduler;
pub mod serve;
pub type ModeFuture = Pin<Box<dyn Future<Output = Result<(), PublicError>> + Send>>;
pub struct Registration {
    pub name: &'static str,
    pub run: fn(Mode, PathBuf) -> ModeFuture,
}
/// One line per mode: the registered name and the function that runs it. New
/// modes add their line here; nothing else touches this file.
macro_rules! register_modes {
    ($($name:literal => $run:path),* $(,)?) => {
        pub const MODES: &[Registration] = &[$(Registration { name: $name, run: $run }),*];
    };
}
register_modes! {
    "install" => install::run,
    "home" => home::run,
    "coordinator" => coordinator::run,
    "serve" => serve::run,
    "loop" => scheduler::run,
    "guardian" => guardian::run,
    "agent hook" => agent::run,
    // p6-01's public commands live in the crate modules that own them.
    "tool" => crate::cli::run,
    "next" => crate::cli::run,
    "watch" => crate::cli::run,
    "drain" => crate::cli::run,
    "query" => crate::cli::run,
    "backup" => crate::cli::run,
    "docs" => crate::cli::run,
    "me" => crate::me::run,
    "doctor" => crate::doctor::run,
    "mcp" => mcp::run,
}
pub struct PendingMode {
    pub name: &'static str,
    pub args: &'static [&'static str],
}
const ID: &str = "019a2b3c-4d5e-7f01-8234-56789abcdef0";
pub const PENDING_MODES: &[PendingMode] = &[PendingMode {
    name: "payload-exec",
    args: &[
        "payload-exec",
        "--run",
        ID,
        "--attempt",
        ID,
        "--socket",
        "control.sock",
    ],
}];
pub fn run(mode: Mode, home: PathBuf) -> ModeFuture {
    if let Some(entry) = MODES.iter().find(|entry| entry.name == mode.name()) {
        return (entry.run)(mode, home);
    }
    Box::pin(async move { Err(PublicError::not_implemented(mode.name())) })
}
