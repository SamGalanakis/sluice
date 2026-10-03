//! Executable mode registration shared with the foundation checks.
use crate::cli::Mode;
use sluice_model::error::PublicError;
use std::{future::Future, path::PathBuf, pin::Pin};
pub mod payload_exec;
pub type ModeFuture = Pin<Box<dyn Future<Output = Result<(), PublicError>> + Send>>;
pub struct Registration {
    pub name: &'static str,
    pub run: fn(Mode, PathBuf) -> ModeFuture,
}
macro_rules! register_modes {
    ($($name:literal => $module:ident),* $(,)?) => {
        $(pub mod $module;)*
        pub const MODES: &[Registration] = &[$(Registration { name: $name, run: $module::run }),*];
    };
}
register_modes! {
    "coordinator" => coordinator,
    "serve" => serve,
    "loop" => scheduler,
    "guardian" => guardian,
    "tool" => tool,
    "agent hook" => agent,
    "import-python-home" => import_python_home,
}
pub struct PendingMode {
    pub name: &'static str,
    pub args: &'static [&'static str],
}
const ID: &str = "019a2b3c-4d5e-7f01-8234-56789abcdef0";
pub const PENDING_MODES: &[PendingMode] = &[
    PendingMode {
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
    },
    PendingMode {
        name: "me",
        args: &["me", "--json"],
    },
    PendingMode {
        name: "doctor",
        args: &["doctor", "--json"],
    },
];
pub fn run(mode: Mode, home: PathBuf) -> ModeFuture {
    if let Some(entry) = MODES.iter().find(|entry| entry.name == mode.name()) {
        return (entry.run)(mode, home);
    }
    Box::pin(async move { Err(PublicError::not_implemented(mode.name())) })
}
