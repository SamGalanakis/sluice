use super::{Mode, ModeFuture};
use std::path::PathBuf;
pub fn run(mode: Mode, home: PathBuf) -> ModeFuture {
    Box::pin(async move {
        let Mode::Serve {
            no_runner,
            host,
            port,
        } = mode
        else {
            unreachable!()
        };
        sluice_web::http::serve(home, host, port, no_runner).await
    })
}
