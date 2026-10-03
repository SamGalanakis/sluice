//! Pinned Python fn boundary. The guardian owns containment and terminal transactions.
//! Configure `helper_dir` to the release's `python/` (tests use the repository's copy).

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use sluice_model::{
    commands::{CommandReply, CommandRequest},
    error::PublicError,
    ids::{ProjectId, RunId, StepId},
    rpc::{
        FnInvocation, JsonMap, MAX_FRAME_BYTES, PROTOCOL_VERSION, RequestId, RpcReply, RpcRequest,
        RpcResult, RunCapability, decode_json, encode_frame,
    },
    types::{Type, check_value_at},
};
use sluice_process::guardian::FnHost;
use std::{
    collections::{BTreeMap, VecDeque},
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::Command,
};
use tokio_util::sync::CancellationToken;

pub const ERROR_TAIL_BYTES: usize = 2048;
pub const STDERR_ARTIFACT_BYTES: usize = 1024 * 1024;

/// Retains rejection separately from ordinary failure for the guardian's completion API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonError {
    Protocol(String),
    Exit(String),
    Failure(String),
    Transient(String),
    Rejected(String),
    Cancelled(String),
    InvalidOutputs(Vec<String>),
    Io(String),
}
impl std::fmt::Display for PythonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Protocol(s)
            | Self::Exit(s)
            | Self::Failure(s)
            | Self::Transient(s)
            | Self::Rejected(s)
            | Self::Cancelled(s)
            | Self::Io(s) => f.write_str(s),
            Self::InvalidOutputs(errors) => f.write_str(&errors.join("; ")),
        }
    }
}
impl std::error::Error for PythonError {}
impl From<std::io::Error> for PythonError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(tail(&e.to_string()))
    }
}
impl From<PublicError> for PythonError {
    fn from(e: PublicError) -> Self {
        Self::Protocol(tail(&e.to_string()))
    }
}
impl PythonError {
    pub fn into_public(self) -> PublicError {
        match self {
            Self::Cancelled(message) => PublicError::Cancelled { message },
            Self::InvalidOutputs(errors) => PublicError::Invalid {
                message: "invalid Python fn outputs".into(),
                errors,
            },
            other => PublicError::FnFailure {
                message: tail(&other.to_string()),
            },
        }
    }
}
pub fn tail(message: &str) -> String {
    let mut start = message.len().saturating_sub(ERROR_TAIL_BYTES);
    while !message.is_char_boundary(start) {
        start += 1;
    }
    message[start..].into()
}

