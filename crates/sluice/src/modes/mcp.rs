//! `sluice mcp`: the MCP server over stdio, with the same tools as `serve`'s `/mcp`.
use super::{Mode, ModeFuture};
use std::{path::PathBuf, sync::Arc};
pub fn run(_mode: Mode, home: PathBuf) -> ModeFuture {
    Box::pin(async move {
        crate::cli::ensure_home(&home)?;
        let program =
            std::env::current_exe().map_err(|e| sluice_model::error::PublicError::Storage {
                message: e.to_string(),
            })?;
        let client = sluice_runtime::client::ensure_coordinator(&home, &program).await?;
        sluice_web::mcp::serve_stdio(sluice_web::mcp::McpServer::new(Arc::new(client))).await
    })
}
