use super::{Mode, ModeFuture};
use std::path::PathBuf;
pub fn run(mode: Mode, home: PathBuf) -> ModeFuture {
    Box::pin(async move {
        let _ = mode;
        sluice_runtime::client::run_home(home, false).await
    })
}
