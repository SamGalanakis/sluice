//! Native engine supervision and agent builtin composition.
pub mod delivery;
pub mod doctor;
pub use doctor::engine_diagnostics;
pub mod engines;
pub mod git;
pub mod model;
pub mod prompt;
pub mod quiet;
pub mod reprime;
pub mod supervisor;

use engines::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sluice_model::{
    error::PublicError,
    rpc::{FnInvocation, JsonMap, JsonValue, decode_json},
};
use sluice_process::{guardian::FnHost, tmux::ApprovedTmux};
use std::{
    collections::BTreeMap,
    future::Future,
    io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use supervisor::*;
use tokio_util::sync::CancellationToken;

pub const AGENT_BUILTINS: [&str; 6] = [
    "agent.run",
    "agent.review",
    "decide.llm",
    "agent.codex",
    "agent.claude",
    "agent.devin",
];
/// The agent builtins with a `model` input, whose result names the model the run resolved.
pub const MODEL_BUILTINS: [&str; 4] = ["agent.run", "agent.codex", "agent.devin", "agent.claude"];
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentDispatchError {
    EngineNotBuilt { engine: String },
    UnknownBuiltin { name: String },
}
pub fn adapter_not_built(engine: &str) -> AgentDispatchError {
    AgentDispatchError::EngineNotBuilt {
        engine: engine.into(),
    }
}

/// The composition root creates this from the frozen reservation and guardian identity.
pub struct AgentEnvironment<E, H> {
    pub config: SupervisorConfig,
    pub prompt: prompt::PromptContext,
    pub engine: E,
    pub host: H,
    pub tmux: Option<Arc<ApprovedTmux>>,
    pub cancel: CancellationToken,
}
pub trait AgentFactory: Send + Sync {
    type Engine: EngineAdapter;
    type Host: SupervisorHost;
    fn environment(
        &self,
        invocation: &FnInvocation,
    ) -> impl Future<Output = Result<AgentEnvironment<Self::Engine, Self::Host>, AgentFailure>> + Send;
}
/// One host per guardian run. It holds session locks across composed transient calls.
/// Concurrent composition is refused; sequential calls use distinct invocation directories.
pub struct AgentFnHost<F> {
    pub factory: F,
    active: AtomicBool,
    sessions: tokio::sync::Mutex<SessionGuard>,
}
impl<F> AgentFnHost<F> {
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            active: AtomicBool::new(false),
            sessions: tokio::sync::Mutex::new(SessionGuard::default()),
        }
    }
}
struct Admission<'a>(&'a AtomicBool);
impl Drop for Admission<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
impl<F: AgentFactory> AgentFnHost<F> {
    /// ctx.builtin calls this entry. A Transient returns intact to the helper; no nested retry.
    pub async fn compose(&self, invocation: FnInvocation) -> Result<JsonMap, AgentFailure> {
        self.execute(invocation, RetryOwner::OuterHelper).await
    }
    pub async fn execute(
        &self,
        invocation: FnInvocation,
        owner: RetryOwner,
    ) -> Result<JsonMap, AgentFailure> {
        if self
            .active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(AgentFailure {
                kind: FailureKind::Invalid,
                message: "concurrent agent composition within one run is refused".into(),
                session: None,
            });
        }
        let _admission = Admission(&self.active);
        let mut environment = self.factory.environment(&invocation).await?;
        if environment.config.run != invocation.run
            || environment.config.attempt != invocation.attempt
            || environment.config.invocation != invocation.invocation
        {
            return Err(bad("agent environment differs from reservation"));
        }
        let request =
            AgentBuiltinRequest::build(&invocation.name, &invocation.inputs, &environment.prompt)?;
        environment.config.engine.clone_from(&request.engine);
        if environment.engine.profile().engine != request.engine {
            return Err(bad("factory selected another engine"));
        }
        environment.config.task.clone_from(&request.task);
        environment.config.cwd.clone_from(&request.cwd);
        environment.config.session.clone_from(&request.session);
        environment.config.model.clone_from(&request.model);
        environment.config.required = request.required.clone();
        environment.config.retry = request.retry;
        environment.config.retry.owner = owner;
        if request.name == "agent.review"
            && git::head(&environment.config.cwd)
                .await
                .map_err(|e| bad(e.to_string()))?
                .is_none()
        {
            return Err(bad("agent.review requires a git worktree with HEAD"));
        }
        let run_dir = environment.config.run_dir.clone();
        let mut sessions = self.sessions.lock().await;
        let result = supervise(
            environment.config,
            &mut environment.engine,
            &mut environment.host,
            &mut sessions,
            environment.tmux.as_deref(),
            &environment.cancel,
        )
        .await?;
        let fields = environment
            .host
            .snapshot(sluice_model::ids::MessageId(0))
            .await
            .map_err(|e| bad(e.to_string()))?
            .submissions;
        let mut outputs = request.outputs(result, &fields)?;
        engine_log(&request, &run_dir, &invocation.inputs, &mut outputs)?;
        Ok(outputs)
    }
}
/// `agent.codex` and `agent.devin` return their engine log: copied to the `log` input's path
/// when one is given, else the run's own.
fn engine_log(
    request: &AgentBuiltinRequest,
    run_dir: &std::path::Path,
    inputs: &JsonMap,
    outputs: &mut JsonMap,
) -> Result<(), AgentFailure> {
    if !matches!(request.name.as_str(), "agent.codex" | "agent.devin") {
        return Ok(());
    }
    let source = run_dir.join(format!("{}.log", request.engine));
    let path = match inputs.0.get("log").map(JsonValue::as_value) {
        Some(Value::String(path)) if !path.is_empty() => {
            let path = PathBuf::from(path);
            std::fs::copy(&source, &path).map_err(|e| bad(format!("copy engine log: {e}")))?;
            path
        }
        None | Some(Value::Null) | Some(Value::String(_)) => source,
        _ => return Err(bad("log must be a string")),
    };
    outputs.0.insert(
        "log".into(),
        JsonValue::try_from(Value::String(path.to_string_lossy().into_owned()))
            .map_err(|e| bad(e.to_string()))?,
    );
    Ok(())
}

