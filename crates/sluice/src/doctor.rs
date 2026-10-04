//! `sluice doctor` (SPEC §9): host prerequisites, engine probes, and the
//! installation and pinned release checks, without broker activation. The
//! report gates `serve`/`loop` on this machine — nothing degrades silently.
use crate::modes::ModeFuture;
use serde_json::Value;
use sluice_model::error::PublicError;
use sluice_process::host::HostCheck;
use std::{
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

macro_rules! println {
    ($($arg:tt)*) => {
        crate::cli::out(::std::format_args!($($arg)*))
    };
}

/// The private tmux build the engines run under: an explicit prefix, else the
/// release layout (the artifact beside the binary, or one level up as under
/// target/). p7-02 owns where the release places it.
fn tmux_prefix() -> PathBuf {
    if let Some(prefix) = std::env::var_os("SLUICE_TMUX_PREFIX") {
        return PathBuf::from(prefix);
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("sluice"));
    let dir = exe.parent().map(PathBuf::from).unwrap_or_default();
    for candidate in [dir.join("private-tmux"), dir.join("../private-tmux")] {
        if candidate.join("bin").join("tmux").exists() {
            return candidate;
        }
    }
    dir.join("private-tmux")
}

fn storage(error: impl std::fmt::Display) -> PublicError {
    PublicError::Storage {
        message: error.to_string(),
    }
}

/// Each engine's executable on PATH and its `--version` (devin's `--help` too), checked
/// against its profile. The probes run with HOME and the engines' config and XDG dirs pointed
/// at a private scratch directory removed afterwards: no session starts and no credential is
/// read.
async fn engines(home: &Path) -> Result<Vec<Value>, PublicError> {
    let probe = std::env::temp_dir().join(format!(
        "sluice-doctor-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&probe)
        .map_err(storage)?;
    let report = sluice_agents::doctor::engine_diagnostics(home, &probe).await;
    let _ = std::fs::remove_dir_all(&probe);
    report
        .map_err(storage)?
        .iter()
        .map(|d| serde_json::to_value(d).map_err(storage))
        .collect()
}

pub fn run(mode: crate::cli::Mode, home: PathBuf) -> ModeFuture {
    Box::pin(async move {
        let crate::cli::Mode::Doctor { json } = mode else {
            return Err(PublicError::not_implemented(mode.name()));
        };
        let report = HostCheck::new(tmux_prefix()).run().await;
        let engines = engines(&home).await?;
        let release = crate::release::doctor()?;
        if json {
            let mut value = serde_json::to_value(&report).map_err(storage)?;
            value["engines"] = Value::Array(engines);
            for key in ["installation", "manifest", "manifest_ok", "error"] {
                value[key] = release[key].clone();
            }
            println!("{}", serde_json::to_string_pretty(&value).map_err(storage)?);
        } else {
            for check in &report.checks {
                println!(
                    "{} {:<24} {}",
                    if check.passed { "ok " } else { "FAIL" },
                    format!("{:?}", check.prerequisite),
                    check.message,
                );
            }
            // An engine that is missing or unsupported is reported, not a readiness failure:
            // only the steps that use it need it.
            for engine in &engines {
                let name = format!("engine {}", engine["engine"].as_str().unwrap_or("?"));
                let version = engine["version"].as_str().unwrap_or("no version");
                let detail = match (engine["executable"].as_str(), engine["error"].as_str()) {
                    (None, _) => "not found on PATH".to_owned(),
                    (Some(path), None) => format!("{version} ({path})"),
                    (Some(path), Some(error)) => format!("{version} ({path}): {error}"),
                };
                println!(
                    "{} {:<24} {} (supported: {})",
                    if engine["supported"] == true {
                        "ok "
                    } else {
                        "warn"
                    },
                    name,
                    detail,
                    engine["supported_range"].as_str().unwrap_or("?"),
                );
            }
            println!(
                "{} {:<24} {}",
                if release["manifest_ok"] == true {
                    "ok "
                } else {
                    "FAIL"
                },
                "Release",
                release["error"]["message"]
                    .as_str()
                    .unwrap_or("selected release manifest verified"),
            );
            println!(
                "{}",
                if report.ready {
                    "ready"
                } else {
                    "not ready — fix the FAIL checks"
                }
            );
        }
        if release["manifest_ok"] == false {
            return Err(PublicError::Invalid {
                message: "selected release manifest verification failed".into(),
                errors: vec![],
            });
        }
        Ok(())
    })
}
