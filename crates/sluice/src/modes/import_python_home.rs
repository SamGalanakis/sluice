use super::{Mode, ModeFuture};
use std::path::PathBuf;
#[path = "../import_python_home.rs"]
mod implementation;
pub use implementation::dispatch_import_python_home as early_dispatch;
pub fn run(mode: Mode, _home: PathBuf) -> ModeFuture {
    Box::pin(async move {
        if let Mode::ImportPythonHome { src, dst } = &mode {
            sluice_process::host::guard_scratch_home(src)?;
            sluice_process::host::guard_scratch_home(dst)?;
        }
        Err(sluice_model::error::PublicError::not_implemented(
            mode.name(),
        ))
    })
}
