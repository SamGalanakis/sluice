//! Claude Code 2.1.283 and 2.1.284 hook/status/TUI profile.
use super::protocol::failure;
use crate::engines::{EngineError, EngineErrorKind, EngineProfile};
use std::{collections::BTreeMap, path::Path};

pub const VERSION_RANGE: &str = ">=2.1.283 <=2.1.284";
pub const SCRUB_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "AI_AGENT",
];
pub const HOOKS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
    "StopFailure",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "SubagentStart",
    "SubagentStop",
    "SessionEnd",
];
pub fn profile() -> EngineProfile {
    EngineProfile {
        engine: "claude".into(),
        version_range: VERSION_RANGE.into(),
        required_capabilities: vec![
            "hooks".into(),
            "interactive-status".into(),
            "transcript-jsonl".into(),
            crate::engines::INLINE_COMPACTION_CONTEXT.into(),
        ],
        models: BTreeMap::from([("opus".into(), "opus".into())]),
        efforts: vec![],
        reports_waiting: true,
    }
}
pub fn validate_version(text: &str) -> Result<String, EngineError> {
    let version = text.split_whitespace().next().unwrap_or_default();
    if !matches!(version, "2.1.283" | "2.1.284") || !text.contains("(Claude Code)") {
        return Err(failure(
            EngineErrorKind::CapabilityMismatch,
            format!(
                "claude: unsupported version; require {VERSION_RANGE}. Update the profile and hook fixtures before delivery."
            ),
        ));
    }
    Ok(version.into())
}
pub fn argv(
    binary: &Path,
    settings: &Path,
    session: Option<&str>,
    mcp: Option<&str>,
) -> Vec<String> {
    // EngineLaunch has an additive environment map but no removal list. env's
    // direct exec preserves the guardian cgroup and removes parent-session markers.
    let mut args = vec!["/usr/bin/env".into()];
    for name in SCRUB_ENV {
        args.extend(["-u".into(), (*name).into()]);
    }
    args.extend([
        binary.to_string_lossy().into_owned(),
        "--model".into(),
        "opus".into(),
        "--dangerously-skip-permissions".into(),
        "--disallowedTools".into(),
        "AskUserQuestion".into(),
        "--settings".into(),
        settings.to_string_lossy().into_owned(),
    ]);
    if let Some(mcp) = mcp.filter(|s| !s.trim().is_empty()) {
        args.extend([
            "--strict-mcp-config".into(),
            "--mcp-config".into(),
            mcp.into(),
        ]);
    }
    if let Some(session) = session {
        args.extend(["--resume".into(), session.into()]);
    }
    args
}