/// The outputs the done signal gives a bare agent fn's run (`name`, frozen `inputs`) that has
/// submitted `submission`, derived after the fact for `step_settle`: the session, last
/// message and git baseline its supervisor checkpointed in `run_dir` (`native.json`, read by
/// field so an older release's checkpoint reads too), the git facts of its `cwd` now, and the
/// model its `model` input composes. An `Err` says why they cannot be derived.
pub async fn settled_outputs(
    name: &str,
    inputs: &JsonMap,
    run_dir: &std::path::Path,
    submission: &BTreeMap<String, Value>,
) -> Result<JsonMap, String> {
    if !AGENT_BUILTINS.contains(&name) {
        return Err(format!("{name} is not an agent fn"));
    }
    // A run launched before the model object took the retired string form; its frozen
    // result has no `model` output, so the request is built without it.
    let request = match AgentBuiltinRequest::build(name, inputs, &prompt::PromptContext::default())
    {
        Ok(request) => request,
        Err(first) => {
            let mut bare = inputs.clone();
            bare.0.shift_remove("model");
            bare.0.shift_remove("effort");
            let mut request =
                AgentBuiltinRequest::build(name, &bare, &prompt::PromptContext::default())
                    .map_err(|_| first.message.clone())?;
            request.model = None;
            request
        }
    };
    let checkpoint: Value = match std::fs::read(run_dir.join("native.json")) {
        Ok(bytes) => decode_json::<JsonValue>(&bytes)
            .map_err(|e| format!("its supervisor's checkpoint does not read: {e}"))?
            .into_value(),
        Err(e) => {
            return Err(format!(
                "its supervisor left no checkpoint in {} ({e})",
                run_dir.display()
            ));
        }
    };
    let session = checkpoint["session"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("its supervisor recorded no session")?
        .to_owned();
    let final_text = checkpoint["final_text"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let git = git::facts(&request.cwd, checkpoint["head_before"].as_str())
        .await
        .map_err(|e| format!("git facts of {}: {e}", request.cwd.display()))?;
    let model = request
        .model
        .clone()
        .or_else(|| model::default_for(&request.engine))
        .and_then(|choice| model::compose(&request.engine, &choice).ok())
        .map(|resolved| resolved.id);
    let result = AgentResult {
        final_text,
        report: None,
        session,
        git,
        notes: vec![],
        model,
    };
    let mut outputs = request.outputs(result, submission).map_err(|e| e.message)?;
    engine_log(&request, run_dir, inputs, &mut outputs).map_err(|e| e.message)?;
    Ok(outputs)
}
impl<F: AgentFactory> FnHost for AgentFnHost<F> {
    async fn invoke(&self, invocation: FnInvocation) -> Result<JsonMap, PublicError> {
        self.execute(invocation, RetryOwner::Standalone)
            .await
            .map_err(|error| match error.kind {
                FailureKind::Transient => PublicError::Transient {
                    message: error.message,
                },
                _ => PublicError::AgentFailure {
                    kind: format!("{:?}", error.kind),
                    message: error.message,
                    session: error.session,
                },
            })
    }
}
fn bad(message: impl Into<String>) -> AgentFailure {
    AgentFailure {
        kind: FailureKind::Invalid,
        message: message.into(),
        session: None,
    }
}
#[derive(Debug, Clone)]
pub struct AgentBuiltinRequest {
    pub name: String,
    pub engine: String,
    pub cwd: PathBuf,
    pub task: String,
    pub session: Option<String>,
    /// The `model` input; `None` runs the engine default.
    pub model: Option<model::ModelChoice>,
    pub required: Vec<String>,
    pub report_path: Option<PathBuf>,
    pub retry: RetryPolicy,
    pub decision_options: Vec<String>,
    pub threshold: f64,
}
impl AgentBuiltinRequest {
    pub fn build(
        name: &str,
        inputs: &JsonMap,
        context: &prompt::PromptContext,
    ) -> Result<Self, AgentFailure> {
        if !AGENT_BUILTINS.contains(&name) {
            return Err(bad(format!("unknown agent builtin {name}")));
        }
        let string = |key: &str| -> Result<Option<String>, AgentFailure> {
            match inputs.0.get(key).map(JsonValue::as_value) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.clone())),
                _ => Err(bad(format!("{key} must be a string"))),
            }
        };
        let required = |key: &str| string(key)?.ok_or_else(|| bad(format!("{key} is required")));
        let engine = match name {
            "agent.claude" | "agent.review" | "decide.llm" => "claude".into(),
            "agent.codex" => "codex".into(),
            "agent.devin" => "devin".into(),
            _ => required("engine")?,
        };
        if !["claude", "codex", "devin", "fake"].contains(&engine.as_str()) {
            return Err(bad(format!("unknown engine {engine}")));
        }
        let model = model::from_inputs(
            &engine,
            inputs.0.get("model").map(JsonValue::as_value),
            inputs.0.get("effort").map(JsonValue::as_value),
        )
        .map_err(bad)?;
        let listen = match inputs.0.get("listen").map(JsonValue::as_value) {
            None | Some(Value::Null) => true,
            Some(Value::Bool(b)) => *b,
            _ => return Err(bad("listen must be boolean")),
        };
        let mut ctx = context.clone();
        ctx.listen = listen;
        let mut options = Vec::new();
        let mut threshold = 0.8;
        let text = match name {
            "agent.claude" => required("prompt")?,
            "agent.review" => prompt::review(
                &required("base")?,
                &required("standards")?,
                &string("notes")?.unwrap_or_default(),
            ),
            "decide.llm" => {
                options = inputs
                    .0
                    .get("options")
                    .and_then(|v| v.as_value().as_array())
                    .ok_or_else(|| bad("options must be an array"))?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| bad("options must be strings"))
                    })
                    .collect::<Result<_, _>>()?;
                if let Some(v) = inputs.0.get("threshold")
                    && !v.as_value().is_null()
                {
                    threshold = v
                        .as_value()
                        .as_f64()
                        .ok_or_else(|| bad("threshold must be a number"))?;
                }
                if !(0.0..=1.0).contains(&threshold) {
                    return Err(bad("threshold must be in 0..1"));
                }
                let (text, declarations) = prompt::decision(
                    &required("question")?,
                    &options,
                    inputs.0.get("context").map(JsonValue::as_value),
                )
                .map_err(|e| bad(e.to_string()))?;
                for (name, port) in declarations {
                    if ctx.outputs.insert(name, port).is_some() {
                        return Err(bad("decision declaration collides with outer output"));
                    }
                }
                text
            }
            _ => required("spec")?,
        };
        let values = inputs
            .0
            .iter()
            .map(|(k, v)| (k.clone(), v.as_value().clone()))
            .collect();
        let mut retry = if name == "decide.llm" {
            RetryPolicy::decision()
        } else {
            RetryPolicy::agent()
        };
        if let Some(raw) = std::env::var_os("SLUICE_BACKOFF") {
            let seconds = raw
                .to_str()
                .ok_or_else(|| bad("SLUICE_BACKOFF is not UTF-8"))?
                .parse::<f64>()
                .map_err(|_| bad("invalid SLUICE_BACKOFF"))?;
            retry.backoff = std::time::Duration::try_from_secs_f64(seconds)
                .map_err(|_| bad("invalid SLUICE_BACKOFF"))?;
        }
        Ok(Self {
            name: name.into(),
            engine,
            cwd: PathBuf::from(if name == "decide.llm" {
                string("cwd")?.unwrap_or_else(|| ".".into())
            } else {
                required("cwd")?
            }),
            task: prompt::build(&text, &values, &ctx),
            session: string("session")?,
            model,
            required: prompt::required_outputs(&ctx),
            report_path: string("report_path")?.map(PathBuf::from),
            retry,
            decision_options: options,
            threshold,
        })
    }
    pub fn outputs(
        &self,
        result: AgentResult,
        submissions: &BTreeMap<String, Value>,
    ) -> Result<JsonMap, AgentFailure> {
        let mut fields = submissions.clone();
        let report = self
            .report_path
            .as_ref()
            .map(|path| {
                match std::fs::read_to_string(if path.is_absolute() {
                    path.clone()
                } else {
                    self.cwd.join(path)
                }) {
                    Ok(text) => Ok(Some(text)),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(bad(e.to_string())),
                }
            })
            .transpose()?
            .flatten();
        match self.name.as_str() {
            "agent.review" => {
                let facts = result
                    .git
                    .as_ref()
                    .ok_or_else(|| bad("agent.review requires a git worktree with HEAD"))?;
                fields.insert("summary".into(), Value::String(result.final_text.clone()));
                fields.insert("sha".into(), Value::String(facts.head_after.clone()));
                fields.insert("commits".into(), Value::from(facts.commits));
            }
            "agent.claude" => {
                fields.insert("result".into(), Value::String(result.final_text.clone()));
            }
            "decide.llm" => {
                let choice = fields
                    .get("choice")
                    .and_then(Value::as_str)
                    .ok_or_else(|| bad("choice not submitted"))?;
                let probability = fields
                    .get("p")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| bad("p not submitted"))?;
                if !self.decision_options.iter().any(|o| o == choice) {
                    return Err(bad("choice not in options"));
                }
                if !(0.0..=1.0).contains(&probability) {
                    return Err(bad("p must be in 0..1"));
                }
                fields.insert(
                    "confident".into(),
                    Value::Bool(probability >= self.threshold),
                );
            }
            _ => {
                fields.insert("final".into(), Value::String(result.final_text.clone()));
                fields.insert(
                    "report".into(),
                    report.map(Value::String).unwrap_or(Value::Null),
                );
            }
        }
        if MODEL_BUILTINS.contains(&self.name.as_str())
            && let Some(model) = result.model
        {
            fields.insert("model".into(), Value::String(model));
        }
        if self.name != "decide.llm" {
            fields.insert("session".into(), Value::String(result.session));
            if let Some(facts) = result.git {
                fields.insert(
                    "git".into(),
                    serde_json::to_value(facts).map_err(|e| bad(e.to_string()))?,
                );
            }
        }
        fields
            .into_iter()
            .map(|(k, v)| {
                JsonValue::try_from(v)
                    .map(|v| (k, v))
                    .map_err(|e| bad(e.to_string()))
            })
            .collect::<Result<_, _>>()
            .map(JsonMap)
    }
}

