//! Tested Devin CLI profile. Updating this requires new wire and real-session evidence.
use super::super::{EngineError, EngineErrorKind, EngineProfile};
use std::collections::BTreeMap;

pub const VERSION: &str = "3000.11.3";
pub const FUSION: &str = "fusion-claude-opus-5-5-high-sidekick-swe-2-medium";
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
        models: BTreeMap::from([
            ("swe-2-high".into(), "swe-2-high".into()),
            ("high".into(), "swe-2-high".into()),
            ("fusion".into(), FUSION.into()),
            (FUSION.into(), FUSION.into()),
        ]),
        efforts: vec![],
        reports_waiting: false,
    }
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
