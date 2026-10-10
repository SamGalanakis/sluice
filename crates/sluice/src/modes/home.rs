//! `sluice home`: the home's storage as this release sees it.
use super::{Mode, ModeFuture};
use crate::cli::HomeCommand;
use std::path::PathBuf;

pub fn run(mode: Mode, _home: PathBuf) -> ModeFuture {
    Box::pin(async move {
        let Mode::Home { command } = mode else {
            unreachable!()
        };
        match command {
            HomeCommand::Schema => {
                crate::cli::out(format_args!(
                    "{}",
                    serde_json::json!({"schema": sluice_store::schema::SCHEMA_VERSION})
                ));
                Ok(())
            }
        }
    })
}
