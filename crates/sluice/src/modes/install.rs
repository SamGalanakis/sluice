use super::{Mode, ModeFuture};
use crate::{cli::InstallCommand, install::Installation};
use sluice_model::error::PublicError;
use std::path::{Path, PathBuf};
/// The release check `select` and `unfence` run: a release with a manifest must verify (a
/// rollback artifact without one is taken as it is).
fn check(release: &Path) -> Result<(), PublicError> {
    if release.join("manifest.json").is_file() {
        crate::release::verify(release)?;
    }
    Ok(())
}
fn json(value: impl serde::Serialize) -> Result<serde_json::Value, PublicError> {
    serde_json::to_value(value).map_err(|e| PublicError::Storage {
        message: e.to_string(),
    })
}
pub fn run(mode: Mode, _home: PathBuf) -> ModeFuture {
    Box::pin(async move {
        let Mode::Install { command } = mode else {
            unreachable!()
        };
        let result = tokio::task::spawn_blocking(move || {
            let installation = Installation::configured;
            match command {
                // `verify` reads only the release: it needs no installation and changes
                // nothing. Its check is `release::verify`, the one `select` runs, so a deploy
                // can refuse a release before it fences and stops anything.
                InstallCommand::Verify { release_dir } => {
                    let manifest = crate::release::verify(&release_dir)?;
                    Ok(serde_json::json!({
                        "release": std::fs::canonicalize(&release_dir).map_err(|e| {
                            PublicError::Storage { message: e.to_string() }
                        })?,
                        "release_id": manifest.release_id,
                        "git_sha": manifest.git_sha,
                        "schema": manifest.schema,
                    }))
                }
                InstallCommand::Fence { reason } => json(installation()?.fence(reason)?),
                InstallCommand::Unfence => json(installation()?.unfence_checked(check)?),
                InstallCommand::Select { release_dir, home } => {
                    let installation = installation()?;
                    check(&release_dir)?;
                    json(installation.select(&release_dir, &home)?)
                }
                InstallCommand::Status => json(installation()?.status()?),
            }
        })
        .await
        .map_err(|e| PublicError::Storage {
            message: e.to_string(),
        })??;
        println!("{}", serde_json::to_string_pretty(&result).expect("status"));
        Ok(())
    })
}