#[derive(Debug, Clone)]
pub struct PythonConfig {
    pub uv: PathBuf,
    pub helper_dir: PathBuf,
    pub bin: PathBuf,
    /// Snapshot before uv starts. Never inherit the ambient Python package path.
    pub environment: BTreeMap<OsString, OsString>,
}
impl PythonConfig {
    pub fn new(helper_dir: PathBuf, bin: PathBuf) -> Self {
        Self {
            uv: "uv".into(),
            helper_dir,
            bin,
            environment: std::env::vars_os().collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PinnedPythonFn {
    pub bundle_dir: PathBuf,
    pub sibling_helper_root: Option<PathBuf>,
}

/// Paths and frozen schema supplied by reservation, never rediscovered by the helper.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonContext {
    pub home: PathBuf,
    pub run_dir: PathBuf,
    /// The run's project metadata directory: <home>/projects/<ProjectId>.
    pub project_dir: PathBuf,
    pub project: String,
    pub prev_run: Option<RunId>,
    pub extra_inputs: JsonMap,
    pub outputs: JsonMap,
    /// Fn return schema, separate from the outer step's declared submission fields.
    #[serde(default)]
    pub returns: JsonMap,
    pub control_socket: Option<PathBuf>,
    pub run_capability: Option<RunCapability>,
}

/// Small additive callback handled by the guardian in one store writer transaction.
/// Capture `completion_target`, then call `register_completion_action`. Repeat registration
/// must consult the already registered action before capturing any new target.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetryOnFailure {
    pub project: ProjectId,
    pub run: RunId,
    pub step: StepId,
    pub message: String,
    pub author: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "command",
    content = "args",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum HelperExtension {
    RetryOnFailure(RetryOnFailure),
    Tool(ToolRequest),
}
/// Named command-service JSON, shared with the CLI/MCP adapter at integration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRequest {
    pub name: String,
    pub args: JsonMap,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
// Keep the model command by value so frame decoders and dispatch can move it directly.
// Its fixed 496-byte size is bounded independently of the 16 MiB JSON payload.
#[allow(clippy::large_enum_variant)]
pub enum HelperCommand {
    Runtime(CommandRequest),
    Extension(HelperExtension),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperRequest {
    pub protocol: u16,
    pub request_id: RequestId,
    pub run_capability: Option<RunCapability>,
    pub command: HelperCommand,
}
impl HelperRequest {
    pub fn decode_frame(bytes: &[u8]) -> Result<Self, PublicError> {
        let size = bytes
            .get(..4)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map(u32::from_be_bytes)
            .map(|n| n as usize);
        if size.is_none_or(|n| n > MAX_FRAME_BYTES || n + 4 != bytes.len()) {
            return Err(PublicError::BadRequest {
                message: "invalid helper frame length".into(),
            });
        }
        let value: Self = decode_json(&bytes[4..])?;
        if value.protocol != PROTOCOL_VERSION {
            return Err(PublicError::BadRequest {
                message: "unsupported helper protocol".into(),
            });
        }
        Ok(value)
    }
}

/// FnHost adapter for a single reservation and its pinned bundle.
/// Use `execute` directly when the guardian needs the typed Rejected outcome.
pub struct PythonHost {
    pub config: PythonConfig,
    pub bundle: PinnedPythonFn,
    pub context: PythonContext,
    pub cancellation: CancellationToken,
}
impl FnHost for PythonHost {
    async fn invoke(&self, invocation: FnInvocation) -> Result<JsonMap, PublicError> {
        self.execute(&invocation)
            .await
            .map_err(PythonError::into_public)
    }
}
impl PythonHost {
    pub fn command(&self, invocation: &FnInvocation) -> Result<Command, PythonError> {
        self.script_command(invocation, &self.bundle.bundle_dir.join("main.py"))
    }
    pub(crate) fn script_command(
        &self,
        invocation: &FnInvocation,
        script: &Path,
    ) -> Result<Command, PythonError> {
        let mut command = Command::new(&self.config.uv);
        command.args(["run", "--no-project", "--quiet"]).arg(script);
        self.set_environment(&mut command, invocation)?;
        Ok(command)
    }
    pub(crate) fn set_environment(
        &self,
        command: &mut Command,
        invocation: &FnInvocation,
    ) -> Result<(), PythonError> {
        command
            .env_clear()
            .envs(&self.config.environment)
            .current_dir(&self.bundle.bundle_dir);
        for key in [
            "PYTHONHOME",
            "VIRTUAL_ENV",
            "UV_PROJECT_ENVIRONMENT",
            "UV_ACTIVE",
        ] {
            command.env_remove(key);
        }
        for key in ["PATH", "PYTHONPATH", "VIRTUAL_ENV"] {
            command.env(
                format!("SLUICE_HOST_{key}"),
                self.config
                    .environment
                    .get(std::ffi::OsStr::new(key))
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        let roots = std::iter::once(self.config.helper_dir.as_path())
            .chain(self.bundle.sibling_helper_root.as_deref());
        let pythonpath =
            std::env::join_paths(roots).map_err(|e| PythonError::Protocol(tail(&e.to_string())))?;
        command
            .env("PYTHONPATH", pythonpath)
            .env("SLUICE_HOME", &self.context.home)
            .env("SLUICE_BIN", &self.config.bin)
            .env("SLUICE_PROJECT_ID", invocation.project.to_string())
            .env("SLUICE_PROJECT", &self.context.project)
            .env(
                "SLUICE_STEP",
                invocation
                    .step
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            )
            .env("SLUICE_RUN_ID", invocation.run.to_string())
            .env("SLUICE_RUN_DIR", &self.context.run_dir)
            .env("SLUICE_PROJECT_DIR", &self.context.project_dir)
            .env("SLUICE_FN_DIR", &self.bundle.bundle_dir)
            .env(
                "SLUICE_PREV_RUN",
                self.context
                    .prev_run
                    .map(|r| r.to_string())
                    .unwrap_or_default(),
            );
        if let Some(path) = &self.context.control_socket {
            command.env("SLUICE_CONTROL_SOCKET", path);
        }
        if let Some(capability) = &self.context.run_capability {
            command.env(
                "SLUICE_RUN_CAPABILITY",
                serde_json::to_value(capability)
                    .expect("capability")
                    .as_str()
                    .expect("string"),
            );
        }
        Ok(())
    }
    pub fn envelope(&self, invocation: &FnInvocation) -> Result<Vec<u8>, PythonError> {
        let mut context = serde_json::to_value(&self.context)
            .map_err(|e| PythonError::Protocol(tail(&e.to_string())))?;
        let fields = context.as_object_mut().expect("context object");
        fields.insert("project_id".into(), serde_json::json!(invocation.project));
        fields.insert(
            "step".into(),
            serde_json::json!(
                invocation
                    .step
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default()
            ),
        );
        fields.insert("run_id".into(), serde_json::json!(invocation.run));
        fields.insert("attempt_id".into(), serde_json::json!(invocation.attempt));
        fields.insert(
            "invocation_id".into(),
            serde_json::json!(invocation.invocation),
        );
        fields.insert("fn_dir".into(), serde_json::json!(self.bundle.bundle_dir));
        fields.insert("bin".into(), serde_json::json!(self.config.bin));
        let envelope = serde_json::to_vec(&serde_json::json!({"protocol": PROTOCOL_VERSION, "inputs": invocation.inputs, "context": context}))
            .map_err(|e| PythonError::Protocol(tail(&e.to_string())))?;
        let _: serde_json::Value = decode_json(&envelope)?;
        Ok(envelope)
    }
    pub async fn execute(&self, invocation: &FnInvocation) -> Result<JsonMap, PythonError> {
        let input = self.envelope(invocation)?;
        let command = self.command(invocation)?;
        self.execute_script(invocation, command, input).await
    }
    pub(crate) async fn execute_script(
        &self,
        invocation: &FnInvocation,
        command: Command,
        input: Vec<u8>,
    ) -> Result<JsonMap, PythonError> {
        let output = run_command(
            command,
            input,
            &self.context.run_dir.join("stderr.log"),
            &self.cancellation,
            false,
        )
        .await?;
        let returned = decode_result(&output.stdout, output.status, &output.stderr_tail)?;
        self.validate_result(invocation, returned).await
    }
    pub(crate) async fn validate_result(
        &self,
        invocation: &FnInvocation,
        returned: JsonMap,
    ) -> Result<JsonMap, PythonError> {
        let mut schemas = output_types(&self.context.returns)?;
        for (name, ty) in output_types(&self.context.outputs)? {
            if schemas.insert(name, ty).is_some() {
                return Err(PythonError::Protocol(
                    "return/submission schema collision".into(),
                ));
            }
        }
        validate_outputs(&schemas, &returned, false)?;
        let mut merged = if self.context.control_socket.is_some()
            || (!self.context.outputs.0.is_empty() && self.config.bin.is_file())
        {
            let command = CommandRequest::Submission {
                run: invocation.run,
            };
            let reply = if let Some(path) = &self.context.control_socket {
                runtime_callback(
                    path,
                    self.context.run_capability.clone(),
                    command,
                    &self.cancellation,
                )
                .await?
            } else {
                binary_callback(&self.config, &self.context, command, &self.cancellation).await?
            };
            match reply {
                CommandReply::Data(value) => decode_json::<JsonMap>(
                    &serde_json::to_vec(value.as_value()).expect("JSON value"),
                )?,
                _ => {
                    return Err(PythonError::Protocol(
                        "submission callback must return data".into(),
                    ));
                }
            }
        } else {
            JsonMap::default()
        };
        validate_outputs(&schemas, &merged, false)?;
        merged.0.extend(returned.0);
        validate_outputs(&schemas, &merged, true)?;
        for (name, ty) in schemas {
            if !merged.0.contains_key(&name) && matches!(ty, Type::Optional(_)) {
                merged.0.insert(name, serde_json::Value::Null.try_into()?);
            }
        }
        let _: JsonMap = decode_json(&serde_json::to_vec(&merged).expect("JSON outputs"))?;
        Ok(merged)
    }
}

pub fn output_types(declarations: &JsonMap) -> Result<IndexMap<String, Type>, PythonError> {
    declarations
        .0
        .iter()
        .map(|(name, declaration)| {
            let value = declaration.as_value();
            // A declaration is either a SPEC type expression or {type: expression, doc: ...}.
            let form = if value.as_object().is_some_and(|o| {
                !o.contains_key("fields") && !o.contains_key("items") && !o.contains_key("symbols")
            }) {
                value.get("type").unwrap_or(value)
            } else {
                value
            };
            Type::parse(form)
                .map(|t| (name.clone(), t))
                .map_err(|e| PythonError::Protocol(tail(&e.to_string())))
        })
        .collect()
}
/// Validate partial submissions/returns, then the complete merged result.
pub fn validate_outputs(
    schemas: &IndexMap<String, Type>,
    outputs: &JsonMap,
    complete: bool,
) -> Result<(), PythonError> {
    let mut errors = Vec::new();
    for (name, value) in &outputs.0 {
        match schemas.get(name) {
            Some(ty) => {
                if let Err(found) = check_value_at(ty, value.as_value(), name) {
                    errors.extend(found.into_iter().map(|e| tail(&e.to_string())));
                }
            }
            None => errors.push(tail(&format!("{name}: undeclared output"))),
        }
    }
    if complete {
        for (name, ty) in schemas {
            if !outputs.0.contains_key(name) && !matches!(ty, Type::Optional(_)) {
                errors.push(tail(&format!("{name}: missing required output")));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(PythonError::InvalidOutputs(errors))
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ResultEnvelope {
    Success(SuccessEnvelope),
    Failure(FailureEnvelope),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SuccessEnvelope {
    ok: bool,
    outputs: JsonMap,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FailureEnvelope {
    ok: bool,
    error: ErrorEnvelope,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorEnvelope {
    kind: String,
    message: String,
}
pub fn decode_result(
    bytes: &[u8],
    status: ExitStatus,
    stderr: &str,
) -> Result<JsonMap, PythonError> {
    let result: ResultEnvelope = decode_json(bytes).map_err(|error| {
        if status.success() {
            PythonError::from(error)
        } else {
            PythonError::Exit(tail(&format!("Python exited {status}: {error}; {stderr}")))
        }
    })?;
    match result {
        ResultEnvelope::Success(result) if result.ok && status.success() => Ok(result.outputs),
        ResultEnvelope::Failure(result) if !result.ok && !status.success() => {
            let message = tail(&result.error.message);
            Err(match result.error.kind.as_str() {
                "rejected" => PythonError::Rejected(message),
                "transient" => PythonError::Transient(message),
                "cancelled" => PythonError::Cancelled(message),
                "fn_failure" => PythonError::Failure(message),
                _ => PythonError::Protocol("unknown function error kind".into()),
            })
        }
        _ => Err(PythonError::Exit(tail(&format!(
            "Python exit/envelope mismatch ({status}): {stderr}"
        )))),
    }
}

pub(crate) struct ProcessOutput {
    pub stdout: Vec<u8>,
    pub stderr_tail: String,
    pub stderr: Vec<u8>,
    pub status: ExitStatus,
}
async fn read_stdout(mut stream: impl AsyncRead + Unpin) -> Result<Vec<u8>, PythonError> {
    let mut result = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        if result.len().saturating_add(count) > MAX_FRAME_BYTES {
            return Err(PythonError::Protocol("result exceeds 16 MiB".into()));
        }
        result.extend_from_slice(&chunk[..count]);
    }
    Ok(result)
}
async fn read_stderr(
    mut stream: impl AsyncRead + Unpin,
    path: &Path,
    capture: bool,
) -> Result<(String, Vec<u8>), PythonError> {
    let mut file = std::fs::File::create(path)?;
    let mut stored = 0;
    let mut captured = Vec::new();
    let mut last = VecDeque::with_capacity(ERROR_TAIL_BYTES);
    let mut chunk = [0; 8192];
    loop {
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        if capture {
            if captured.len().saturating_add(count) > MAX_FRAME_BYTES {
                return Err(PythonError::Protocol("stderr result exceeds 16 MiB".into()));
            }
            captured.extend_from_slice(&chunk[..count]);
        }
        let retained = count.min(STDERR_ARTIFACT_BYTES - stored);
        file.write_all(&chunk[..retained])?;
        stored += retained;
        last.extend(&chunk[..count]);
        while last.len() > ERROR_TAIL_BYTES {
            last.pop_front();
        }
    }
    Ok((
        tail(&String::from_utf8_lossy(
            &last.into_iter().collect::<Vec<_>>(),
        )),
        captured,
    ))
}
/// Bounded concurrent pipes. On error/cancel kill and reap the uv/bash child.
/// Descendants and escaped pipe writers remain the enclosing guardian's responsibility.
pub(crate) async fn run_command(
    mut command: Command,
    input: Vec<u8>,
    artifact: &Path,
    cancellation: &CancellationToken,
    capture_stderr: bool,
) -> Result<ProcessOutput, PythonError> {
    if cancellation.is_cancelled() {
        return Err(PythonError::Cancelled("run cancelled before spawn".into()));
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let result = tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(PythonError::Cancelled("run cancelled".into())),
        result = async {
            let write = async {
                let result = stdin.write_all(&input).await;
                drop(stdin);
                if let Err(error) = result && error.kind() != std::io::ErrorKind::BrokenPipe { return Err(error.into()); }
                Ok(())
            };
            let (_, stdout, stderr_tail, status) = tokio::try_join!(write, read_stdout(stdout), read_stderr(stderr, artifact, capture_stderr), async { child.wait().await.map_err(PythonError::from) })?;
            Ok(ProcessOutput { stdout, stderr_tail: stderr_tail.0, stderr: stderr_tail.1, status })
        } => result,
    };
    if result.is_err() {
        // start_kill is harmless if exit already occurred; wait always observes/reaps it.
        let _ = child.start_kill();
        child.wait().await?;
    }
    result
}

async fn runtime_callback(
    path: &Path,
    capability: Option<RunCapability>,
    command: CommandRequest,
    cancellation: &CancellationToken,
) -> Result<CommandReply, PythonError> {
    let request_id = RequestId(sluice_model::ids::InvocationId::new().to_string());
    let request = RpcRequest {
        protocol: PROTOCOL_VERSION,
        request_id: request_id.clone(),
        run_capability: capability,
        command,
    };
    let frame = encode_frame(&request)?;
    tokio::select! {
        _ = cancellation.cancelled() => Err(PythonError::Cancelled("run cancelled during callback".into())),
        result = async {
            let mut socket = UnixStream::connect(path).await?;
            socket.write_all(&frame).await?;
            let size = socket.read_u32().await? as usize;
            if size > MAX_FRAME_BYTES { return Err(PythonError::Protocol("callback exceeds 16 MiB".into())); }
            let mut raw = vec![0; size];
            socket.read_exact(&mut raw).await?;
            let reply: RpcReply = decode_json(&raw)?;
            if reply.protocol != PROTOCOL_VERSION || reply.request_id != request_id { return Err(PythonError::Protocol("callback identity mismatch".into())); }
            match reply.result { RpcResult::Ok(reply) => Ok(*reply), RpcResult::Error(error) => Err(error.into()) }
        } => result,
    }
}

async fn binary_callback(
    config: &PythonConfig,
    context: &PythonContext,
    command: CommandRequest,
    cancellation: &CancellationToken,
) -> Result<CommandReply, PythonError> {
    let request_id = RequestId(sluice_model::ids::InvocationId::new().to_string());
    let request = RpcRequest {
        protocol: PROTOCOL_VERSION,
        request_id: request_id.clone(),
        run_capability: context.run_capability.clone(),
        command,
    };
    let raw = serde_json::to_vec(&request).expect("RPC request");
    let _: RpcRequest = decode_json(&raw)?;
    let mut command = Command::new(&config.bin);
    command
        .args(["internal", "callback"])
        .env_clear()
        .envs(&config.environment);
    let result = run_command(
        command,
        raw,
        &context.run_dir.join("callback-stderr.log"),
        cancellation,
        false,
    )
    .await?;
    if !result.status.success() {
        return Err(PythonError::Exit(tail(&format!(
            "callback exited {}: {}",
            result.status, result.stderr_tail
        ))));
    }
    let reply: RpcReply = decode_json(&result.stdout)?;
    if reply.protocol != PROTOCOL_VERSION || reply.request_id != request_id {
        return Err(PythonError::Protocol("callback identity mismatch".into()));
    }
    match reply.result {
        RpcResult::Ok(reply) => Ok(*reply),
        RpcResult::Error(error) => Err(error.into()),
    }
}
