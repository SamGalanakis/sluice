use super::{Mode, ModeFuture};
use sluice_model::error::PublicError;
use std::path::PathBuf;
pub fn run(mode: Mode, home: PathBuf) -> ModeFuture {
    Box::pin(async move {
        let _ = mode;
        let client = sluice_runtime::client::ensure_coordinator(
            &home,
            &std::env::current_exe().map_err(|e| PublicError::Storage {
                message: e.to_string(),
            })?,
        )
        .await?;
        let _lease = client.acquire_scheduler().await?;
        tracing::info!("holding the scheduler lease");
        sluice_runtime::client::wait_for_signal().await
    })
}
