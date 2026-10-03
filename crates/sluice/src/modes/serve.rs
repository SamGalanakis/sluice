use super::{Mode, ModeFuture};
use std::path::PathBuf;
pub fn run(mode: Mode, home: PathBuf) -> ModeFuture {
    Box::pin(async move {
        let Mode::Serve { no_runner, .. } = mode else {
            unreachable!()
        };
        sluice_runtime::client::run_home(home, !no_runner).await
    })
}
