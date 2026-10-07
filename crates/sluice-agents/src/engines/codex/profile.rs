use super::super::{
    EngineError, EngineErrorKind, EngineProfile,
    version::{self, Major, Policy, ProbeReport, Verdict},
};
use crate::model::ModelChoice;
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};
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

/// Codex's version policy. 0.160.0 is the floor: the adapter, its wire fixtures and the
/// shared credential link (Codex writing `auth.json` in place, through the link) were built and
/// checked against it, and nothing older was. 0.160.1's generated app-server protocol schema is
/// byte-identical to 0.160.0's and its real app-server answered the same launch, thread and
/// turn exchange. Codex is 0.x and ships a minor version per release, so neither a minor nor
/// a major number marks a protocol break: any newer version runs untested, and the probe and the
/// app-server's own replies (an unknown method, a refused `experimentalApi`) say what changed.
pub const POLICY: Policy = Policy {
    engine: "codex",
    tested: &["0.160.0", "0.160.1"],
    floor: "0.160.0",
    newer_major: Major::Accept,
};
/// The flags and subcommands of `codex --help` sluice runs Codex with.
pub const HELP: [(&str, &str); 6] = [
    ("app-server", "app-server"),
    ("resume", "resume"),
    ("debug models", "debug"),
    ("--remote", "--remote"),
    (
        "--dangerously-bypass-approvals-and-sandbox",
        "--dangerously-bypass-approvals-and-sandbox",
    ),
    ("--config", "--config"),
];
/// The app-server requests sluice sends.
pub const REQUESTS: [&str; 9] = [
    "initialize",
    "thread/start",
    "thread/resume",
    "thread/read",
    "thread/unsubscribe",
    "turn/start",
    "turn/steer",
    "turn/interrupt",
    "account/read",
];
/// The app-server notifications sluice reads.
pub const NOTIFICATIONS: [&str; 7] = [
    "thread/started",
    "thread/status/changed",
    "turn/started",
    "turn/completed",
    "item/completed",
    "error",
    "account/rateLimits/updated",
];

pub fn profile() -> EngineProfile {
    EngineProfile {
        engine: "codex".into(),
        version_range: POLICY.summary(),
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

/// Judges `codex --version`'s output (`codex-cli 0.160.1`) by `POLICY`.
pub fn check_version(output: &str) -> Result<Verdict, EngineError> {
    let output = output.trim();
    let version = output
        .lines()
        .find_map(|line| line.trim().strip_prefix("codex-cli "))
        .map_or(output, str::trim);
    POLICY.judge(version)
}

/// What `binary` offers of what sluice needs, without a session or credentials: the flags and
/// subcommands in `codex --help`, the app-server's Unix socket transport in `codex app-server
/// --help`, and every request and notification sluice uses (with the `experimentalApi`
/// capability) in the protocol schema `codex app-server generate-json-schema --experimental`
/// writes. `env` is the launch environment; `CODEX_HOME` is a scratch directory.
pub async fn probe(binary: &Path, env: &BTreeMap<String, String>) -> ProbeReport {
    let mut report = ProbeReport::default();
    let scratch = match version::Scratch::new("codex") {
        Ok(scratch) => scratch,
        Err(e) => {
            report
                .missing
                .push(format!("probe scratch directory ({e})"));
            return report;
        }
    };
    let home = scratch.path().join("home");
    let _ = std::fs::create_dir(&home);
    let mut env = env.clone();
    env.insert("CODEX_HOME".into(), home.to_string_lossy().into_owned());
    match version::run(binary, &["--help"], &env).await {
        Ok(help) => report.help(&help, &HELP),
        Err(e) => report.missing.push(format!("`codex --help` ({e})")),
    }
    let server = match version::run(binary, &["app-server", "--help"], &env).await {
        Ok(help) => help,
        Err(e) => {
            report
                .missing
                .push(format!("`codex app-server --help` ({e})"));
            return report;
        }
    };
    report.check(
        "unix-websocket",
        server.contains("--listen") && server.contains("unix://"),
    );
    let out = scratch.path().join("schema");
    let schema = if server.contains("generate-json-schema") {
        let out_arg = out.to_string_lossy().into_owned();
        version::run(
            binary,
            &[
                "app-server",
                "generate-json-schema",
                "--experimental",
                "--out",
                &out_arg,
            ],
            &env,
        )
        .await
        .and_then(|_| read_schema(&out))
    } else {
        Err("`codex app-server` has no generate-json-schema".into())
    };
    match schema {
        Ok(schema) => check_schema(&mut report, &schema),
        Err(e) => {
            report.skipped.push(format!("protocol schema: {e}"));
            report.assume("experimentalApi");
            for method in REQUESTS.iter().chain(&NOTIFICATIONS) {
                report.assume(*method);
            }
        }
    }
    // Only a session shows these: the TUI attaching to the app-server, and Codex writing a
    // refreshed `auth.json` in place through the shared link.
    report.assume("remote TUI attach");
    report.assume("auth.json written in place");
    report
}

/// The generated schema's request and notification method names, and whether `initialize`
/// takes the `experimentalApi` capability.
pub struct Schema {
    pub requests: Vec<String>,
    pub notifications: Vec<String>,
    pub experimental_api: bool,
}
fn methods(schema: &Value) -> Vec<String> {
    schema["oneOf"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|variant| variant["properties"]["method"]["enum"].as_array())
        .flatten()
        .filter_map(|m| m.as_str().map(String::from))
        .collect()
}
pub fn read_schema(dir: &Path) -> Result<Schema, String> {
    let read = |name: &str| -> Result<Value, String> {
        let bytes = std::fs::read(dir.join(name)).map_err(|e| format!("{name}: {e}"))?;
        serde_json::from_slice(&bytes).map_err(|e| format!("{name}: {e}"))
    };
    let requests = read("ClientRequest.json")?;
    let notifications = read("ServerNotification.json")?;
    let schema = Schema {
        experimental_api:
            requests["definitions"]["InitializeCapabilities"]["properties"]["experimentalApi"]
                .is_object(),
        requests: methods(&requests),
        notifications: methods(&notifications),
    };
    if schema.requests.is_empty() || schema.notifications.is_empty() {
        return Err("schema lists no methods".into());
    }
    Ok(schema)
}
pub fn check_schema(report: &mut ProbeReport, schema: &Schema) {
    report.check("experimentalApi", schema.experimental_api);
    for method in REQUESTS {
        report.check(method, schema.requests.iter().any(|m| m == method));
    }
    for method in NOTIFICATIONS {
        report.check(method, schema.notifications.iter().any(|m| m == method));
    }
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
    // The private home's auth.json links the owner's file: a keyring entry is keyed by the
    // private home's path, and saving there would delete the link.
    doc["cli_auth_credentials_store"] = value("file");
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
