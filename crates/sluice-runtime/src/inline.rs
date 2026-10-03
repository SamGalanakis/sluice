//! inline.bash and inline.python entry point for the guardian dispatch table.
use crate::python::{PythonError, PythonHost, output_types, run_command};
use sluice_model::{
    plan::FnSignature,
    rpc::{FnInvocation, JsonMap, decode_json},
    types::Type,
};
use std::{path::Path, process::Stdio};
use tokio::process::Command;

pub fn inline_signature(name: &str) -> Option<FnSignature> {
    let mut signature = FnSignature {
        open: true,
        ..FnSignature::default()
    };
    signature.inputs.insert("code".into(), Type::String);
    signature
        .inputs
        .insert("cwd".into(), Type::Optional(Box::new(Type::String)));
    match name {
        "inline.bash" => {
            signature
                .inputs
                .insert("check".into(), Type::Optional(Box::new(Type::Boolean)));
            for (name, ty) in [
                ("stdout", Type::String),
                ("stderr", Type::String),
                ("code", Type::Int),
            ] {
                signature.outputs.insert(name.into(), ty);
            }
        }
        "inline.python" => {
            signature
                .outputs
                .insert("value".into(), Type::Optional(Box::new(Type::Any)));
            signature.outputs.insert("stdout".into(), Type::String);
        }
        _ => return None,
    }
    Some(signature)
}

/// Wire this function for the two inline names. Host.context.outputs is the complete
/// frozen result schema, including builtin return fields and declared submission fields.
pub async fn invoke_inline(
    host: &PythonHost,
    invocation: &FnInvocation,
) -> Result<JsonMap, PythonError> {
    match invocation.name.as_str() {
        "inline.python" => {
            let command =
                host.script_command(invocation, &host.config.helper_dir.join("inline_python.py"))?;
            host.execute_script(invocation, command, host.envelope(invocation)?)
                .await
        }
        "inline.bash" => bash(host, invocation).await,
        _ => Err(PythonError::Protocol("unknown inline fn".into())),
    }
}
fn variable(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
async fn bash(host: &PythonHost, invocation: &FnInvocation) -> Result<JsonMap, PythonError> {
    let inputs = &invocation.inputs.0;
    let code = inputs
        .get("code")
        .and_then(|v| v.as_value().as_str())
        .ok_or_else(|| PythonError::Protocol("code must be a string".into()))?;
    let out_path = host.context.run_dir.join("out.json");
    match std::fs::remove_file(&out_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut command = Command::new("bash");
    command.args(["-e", "-o", "pipefail", "-c", code]);
    // bash does not enter uv's environment; retain the host's original tool environment.
    command
        .env_clear()
        .envs(&host.config.environment)
        .current_dir(&host.bundle.bundle_dir);
    if let Some(cwd) = inputs.get("cwd").and_then(|v| v.as_value().as_str()) {
        command.current_dir(cwd);
    }
    let mut seen = std::collections::BTreeMap::new();
    for name in host.context.extra_inputs.0.keys() {
        let var = variable(name);
        if var == "OUT" || var.as_bytes().first().is_none_or(u8::is_ascii_digit) {
            return Err(PythonError::Protocol(format!(
                "extra input {name:?} uses reserved/invalid variable {var:?}"
            )));
        }
        if let Some(previous) = seen.insert(var.clone(), name) {
            return Err(PythonError::Protocol(format!(
                "inputs {previous:?} and {name:?} share environment variable {var}"
            )));
        }
        match inputs.get(name).map(|v| v.as_value()) {
            Some(value) if !value.is_null() => {
                command.env(
                    var,
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string()),
                );
            }
            _ => {
                command.env_remove(var);
            }
        }
    }
    for key in [
        "PYTHONHOME",
        "UV_PROJECT_ENVIRONMENT",
        "UV_ACTIVE",
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
        "CODEX_THREAD_ID",
    ] {
        command.env_remove(key);
    }
    command.env("OUT", &out_path).stdin(Stdio::null());
    // Preserve callbacks for tools started by inline code too.
    for (key, value) in [
        ("SLUICE_HOME", host.context.home.as_os_str()),
        ("SLUICE_BIN", host.config.bin.as_os_str()),
        ("SLUICE_RUN_DIR", host.context.run_dir.as_os_str()),
        ("SLUICE_FN_DIR", host.bundle.bundle_dir.as_os_str()),
    ] {
        command.env(key, value);
    }
    command
        .env("SLUICE_PROJECT_ID", invocation.project.to_string())
        .env("SLUICE_PROJECT", &host.context.project)
        .env("SLUICE_RUN_ID", invocation.run.to_string())
        .env(
            "SLUICE_STEP",
            invocation
                .step
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
        )
        .env(
            "SLUICE_PREV_RUN",
            host.context
                .prev_run
                .map(|r| r.to_string())
                .unwrap_or_default(),
        );
    if let Some(path) = &host.context.control_socket {
        command.env("SLUICE_CONTROL_SOCKET", path);
    }
    if let Some(capability) = &host.context.run_capability {
        command.env(
            "SLUICE_RUN_CAPABILITY",
            serde_json::to_value(capability)
                .expect("capability string")
                .as_str()
                .expect("string"),
        );
    }
    let result = run_command(
        command,
        Vec::new(),
        &host.context.run_dir.join("stderr.log"),
        &host.cancellation,
        true,
    )
    .await?;
    let check = inputs
        .get("check")
        .is_none_or(|v| v.as_value() != &serde_json::Value::Bool(false));
    if check && !result.status.success() {
        return Err(PythonError::Exit(crate::python::tail(&format!(
            "bash exit code {:?}: {}",
            result.status.code(),
            result.stderr_tail
        ))));
    }
    let stdout = String::from_utf8_lossy(&result.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
    let mut returned: JsonMap = decode_json(&serde_json::to_vec(&serde_json::json!({"stdout":stdout, "stderr":stderr, "code":result.status.code().unwrap_or(-1)})).expect("JSON"))?;
    let declarations = output_types(&host.context.outputs)?;
    let declared: Vec<_> = declarations
        .keys()
        .filter(|n| !matches!(n.as_str(), "stdout" | "stderr" | "code"))
        .collect();
    if !declared.is_empty() {
        let raw = read_out(&out_path)?;
        let given: JsonMap = decode_json(&raw)?;
        for name in declared {
            if let Some(value) = given.0.get(name) {
                returned.0.insert(name.clone(), value.clone());
            }
        }
    }
    host.validate_result(invocation, returned).await
}
fn read_out(path: &Path) -> Result<Vec<u8>, PythonError> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| {
        PythonError::Failure(format!(
            "the step declares outputs: write one JSON object to \"$OUT\": {e}"
        ))
    })?;
    let mut raw = Vec::new();
    file.take((sluice_model::rpc::MAX_FRAME_BYTES + 1) as u64)
        .read_to_end(&mut raw)?;
    Ok(raw)
}
