//! `sluice home`: the home's storage as this release sees it.
use super::{Mode, ModeFuture};
use crate::cli::HomeCommand;
use serde_json::Value;
use sluice_model::error::PublicError;
use std::{
    fs::{File, OpenOptions, TryLockError},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

pub fn run(mode: Mode, home: PathBuf) -> ModeFuture {
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
            HomeCommand::Migrate { dry_run, json } => {
                let report = tokio::task::spawn_blocking(move || migrate(&home, dry_run))
                    .await
                    .map_err(storage)??;
                if json {
                    crate::cli::out(format_args!(
                        "{}",
                        serde_json::to_string_pretty(&report).map_err(storage)?
                    ));
                } else {
                    for line in summary(&report, dry_run) {
                        crate::cli::out(format_args!("{line}"));
                    }
                }
                Ok(())
            }
        }
    })
}

fn storage(error: impl std::fmt::Display) -> PublicError {
    PublicError::Storage {
        message: error.to_string(),
    }
}

/// Converts the home's database with the store's converter (`sluice_store::convert`), holding
/// `coordinator.lock` so no coordinator can open the home meanwhile; a dry run converts a
/// backup-API copy in a scratch directory instead and never touches the home.
fn migrate(home: &Path, dry_run: bool) -> Result<Value, PublicError> {
    let database = home.join(sluice_store::schema::DATABASE_FILE);
    if !database.is_file() {
        return Err(PublicError::BadRequest {
            message: format!("{} has no database to migrate", home.display()),
        });
    }
    let report = if dry_run {
        let scratch = Scratch::new()?;
        let copy = scratch.0.join(sluice_store::schema::DATABASE_FILE);
        let source = rusqlite::Connection::open_with_flags(
            &database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(storage)?;
        source
            .backup(rusqlite::MAIN_DB, &copy, None)
            .map_err(storage)?;
        drop(source);
        sluice_store::convert::convert_home(&copy).map_err(|e| e.into_public(false))?
    } else {
        let _lock = writer_lock(home)?;
        sluice_store::convert::convert_home(&database).map_err(|e| e.into_public(false))?
    };
    Ok(report.to_json())
}

/// A private scratch directory for a dry run's copy, removed with it.
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self, PublicError> {
        let path = std::env::temp_dir().join(format!(
            "sluice-migrate-{}",
            sluice_model::ids::InvocationId::new()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(storage)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The home's writer lock, held for the conversion: a running coordinator holds it.
fn writer_lock(home: &Path) -> Result<File, PublicError> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("coordinator.lock"))
        .map_err(storage)?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => Err(PublicError::Busy {
            message: format!(
                "{} is open: its coordinator holds coordinator.lock; stop it before migrating",
                home.display()
            ),
            retryable: false,
        }),
        Err(TryLockError::Error(error)) => Err(storage(error)),
    }
}

/// One line for the conversion, then one per anchored project and one per warning.
fn summary(report: &Value, dry_run: bool) -> Vec<String> {
    let projects = report["projects"].as_array().cloned().unwrap_or_default();
    let revisions: u64 = projects
        .iter()
        .filter_map(|p| p["revisions"].as_u64())
        .sum();
    let warnings = report["warnings"].as_array().cloned().unwrap_or_default();
    let mut lines = vec![format!(
        "{} schema {} to {}: {} projects, {} revisions, {} anchored, {} warnings",
        if dry_run {
            "would convert"
        } else {
            "converted"
        },
        report["from_schema"],
        sluice_store::schema::SCHEMA_VERSION,
        projects.len(),
        revisions,
        report["anchored"].as_array().map_or(0, Vec::len),
        warnings.len()
    )];
    let anchored = report["anchored"].as_array().cloned().unwrap_or_default();
    lines.extend(anchored.iter().map(|a| {
        format!(
            "anchored: {} at rev {} from its {} ({} folded edits)",
            a["name"].as_str().unwrap_or_default(),
            a["rev"],
            if a["source"] == "snapshot" {
                "completion snapshot"
            } else {
                "stored plan"
            },
            a["folded_edits"]
        )
    }));
    lines.extend(warnings.iter().map(|w| match w {
        Value::String(text) => format!("warning: {text}"),
        other => format!("warning: {other}"),
    }));
    lines
}
