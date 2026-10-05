use super::super::{EngineError, EngineErrorKind, EngineProfile};
use crate::model::ModelChoice;
use serde_json::Value;
use std::path::Path;
use toml_edit::{DocumentMut, value};

/// Codex's short model names: `{"type":"normal","model":"sol",...}` runs `gpt-6.1-sol`. Any
/// other name is a slug from `codex debug models` as it is.
pub const MODEL_NAMES: [(&str, &str); 2] = [("sol", "gpt-6.1-sol"), ("astra", "gpt-6-astra")];
pub fn model_name(name: &str) -> &str {
    MODEL_NAMES
        .iter()
        .find(|(short, _)| *short == name)
        .map_or(name, |(_, slug)| slug)
}

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
        default_model: Some(ModelChoice::normal("sol", Some("high"))),
        reports_waiting: false,
    }
}

/// Every model's slug (its default effort) and `<slug>@<effort>` for each reasoning effort it
/// supports, from `codex debug models`.
pub fn parse_models(listing: &[u8]) -> Result<Vec<String>, EngineError> {
    let bad = || {
        error(
            EngineErrorKind::Fatal,
            "could not list Codex models: unexpected `codex debug models` output",
        )
    };
    let listing: Value = serde_json::from_slice(listing).map_err(|_| bad())?;
    let mut ids = Vec::new();
    for model in listing["models"].as_array().ok_or_else(bad)? {
        let slug = model["slug"].as_str().ok_or_else(bad)?;
        ids.push(slug.to_owned());
        for level in model["supported_reasoning_levels"]
            .as_array()
            .ok_or_else(bad)?
        {
            ids.push(format!(
                "{slug}@{}",
                level["effort"].as_str().ok_or_else(bad)?
            ));
        }
    }
    Ok(ids)
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
        retry_at: None,
    }
}

pub fn private_config(
    source: &str,
    model: &str,
    effort: Option<&str>,
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
    match effort {
        Some(effort) => doc["model_reasoning_effort"] = value(effort),
        None => {
            doc.remove("model_reasoning_effort");
        }
    }
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
