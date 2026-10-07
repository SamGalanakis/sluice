//! Devin CLI profile, tested on 3000.11.3.
use super::super::{
    EngineError, EngineErrorKind, EngineProfile,
    version::{Major, Policy, ProbeReport, Verdict},
};
use crate::model::ModelChoice;
use serde_json::Value;

/// The version the fixtures and the real-session gates were recorded against.
pub const VERSION: &str = "3000.11.3";
/// Devin's version policy. 3000.11.3 is both the only tested version and the floor: no older
/// one was checked. A newer 3000.x runs untested; a newer major is refused, because Devin's
/// major is not a release counter (every release so far is 3000.x), so a new one marks a
/// different CLI line, and sluice drives Devin through its pane, hook journal and session
/// SQLite schema, none of which a probe can show before a session.
pub const POLICY: Policy = Policy {
    engine: "devin",
    tested: &[VERSION],
    floor: VERSION,
    newer_major: Major::Refuse,
};
pub const HOOKS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
    "PostCompaction",
    "SessionEnd",
];
pub const REQUIRED_FLAGS: [&str; 5] = [
    "--config",
    "--export",
    "--model",
    "--resume",
    "--respect-workspace-trust",
];

pub fn profile() -> EngineProfile {
    EngineProfile {
        engine: "devin".into(),
        version_range: format!("{}; session SQLite user_version=0", POLICY.summary()),
        required_capabilities: HOOKS.iter().map(|s| (*s).into()).collect(),
        default_model: Some(ModelChoice::normal("swe-2", Some("high"))),
        reports_waiting: false,
    }
}

/// The model ids in `devin models list --format json`: every family's variants' `model_uid`.
pub fn parse_models(listing: &[u8]) -> Result<Vec<String>, EngineError> {
    let bad = || {
        error(
            EngineErrorKind::Fatal,
            "could not list Devin models: unexpected `devin models list --format json` output",
        )
    };
    let listing: Value = serde_json::from_slice(listing).map_err(|_| bad())?;
    let mut ids = Vec::new();
    for family in listing["families"].as_array().ok_or_else(bad)? {
        for variant in family["variants"].as_array().ok_or_else(bad)? {
            ids.push(variant["model_uid"].as_str().ok_or_else(bad)?.to_owned());
        }
    }
    Ok(ids)
}

/// Judges `devin --version`'s output (`devin 3000.11.3 (9c803229faa4)`) by `POLICY`.
pub fn validate_version(version: &str) -> Result<Verdict, EngineError> {
    let words: Vec<&str> = version.split_whitespace().take(2).collect();
    match words.as_slice() {
        ["devin", version] => POLICY.judge(version),
        _ => POLICY.judge(version.trim()),
    }
}
/// What `devin --help` shows of the flags sluice launches with. The lifecycle hooks show only
/// in a session; the session SQLite schema is checked when a session is looked up.
pub fn probe(help: Result<String, String>) -> ProbeReport {
    let mut report = ProbeReport::default();
    match help {
        Ok(help) => report.help(&help, &REQUIRED_FLAGS.map(|flag| (flag, flag))),
        Err(e) => report.missing.push(format!("`devin --help` ({e})")),
    }
    for hook in HOOKS {
        report.assume(format!("{hook} hook"));
    }
    report
}

pub(crate) fn error(kind: EngineErrorKind, message: impl Into<String>) -> EngineError {
    EngineError {
        kind,
        message: message.into(),
        retry_at: None,
    }
}
