use super::{Mode, ModeFuture};
use sluice_runtime::config::{DEFAULT_HOST, DEFAULT_PORT, HomeConfig};
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
        // The command line wins over config.json's http, which wins over the defaults.
        let config = HomeConfig::load(&home);
        let host = host
            .or(config.host)
            .unwrap_or_else(|| DEFAULT_HOST.to_owned());
        let port = port.or(config.port).unwrap_or(DEFAULT_PORT);
        tracing::info!(%host, port, runner = !no_runner, "serving the dashboard");
        sluice_web::http::serve(home, host, port, no_runner).await
    })
}
