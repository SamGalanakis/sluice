//! `sluice doctor` (SPEC §9): host prerequisites, engine diagnostics, and the
//! installation and pinned release checks, without broker activation. The
//! report gates `serve`/`loop` on this machine — nothing degrades silently.
use crate::modes::ModeFuture;
use serde_json::{Value, json};
use sluice_model::error::PublicError;
use sluice_process::host::HostCheck;
use std::path::PathBuf;

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

/// Every registered engine's declared profile. TODO(p5-05): fold in
/// `sluice_agents::engine_diagnostics(home, probe_home)` — observed installs and
/// versions — once it lands on rust; it was not on this tip.
fn engines() -> Vec<Value> {
    [
        sluice_agents::engines::claude::profile::profile(),
        sluice_agents::engines::codex::profile::profile(),
        sluice_agents::engines::devin::profile::profile(),
    ]
    .iter()
    .map(|p| {
        json!({
            "engine": p.engine,
            "version_range": p.version_range,
            "required_capabilities": p.required_capabilities,
            "models": p.models,
            "efforts": p.efforts,
            "reports_waiting": p.reports_waiting,
            "observed": Value::Null,
        })
    })
    .collect()
}

pub fn run(mode: crate::cli::Mode, _home: PathBuf) -> ModeFuture {
    Box::pin(async move {
        let crate::cli::Mode::Doctor { json } = mode else {
            return Err(PublicError::not_implemented(mode.name()));
        };
        let report = HostCheck::new(tmux_prefix()).run().await;
        let engines = engines();
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
            for engine in &engines {
                println!(
                    "engine {:<18} {} (unprobed — engine_diagnostics pending p5-05)",
                    engine["engine"].as_str().unwrap_or("?"),
                    engine["version_range"].as_str().unwrap_or("?"),
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
