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
                        });
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(EngineError {
                            kind: EngineErrorKind::Fatal,
                            message: error.to_string(),
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
                .with_environment(environment.clone()),
            )),
            "devin" => Adapter::Devin(Box::new(devin::Devin::new(devin::DevinOptions {
                hook_binary: bin.clone(),
                environment,
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
        };
        // The guardian's hook transport journals in the outer run directory. Each
        // logical supervisor owns that journal only while it is executing.
        if directory != outer_dir {
            use std::os::unix::fs::symlink;
            for name in ["engine-hooks", "control.sock"] {
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
}
impl RunHost {
    async fn read(&self, request: C) -> io::Result<R> {
        loop {
            let result = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Err(io::Error::new(io::ErrorKind::Interrupted, "agent cancelled")),
                result = self.link.request(request.clone()) => result,
            };
            match result {
                Ok(reply) => return Ok(reply),
                Err(sluice_model::error::PublicError::Busy {
                    retryable: true, ..
                }) => {
                    tokio::select! {
                        biased;
                        _ = self.cancel.cancelled() => return Err(io::Error::new(io::ErrorKind::Interrupted, "agent cancelled")),
                        _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {},
                    }
                }
                Err(error) => return Err(io::Error::other(error)),
            }
        }
    }
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
            // The guardian polls delivery independently. It must durably offer
            // the message before accepting an acknowledgement of native delivery.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                match socket::call::<_, socket::ControlReply>(
                    &self.outer_dir.join("control.sock"),
                    &self.launch.capability,
                    socket::ControlCommand::DeliveryAck(ack.clone()),
                )
                .await
                {
                    Ok(_) => break,
                    Err(sluice_model::error::PublicError::BadRequest { .. })
                        if tokio::time::Instant::now() < deadline =>
                    {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    Err(error) => return Err(io::Error::other(error)),
                }
            }
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
        // The run says it to the orchestrator; its step and thread are derived.
        let command = CommandRequest::Say(Say {
            project: ProjectSelector::Id(self.launch.invocation.project),
            to: "orchestrator".into(),
            body: body.into(),
            data: None,
            run: Some(self.launch.identity.run),
            owner: false,
        });
        self.link
            .request(C::Callback {
                identity: self.launch.identity.clone(),
                request: Box::new(RpcRequest {
                    protocol: 1,
                    request_id: RequestId(InvocationId::new().to_string()),
                    run_capability: Some(self.launch.capability.clone()),
                    command,
                }),
            })
            .await
            .map_err(io::Error::other)?;
        Ok(())
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
