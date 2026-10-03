use super::super::{EngineError, EngineErrorKind, EngineProfile};
use std::{collections::BTreeMap, path::Path};
use toml_edit::{DocumentMut, value};

pub fn profile() -> EngineProfile {
    EngineProfile {
        engine: "codex".into(),
        version_range: "0.160.0".into(),
        required_capabilities: [
            "unix-websocket",
            "experimentalApi",
            "thread/resume",
            "turn/steer",
        ]
        .map(String::from)
        .to_vec(),
        models: BTreeMap::from([
            ("sol".into(), "gpt-6.1-sol".into()),
            ("astra".into(), "gpt-6-astra".into()),
        ]),
        efforts: ["minimal", "low", "medium", "high", "xhigh", "max"]
            .map(String::from)
            .to_vec(),
        reports_waiting: false,
    }
}

pub fn check_version(version: &str) -> Result<(), EngineError> {
    if version.trim() != "codex-cli 0.160.0" {
        return Err(error(
            EngineErrorKind::CapabilityMismatch,
            "Codex requires tested CLI 0.160.0; update the profile and wire fixtures for this version",
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

pub fn private_config(
    source: &str,
    model: &str,
    effort: &str,
    search: bool,
) -> Result<String, EngineError> {
    let mut doc = source
        .parse::<DocumentMut>()
        .map_err(|_| error(EngineErrorKind::Fatal, "invalid Codex config TOML"))?;
    if let Some(servers) = doc.get_mut("mcp_servers") {
        let table = servers
            .as_table_like_mut()
            .ok_or_else(|| error(EngineErrorKind::Fatal, "Codex MCP servers must be a table"))?;
        for (_, server) in table.iter_mut() {
            let server = server
                .as_table_like_mut()
                .ok_or_else(|| error(EngineErrorKind::Fatal, "Codex MCP server must be a table"))?;
            server.insert("enabled", value(false));
            server.remove("env");
            server.remove("http_headers");
        }
    }
    doc["model"] = value(model);
    doc["model_reasoning_effort"] = value(effort);
    doc["check_for_update_on_startup"] = value(false);
    if search {
        doc["web_search"] = value("live");
    } else {
        doc.remove("web_search");
    }
    Ok(doc.to_string())
}

pub fn tui_argv(binary: &Path, socket: &Path, session: Option<&str>) -> Vec<String> {
    let mut args = vec![
        binary.to_string_lossy().into_owned(),
        "-c".into(),
        "check_for_update_on_startup=false".into(),
    ];
    if let Some(session) = session {
        args.extend([
            "resume".into(),
            "--remote".into(),
            format!("unix://{}", socket.display()),
            session.into(),
        ]);
    } else {
        args.extend([
            "--dangerously-bypass-approvals-and-sandbox".into(),
            "--remote".into(),
            format!("unix://{}", socket.display()),
        ]);
    }
    args
}
