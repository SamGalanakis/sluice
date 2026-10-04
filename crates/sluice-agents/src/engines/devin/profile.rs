//! Tested Devin CLI profile. Updating this requires new wire and real-session evidence.
use super::super::{EngineError, EngineErrorKind, EngineProfile};
use crate::model::ModelChoice;
use serde_json::Value;

pub const VERSION: &str = "3000.11.3";
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
        version_range: format!("={VERSION}; session SQLite user_version=0"),
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

pub fn validate_cli(version: &str, help: &str) -> Result<(), EngineError> {
    if version.split_whitespace().take(2).collect::<Vec<_>>() != ["devin", VERSION]
        || !REQUIRED_FLAGS.iter().all(|flag| help.contains(flag))
    {
        return Err(error(
            EngineErrorKind::CapabilityMismatch,
            format!(
                "Devin requires tested CLI {VERSION} and config/export/resume capabilities; update the Devin profile and fixtures before delivery"
            ),
        ));
    }
    Ok(())
}

pub(crate) fn error(kind: EngineErrorKind, message: impl Into<String>) -> EngineError {
    EngineError {
        kind,
        message: message.into(),
    }
}
