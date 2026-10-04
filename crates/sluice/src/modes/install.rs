use super::{Mode, ModeFuture};
use crate::{cli::InstallCommand, install::Installation};
use std::path::PathBuf;
pub fn run(mode: Mode, _home: PathBuf) -> ModeFuture {
    Box::pin(async move {
        let Mode::Install { command } = mode else {
            unreachable!()
        };
        if matches!(command, InstallCommand::Unfence) {
            let install = Installation::configured()?;
            let result = sluice_runtime::install::release_cutover_checked(install, |release| {
                if release.join("manifest.json").is_file() {
                    crate::release::verify(release)?;
                }
                Ok(())
            })
            .await?;
            println!("{}", serde_json::to_string_pretty(&result).expect("status"));
            return Ok(());
        }
        let result = tokio::task::spawn_blocking(move || {
            let install = Installation::configured()?;
            match command {
                InstallCommand::Fence { reason } => install.fence(reason),
                InstallCommand::Unfence => install.unfence(),
                InstallCommand::Select { release_dir, home } => {
                    if release_dir.join("manifest.json").is_file() {
                        crate::release::verify(&release_dir)?;
                    }
                    install.select(&release_dir, &home)
                }
                InstallCommand::Status => install.status(),
            }
        })
        .await
        .map_err(|e| sluice_model::error::PublicError::Storage {
            message: e.to_string(),
        })??;
        println!("{}", serde_json::to_string_pretty(&result).expect("status"));
        Ok(())
    })
}
