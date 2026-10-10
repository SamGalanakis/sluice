//! Pinned release manifest verification and release-local execution paths.
use serde::{Deserialize, Serialize};
use sluice_model::error::PublicError;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::process::CommandExt,
    path::{Component, Path},
};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub release_id: String,
    pub git_sha: String,
    pub guardian_protocol_major: u16,
    pub guardian_protocol_minor: u16,
    pub files: BTreeMap<String, String>,
    pub private_tmux: sluice_process::tmux::TmuxManifest,
    pub build_toolchain: BTreeMap<String, String>,
    /// The database schema the release's `sluice` reads and writes (`sluice home schema`),
    /// recorded by `scripts/build-release`. A release whose schema differs from the home's
    /// goes out only through `scripts/deploy --schema-cutover`. Releases built before the
    /// field existed are schema 1.
    #[serde(default = "schema_one")]
    pub schema: u32,
}
fn schema_one() -> u32 {
    1
}
fn invalid(message: impl Into<String>) -> PublicError {
    PublicError::Invalid {
        message: message.into(),
        errors: vec![],
    }
}
pub fn verify(root: &Path) -> Result<Manifest, PublicError> {
    let root = fs::canonicalize(root).map_err(|e| invalid(e.to_string()))?;
    let bytes = fs::read(root.join("manifest.json")).map_err(|e| invalid(e.to_string()))?;
    let manifest: Manifest = sluice_model::rpc::decode_json(&bytes)?;
    for required in [
        "bin/sluice",
        "python/sluice_fn/__init__.py",
        "python/inline_python.py",
        "tmux/bin/tmux",
        "tmux/tmux-manifest.json",
    ] {
        if !manifest.files.contains_key(required) {
            return Err(invalid(format!("manifest omits {required}")));
        }
    }
    if manifest.guardian_protocol_major != sluice_model::rpc::PROTOCOL_VERSION {
        return Err(invalid("guardian protocol major mismatch"));
    }
    if manifest.private_tmux.source_sha256 != sluice_process::tmux::SOURCE_SHA256
        || !["--disable-systemd", "--disable-cgroups"]
            .iter()
            .all(|flag| {
                manifest
                    .private_tmux
                    .configure_flags
                    .iter()
                    .any(|f| f == flag)
            })
        || manifest
            .private_tmux
            .generated_config_lines
            .iter()
            .any(|s| s.contains("HAVE_SYSTEMD") || s.contains("ENABLE_CGROUPS"))
    {
        return Err(invalid("unapproved private tmux manifest"));
    }
    let tmux: sluice_process::tmux::TmuxManifest = sluice_model::rpc::decode_json(
        &fs::read(root.join("tmux/tmux-manifest.json")).map_err(|e| invalid(e.to_string()))?,
    )?;
    if serde_json::to_value(&tmux).map_err(|e| invalid(e.to_string()))?
        != serde_json::to_value(&manifest.private_tmux).map_err(|e| invalid(e.to_string()))?
    {
        return Err(invalid("private tmux manifests disagree"));
    }
    for (name, expected) in &manifest.files {
        if Path::new(name)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(invalid("manifest path is not relative"));
        }
        let path =
            fs::canonicalize(root.join(name)).map_err(|e| invalid(format!("{name}: {e}")))?;
        if !path.starts_with(&root) {
            return Err(invalid(format!("{name} escapes release")));
        }
        let digest = sluice_store::artifacts::fingerprint(
            &fs::read(path).map_err(|e| invalid(e.to_string()))?,
        );
        if &digest != expected {
            return Err(invalid(format!("digest mismatch: {name}")));
        }
    }
    if manifest.files["tmux/bin/tmux"] != manifest.private_tmux.binary_sha256 {
        return Err(invalid("private tmux binary digest mismatch"));
    }
    let material = serde_json::to_vec(&manifest.files).map_err(|e| invalid(e.to_string()))?;
    let expected = format!(
        "{}-{}",
        manifest.git_sha,
        sluice_store::artifacts::fingerprint(&material)
    );
    if manifest.release_id != expected {
        return Err(invalid("release content id mismatch"));
    }
    Ok(manifest)
}
/// Called before threads exist. Re-exec sets the pinned paths without mutating a
/// multithreaded process environment. Guardians derive paths from their own binary.
pub fn early_dispatch() -> Result<(), PublicError> {
    let Some(root) = crate::install::current_release() else {
        return Ok(());
    };
    let python = root.join("python");
    let tmux = root.join("tmux");
    if std::env::var_os("SLUICE_PYTHON_DIR").as_deref() == Some(python.as_os_str())
        && std::env::var_os("SLUICE_TMUX_PREFIX").as_deref() == Some(tmux.as_os_str())
        && std::env::var_os("PYTHONDONTWRITEBYTECODE").as_deref() == Some(std::ffi::OsStr::new("1"))
    {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| invalid(e.to_string()))?;
    let error = std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env("SLUICE_PYTHON_DIR", python)
        .env("SLUICE_TMUX_PREFIX", tmux)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .exec();
    Err(invalid(error.to_string()))
}
/// Doctor's single release/installation check; no coordinator activation.
pub fn doctor() -> Result<serde_json::Value, PublicError> {
    let status = crate::install::Installation::configured()?.status()?;
    let (manifest, error) = if let Some(selection) = &status.selection {
        match verify(&selection.release_path) {
            Ok(manifest) => (Some(manifest), None),
            Err(error) => (None, Some(error)),
        }
    } else {
        (None, Some(invalid("installation has no selected release")))
    };
    Ok(
        serde_json::json!({"installation":status,"manifest":manifest,"manifest_ok":error.is_none(),"error":error}),
    )
}
