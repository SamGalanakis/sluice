//! Reservation-to-supervisor adapter. No admission or store ownership lives here.
use crate::{
    compose::{ReservationContext, failure},
    execution::Launch,
};
use serde_json::json;
use sluice_agents::{
    AgentEnvironment, AgentFactory, AgentFnHost, FixtureEngine,
    engines::*,
    prompt::{Port, PromptContext},
    supervisor::*,
};
use sluice_model::{commands::*, ids::*, rpc::*};
use sluice_process::{
    identity::{OwnedProcess, ProcessIdentity, Signal},
    journal::DeliveryAck,
    socket::{
        self, CoordinatorCommand as C, CoordinatorLink, CoordinatorReply as R, UnixCoordinatorLink,
    },
    tmux::ApprovedTmux,
};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

pub enum Adapter {
    Codex(Box<codex::Codex>),
    Claude(Box<claude::Claude>),
    Devin(Box<devin::Devin>),
    Fake(Box<FixtureEngine>),
}
macro_rules! adapter_call {
    ($self:expr, $method:ident $(, $arg:expr)*) => { match $self { Adapter::Codex(e) => e.$method($($arg),*), Adapter::Claude(e) => e.$method($($arg),*), Adapter::Devin(e) => e.$method($($arg),*), Adapter::Fake(e) => e.$method($($arg),*) } };
}
impl EngineAdapter for Adapter {
    fn profile(&self) -> EngineProfile {
        adapter_call!(self, profile)
    }
    fn version(&self) -> Option<version::Verdict> {
        adapter_call!(self, version)
    }
    fn recover_in_place(&mut self, error: &EngineError) -> bool {
        adapter_call!(self, recover_in_place, error)
    }
    fn on_hook(&mut self, hook: HookEvent) -> Result<HookReply, EngineError> {
        adapter_call!(self, on_hook, hook)
    }
    async fn models(&mut self) -> Result<Vec<String>, EngineError> {
        match self {
            Self::Codex(e) => e.models().await,
            Self::Claude(e) => e.models().await,
            Self::Devin(e) => e.models().await,
            Self::Fake(e) => e.models().await,
        }
    }
    async fn session(&mut self, session: &str) -> Result<Option<SessionMetadata>, EngineError> {
        match self {
            Self::Codex(e) => e.session(session).await,
            Self::Claude(e) => e.session(session).await,
            Self::Devin(e) => e.session(session).await,
            Self::Fake(e) => {
                let path = e.home.join("fake-sessions.json");
                let sessions: BTreeMap<String, PathBuf> = std::fs::read(path)
                    .ok()
                    .map(|b| decode_json(&b))
                    .transpose()
                    .map_err(|e| EngineError {
                        kind: EngineErrorKind::Fatal,
                        message: e.to_string(),
                        retry_at: None,
                    })?
                    .unwrap_or_default();
                Ok(sessions.get(session).map(|cwd| SessionMetadata {
                    id: session.into(),
                    cwd: cwd.clone(),
                }))
            }
        }
    }
    async fn prepare(
        &mut self,
        ctx: &EngineContext,
        session: Option<&str>,
    ) -> Result<Option<EngineLaunch>, EngineError> {
        match self {
            Self::Codex(e) => e.prepare(ctx, session).await,
            Self::Claude(e) => e.prepare(ctx, session).await,
            Self::Devin(e) => e.prepare(ctx, session).await,
            Self::Fake(e) => e.prepare(ctx, session).await,
        }
    }
    async fn execute(
        &mut self,
        ctx: &EngineContext,
        cmd: EngineCommand,
    ) -> Result<DeliveryOutcome, EngineError> {
        match self {
            Self::Codex(e) => e.execute(ctx, cmd).await,
            Self::Claude(e) => e.execute(ctx, cmd).await,
            Self::Devin(e) => e.execute(ctx, cmd).await,
            Self::Fake(e) => e.execute(ctx, cmd).await,
        }
    }
    async fn observe(&mut self, ctx: &EngineContext) -> Result<EngineObservation, EngineError> {
        match self {
            Self::Codex(e) => e.observe(ctx).await,
            Self::Claude(e) => e.observe(ctx).await,
            Self::Devin(e) => e.observe(ctx).await,
            Self::Fake(e) => e.observe(ctx).await,
        }
    }
    async fn close(&mut self) -> io::Result<()> {
        match self {
            Self::Codex(e) => e.close().await,
            Self::Claude(e) => e.close().await,
            Self::Devin(e) => e.close().await,
            Self::Fake(e) => e.close().await,
        }
    }
}
/// Acceptance-only fault boundary; normal adapter state and session maps remain authoritative.
pub struct FixtureFaultEngine {
    inner: Adapter,
    marker: Option<PathBuf>,
}
impl FixtureFaultEngine {
    fn new(inner: Adapter, home: &Path) -> Result<Self, AgentFailure> {
        let marker = std::env::var_os("SLUICE_TEST_AGENT_TRANSIENT_MARKER").map(PathBuf::from);
        if let Some(path) = &marker
            && (std::env::var_os("SLUICE_FIXTURE").is_none()
                || !path.is_absolute()
                || !path
                    .parent()
                    .ok_or_else(|| invalid("fault marker parent missing"))?
                    .canonicalize()
                    .map_err(invalid)?
                    .starts_with(home.canonicalize().map_err(invalid)?))
        {
            return Err(invalid(
                "fault marker requires SLUICE_FIXTURE and a path under its private home",
            ));
        }
        Ok(Self { inner, marker })
    }
}
impl EngineAdapter for FixtureFaultEngine {
    fn profile(&self) -> EngineProfile {
        self.inner.profile()
    }
    fn version(&self) -> Option<version::Verdict> {
        self.inner.version()
    }
    fn recover_in_place(&mut self, error: &EngineError) -> bool {
        self.inner.recover_in_place(error)
    }
    fn on_hook(&mut self, hook: HookEvent) -> Result<HookReply, EngineError> {
        self.inner.on_hook(hook)
    }
    async fn models(&mut self) -> Result<Vec<String>, EngineError> {
        self.inner.models().await
    }
    async fn session(&mut self, session: &str) -> Result<Option<SessionMetadata>, EngineError> {
        self.inner.session(session).await
    }
    async fn prepare(
        &mut self,
        ctx: &EngineContext,
        session: Option<&str>,
    ) -> Result<Option<EngineLaunch>, EngineError> {
        self.inner.prepare(ctx, session).await
    }
    async fn execute(
        &mut self,
        ctx: &EngineContext,
        cmd: EngineCommand,
    ) -> Result<DeliveryOutcome, EngineError> {
        self.inner.execute(ctx, cmd).await
    }
    async fn observe(&mut self, ctx: &EngineContext) -> Result<EngineObservation, EngineError> {
        let mut observed = self.inner.observe(ctx).await?;
        if observed.turns_completed > 0
            && observed.error.is_none()
            && let Some(marker) = self.marker.as_ref()
        {
            let mut receipt = marker.as_os_str().to_os_string();
            receipt.push(".injected");
            let receipt = PathBuf::from(receipt);
            if receipt.exists() {
                self.marker = None;
            } else if std::fs::symlink_metadata(marker).is_ok_and(|m| m.file_type().is_file()) {
                match std::fs::rename(marker, &receipt) {
                    Ok(()) => {
                        self.marker = None;
                        observed.error = Some(EngineError {
                            kind: EngineErrorKind::Transient,
                            message: "fixture transient after completed turn".into(),
                            retry_at: None,
                        });
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(EngineError {
                            kind: EngineErrorKind::Fatal,
                            message: error.to_string(),
                            retry_at: None,
                        });
                    }
                }
            }
        }
        Ok(observed)
    }
    async fn close(&mut self) -> io::Result<()> {
        self.inner.close().await
    }
}
fn invalid(e: impl std::fmt::Display) -> AgentFailure {
    AgentFailure {
        kind: FailureKind::Invalid,
        message: e.to_string(),
        session: None,
        retry_at: None,
    }
}
fn public(error: AgentFailure) -> sluice_model::error::PublicError {
    if error.kind == FailureKind::Transient {
        sluice_model::error::PublicError::Transient {
            message: error.message,
        }
    } else {
        sluice_model::error::PublicError::AgentFailure {
            kind: format!("{:?}", error.kind),
            message: error.message,
            session: error.session,
        }
    }
}
pub struct Factory {
    home: PathBuf,
    launch: Launch,
    context: ReservationContext,
    cancel: CancellationToken,
}
pub struct RunAgents {
    host: AgentFnHost<Factory>,
    continuation: tokio::sync::Mutex<Option<FnInvocation>>,
}
impl RunAgents {
    pub fn new(
        home: PathBuf,
        launch: Launch,
        context: ReservationContext,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            host: AgentFnHost::new(Factory {
                home,
                launch,
                context,
                cancel,
            }),
            continuation: tokio::sync::Mutex::new(None),
        }
    }
    pub async fn invoke(
        &self,
        invocation: FnInvocation,
    ) -> Result<JsonMap, sluice_model::error::PublicError> {
        self.host
            .execute(invocation, RetryOwner::Standalone)
            .await
            .map_err(public)
    }
    pub async fn compose(
        &self,
        mut invocation: FnInvocation,
    ) -> Result<JsonMap, sluice_model::error::PublicError> {
        let mut pending = self
            .continuation
            .try_lock()
            .map_err(|_| failure("concurrent composition refused"))?;
        if let Some(previous) = pending.as_ref() {
            if previous.name != invocation.name || previous.inputs != invocation.inputs {
                return Err(failure("transient re-entry changed agent request"));
            }
            invocation.invocation = previous.invocation;
        }
        let result = self.host.compose(invocation.clone()).await;
        *pending = result
            .as_ref()
            .err()
            .filter(|e| e.kind == FailureKind::Transient)
            .map(|_| invocation);
        result.map_err(public)
    }
}
impl AgentFactory for Factory {
    type Engine = FixtureFaultEngine;
    type Host = RunHost;
    async fn environment(
        &self,
        invocation: &FnInvocation,
    ) -> Result<AgentEnvironment<FixtureFaultEngine, RunHost>, AgentFailure> {
        let outer_dir = self.home.join("runs").join(invocation.run.to_string());
        let directory = if invocation.invocation == self.launch.invocation.invocation {
            outer_dir.clone()
        } else {
            outer_dir
                .join("invocations")
                .join(invocation.invocation.to_string())
        };
        std::fs::create_dir_all(&directory).map_err(invalid)?;
        let mut prompt = PromptContext {
            project: invocation.project.to_string(),
            step: invocation
                .step
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            run: invocation.run.to_string(),
            listen: true,
            header: callback_header(&std::env::current_exe().map_err(invalid)?),
            ..Default::default()
        };
        for (name, ty) in crate::python::output_types(&self.context.outputs).map_err(invalid)? {
            prompt.outputs.insert(
                name.clone(),
                Port {
                    r#type: ty,
                    doc: self
                        .context
                        .outputs
                        .0
                        .get(&name)
                        .and_then(|v| v.as_value().get("doc"))
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .into(),
                },
            );
        }
        if let Some(step) = &invocation.step {
            prompt.previous = launch_note(
                &self.home,
                invocation.project,
                step,
                invocation.run,
                &invocation.inputs,
            )
            .await;
        }
        let request = sluice_agents::AgentBuiltinRequest::build(
            &invocation.name,
            &invocation.inputs,
            &prompt,
        )?;
        let bin = std::env::current_exe().map_err(invalid)?;
        let mut environment = environment::host_environment();
        // .env secrets go over the host allowlist; the SLUICE_* run variables still win.
        environment.extend(crate::dotenv::run_environment(
            &self.home,
            Some(invocation.project),
        ));
        for (name, value) in [
            ("SLUICE_HOME", self.home.to_string_lossy().into_owned()),
            ("SLUICE_BIN", bin.to_string_lossy().into_owned()),
            ("SLUICE_PROJECT_ID", invocation.project.to_string()),
            ("SLUICE_PROJECT", self.context.project_name.clone()),
            (
                "SLUICE_STEP",
                invocation
                    .step
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            ),
            ("SLUICE_RUN_ID", invocation.run.to_string()),
            ("SLUICE_RUN_DIR", directory.to_string_lossy().into_owned()),
            (
                "SLUICE_PROJECT_DIR",
                self.home
                    .join("projects")
                    .join(invocation.project.to_string())
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "SLUICE_FN_DIR",
                self.context
                    .execution
                    .fn_dir
                    .as_ref()
                    .unwrap_or(&directory)
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "SLUICE_PREV_RUN",
                self.launch
                    .prev_run
                    .map(|run| run.to_string())
                    .unwrap_or_default(),
            ),
            (
                "SLUICE_CONTROL_SOCKET",
                outer_dir
                    .join("control.sock")
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "SLUICE_RUN_CAPABILITY",
                serde_json::to_value(&self.launch.capability)
                    .map_err(invalid)?
                    .as_str()
                    .ok_or_else(|| invalid("invalid capability"))?
                    .to_owned(),
            ),
        ] {
            environment.insert(name.into(), value);
        }
        if std::env::var_os("SLUICE_FIXTURE").is_some() {
            for name in ["SLUICE_FAKE_CLAUDE", "FAKE_DEVIN", "SLUICE_CODEX_FIXTURE"] {
                if let Ok(value) = std::env::var(name) {
                    environment.insert(name.into(), value);
                }
            }
        }
        let quota_threshold = account::threshold_from_env().map_err(invalid)?;
        let screen_grace = screen::grace_from_env().map_err(invalid)?;
        let engine = match request.engine.as_str() {
            "fake" => {
                let binary = std::env::var_os("SLUICE_FAKE_ENGINE_BIN")
                    .ok_or_else(|| invalid("fake engine executable is required"))?;
                let script = std::env::var_os("SLUICE_FAKE_ENGINE_SCRIPT")
                    .ok_or_else(|| invalid("fake engine script is required"))?;
                if !Path::new(&binary).is_absolute() || !Path::new(&script).is_absolute() {
                    return Err(invalid("fake engine paths must be absolute"));
                }
                Adapter::Fake(Box::new(FixtureEngine::new(
                    binary.into(),
                    script.into(),
                    self.home.clone(),
                )))
            }
            "codex" => {
                let mut options = codex::CodexOptions::new(
                    std::env::var_os("SLUICE_CODEX_BIN")
                        .unwrap_or_else(|| "codex".into())
                        .into(),
                    std::env::var_os("CODEX_HOME")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                                .join(".codex")
                        }),
                    self.home.clone(),
                );
                options.environment = environment.clone();
                options.quota_threshold = quota_threshold;
                Adapter::Codex(Box::new(codex::Codex::new(options)))
            }
            "claude" => Adapter::Claude(Box::new(
                claude::Claude::new(
                    std::env::var_os("SLUICE_CLAUDE_BIN")
                        .unwrap_or_else(|| "claude".into())
                        .into(),
                    std::env::var_os("CLAUDE_CONFIG_DIR")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                                .join(".claude")
                        }),
                    bin.clone(),
                    invocation.run,
                )
                .with_mcp(std::env::var("SLUICE_CLAUDE_MCP_CONFIG").ok())
                .with_config_dir_passed(std::env::var_os("CLAUDE_CONFIG_DIR").is_some())
                .with_environment(environment.clone())
                .with_quota_threshold(quota_threshold)
                .with_screen_grace(screen_grace)
                .with_probe_cache(self.home.join(version::CACHE_DIR)),
            )),
            "devin" => Adapter::Devin(Box::new(devin::Devin::new(devin::DevinOptions {
                hook_binary: bin.clone(),
                environment,
                screen_grace,
                probe_cache: Some(self.home.join(version::CACHE_DIR)),
                ..Default::default()
            }))),
            _ => return Err(invalid("unknown engine")),
        };
        let engine = FixtureFaultEngine::new(engine, &self.home)?;
        let tmux = if request.engine == "fake" {
            None
        } else {
            let prefix = std::env::var_os("SLUICE_TMUX_PREFIX")
                .map(PathBuf::from)
                .unwrap_or_else(|| bin.parent().unwrap_or(Path::new(".")).join("private-tmux"));
            Some(Arc::new(
                ApprovedTmux::load(&prefix).await.map_err(invalid)?,
            ))
        };
        let link = UnixCoordinatorLink {
            path: self.home.join("coordinator.sock"),
            capability: self.launch.capability.clone(),
        };
        let mut messages: Vec<socket::DeliveryMessage> =
            decode_json(&std::fs::read(outer_dir.join("messages.json")).map_err(invalid)?)
                .map_err(invalid)?;
        let existing = Checkpoint::read(&directory).map_err(invalid)?;
        let acknowledgements: Vec<DeliveryAck> =
            match std::fs::read(outer_dir.join("delivery.json")) {
                Ok(bytes) => decode_json(&bytes).map_err(invalid)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
                Err(error) => return Err(invalid(error)),
            };
        let acknowledged = |id: MessageId| {
            acknowledgements
                .iter()
                .any(|ack| ack.invocation == self.launch.invocation.invocation && ack.message == id)
        };
        messages.retain(|message| !acknowledged(message.id));
        if directory != outer_dir
            && existing.is_none()
            && let Some(parent) = Checkpoint::read(&outer_dir).map_err(invalid)?
        {
            if parent.run != invocation.run {
                return Err(invalid("parent message checkpoint belongs to another run"));
            }
            let mut delivery = parent.delivery;
            delivery
                .entries
                .retain(|entry| matches!(entry.id, InputId::Message { .. }));
            for entry in &mut delivery.entries {
                if let InputId::Message { id } = entry.id
                    && acknowledged(id)
                {
                    entry.state = sluice_agents::delivery::DeliveryState::Acknowledged;
                }
            }
            let seed = MessageContinuation {
                run: invocation.run,
                delivery,
                live_after: parent.live_after,
            };
            std::fs::write(
                directory.join("message-continuation.json"),
                serde_json::to_vec(&seed).map_err(invalid)?,
            )
            .map_err(invalid)?;
        }
        let previous = self
            .launch
            .prev_run
            .map(|run| Checkpoint::read(&self.home.join("runs").join(run.to_string())))
            .transpose()
            .map_err(invalid)?
            .flatten()
            .map(|c| PreviousSession {
                engine: c.engine,
                cwd: c.cwd,
                session: c.session,
            });
        let limits = if request.engine == "fake" && std::env::var_os("SLUICE_FIXTURE").is_some() {
            Limits::test_profile()
        } else {
            Limits::from_env().map_err(invalid)?
        };
        let host = RunHost {
            link,
            launch: self.launch.clone(),
            invocation: invocation.invocation,
            outer_dir: outer_dir.clone(),
            directory: directory.clone(),
            cancel: self.cancel.clone(),
            patience: PATIENCE,
        };
        // The guardian's hook transport journals in the outer run directory. Each
        // logical supervisor owns that journal only while it is executing.
        if directory != outer_dir {
            use std::os::unix::fs::symlink;
            for name in [sluice_process::hook_journal::DIRECTORY, "control.sock"] {
                let path = directory.join(name);
                if !path.exists() && std::fs::symlink_metadata(&path).is_err() {
                    symlink(outer_dir.join(name), path).map_err(invalid)?;
                }
            }
            std::fs::copy(
                outer_dir.join("hook-capability.json"),
                directory.join("hook-capability.json"),
            )
            .map_err(invalid)?;
        }
        Ok(AgentEnvironment {
            config: SupervisorConfig {
                run: invocation.run,
                attempt: invocation.attempt,
                invocation: invocation.invocation,
                project: invocation.project.to_string(),
                home: self.home.clone(),
                run_dir: directory,
                cwd: request.cwd,
                engine: request.engine,
                task: request.task,
                required: request.required,
                session: request.session,
                previous,
                assigned: self.launch.assigned,
                messages,
                model: request.model,
                limits,
                retry: request.retry,
                internal_attempt: existing
                    .as_ref()
                    .map(|c| c.internal_attempt + 1)
                    .unwrap_or(1),
            },
            prompt,
            engine,
            host,
            tmux,
            cancel: self.cancel.clone(),
        })
    }
}
pub struct RunHost {
    link: UnixCoordinatorLink,
    launch: Launch,
    invocation: InvocationId,
    outer_dir: PathBuf,
    directory: PathBuf,
    cancel: CancellationToken,
    /// How long acknowledgements and notes wait out an unavailable peer.
    patience: Patience,
}
impl RunHost {
    /// A read waits out a busy or stalled coordinator for as long as the run lives.
    async fn read(&self, request: C) -> io::Result<R> {
        patiently(&self.cancel, None, || self.link.request(request.clone())).await
    }
}
/// How long host calls wait on their peers.
#[derive(Debug, Clone, Copy)]
struct Patience {
    /// One try of a call to the run's guardian.
    call: std::time::Duration,
    /// All tries of an acknowledgement or a note, before the run fails `Transient`.
    total: std::time::Duration,
    /// How long a guardian's refusal of an acknowledgement may mean it has not offered the
    /// message yet, rather than that the acknowledgement is wrong.
    offer_lag: std::time::Duration,
}
const PATIENCE: Patience = Patience {
    call: socket::RPC_TIMEOUT,
    total: HOST_PATIENCE,
    offer_lag: std::time::Duration::from_secs(60),
};
/// Reports to the run's guardian that the engine accepted `ack.message`. The guardian answers
/// once the acknowledgement is durable in its `delivery.json`. Until it has offered the message
/// itself it refuses the acknowledgement, and its own watch on the coordinator can lag this
/// supervisor's read by as long as the coordinator stalls, so a refusal is retried like a busy
/// guardian for the first minute. A guardian that is busy or does not answer is retried until
/// `patience` has passed, which fails the run `Transient`.
async fn acknowledge_delivery(
    control: &Path,
    capability: &RunCapability,
    ack: DeliveryAck,
    cancel: &CancellationToken,
    patience: Patience,
) -> io::Result<()> {
    let offered_by = tokio::time::Instant::now() + patience.offer_lag;
    patiently(cancel, Some(patience.total), || async {
        match socket::call_within::<_, socket::ControlReply>(
            control,
            capability,
            socket::ControlCommand::DeliveryAck(ack.clone()),
            patience.call,
        )
        .await
        {
            Err(sluice_model::error::PublicError::BadRequest { message })
                if tokio::time::Instant::now() < offered_by =>
            {
                Err(sluice_model::error::PublicError::Busy {
                    message,
                    retryable: true,
                })
            }
            result => result.map(|_| ()),
        }
    })
    .await
}
/// Says `body` to the orchestrator for this run. Every try carries the same request id, so a
/// try the coordinator committed but could not answer in time is never posted twice.
async fn say_to_orchestrator(
    link: &impl CoordinatorLink,
    launch: &Launch,
    body: &str,
    cancel: &CancellationToken,
    patience: Patience,
) -> io::Result<()> {
    // The run says it to the orchestrator; its step and thread are derived.
    let command = C::Callback {
        identity: launch.identity.clone(),
        request: Box::new(RpcRequest {
            protocol: 1,
            request_id: RequestId(InvocationId::new().to_string()),
            run_capability: Some(launch.capability.clone()),
            command: CommandRequest::Say(Say {
                project: ProjectSelector::Id(launch.invocation.project),
                to: "orchestrator".into(),
                body: body.into(),
                data: None,
                run: Some(launch.identity.run),
                owner: false,
            }),
        }),
    };
    patiently(cancel, Some(patience.total), || {
        link.request(command.clone())
    })
    .await
    .map(|_| ())
}
impl SupervisorHost for RunHost {
    async fn snapshot(&mut self, after: MessageId) -> io::Result<HostSnapshot> {
        let submissions = match self
            .read(C::Submissions(self.launch.identity.clone()))
            .await
            .map_err(io::Error::other)?
        {
            R::Submissions(s) => s
                .fields
                .0
                .into_iter()
                .map(|(n, v)| (n, v.into_value()))
                .collect(),
            _ => return Err(io::Error::other("submission reply")),
        };
        let messages = match self
            .read(C::Messages {
                identity: self.launch.identity.clone(),
                after,
                through: None,
                limit: 128,
            })
            .await
            .map_err(io::Error::other)?
        {
            R::Messages(m) => m,
            _ => return Err(io::Error::other("message reply")),
        };
        if let R::CancelIntent(true) = self
            .read(C::CancelIntent(self.launch.identity.clone()))
            .await
            .map_err(io::Error::other)?
        {
            self.cancel.cancel();
        }
        Ok(HostSnapshot {
            submissions,
            messages,
            roots: vec![ProcessIdentity::read(std::process::id())?],
            ..Default::default()
        })
    }
    async fn acknowledge(&mut self, ids: &[MessageId]) -> io::Result<()> {
        for message in ids {
            let ack = DeliveryAck {
                invocation: self.launch.invocation.invocation,
                message: *message,
            };
            acknowledge_delivery(
                &self.outer_dir.join("control.sock"),
                &self.launch.capability,
                ack,
                &self.cancel,
                self.patience,
            )
            .await?;
        }
        Ok(())
    }
    async fn me(&mut self) -> io::Result<String> {
        let snapshot = self.snapshot(self.launch.assigned.through).await?;
        Ok(format!(
            "Project {}. Step {}. Run {}.\nCurrent submissions: {}",
            self.launch.invocation.project,
            self.launch
                .invocation
                .step
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            self.launch.identity.run,
            json!(snapshot.submissions)
        ))
    }
    async fn note(&mut self, body: &str) -> io::Result<()> {
        say_to_orchestrator(&self.link, &self.launch, body, &self.cancel, self.patience).await
    }
    async fn checkpoint(&mut self, checkpoint: &Checkpoint) -> io::Result<()> {
        checkpoint.save(&self.directory)?;
        if self.directory != self.outer_dir {
            checkpoint.save(&self.outer_dir)?;
        }
        std::fs::write(
            self.directory.join("invocation-id"),
            self.invocation.to_string(),
        )?;
        Ok(())
    }
    async fn cleanup(&mut self) -> io::Result<()> {
        // Reap every member of this admitted leaf except the dispatcher control.
        // The guardian later proves the complete subtree empty before settlement.
        let me = ProcessIdentity::read(std::process::id())?;
        let root = me
            .cgroup
            .split("/payload/")
            .next()
            .ok_or_else(|| io::Error::other("payload leaf absent"))?;
        if root == me.cgroup {
            return Err(io::Error::other(
                "agent is outside an admitted payload leaf",
            ));
        }
        let group = sluice_process::cgroup::Cgroup::open_service(root)?;
        let started = tokio::time::Instant::now();
        let deadline = started + std::time::Duration::from_secs(5);
        loop {
            let mut remaining = Vec::new();
            for pid in group.member_pids()? {
                if pid == me.pid {
                    continue;
                }
                let Ok(identity) = ProcessIdentity::read(pid) else {
                    continue;
                };
                if identity.cgroup != me.cgroup
                    && !identity.cgroup.starts_with(&format!("{}/", me.cgroup))
                {
                    continue;
                }
                let process = match OwnedProcess::open(&identity) {
                    Ok(process) => process,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                if !process.exited()? {
                    remaining.push(process);
                }
            }
            if remaining.is_empty() {
                return Ok(());
            }
            for process in remaining {
                process.signal(if started.elapsed() < std::time::Duration::from_secs(1) {
                    Signal::TERM
                } else {
                    Signal::KILL
                })?;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(io::Error::other("agent leaf cleanup failed"));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}

/// Which `sluice` an agent runs. The installation's launcher on PATH always runs the selected
/// release, whose schema matches the home's; this run's own binary may be an older release that
/// cannot read the store after a deploy. Without a launcher on PATH (a checkout), and in test
/// mode (the launcher would point at the live installation's home), the agent uses this binary.
/// The note on the step's previous attempt that heads the agent's task (SPEC §7.4), derived
/// at launch from the store and the step's `cwd`. Never fails the launch: a store it cannot
/// read leaves the note out (and says so on stderr); a git it cannot run is in the note.
async fn launch_note(
    home: &Path,
    project: ProjectId,
    step: &StepId,
    run: RunId,
    inputs: &JsonMap,
) -> Option<String> {
    let reads = match sluice_store::ReadPool::open(home, 1) {
        Ok(reads) => reads,
        Err(error) => {
            eprintln!("previous-attempt note left out: {error}");
            return None;
        }
    };
    let id = step.clone();
    let note = reads
        .snapshot(move |sql| sluice_store::attempts::attempt_note(sql, project, &id, Some(run)))
        .await;
    let mut note = match note {
        Ok(note) => note,
        Err(error) => {
            eprintln!("previous-attempt note left out: {error}");
            return None;
        }
    };
    let cwd = inputs.0.get("cwd").and_then(|v| v.as_value().as_str());
    note.worktree = sluice_agents::git::worktree_of(cwd).await;
    Some(note.text())
}
fn callback_header(this: &std::path::Path) -> String {
    let path = std::env::var_os("SLUICE_HOST_PATH").or_else(|| std::env::var_os("PATH"));
    let launcher = std::env::var_os("SLUICE_TEST").is_none()
        && path
            .iter()
            .flat_map(std::env::split_paths)
            .any(|dir| dir.join("sluice").is_file());
    let tool = if launcher {
        "Run every sluice command as plain `sluice` from PATH, never a path under releases/.".into()
    } else {
        format!("Run every sluice command with {}.", this.display())
    };
    format!("{tool} SLUICE_PROJECT_ID and SLUICE_RUN_ID identify this invocation.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sluice_model::error::PublicError;
    use sluice_process::{journal::AttemptKey, socket::RPC_TIMEOUT};
    use std::{sync::Mutex, time::Duration};
    use tokio::net::UnixListener;

    fn launch() -> Launch {
        let (project, run, attempt) = (ProjectId::new(), RunId::new(), AttemptId::new());
        Launch {
            identity: AttemptKey {
                home: HomeId::new(),
                project: Some(project),
                step: Some("work".parse().unwrap()),
                generation: StepGeneration(1),
                work: WorkGeneration(1),
                run,
                attempt,
            },
            invocation: FnInvocation {
                project,
                step: Some("work".parse().unwrap()),
                run,
                attempt,
                invocation: InvocationId::new(),
                name: "agent.run".into(),
                inputs: JsonMap::default(),
            },
            assigned: socket::AssignedRange {
                after: MessageId(0),
                through: MessageId(0),
            },
            prev_run: None,
            capability: RunCapability::new("fixture-secret"),
            timeout_seconds: None,
            context: None,
        }
    }
    /// A run's guardian that answers each acknowledgement with `answer(n)` for the nth
    /// connection, `None` holding it unanswered as a guardian stalled on its coordinator did.
    fn guardian(
        control: &Path,
        answer: impl Fn(usize) -> Option<Result<socket::ControlReply, PublicError>> + Send + 'static,
    ) -> Arc<Mutex<usize>> {
        let listener = UnixListener::bind(control).unwrap();
        let seen = Arc::new(Mutex::new(0));
        let count = seen.clone();
        tokio::spawn(async move {
            let mut held = Vec::new();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let Ok(request) =
                    socket::read_frame::<socket::Request<socket::ControlCommand>>(&mut stream)
                        .await
                else {
                    continue;
                };
                let n = {
                    let mut seen = count.lock().unwrap();
                    *seen += 1;
                    *seen
                };
                match answer(n) {
                    None => held.push(stream),
                    Some(result) => {
                        let reply = socket::Reply {
                            protocol: PROTOCOL_VERSION,
                            request_id: request.request_id,
                            result,
                        };
                        // A client that gave up first hangs up; that is its business.
                        let _ = socket::write_frame(&mut stream, &reply).await;
                    }
                }
            }
        });
        seen
    }
    fn ack(launch: &Launch) -> DeliveryAck {
        DeliveryAck {
            invocation: launch.invocation.invocation,
            message: MessageId(7),
        }
    }

    /// Real sockets on real time: tries of a tenth of a second stand in for RPC_TIMEOUT.
    const QUICK: Patience = Patience {
        call: Duration::from_millis(100),
        total: Duration::from_secs(2),
        offer_lag: Duration::from_millis(300),
    };

    #[tokio::test]
    async fn an_acknowledgement_outlasts_a_guardian_that_does_not_answer_for_a_while() {
        let dir = tempfile::tempdir().unwrap();
        let control = dir.path().join("control.sock");
        // Three tries go unanswered, each for the whole RPC timeout, then the guardian answers.
        let seen = guardian(&control, |n| {
            (n > 3).then_some(Ok(socket::ControlReply::Ack))
        });
        let launch = launch();
        let started = tokio::time::Instant::now();
        acknowledge_delivery(
            &control,
            &launch.capability,
            ack(&launch),
            &CancellationToken::new(),
            QUICK,
        )
        .await
        .unwrap();
        assert_eq!(*seen.lock().unwrap(), 4);
        assert!(started.elapsed() >= QUICK.call * 3);
    }

    #[tokio::test]
    async fn an_acknowledgement_unanswered_past_its_patience_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let control = dir.path().join("control.sock");
        guardian(&control, |_| None);
        let launch = launch();
        let error = acknowledge_delivery(
            &control,
            &launch.capability,
            ack(&launch),
            &CancellationToken::new(),
            QUICK,
        )
        .await
        .unwrap_err();
        let inner = error.get_ref().expect("a typed error");
        assert!(inner.is::<HostUnavailable>(), "{error}");
        assert!(
            error
                .to_string()
                .starts_with("the run's guardian did not answer within 0.1s"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_refusal_is_retried_only_while_the_message_may_not_be_offered_yet() {
        let dir = tempfile::tempdir().unwrap();
        let refused = || {
            Err(PublicError::BadRequest {
                message: "wrong invocation acknowledgement".into(),
            })
        };
        let launch = launch();
        let control = dir.path().join("lagging");
        std::fs::create_dir(&control).unwrap();
        let control = control.join("control.sock");
        // The guardian's watch hands it the message a little after this supervisor read it.
        let seen = guardian(&control, move |n| {
            Some(if n < 4 {
                refused()
            } else {
                Ok(socket::ControlReply::Ack)
            })
        });
        acknowledge_delivery(
            &control,
            &launch.capability,
            ack(&launch),
            &CancellationToken::new(),
            QUICK,
        )
        .await
        .unwrap();
        assert_eq!(*seen.lock().unwrap(), 4);
        // A refusal that outlasts any offer lag is definite.
        let control = dir.path().join("refusing");
        std::fs::create_dir(&control).unwrap();
        let control = control.join("control.sock");
        guardian(&control, move |_| Some(refused()));
        let error = acknowledge_delivery(
            &control,
            &launch.capability,
            ack(&launch),
            &CancellationToken::new(),
            QUICK,
        )
        .await
        .unwrap_err();
        assert!(!error.get_ref().unwrap().is::<HostUnavailable>(), "{error}");
        assert!(error.to_string().contains("wrong invocation"), "{error}");
    }

    /// A coordinator that takes `stalls` requests without answering in time, then answers.
    struct StalledCoordinator {
        stalls: usize,
        requests: Mutex<Vec<RequestId>>,
    }
    impl CoordinatorLink for StalledCoordinator {
        async fn request(&self, command: C) -> Result<R, PublicError> {
            let C::Callback { request, .. } = command else {
                panic!("a note is a callback");
            };
            let n = {
                let mut requests = self.requests.lock().unwrap();
                requests.push(request.request_id.clone());
                requests.len()
            };
            if n <= self.stalls {
                tokio::time::sleep(RPC_TIMEOUT).await;
                return Err(PublicError::Busy {
                    message: "the coordinator did not answer within 5s".into(),
                    retryable: true,
                });
            }
            Ok(R::Callback(Box::new(CommandReply::Ack)))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_note_outlasts_a_stalled_coordinator_and_is_said_once() {
        let coordinator = StalledCoordinator {
            stalls: 20,
            requests: Mutex::default(),
        };
        let started = tokio::time::Instant::now();
        say_to_orchestrator(
            &coordinator,
            &launch(),
            "quiet for 45 minutes",
            &CancellationToken::new(),
            PATIENCE,
        )
        .await
        .unwrap();
        // A hundred seconds of stall, as twelve back-to-back plan edits cost.
        assert!(started.elapsed() >= Duration::from_secs(100));
        let requests = coordinator.requests.lock().unwrap();
        assert_eq!(requests.len(), 21);
        // Every try is the same request, so the coordinator posts it once however many it took.
        assert!(requests.iter().all(|id| *id == requests[0]));
    }
}
