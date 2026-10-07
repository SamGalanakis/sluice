//! Read-only engine profile diagnostics. Version and capability probes run in a caller-owned
//! private home.
use crate::engines::{
    EngineProfile, claude, codex, devin,
    version::{self, Major, Policy, ProbeReport},
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt;

#[derive(Debug, Serialize)]
pub struct EngineDiagnostic {
    pub engine: String,
    pub executable: Option<PathBuf>,
    pub version: Option<String>,
    /// Whether launches run this version: the policy accepts it and no probe found a
    /// capability missing.
    pub supported: bool,
    /// `tested`, `untested, accepted`, `refused`, `not found` or `unknown` (no version read).
    pub status: String,
    /// The policy in one line (`Policy::summary`).
    pub supported_range: String,
    pub tested: Vec<String>,
    pub floor: String,
    /// Whether a major version newer than every tested one is `accepted` or `refused`.
    pub newer_major: String,
    /// What the capability probes found; `None` when they did not run.
    pub probes: Option<ProbeReport>,
    /// These are profile requirements, not a claim that a session negotiated them.
    pub required_capabilities: Vec<String>,
    /// What an agent fn runs on this engine when its `model` input is left out.
    pub default_model: Option<crate::model::ModelChoice>,
    pub reports_waiting: bool,
    pub private_home: PathBuf,
    pub private_home_status: String,
    pub error: Option<String>,
}

fn executable(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}
fn private_status(path: &Path) -> String {
    match path.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => "error: symlink".into(),
        Ok(m) if !m.is_dir() => "error: not a directory".into(),
        Ok(m) if m.permissions().mode() & 0o077 != 0 => {
            "error: permissions must exclude group and other".into()
        }
        Ok(_) => "private".into(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => "not created".into(),
        Err(_) => "error: cannot inspect".into(),
    }
}
async fn probe(binary: &Path, arg: &str, home: &Path) -> Result<String, String> {
    let mut child = tokio::process::Command::new(binary)
        .arg(arg)
        .env("HOME", home)
        .env("CODEX_HOME", home)
        .env("CLAUDE_CONFIG_DIR", home)
        .env("XDG_CONFIG_HOME", home)
        .env("XDG_DATA_HOME", home)
        .env("XDG_CACHE_HOME", home)
        .env("XDG_STATE_HOME", home)
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("AI_AGENT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "cannot start version probe".to_owned())?;
    let mut stdout = child.stdout.take().ok_or("probe stdout missing")?;
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > 16 * 1024 {
            return Err(io::Error::other("probe output exceeds 16 KiB"));
        }
        if !child.wait().await?.success() {
            return Err(io::Error::other("probe exited unsuccessfully"));
        }
        String::from_utf8(bytes).map_err(io::Error::other)
    })
    .await;
    // Reap even after timeout or bounded-output rejection.
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
        .map_err(|_| "version probe timed out".to_owned())?
        .map(|s| s.trim().to_owned())
        .map_err(|e| e.to_string())
}

/// Reports all engine profiles without reading credentials or starting an agent session.
/// `probe_home` must be a private scratch directory owned by the caller, distinct from
/// the engine homes being inspected. The CLI owns its creation and removal.
pub async fn engine_diagnostics(
    home: &Path,
    probe_home: &Path,
) -> io::Result<Vec<EngineDiagnostic>> {
    let home = sluice_process::host::resolve_path(home).map_err(io::Error::other)?;
    let probe_home =
        sluice_process::host::guard_scratch_home(probe_home).map_err(io::Error::other)?;
    if private_status(&probe_home) != "private" || home == probe_home {
        return Err(io::Error::other(
            "doctor requires a separate private probe home",
        ));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut result = Vec::new();
    for profile in [
        codex::profile::profile(),
        claude::profile::profile(),
        devin::profile::profile(),
    ] {
        result.push(diagnose(profile, &path, &home, &probe_home).await);
    }
    Ok(result)
}
fn policy(engine: &str) -> Policy {
    match engine {
        "codex" => codex::profile::POLICY,
        "claude" => claude::profile::POLICY,
        _ => devin::profile::POLICY,
    }
}
/// The probe environment: this process's, with HOME and the engines' config dirs pointed at
/// `home` and parent-session markers removed.
fn probe_env(home: &Path) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    for name in [
        "HOME",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_STATE_HOME",
    ] {
        env.insert(name.into(), home.to_string_lossy().into_owned());
    }
    for name in ["CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "AI_AGENT"] {
        env.remove(name);
    }
    env.insert("DISABLE_AUTOUPDATER".into(), "1".into());
    env
}
async fn diagnose(
    profile: EngineProfile,
    path: &std::ffi::OsStr,
    home: &Path,
    probe_home: &Path,
) -> EngineDiagnostic {
    let binary = executable(&profile.engine, path);
    let private_home = home.join(match profile.engine.as_str() {
        "codex" => "codex-native-homes",
        "claude" => "claude",
        _ => "devin",
    });
    let policy = policy(&profile.engine);
    let mut report = EngineDiagnostic {
        executable: binary.clone(),
        version: None,
        supported: false,
        status: "not found".into(),
        private_home_status: private_status(&private_home),
        private_home,
        supported_range: profile.version_range,
        tested: policy.tested.iter().map(|v| (*v).into()).collect(),
        floor: policy.floor.into(),
        newer_major: match policy.newer_major {
            Major::Accept => "accepted".into(),
            Major::Refuse => "refused".into(),
        },
        probes: None,
        required_capabilities: profile.required_capabilities,
        default_model: profile.default_model,
        reports_waiting: profile.reports_waiting,
        engine: profile.engine,
        error: None,
    };
    let Some(binary) = binary else {
        report.error = Some("executable not found on PATH".into());
        return report;
    };
    report.status = "unknown".into();
    let version = match probe(&binary, "--version", probe_home).await {
        Ok(v) => v,
        Err(e) => {
            report.error = Some(e);
            return report;
        }
    };
    let verdict = match report.engine.as_str() {
        "codex" => codex::profile::check_version(&version),
        "claude" => claude::profile::validate_version(&version),
        _ => devin::profile::validate_version(&version),
    };
    report.version = Some(version);
    let verdict = match verdict {
        Ok(verdict) => verdict,
        Err(e) => {
            report.status = "refused".into();
            report.error = Some(e.to_string());
            return report;
        }
    };
    let env = probe_env(probe_home);
    let probes = match report.engine.as_str() {
        "codex" => codex::profile::probe(&binary, &env).await,
        "claude" => claude::profile::probe(version::run(&binary, &["--help"], &env).await),
        _ => devin::profile::probe(version::run(&binary, &["--help"], &env).await),
    };
    match probes.require(&verdict) {
        Ok(()) => {
            report.supported = true;
            report.status = if verdict.tested() {
                "tested".into()
            } else {
                "untested, accepted".into()
            };
        }
        Err(e) => {
            report.status = "refused".into();
            report.error = Some(e.to_string());
        }
    }
    report.probes = Some(probes);
    report
}
