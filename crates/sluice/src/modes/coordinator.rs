use super::{Mode, ModeFuture};
use std::path::PathBuf;
pub fn run(mode: Mode, home: PathBuf) -> ModeFuture {
    Box::pin(async move {
        let Mode::Coordinator { maintenance } = mode else {
            unreachable!()
        };
        sluice_runtime::client::run_home_maintenance(home, false, maintenance).await
    })
}