/// The one model the fixture engines list and run.
pub const FIXTURE_MODEL: &str = "fixture";
/// A deterministic adapter for pure policy tests and scripted fixture processes.
pub struct ScriptedEngine {
    pub profile: EngineProfile,
    pub frames: std::collections::VecDeque<ScriptFrame>,
    pub commands: Vec<EngineCommand>,
    pub observation: EngineObservation,
    pub sessions: BTreeMap<String, SessionMetadata>,
    pub closed: u32,
    pub hook_reply: Option<HookReply>,
    pub hooks: Vec<HookEvent>,
}
impl ScriptedEngine {
    pub fn new(frames: Vec<ScriptFrame>) -> Self {
        Self {
            profile: EngineProfile {
                engine: "fake".into(),
                version_range: "fixture-v1".into(),
                required_capabilities: vec![],
                default_model: Some(model::ModelChoice::normal(FIXTURE_MODEL, None)),
                reports_waiting: true,
            },
            frames: frames.into(),
            commands: vec![],
            observation: EngineObservation::default(),
            sessions: BTreeMap::new(),
            closed: 0,
            hook_reply: None,
            hooks: vec![],
        }
    }
    async fn advance(&mut self) -> Result<DeliveryOutcome, EngineError> {
        let frame = self.frames.pop_front().expect("frame");
        tokio::time::sleep(std::time::Duration::from_millis(frame.delay_ms)).await;
        self.observation = frame.observation;
        match frame.error {
            Some(error) => Err(error),
            None => Ok(frame.outcome),
        }
    }
}
impl EngineAdapter for ScriptedEngine {
    fn profile(&self) -> EngineProfile {
        self.profile.clone()
    }
    /// Lists exactly its profile's default model, so a test may give it a real engine's
    /// profile.
    async fn models(&mut self) -> Result<Vec<String>, EngineError> {
        Ok(self
            .profile
            .default_model
            .iter()
            .filter_map(|m| model::compose(&self.profile.engine, m).ok())
            .map(|m| m.id)
            .collect())
    }
    async fn session(&mut self, session: &str) -> Result<Option<SessionMetadata>, EngineError> {
        Ok(self.sessions.get(session).cloned())
    }
    async fn prepare(
        &mut self,
        _context: &EngineContext,
        _session: Option<&str>,
    ) -> Result<Option<EngineLaunch>, EngineError> {
        Ok(None)
    }
    async fn execute(
        &mut self,
        _context: &EngineContext,
        command: EngineCommand,
    ) -> Result<DeliveryOutcome, EngineError> {
        self.commands.push(command.clone());
        if self.frames.front().is_some_and(|frame| {
            frame
                .command
                .as_ref()
                .is_some_and(|expected| command_matches(expected, &command))
        }) {
            self.advance().await
        } else {
            Ok(DeliveryOutcome::Acknowledged)
        }
    }
    async fn observe(
        &mut self,
        _context: &EngineContext,
    ) -> Result<EngineObservation, EngineError> {
        if self
            .frames
            .front()
            .is_some_and(|frame| frame.command.is_none())
        {
            let _ = self.advance().await?;
        }
        Ok(self.observation.clone())
    }
    async fn close(&mut self) -> io::Result<()> {
        self.closed += 1;
        Ok(())
    }
    fn on_hook(&mut self, hook: HookEvent) -> Result<HookReply, EngineError> {
        self.hooks.push(hook);
        self.hook_reply.clone().ok_or_else(|| EngineError {
            kind: EngineErrorKind::CapabilityMismatch,
            message: "fixture hooks unsupported".into(),
        })
    }
}
pub fn command_matches(expected: &EngineCommand, actual: &EngineCommand) -> bool {
    match (expected, actual) {
        (
            EngineCommand::DeliverText { id: a, text },
            EngineCommand::DeliverText { id: b, text: body },
        )
        | (EngineCommand::Steer { id: a, text }, EngineCommand::Steer { id: b, text: body }) => {
            a == b && (text == "*" || text == body)
        }
        _ => expected == actual,
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum FixtureRequest {
    Command { command: EngineCommand },
    Observe,
    Hook { hook: HookEvent },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureReply {
    pub observation: EngineObservation,
    pub outcome: DeliveryOutcome,
    pub error: Option<EngineError>,
    pub hook: Option<HookReply>,
}
/// Line-delimited JSON fixture. Scripts never execute shell or provider commands.
pub fn fixture_engine_main(script: &std::path::Path) -> io::Result<()> {
    use std::io::{BufRead, Read, Write};
    let frames: Vec<ScriptFrame> =
        decode_json(&std::fs::read(script)?).map_err(io::Error::other)?;
    let mut frames: std::collections::VecDeque<_> = frames.into();
    let mut observation = EngineObservation::default();
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    loop {
        let mut bytes = Vec::new();
        let count = input
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_until(b'\n', &mut bytes)?;
        if count == 0 {
            break;
        }
        if count > 1024 * 1024 {
            return Err(io::Error::other("fixture input too large"));
        }
        let request: FixtureRequest = decode_json(&bytes).map_err(io::Error::other)?;
        let matches = frames.front().is_some_and(|frame| match &request {
            FixtureRequest::Command { command } => frame
                .command
                .as_ref()
                .is_some_and(|e| command_matches(e, command)),
            FixtureRequest::Observe => frame.command.is_none(),
            FixtureRequest::Hook { .. } => false,
        });
        let mut outcome = DeliveryOutcome::Acknowledged;
        let mut error = None;
        if matches {
            let frame = frames.pop_front().expect("frame");
            std::thread::sleep(std::time::Duration::from_millis(frame.delay_ms));
            observation = frame.observation;
            outcome = frame.outcome;
            error = frame.error;
        }
        serde_json::to_writer(
            &mut output,
            &FixtureReply {
                observation: observation.clone(),
                outcome,
                error,
                hook: None,
            },
        )
        .map_err(io::Error::other)?;
        output.write_all(b"\n")?;
        output.flush()?;
        if matches!(
            request,
            FixtureRequest::Command {
                command: EngineCommand::RequestExit
            }
        ) {
            break;
        }
    }
    Ok(())
}

/// CLI entry for `sluice agent-hook <engine> <event>`. The application owns the process;
/// this synchronous client has bounded stdin/frame bytes and socket read/write deadlines.
pub fn engine_hook_cli(
    engine: &str,
    event: &str,
    explicit_run: Option<sluice_model::ids::RunId>,
) -> Result<i32, PublicError> {
    use sluice_model::{
        ids::{InvocationId, RunId},
        rpc::{PROTOCOL_VERSION, RequestId, RunCapability},
    };
    use sluice_process::socket::{ControlCommand, ControlReply, Reply, Request};
    use std::io::{Read, Write};
    let invalid = |message: String| PublicError::BadRequest { message };
    if !["codex", "claude", "devin", "fake"].contains(&engine)
        || event.is_empty()
        || event.len() > 128
    {
        return Err(invalid("invalid engine hook arguments".into()));
    }
    let directory = std::env::var_os("SLUICE_RUN_DIR")
        .ok_or_else(|| invalid("SLUICE_RUN_DIR is required".into()))?;
    let directory = sluice_process::host::guard_scratch_home(&PathBuf::from(directory))?;
    let run: RunId = std::env::var("SLUICE_RUN_ID")
        .map_err(|_| invalid("SLUICE_RUN_ID is required".into()))?
        .parse()
        .map_err(|e| invalid(format!("invalid run id: {e}")))?;
    if explicit_run.is_some_and(|explicit| explicit != run) {
        return Err(invalid("hook run id differs from admitted run".into()));
    }
    let capability: RunCapability = decode_json(
        &std::fs::read(directory.join("hook-capability.json"))
            .map_err(|e| invalid(e.to_string()))?,
    )?;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| invalid(e.to_string()))?;
    if bytes.len() > 1024 * 1024 {
        return Err(invalid("engine hook stdin exceeds 1 MiB".into()));
    }
    let payload: JsonValue = decode_json(&bytes)?;
    let request_id = RequestId(InvocationId::new().to_string());
    let command = ControlCommand::EngineHook {
        engine: engine.into(),
        run,
        event: event.into(),
        payload,
    };
    let request = Request {
        protocol: PROTOCOL_VERSION,
        request_id: request_id.clone(),
        run_capability: Some(capability),
        command,
    };
    let body = serde_json::to_vec(&request).map_err(|e| invalid(e.to_string()))?;
    // A composed agent's run directory (`runs/<run>/invocations/<invocation>`) makes the
    // socket's absolute path longer than the 108 bytes a Unix socket address holds, so
    // connect relative to the directory, as the private tmux does with its socket. This
    // process is short-lived and has no other threads to see the working directory change.
    std::env::set_current_dir(&directory).map_err(|e| invalid(e.to_string()))?;
    let mut stream = std::os::unix::net::UnixStream::connect("control.sock")
        .map_err(|e| invalid(format!("cannot reach the run's control socket: {e}")))?;
    let timeout = Some(std::time::Duration::from_secs(5));
    stream
        .set_read_timeout(timeout)
        .map_err(|e| invalid(e.to_string()))?;
    stream
        .set_write_timeout(timeout)
        .map_err(|e| invalid(e.to_string()))?;
    stream
        .write_all(&(body.len() as u32).to_be_bytes())
        .and_then(|_| stream.write_all(&body))
        .map_err(|e| invalid(format!("hook acceptance unknown; do not replay: {e}")))?;
    let mut size = [0; 4];
    stream
        .read_exact(&mut size)
        .map_err(|e| invalid(format!("hook reply unknown; do not replay: {e}")))?;
    let size = u32::from_be_bytes(size) as usize;
    if size == 0 || size > 1024 * 1024 {
        return Err(invalid("invalid engine hook reply size".into()));
    }
    let mut bytes = vec![0; size];
    stream
        .read_exact(&mut bytes)
        .map_err(|e| invalid(e.to_string()))?;
    let reply: Reply<ControlReply> = decode_json(&bytes)?;
    if reply.protocol != PROTOCOL_VERSION || reply.request_id != request_id {
        return Err(invalid("engine hook reply identity mismatch".into()));
    }
    let ControlReply::EngineHook(reply) = reply.result? else {
        return Err(invalid("unexpected engine hook reply".into()));
    };
    if !(0..=255).contains(&reply.exit_code) {
        return Err(invalid("invalid engine hook exit status".into()));
    }
    if let Some(stdout) = reply.stdout {
        println!("{}", stdout.as_value());
    }
    Ok(reply.exit_code)
}

/// Scripted fixture client used by integration tests. It starts only the explicitly supplied
/// fixture executable, with a guarded scratch home, and retains its child wait authority.
pub struct FixtureEngine {
    pub binary: PathBuf,
    pub script: PathBuf,
    pub home: PathBuf,
    child: Option<tokio::process::Child>,
    input: Option<tokio::process::ChildStdin>,
    output: Option<tokio::io::BufReader<tokio::process::ChildStdout>>,
}
impl FixtureEngine {
    pub fn new(binary: PathBuf, script: PathBuf, home: PathBuf) -> Self {
        Self {
            binary,
            script,
            home,
            child: None,
            input: None,
            output: None,
        }
    }
    async fn exchange(&mut self, request: FixtureRequest) -> Result<FixtureReply, EngineError> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let fail = |e: String| EngineError {
            kind: EngineErrorKind::UnknownAcceptance,
            message: e,
        };
        let mut bytes = serde_json::to_vec(&request).map_err(|e| fail(e.to_string()))?;
        bytes.push(b'\n');
        self.input
            .as_mut()
            .ok_or_else(|| fail("fixture is not started".into()))?
            .write_all(&bytes)
            .await
            .map_err(|e| fail(e.to_string()))?;
        let mut bytes = Vec::new();
        let reader = self
            .output
            .as_mut()
            .ok_or_else(|| fail("fixture stdout missing".into()))?;
        // A bounded fill_buf loop avoids an unbounded line allocation.
        loop {
            let part = reader.fill_buf().await.map_err(|e| fail(e.to_string()))?;
            if part.is_empty() {
                return Err(fail("fixture stdout closed".into()));
            }
            let count = part
                .iter()
                .position(|b| *b == b'\n')
                .map(|n| n + 1)
                .unwrap_or(part.len());
            let done = part[count - 1] == b'\n';
            if bytes.len() + count > 1024 * 1024 {
                return Err(fail("fixture reply too large".into()));
            }
            bytes.extend_from_slice(&part[..count]);
            reader.consume(count);
            if done {
                break;
            }
        }
        decode_json(&bytes).map_err(|e| fail(e.to_string()))
    }
}
impl EngineAdapter for FixtureEngine {
    fn profile(&self) -> EngineProfile {
        ScriptedEngine::new(vec![]).profile
    }
    async fn models(&mut self) -> Result<Vec<String>, EngineError> {
        Ok(vec![FIXTURE_MODEL.into()])
    }
    async fn session(&mut self, _session: &str) -> Result<Option<SessionMetadata>, EngineError> {
        Ok(None)
    }
    async fn prepare(
        &mut self,
        context: &EngineContext,
        _session: Option<&str>,
    ) -> Result<Option<EngineLaunch>, EngineError> {
        sluice_process::host::guard_scratch_home(&self.home).map_err(|e| EngineError {
            kind: EngineErrorKind::Fatal,
            message: e.to_string(),
        })?;
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .arg("engine")
            .arg(&self.script)
            .current_dir(&context.cwd)
            .env("SLUICE_HOME", &self.home)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|e| EngineError {
            kind: EngineErrorKind::Fatal,
            message: e.to_string(),
        })?;
        self.input = child.stdin.take();
        self.output = child.stdout.take().map(tokio::io::BufReader::new);
        self.child = Some(child);
        Ok(None)
    }
    async fn execute(
        &mut self,
        _context: &EngineContext,
        command: EngineCommand,
    ) -> Result<DeliveryOutcome, EngineError> {
        let reply = self.exchange(FixtureRequest::Command { command }).await?;
        if let Some(error) = reply.error {
            Err(error)
        } else {
            Ok(reply.outcome)
        }
    }
    async fn observe(
        &mut self,
        _context: &EngineContext,
    ) -> Result<EngineObservation, EngineError> {
        let reply = self.exchange(FixtureRequest::Observe).await?;
        if let Some(error) = reply.error {
            Err(error)
        } else {
            Ok(reply.observation)
        }
    }
    async fn close(&mut self) -> io::Result<()> {
        self.input.take();
        self.output.take();
        if let Some(mut child) = self.child.take() {
            child.kill().await?;
        }
        Ok(())
    }
}
