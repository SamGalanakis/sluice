//! Claude Code hook/status/TUI profile, tested on 2.1.283 and 2.1.284.
use super::protocol::failure;
use crate::engines::{
    EngineError, EngineErrorKind, EngineProfile,
    version::{Major, Policy, ProbeReport, Verdict},
};
use crate::model::{ModelChoice, ResolvedModel};
use std::path::Path;

/// Claude Code's version policy. 2.1.283 is the floor: the hook, status, transcript and screen
/// fixtures were recorded on 2.1.284 and the wrapped composer on 2.1.283, and nothing older was
/// checked. Claude Code updates itself in patch releases several times a week, and its 1.x to
/// 2.x change kept the hooks, `--settings` and transcript sluice reads, so a newer version,
/// a newer major too, runs untested: the `--help` probe and the session's own hooks say what
/// changed.
pub const POLICY: Policy = Policy {
    engine: "claude",
    tested: &["2.1.283", "2.1.284"],
    floor: "2.1.283",
    newer_major: Major::Accept,
};
/// The flags of `claude --help` sluice launches Claude with (`argv`).
pub const HELP: [(&str, &str); 8] = [
    ("--model", "--model"),
    ("--effort", "--effort"),
    (
        "--dangerously-skip-permissions",
        "--dangerously-skip-permissions",
    ),
    ("--disallowedTools", "--disallowedTools"),
    ("--settings", "--settings"),
    ("--strict-mcp-config", "--strict-mcp-config"),
    ("--mcp-config", "--mcp-config"),
    ("--resume", "--resume"),
];
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
/// Claude has no model listing, so its models are pinned: Opus by the CLI's alias (the latest
/// Opus) or by its full name, at each effort `claude --effort` takes.
pub const MODELS: [&str; 2] = ["opus", "claude-opus-5-5"];
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// Each pinned model alone (the effort Claude's settings give) and `<model>@<effort>`.
pub fn models() -> Vec<String> {
    MODELS
        .iter()
        .flat_map(|model| {
            std::iter::once(model.to_string()).chain(
                EFFORTS
                    .iter()
                    .map(move |effort| format!("{model}@{effort}")),
            )
        })
        .collect()
}
pub fn profile() -> EngineProfile {
    EngineProfile {
        engine: "claude".into(),
        version_range: POLICY.summary(),
        required_capabilities: vec![
            "hooks".into(),
            "interactive-status".into(),
            "transcript-jsonl".into(),
            crate::engines::INLINE_COMPACTION_CONTEXT.into(),
        ],
        default_model: Some(ModelChoice::normal("opus", Some("high"))),
        reports_waiting: true,
    }
}
/// Judges `claude --version`'s output (`2.1.284 (Claude Code)`) by `POLICY`.
pub fn validate_version(text: &str) -> Result<Verdict, EngineError> {
    let version = text.split_whitespace().next().unwrap_or_default();
    if !text.contains("(Claude Code)") {
        return Err(failure(
            EngineErrorKind::CapabilityMismatch,
            format!(
                "claude: `claude --version` printed `{}`, not a Claude Code version; sluice needs Claude Code {} or newer (tested {})",
                text.trim().chars().take(80).collect::<String>(),
                POLICY.floor,
                POLICY.tested_list()
            ),
        ));
    }
    POLICY.judge(version)
}
/// What `claude --help` shows of the flags sluice launches with. The hooks, the status file,
/// the transcript JSONL and inline compaction context show only in a session: the launch waits
/// for its `SessionStart` hook, and a failure there on an untested version names the version.
pub fn probe(help: Result<String, String>) -> ProbeReport {
    let mut report = ProbeReport::default();
    match help {
        Ok(help) => report.help(&help, &HELP),
        Err(e) => report.missing.push(format!("`claude --help` ({e})")),
    }
    for capability in [
        "hooks",
        "interactive-status",
        "transcript-jsonl",
        crate::engines::INLINE_COMPACTION_CONTEXT,
    ] {
        report.assume(capability);
    }
    report
}
pub fn argv(
    binary: &Path,
    model: &ResolvedModel,
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
        model.model.clone(),
    ]);
    if let Some(effort) = &model.effort {
        args.extend(["--effort".into(), effort.clone()]);
    }
    args.extend([
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
