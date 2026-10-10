//! Engine CLI versions and capabilities (SPEC §15, "Engine versions and capabilities").
//!
//! A version is judged against its engine's `Policy`: one sluice was tested against runs as
//! it is; one at or above the floor that was not tested runs too, with a warning and a note
//! in the run, because a CLI update must not take lanes down unless the engine stopped doing
//! something sluice needs. What sluice needs is then probed (`ProbeReport`), not inferred from
//! the version number: a capability the probe finds missing fails the launch, naming it.
use super::{EngineError, EngineErrorKind};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs::Permissions,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};

/// Whether a major version above every tested one runs (untested, with a warning) or is
/// refused until it has been tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Major {
    Accept,
    Refuse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub engine: &'static str,
    /// The versions the wire fixtures and the live or real-binary gates were recorded against.
    pub tested: &'static [&'static str],
    /// The oldest version known to have every capability sluice needs.
    pub floor: &'static str,
    pub newer_major: Major,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Standing {
    Tested,
    Untested,
}

/// A version the policy accepted, and whether it was tested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub policy: Policy,
    pub version: String,
    pub standing: Standing,
}

/// A dotted numeric version (`0.160.1`, `2.1.284`, `3000.11.3`); a pre-release or build
/// suffix (`0.162.0-alpha.18`) is kept for display and ignored in comparisons.
fn numbers(version: &str) -> Option<Vec<u64>> {
    let core = version.split(['-', '+']).next()?;
    let parts = core
        .split('.')
        .map(|p| p.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    (parts.len() >= 2).then_some(parts)
}

impl Policy {
    pub fn tested_list(&self) -> String {
        self.tested.join(", ")
    }
    /// One line for profiles and doctor: what was tested, the floor and the major rule.
    pub fn summary(&self) -> String {
        format!(
            "tested {}; floor {}; newer {}",
            self.tested_list(),
            self.floor,
            match self.newer_major {
                Major::Accept => "versions accepted untested, with a warning",
                Major::Refuse =>
                    "minor and patch versions accepted untested, a newer major refused",
            }
        )
    }
    fn refuse(&self, message: String) -> EngineError {
        EngineError {
            kind: EngineErrorKind::CapabilityMismatch,
            message,
            retry_at: None,
        }
    }
    /// Judges `version`, the bare version an engine's `--version` printed.
    pub fn judge(&self, version: &str) -> Result<Verdict, EngineError> {
        let engine = self.engine;
        let floor = numbers(self.floor).expect("policy floor is a version");
        let Some(found) = numbers(version) else {
            return Err(self.refuse(format!(
                "{engine}: cannot read a version from `{engine} --version` (got `{}`); sluice needs {engine} {} or newer (tested {})",
                version.chars().take(80).collect::<String>(),
                self.floor,
                self.tested_list()
            )));
        };
        if found < floor {
            return Err(self.refuse(format!(
                "{engine} {version} is older than {}, the oldest version with every capability sluice needs (tested {}); update {engine} on this host, then step_retry",
                self.floor,
                self.tested_list()
            )));
        }
        if self.tested.contains(&version) {
            return Ok(Verdict {
                policy: *self,
                version: version.into(),
                standing: Standing::Tested,
            });
        }
        let top_major = self
            .tested
            .iter()
            .filter_map(|v| numbers(v))
            .map(|v| v[0])
            .max()
            .unwrap_or(floor[0]);
        if found[0] > top_major && self.newer_major == Major::Refuse {
            return Err(self.refuse(format!(
                "{engine} {version} is a newer major version than the tested {}; sluice runs an untested {engine} major only after its wire fixtures are recorded for it (floor {}). Pin {engine} to a {top_major}.x release on this host, then step_retry",
                self.tested_list(),
                self.floor
            )));
        }
        Ok(Verdict {
            policy: *self,
            version: version.into(),
            standing: Standing::Untested,
        })
    }
}

impl Verdict {
    pub fn tested(&self) -> bool {
        self.standing == Standing::Tested
    }
    /// The run's one-line note for an untested version.
    pub fn note(&self) -> Option<String> {
        if self.tested() {
            return None;
        }
        let engine = self.policy.engine;
        let newest = self.policy.tested.iter().filter_map(|v| numbers(v)).max();
        let relation = if numbers(&self.version) > newest {
            "newer than"
        } else {
            "not one of"
        };
        Some(format!(
            "{engine} {} is {relation} the tested {}; accepted untested (floor {})",
            self.version,
            self.policy.tested_list(),
            self.policy.floor
        ))
    }
    /// What a protocol failure on an untested version names as its likely cause.
    pub fn hint(&self) -> Option<String> {
        (!self.tested()).then(|| {
            format!(
                "untested {} {} (tested {})",
                self.policy.engine,
                self.version,
                self.policy.tested_list()
            )
        })
    }
    /// `message`, with this version named when it is untested: "…unavailable; likely cause:
    /// untested codex 0.161.0 (…)", the message's own full stop dropped before the join.
    pub fn explain(&self, message: impl Into<String>) -> String {
        let message = message.into();
        match self.hint() {
            Some(hint) if !message.contains(&hint) => format!(
                "{}; likely cause: {hint}",
                message.trim_end().trim_end_matches(['.', ';'])
            ),
            _ => message,
        }
    }
}

/// What a probe found. `assumed` lists what no cheap probe can show before a session starts;
/// the session itself shows it, and its failure on an untested version names the version.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeReport {
    pub verified: Vec<String>,
    pub missing: Vec<String>,
    pub assumed: Vec<String>,
    /// Probes that could not run (a probe command this version lacks or that failed); what
    /// they would have shown is in `assumed`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}
impl ProbeReport {
    pub fn check(&mut self, capability: impl Into<String>, present: bool) {
        let capability = capability.into();
        if present {
            self.verified.push(capability);
        } else {
            self.missing.push(capability);
        }
    }
    pub fn assume(&mut self, capability: impl Into<String>) {
        self.assumed.push(capability.into());
    }
    /// Each `(capability, flag or subcommand)` in `help`: a flag (`--resume`) anywhere, a
    /// subcommand (`resume`) as the first word of a line.
    pub fn help(&mut self, help: &str, needs: &[(&str, &str)]) {
        for (capability, needle) in needs {
            self.check(*capability, help_has(help, needle));
        }
    }
    /// Fails with every missing capability named, the version and what was tested.
    pub fn require(&self, verdict: &Verdict) -> Result<(), EngineError> {
        if self.missing.is_empty() {
            return Ok(());
        }
        let engine = verdict.policy.engine;
        Err(EngineError {
            kind: EngineErrorKind::CapabilityMismatch,
            message: format!(
                "{engine} {} lacks {} that sluice needs (tested {}; {})",
                verdict.version,
                self.missing
                    .iter()
                    .map(|c| if c.contains('`') {
                        c.clone()
                    } else {
                        format!("`{c}`")
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
                verdict.policy.tested_list(),
                if verdict.tested() {
                    "a tested version, so its installation is likely damaged"
                } else {
                    "this version is untested"
                }
            ),
            retry_at: None,
        })
    }
}

fn help_has(help: &str, needle: &str) -> bool {
    let boundary = |c: Option<char>| c.is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '-'));
    if needle.starts_with('-') {
        help.match_indices(needle).any(|(at, _)| {
            boundary(help[..at].chars().next_back())
                && boundary(help[at + needle.len()..].chars().next())
        })
    } else {
        help.lines().any(|line| {
            let line = line.trim_start();
            line.strip_prefix(needle)
                .is_some_and(|rest| boundary(rest.chars().next()))
        })
    }
}

/// Runs `binary args…` with exactly `env`, stdin closed, for at most 20 s and 1 MiB of
/// stdout; its stdout when it exits 0.
pub async fn run(
    binary: &Path,
    args: &[&str],
    env: &BTreeMap<String, String>,
) -> Result<String, String> {
    let mut child = Command::new(binary)
        .args(args)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            format!(
                "cannot start `{} {}`: {e}",
                binary.display(),
                args.join(" ")
            )
        })?;
    let mut stdout = child.stdout.take().ok_or("probe stdout missing")?;
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| e.to_string())?;
        if bytes.len() > 1024 * 1024 {
            return Err("output exceeds 1 MiB".to_owned());
        }
        let status = child.wait().await.map_err(|e| e.to_string())?;
        if !status.success() {
            return Err(format!("failed ({status})"));
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    })
    .await;
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
        .map_err(|_| "timed out".to_owned())?
        .map_err(|e| format!("`{} {}` {e}", binary.display(), args.join(" ")))
}

