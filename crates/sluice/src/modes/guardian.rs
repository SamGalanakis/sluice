use super::{Mode, ModeFuture};
use std::path::PathBuf;
pub fn run(mode: Mode, home: PathBuf) -> ModeFuture {
    Box::pin(async move {
        let Mode::Guardian(args) = mode else {
            unreachable!()
        };
        sluice_runtime::execution::guardian_entry(home, args.run, args.attempt, args.socket).await
    })
}