/// `binary` as Command would find it: as given when it has a slash, else the first
/// executable of that name on `path`.
pub fn resolve(binary: &Path, path: Option<&str>) -> Option<PathBuf> {
    if binary.components().count() > 1 {
        return Some(binary.to_owned());
    }
    let path = path
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os("PATH"))?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(binary))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// Where under the sluice home launches keep their probe reports.
pub const CACHE_DIR: &str = "engine-probes";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheKey {
    path: PathBuf,
    version: String,
    mtime_ns: i128,
    size: u64,
    inode: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheEntry {
    key: CacheKey,
    report: ProbeReport,
}

fn key(binary: &Path, path: Option<&str>, version: &str) -> Option<CacheKey> {
    let resolved = std::fs::canonicalize(resolve(binary, path)?).ok()?;
    let meta = resolved.metadata().ok()?;
    Some(CacheKey {
        version: version.into(),
        mtime_ns: i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
        size: meta.size(),
        inode: meta.ino(),
        path: resolved,
    })
}

/// The probe report for this binary and version: from `<cache>/<engine>.json` when it was
/// written for the same file (canonical path, mtime, size, inode) and version, else from
/// `probe`, kept when nothing was missing so later launches skip it. A report with something
/// missing is never kept: the launch fails, and the next one probes again.
pub async fn cached<F>(
    cache: Option<&Path>,
    engine: &str,
    binary: &Path,
    path: Option<&str>,
    version: &str,
    probe: F,
) -> ProbeReport
where
    F: std::future::Future<Output = ProbeReport>,
{
    let key = key(binary, path, version);
    let file = cache.map(|dir| dir.join(format!("{engine}.json")));
    if let (Some(file), Some(key)) = (&file, &key)
        && let Ok(bytes) = std::fs::read(file)
        && let Ok(entry) = serde_json::from_slice::<CacheEntry>(&bytes)
        && entry.key == *key
    {
        return entry.report;
    }
    let report = probe.await;
    if report.missing.is_empty()
        && let (Some(file), Some(key), Some(dir)) = (&file, key, cache)
    {
        let saved = (|| {
            std::fs::create_dir_all(dir)?;
            let bytes = serde_json::to_vec(&CacheEntry {
                key,
                report: report.clone(),
            })
            .map_err(std::io::Error::other)?;
            // A temp file of this process's own, so concurrent launches (or one that died
            // mid-write) never block each other; the rename makes the last one win.
            let temp = dir.join(format!(".{engine}.{}.json", std::process::id()));
            let result = std::fs::write(&temp, &bytes)
                .and_then(|()| std::fs::set_permissions(&temp, Permissions::from_mode(0o600)))
                .and_then(|()| std::fs::rename(&temp, file));
            if result.is_err() {
                let _ = std::fs::remove_file(&temp);
            }
            result
        })();
        if let Err(e) = saved {
            tracing::debug!(engine, error = %e, "could not keep the engine probe");
        }
    }
    report
}

/// A scratch directory (0700) for a probe's own home or output, removed on drop.
pub struct Scratch(PathBuf);
impl Scratch {
    pub fn new(engine: &str) -> std::io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "sluice-probe-{engine}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `report` as doctor's text shows it.
pub fn describe(report: &ProbeReport) -> String {
    let mut text = format!("{} verified", report.verified.len());
    if !report.missing.is_empty() {
        let _ = write!(text, "; missing: {}", report.missing.join(", "));
    }
    if !report.assumed.is_empty() {
        let _ = write!(text, "; assumed: {}", report.assumed.join(", "));
    }
    if !report.skipped.is_empty() {
        let _ = write!(text, "; not probed: {}", report.skipped.join("; "));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    const POLICY: Policy = Policy {
        engine: "codex",
        tested: &["0.160.0", "0.160.1"],
        floor: "0.160.0",
        newer_major: Major::Accept,
    };
    #[test]
    fn versions_compare_numerically_and_ignore_suffixes() {
        assert!(numbers("0.161.0") > numbers("0.160.10"));
        assert!(numbers("0.99.0") < numbers("0.160.0"));
        assert_eq!(numbers("0.162.0-alpha.18"), Some(vec![0, 162, 0]));
        assert_eq!(numbers("abc"), None);
        assert_eq!(numbers("7"), None);
    }
    #[test]
    fn help_matches_whole_flags_and_leading_subcommands() {
        let help = "Commands:\n  resume  Resume a session\n  app-server  Run\nOptions:\n  --remote <ADDR>\n  --resume-last\n";
        assert!(help_has(help, "resume"));
        assert!(help_has(help, "app-server"));
        assert!(help_has(help, "--remote"));
        assert!(!help_has(help, "--resume"));
        assert!(!help_has(help, "debug"));
        assert!(!help_has(help, "server"));
    }
    #[test]
    fn notes_say_newer_or_not_one_of() {
        let newer = POLICY.judge("0.161.0").unwrap();
        assert_eq!(
            newer.note().unwrap(),
            "codex 0.161.0 is newer than the tested 0.160.0, 0.160.1; accepted untested (floor 0.160.0)"
        );
        let between = Policy {
            tested: &["0.160.0", "0.162.0"],
            ..POLICY
        }
        .judge("0.161.0")
        .unwrap();
        assert!(between.note().unwrap().contains("is not one of the tested"));
        assert_eq!(POLICY.judge("0.160.1").unwrap().note(), None);
    }
    #[test]
    fn a_likely_cause_joins_a_sentence_without_its_full_stop() {
        let newer = POLICY.judge("0.161.0").unwrap();
        assert_eq!(
            newer.explain("The session ended early."),
            "The session ended early; likely cause: untested codex 0.161.0 (tested 0.160.0, 0.160.1)"
        );
        assert_eq!(
            POLICY.judge("0.160.1").unwrap().explain("Ended."),
            "Ended.",
            "a tested version leaves the message as it was"
        );
    }
}
