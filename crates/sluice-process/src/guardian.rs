//! A guardian owns one admitted run independently of coordinator connections.
use crate::{
    cgroup::{Cgroup, RunCgroups},
    identity::{OwnedProcess, ProcessIdentity},
    journal::*,
    locks::{FileLock, LockAttempt},
    signals::{StopPolicy, stop_invocation},
    socket::*,
    spawn::{PreparedLaunch, RunningPayload},
};
use serde::{Deserialize, Serialize};
use sluice_model::{
    error::PublicError,
    ids::*,
    rpc::{FnInvocation, JsonMap, MAX_FRAME_BYTES, PROTOCOL_VERSION, RunCapability, decode_json},
};
use std::{
    ffi::OsString,
    fs,
    future::Future,
    io::{self, Read},
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

/// The executable injects fn dispatch without a process-to-agents dependency.
pub trait FnHost: Send + Sync {
    fn invoke(
        &self,
        invocation: FnInvocation,
    ) -> impl Future<Output = Result<JsonMap, PublicError>> + Send;
}
const MAX_DELIVERIES: usize = 16_384;
const MAX_INVOCATIONS: usize = 1024;
pub struct UnimplementedFnHost;
impl FnHost for UnimplementedFnHost {
    async fn invoke(&self, _invocation: FnInvocation) -> Result<JsonMap, PublicError> {
        Err(PublicError::not_implemented("fn dispatch"))
    }
}
/// A host launches its dispatcher behind the OS barrier. Fixtures implement the
/// same lifetime contract without requiring delegation in every unit test.
pub trait PayloadHost: FnHost {
    type Invocation: PayloadInvocation;
    fn start(
        &self,
        request: LaunchRequest,
        cancel: CancellationToken,
    ) -> impl Future<Output = io::Result<Self::Invocation>> + Send;
    fn empty_without_payload(&self) -> io::Result<CleanupEvidence>;
}
pub trait PayloadInvocation: Send {
    fn engine_hook(
        &mut self,
        _request: EngineHookRequest,
    ) -> impl Future<Output = Result<EngineHookReply, PublicError>> + Send {
        async { Err(PublicError::not_implemented("engine hook transport")) }
    }
    fn id(&self) -> InvocationId;
    fn executor(&self) -> &ProcessIdentity;
    fn exit_evidence(&self) -> Option<ExitEvidence> {
        None
    }
    /// Poll must be cancellation safe. Returning a result closes invocation admission.
    fn poll(
        &mut self,
    ) -> impl Future<Output = io::Result<Option<(PayloadResult, ExitEvidence)>>> + Send;
    fn deliver(
        &mut self,
        messages: &[DeliveryMessage],
    ) -> impl Future<Output = io::Result<()>> + Send;
    fn cleanup(&mut self) -> impl Future<Output = io::Result<CleanupEvidence>> + Send;
}
#[derive(Debug, Clone)]
pub struct LaunchRequest {
    pub invocation: FnInvocation,
    pub run_dir: PathBuf,
    pub checkpoint: JsonMap,
    pub prev_run: Option<RunId>,
    pub capability: RunCapability,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardianArgs {
    pub home_dir: PathBuf,
    pub run_dir: PathBuf,
    pub guardian: GuardianIdentity,
    pub invocation: FnInvocation,
    pub assigned: AssignedRange,
    pub prev_run: Option<RunId>,
    pub capability: RunCapability,
    pub poll_interval: Duration,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardianOutcome {
    ClaimRefused,
    Completed(DurableAck),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchCheckpoint {
    invocation: InvocationId,
    executor: ProcessIdentity,
}

/// Library entry for the binary's guardian mode. The caller owns this future for
/// the service lifetime; HTTP/direct-call futures must never own or cancel it.
pub async fn guardian_main<L: CoordinatorLink, H: PayloadHost>(
    args: GuardianArgs,
    link: &L,
    host: &H,
) -> io::Result<GuardianOutcome> {
    if !args.run_dir.starts_with(&args.home_dir)
        || args.poll_interval.is_zero()
        || args.invocation.run != args.guardian.identity.run
        || args.invocation.attempt != args.guardian.identity.attempt
        || args.invocation.step != args.guardian.identity.step
        || args
            .guardian
            .identity
            .project
            .is_some_and(|project| project != args.invocation.project)
        || args.assigned.after > args.assigned.through
    {
        return Err(invalid("invalid guardian arguments"));
    }
    fs::create_dir_all(&args.run_dir)?;
    if !fs::canonicalize(&args.run_dir)?.starts_with(fs::canonicalize(&args.home_dir)?) {
        return Err(invalid("run directory escapes home"));
    }
    let _lock = match FileLock::try_acquire(
        &args.run_dir.join("guardian.lock"),
        None,
        Some(args.invocation.run),
    )? {
        LockAttempt::Acquired(lock) => lock,
        LockAttempt::Conflict(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "guardian already owns run",
            ));
        }
    };
    if let Some(journal) = CompletionJournal::read(&args.run_dir, &args.guardian.identity)? {
        return replay_completion(&args.run_dir, &journal, link, args.poll_interval)
            .await
            .map(GuardianOutcome::Completed);
    }
    if let Some(mut journal) = read_json::<CompletionJournal>(&args.run_dir.join("collected.json"))?
    {
        journal.validate(&args.guardian.identity)?;
        freeze_submissions(&mut journal, link, args.poll_interval).await?;
        journal.write(&args.run_dir)?;
        return replay_completion(&args.run_dir, &journal, link, args.poll_interval)
            .await
            .map(GuardianOutcome::Completed);
    }
    // Never treat a prior grant checkpoint as permission to launch again.
    if args.run_dir.join("admitted.json").exists()
        || read_json::<LaunchCheckpoint>(&args.run_dir.join("launch.json"))?.is_some()
    {
        return Err(invalid(
            "attempt was already launched; adopt its service or mark it lost",
        ));
    }
    let reply = retry_request(
        link,
        CoordinatorCommand::Claim(args.guardian.clone()),
        args.poll_interval,
    )
    .await?;
    if !matches!(reply, CoordinatorReply::Claimed(true)) {
        return Ok(GuardianOutcome::ClaimRefused);
    }
    atomic_json(&args.run_dir, "hook-capability.json", &args.capability)?;
    let (events, mut control) = mpsc::channel(16);
    let server = ControlServer::bind(
        args.run_dir.join("control.sock"),
        args.capability.clone(),
        events,
    )?;
    let result = run_claimed(&args, link, host, &mut control).await;
    server.close().await;
    result
}
async fn retry_request<L: CoordinatorLink>(
    link: &L,
    command: CoordinatorCommand,
    delay: Duration,
) -> io::Result<CoordinatorReply> {
    loop {
        match link.request(command.clone()).await {
            Ok(r) => return Ok(r),
            Err(PublicError::Busy { .. }) => tokio::time::sleep(delay).await,
            Err(e) => return Err(io::Error::other(e)),
        }
    }
}
async fn cancelled<L: CoordinatorLink>(
    link: &L,
    identity: &AttemptKey,
) -> Result<bool, PublicError> {
    match link
        .request(CoordinatorCommand::CancelIntent(identity.clone()))
        .await?
    {
        CoordinatorReply::CancelIntent(c) => Ok(c),
        _ => Err(PublicError::BadRequest {
            message: "wrong cancellation reply".into(),
        }),
    }
}
async fn messages<L: CoordinatorLink>(
    link: &L,
    id: &AttemptKey,
    range: AssignedRange,
    delay: Duration,
) -> io::Result<Vec<DeliveryMessage>> {
    let mut cursor = range.after;
    let mut result = Vec::new();
    while cursor < range.through {
        let reply = retry_request(
            link,
            CoordinatorCommand::Messages {
                identity: id.clone(),
                after: cursor,
                through: Some(range.through),
                limit: 128,
            },
            delay,
        )
        .await?;
        let CoordinatorReply::Messages(batch) = reply else {
            return Err(invalid("wrong message reply"));
        };
        validate_messages(&batch, cursor, Some(range.through))?;
        if batch.is_empty() {
            break;
        }
        cursor = batch.last().expect("nonempty batch").id;
        result.extend(batch);
        if result.len() > MAX_DELIVERIES {
            return Err(invalid("assigned message count exceeds guardian limit"));
        }
        // The final view must fit the same public envelope limit.
        if serde_json::to_vec(&result).map_err(io::Error::other)?.len() > MAX_FRAME_BYTES {
            return Err(invalid("assigned message view exceeds limit"));
        }
    }
    Ok(result)
}
fn validate_messages(
    batch: &[DeliveryMessage],
    after: MessageId,
    through: Option<MessageId>,
) -> io::Result<()> {
    if batch.len() > 128
        || serde_json::to_vec(batch).map_err(io::Error::other)?.len() > MAX_FRAME_BYTES
    {
        return Err(invalid("message batch exceeds guardian limit"));
    }
    let mut previous = after;
    for m in batch {
        if m.id <= previous || through.is_some_and(|end| m.id > end) {
            return Err(invalid("unordered or out of range message delivery"));
        }
        previous = m.id;
    }
    Ok(())
}
async fn run_claimed<L: CoordinatorLink, H: PayloadHost>(
    args: &GuardianArgs,
    link: &L,
    host: &H,
    control: &mut mpsc::Receiver<ControlEvent>,
) -> io::Result<GuardianOutcome> {
    let id = &args.guardian.identity;
    let backlog = messages(link, id, args.assigned, args.poll_interval).await?;
    atomic_json(&args.run_dir, "messages.json", &backlog)?;
    let cancel = CancellationToken::new();
    let mut checkpoint = JsonMap::default();
    let mut invocation = args.invocation.clone();
    let mut exits = Vec::new();
    let mut cleanup = Vec::new();
    let mut delivery = DeliveryState {
        acks: Vec::new(),
        offered: backlog.iter().map(|m| m.id).collect(),
    };
    let cursor = args.assigned.through;
    let mut result;
    let mut started = Vec::new();
    'invocations: loop {
        // Before spawning, an unavailable coordinator delays admission. Once a
        // payload exists, outages only delay notifications, never its execution.
        loop {
            match cancelled(link, id).await {
                Ok(true) => {
                    cancel.cancel();
                    break;
                }
                Ok(false) => break,
                Err(PublicError::Busy { .. }) => tokio::time::sleep(args.poll_interval).await,
                Err(e) => return Err(io::Error::other(e)),
            }
        }
        if cancel.is_cancelled() {
            result = PayloadResult::Cancelled("cancel intent before spawn".into());
            if cleanup.is_empty() {
                cleanup.push(host.empty_without_payload()?);
            }
            break;
        }
        atomic_json(
            &args.run_dir,
            "messages.json",
            &backlog
                .iter()
                .filter(|m| !delivery.acks.iter().any(|ack| ack.message == m.id))
                .collect::<Vec<_>>(),
        )?;
        atomic_json(&args.run_dir, "admitted.json", &invocation.invocation)?;
        let request = LaunchRequest {
            invocation: invocation.clone(),
            run_dir: args.run_dir.clone(),
            checkpoint: checkpoint.clone(),
            prev_run: args.prev_run,
            capability: args.capability.clone(),
        };
        // Do not drop a started spawn_blocking operation when cancellation wins.
        let launch = host.start(request, cancel.clone());
        tokio::pin!(launch);
        let launch_result = loop {
            tokio::select! {
                r = &mut launch => break r,
                event = control.recv() => { if let Some(event) = event { handle_control::<L, H::Invocation>(event, args, link, &cancel, &mut delivery, invocation.invocation, None, None).await; } },
                _ = tokio::time::sleep(args.poll_interval) => { if matches!(cancelled(link, id).await, Ok(true)) { cancel.cancel(); } },
            }
        };
        let mut payload = match launch_result {
            Ok(p) => p,
            Err(e) => {
                // Host::start guarantees cleanup of any prepared leaf on error.
                cleanup.push(host.empty_without_payload()?);
                result = if cancel.is_cancelled() {
                    PayloadResult::Cancelled(e.to_string())
                } else {
                    PayloadResult::Unknown(e.to_string())
                };
                break;
            }
        };
        started.push((payload.id(), payload.executor().clone()));
        let mut retry = None;
        let mut offered = cursor;
        loop {
            if cancel.is_cancelled() {
                result = PayloadResult::Cancelled("cancel intent".into());
                break;
            }
            tokio::select! {
                event = control.recv() => {
                    if let Some(event) = event { handle_control(event, args, link, &cancel, &mut delivery, payload.id(), Some(&mut retry), Some(&mut payload)).await; }
                },
                _ = tokio::time::sleep(args.poll_interval) => {
                    if matches!(cancelled(link, id).await, Ok(true)) { cancel.cancel(); }
                    for (invocation, executor) in &started {
                        let _ = link.request(CoordinatorCommand::Started { identity: id.clone(), invocation: *invocation, executor: executor.clone() }).await;
                    }
                    for ack in &delivery.acks { let _ = link.request(CoordinatorCommand::DeliverAck { identity: id.clone(), ack: ack.clone() }).await; }
                    if let Ok(CoordinatorReply::Messages(batch)) = link.request(CoordinatorCommand::Messages { identity: id.clone(), after: offered, through: None, limit: 128 }).await {
                        if let Err(e) = validate_messages(&batch, offered, None) { result = PayloadResult::Unknown(e.to_string()); break; }
                        if !batch.is_empty() {
                            let pending: Vec<_> = batch.iter().filter(|m| !delivery.acks.iter().any(|ack| ack.message == m.id)).cloned().collect();
                            if delivery.offered.len().saturating_add(pending.len()) > MAX_DELIVERIES {
                                result = PayloadResult::Unknown("delivery count exceeds guardian limit".into()); break;
                            }
                            delivery.offered.extend(pending.iter().map(|m| m.id));
                            if let Err(e) = payload.deliver(&pending).await { result = PayloadResult::Unknown(e.to_string()); break; }
                            offered = batch.last().expect("nonempty batch").id;
                        }
                    }
                    match payload.poll().await {
                        Ok(Some((r, exit))) => { result = r; exits.push(exit); break; },
                        Ok(None) => {},
                        Err(e) => { result = PayloadResult::Failed(PublicError::FnFailure { message: e.to_string() }); break; }
                    }
                }
            }
            // Re-entry is requested by the fn host/helper's policy, after its
            // prior invocation has exited. It is never inferred from an error.
        }
        cleanup.push(payload.cleanup().await?);
        if let Some(exit) = payload.exit_evidence()
            && !exits.iter().any(|old| old.executor == exit.executor)
        {
            exits.push(exit);
        }
        if let Some((next, backoff_ms)) = retry {
            if matches!(
                result,
                PayloadResult::Unknown(_) | PayloadResult::Cancelled(_) | PayloadResult::Lost(_)
            ) {
                break;
            }
            if started.len() >= MAX_INVOCATIONS {
                result = PayloadResult::Failed(PublicError::FnFailure {
                    message: "invocation count exceeds guardian limit".into(),
                });
                break;
            }
            checkpoint = next;
            atomic_json(
                &args.run_dir,
                "checkpoint.json",
                &(invocation.invocation, &checkpoint, &delivery.acks),
            )?;
            let until = tokio::time::Instant::now() + Duration::from_millis(backoff_ms);
            while tokio::time::Instant::now() < until {
                if matches!(cancelled(link, id).await, Ok(true)) {
                    cancel.cancel();
                }
                if cancel.is_cancelled() {
                    result = PayloadResult::Cancelled("cancel intent during backoff".into());
                    break 'invocations;
                }
                tokio::select! {
                    event = control.recv() => { if let Some(event) = event { handle_control::<L, H::Invocation>(event, args, link, &cancel, &mut delivery, invocation.invocation, None, None).await; } },
                    _ = tokio::time::sleep(args.poll_interval.min(until.saturating_duration_since(tokio::time::Instant::now()))) => {},
                }
            }
            invocation.invocation = InvocationId::new();
            continue;
        }
        break;
    }
    let mut journal = CompletionJournal {
        protocol: PROTOCOL_VERSION,
        identity: id.clone(),
        completion_id: InvocationId::new().to_string(),
        result,
        starts: started
            .iter()
            .map(|(invocation, executor)| StartEvidence {
                invocation: *invocation,
                executor: executor.clone(),
            })
            .collect(),
        exits,
        cleanup,
        submissions: JsonMap::default(),
        submission_version: None,
        delivery_acks: delivery.acks,
    };
    atomic_json(&args.run_dir, "collected.json", &journal)?;
    freeze_submissions(&mut journal, link, args.poll_interval).await?;
    journal.write(&args.run_dir)?;
    // Persist/replay start before complete even if the executor already finished.
    for (invocation, executor) in started {
        retry_request(
            link,
            CoordinatorCommand::Started {
                identity: id.clone(),
                invocation,
                executor,
            },
            args.poll_interval,
        )
        .await?;
    }
    let ack = replay_completion(&args.run_dir, &journal, link, args.poll_interval).await?;
    Ok(GuardianOutcome::Completed(ack))
}
struct DeliveryState {
    acks: Vec<DeliveryAck>,
    offered: std::collections::BTreeSet<MessageId>,
}
async fn freeze_submissions<L: CoordinatorLink>(
    journal: &mut CompletionJournal,
    link: &L,
    delay: Duration,
) -> io::Result<()> {
    match retry_request(
        link,
        CoordinatorCommand::Submissions(journal.identity.clone()),
        delay,
    )
    .await?
    {
        CoordinatorReply::Submissions(snapshot) => {
            journal.submissions = snapshot.fields;
            journal.submission_version = snapshot.version;
            Ok(())
        }
        _ => Err(invalid("wrong submission reply")),
    }
}
#[allow(clippy::too_many_arguments)]
async fn handle_control<L: CoordinatorLink, I: PayloadInvocation>(
    event: ControlEvent,
    args: &GuardianArgs,
    link: &L,
    cancel: &CancellationToken,
    delivery: &mut DeliveryState,
    current: InvocationId,
    retry: Option<&mut Option<(JsonMap, u64)>>,
    payload: Option<&mut I>,
) {
    let result = match event.request.command {
        ControlCommand::EngineHook {
            engine,
            run,
            event,
            payload: body,
        } => {
            if !["codex", "claude", "devin", "fake"].contains(&engine.as_str())
                || run != args.invocation.run
                || event.is_empty()
                || event.len() > 128
                || serde_json::to_vec(&body).map_or(true, |bytes| bytes.len() > 1024 * 1024)
            {
                Err(PublicError::BadRequest {
                    message: "invalid hook run/event/payload".into(),
                })
            } else if let Some(payload) = payload {
                match tokio::time::timeout(
                    Duration::from_secs(3),
                    payload.engine_hook(EngineHookRequest {
                        engine,
                        run,
                        event,
                        payload: body,
                    }),
                )
                .await
                {
                    Ok(result) => result.map(ControlReply::EngineHook),
                    Err(_) => Err(PublicError::Busy {
                        message: "engine hook decision timed out; do not replay".into(),
                        retryable: false,
                    }),
                }
            } else {
                Err(PublicError::Conflict {
                    message: "no active engine invocation".into(),
                    current_rev: None,
                })
            }
        }
        ControlCommand::Challenge(challenge) if challenge == args.guardian.socket_challenge => {
            Ok(ControlReply::Identity(Box::new(args.guardian.clone())))
        }
        ControlCommand::Challenge(_) => Err(PublicError::BadRequest {
            message: "wrong socket challenge".into(),
        }),
        ControlCommand::Cancel => match cancelled(link, &args.guardian.identity).await {
            Ok(true) => {
                cancel.cancel();
                Ok(ControlReply::Ack)
            }
            Ok(false) => Err(PublicError::Conflict {
                message: "cancel intent must commit first".into(),
                current_rev: None,
            }),
            Err(e) => Err(e),
        },
        ControlCommand::DeliveryAck(ack)
            if ack.invocation == current && delivery.offered.contains(&ack.message) =>
        {
            if !delivery.acks.contains(&ack) {
                delivery.acks.push(ack.clone());
            }
            match atomic_json(&args.run_dir, "delivery.json", &delivery.acks) {
                Ok(()) => {
                    let _ = link
                        .request(CoordinatorCommand::DeliverAck {
                            identity: args.guardian.identity.clone(),
                            ack,
                        })
                        .await;
                    Ok(ControlReply::Ack)
                }
                Err(e) => Err(PublicError::Storage {
                    message: e.to_string(),
                }),
            }
        }
        ControlCommand::DeliveryAck(_) => Err(PublicError::BadRequest {
            message: "wrong invocation acknowledgement".into(),
        }),
        ControlCommand::Callback(command) => {
            let request = sluice_model::rpc::RpcRequest {
                protocol: event.request.protocol,
                request_id: event.request.request_id,
                run_capability: Some(args.capability.clone()),
                command: *command,
            };
            match link
                .request(CoordinatorCommand::Callback {
                    identity: args.guardian.identity.clone(),
                    request: Box::new(request),
                })
                .await
            {
                Ok(CoordinatorReply::Callback(reply)) => Ok(ControlReply::Callback(reply)),
                Ok(_) => Err(PublicError::BadRequest {
                    message: "wrong callback reply".into(),
                }),
                Err(e) => Err(e),
            }
        }
        ControlCommand::Retry {
            checkpoint,
            backoff_ms,
        } if backoff_ms <= 86_400_000 => {
            if let Some(slot) = retry {
                if slot.is_none() {
                    *slot = Some((checkpoint, backoff_ms));
                    Ok(ControlReply::Ack)
                } else {
                    Err(PublicError::Conflict {
                        message: "retry already requested".into(),
                        current_rev: None,
                    })
                }
            } else {
                Err(PublicError::Conflict {
                    message: "invocation is not executing".into(),
                    current_rev: None,
                })
            }
        }
        ControlCommand::Retry { .. } => Err(PublicError::BadRequest {
            message: "retry backoff exceeds one day".into(),
        }),
    };
    let _ = event.reply.send(result);
}
pub async fn replay_completion<L: CoordinatorLink>(
    dir: &Path,
    journal: &CompletionJournal,
    link: &L,
    delay: Duration,
) -> io::Result<DurableAck> {
    journal.validate(&journal.identity)?;
    loop {
        for start in &journal.starts {
            retry_request(
                link,
                CoordinatorCommand::Started {
                    identity: journal.identity.clone(),
                    invocation: start.invocation,
                    executor: start.executor.clone(),
                },
                delay,
            )
            .await?;
        }
        // Repeat acknowledgements even after the first completion response was lost.
        for ack in &journal.delivery_acks {
            let _ = link
                .request(CoordinatorCommand::DeliverAck {
                    identity: journal.identity.clone(),
                    ack: ack.clone(),
                })
                .await;
        }
        match link
            .request(CoordinatorCommand::Complete(Box::new(journal.clone())))
            .await
        {
            Ok(CoordinatorReply::Completed(ack)) if ack.matches(journal) => {
                journal.remove(dir)?;
                return Ok(ack);
            }
            Ok(_) => return Err(invalid("completion acknowledgement has wrong identity")),
            Err(PublicError::Busy { .. }) => tokio::time::sleep(delay).await,
            Err(e) => return Err(io::Error::other(e)),
        }
    }
}

/// Real OS host. Construct inside the admitted delegated service, before calling
/// guardian_main; no payload is created by construction.
pub struct OsFnHost<H> {
    pub dispatcher: H,
    groups: Arc<RunCgroups>,
    program: PathBuf,
    payload_args: Vec<OsString>,
    home: PathBuf,
}
impl<H: FnHost> OsFnHost<H> {
    pub fn attach(
        dispatcher: H,
        service_group: Cgroup,
        program: PathBuf,
        payload_args: Vec<OsString>,
        home: PathBuf,
    ) -> io::Result<Self> {
        let mut guardian = OwnedProcess::capture(std::process::id())?;
        let groups = Arc::new(RunCgroups::create(service_group, &mut guardian)?);
        Ok(Self {
            dispatcher,
            groups,
            program,
            payload_args,
            home,
        })
    }
    pub fn control_identity(&self) -> io::Result<ProcessIdentity> {
        ProcessIdentity::read(std::process::id())
    }
}
impl<H: FnHost> FnHost for OsFnHost<H> {
    async fn invoke(&self, invocation: FnInvocation) -> Result<JsonMap, PublicError> {
        self.dispatcher.invoke(invocation).await
    }
}
impl<H: FnHost> PayloadHost for OsFnHost<H> {
    type Invocation = OsInvocation;
    async fn start(
        &self,
        request: LaunchRequest,
        cancel: CancellationToken,
    ) -> io::Result<OsInvocation> {
        let group = Arc::new(self.groups.invocation(request.invocation.invocation)?);
        if let Err(e) = atomic_json(&request.run_dir, "invocation.json", &request.invocation) {
            stop_invocation(&group, StopPolicy::default()).await?;
            return Err(e);
        }
        let program = self.program.clone();
        let args = self.payload_args.clone();
        let home = self.home.clone();
        let leaf = group.clone();
        let dir = request.run_dir.clone();
        let invocation = request.invocation.invocation;
        let callback_capability = serde_json::to_value(&request.capability)
            .map_err(io::Error::other)?
            .as_str()
            .ok_or_else(|| invalid("capability is not text"))?
            .to_owned();
        let project = request.invocation.project.to_string();
        let step = request
            .invocation
            .step
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        let run = request.invocation.run.to_string();
        let prev_run = request
            .prev_run
            .map(|id| id.to_string())
            .unwrap_or_default();
        let launched = tokio::task::spawn_blocking(move || {
            let mut command = Command::new(&program);
            command
                .current_dir(&dir)
                .env("SLUICE_HOME", home)
                .env("SLUICE_RUN_DIR", &dir)
                .env("SLUICE_PROJECT_ID", project)
                .env("SLUICE_STEP", step)
                .env("SLUICE_RUN_ID", run)
                .env("SLUICE_PREV_RUN", prev_run)
                .env("SLUICE_BIN", &program)
                .env("SLUICE_CONTROL_SOCKET", dir.join("control.sock"))
                .env("SLUICE_RUN_CAPABILITY", callback_capability)
                .env("SLUICE_INVOCATION_ID", invocation.to_string())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let prepared = PreparedLaunch::spawn(&mut command, &args, Duration::from_secs(5))?;
            prepared.continue_in(&leaf, &cancel, |executor| {
                atomic_json(
                    &dir,
                    "launch.json",
                    &LaunchCheckpoint {
                        invocation,
                        executor: executor.clone(),
                    },
                )
            })
        })
        .await
        .map_err(io::Error::other)?;
        let mut payload = match launched {
            Ok(p) => p,
            Err(e) => {
                stop_invocation(&group, StopPolicy::default()).await?;
                return Err(e);
            }
        };
        let stdout = payload
            .take_stdout()
            .ok_or_else(|| invalid("missing payload stdout"))?;
        let stderr = payload
            .take_stderr()
            .ok_or_else(|| invalid("missing payload stderr"))?;
        let output = tokio::task::spawn_blocking(move || {
            let mut bytes = Vec::new();
            stdout
                .take(MAX_FRAME_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_FRAME_BYTES {
                return Err(invalid("payload result exceeds limit"));
            }
            Ok(bytes)
        });
        let errors = tokio::task::spawn_blocking(move || {
            let mut reader = stderr;
            let mut tail = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let n = reader.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                tail.extend_from_slice(&buffer[..n]);
                if tail.len() > 2048 {
                    tail.drain(..tail.len() - 2048);
                }
            }
            Ok(tail)
        });
        Ok(OsInvocation {
            id: invocation,
            group,
            payload,
            output: Some(output),
            errors: Some(errors),
            bytes: None,
            result: None,
            exit: None,
            cleanup_evidence: None,
            run_dir: request.run_dir,
        })
    }
    fn empty_without_payload(&self) -> io::Result<CleanupEvidence> {
        if !self.groups.payload_empty()? {
            return Err(invalid("payload subtree is not empty"));
        }
        Ok(CleanupEvidence {
            cgroup: format!("{}/payload", self.groups.service.path()),
            empty: true,
            escalated: false,
        })
    }
}
pub struct OsInvocation {
    id: InvocationId,
    group: Arc<Cgroup>,
    payload: RunningPayload,
    output: Option<JoinHandle<io::Result<Vec<u8>>>>,
    errors: Option<JoinHandle<io::Result<Vec<u8>>>>,
    bytes: Option<Vec<u8>>,
    result: Option<(PayloadResult, ExitEvidence)>,
    exit: Option<ExitEvidence>,
    cleanup_evidence: Option<CleanupEvidence>,
    run_dir: PathBuf,
}
#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum FnEnvelope {
    Success { ok: bool, outputs: JsonMap },
    Failure { ok: bool, error: FnEnvelopeError },
}
#[derive(Deserialize)]
#[serde(untagged)]
enum FnEnvelopeError {
    Public(PublicError),
    Helper(HelperError),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HelperError {
    kind: HelperErrorKind,
    message: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum HelperErrorKind {
    Failure,
    FnFailure,
    Rejected,
    Transient,
    Cancelled,
}
fn error_tail(message: &str) -> String {
    let mut start = message.len().saturating_sub(2048);
    while !message.is_char_boundary(start) {
        start += 1;
    }
    message[start..].to_owned()
}
fn bounded_error(mut error: PublicError) -> PublicError {
    let message = match &mut error {
        PublicError::BadRequest { message }
        | PublicError::NotFound { message }
        | PublicError::Conflict { message, .. }
        | PublicError::Invalid { message, .. }
        | PublicError::Busy { message, .. }
        | PublicError::Storage { message }
        | PublicError::CursorExpired { message }
        | PublicError::ProcessLost { message }
        | PublicError::Cancelled { message }
        | PublicError::Transient { message }
        | PublicError::Rejected { message }
        | PublicError::FnFailure { message }
        | PublicError::AgentFailure { message, .. } => Some(message),
        _ => None,
    };
    if let Some(message) = message {
        *message = error_tail(message);
    }
    error
}
pub fn decode_result(bytes: &[u8], code: Option<i32>) -> io::Result<PayloadResult> {
    match decode_json::<FnEnvelope>(bytes).map_err(io::Error::other)? {
        FnEnvelope::Success { ok: true, outputs } if code == Some(0) => {
            Ok(PayloadResult::Succeeded(outputs))
        }
        FnEnvelope::Failure { ok: false, error } if code == Some(1) => Ok(match error {
            FnEnvelopeError::Public(PublicError::Rejected { message }) => {
                PayloadResult::Rejected(error_tail(&message))
            }
            FnEnvelopeError::Public(PublicError::Cancelled { message }) => {
                PayloadResult::Cancelled(error_tail(&message))
            }
            FnEnvelopeError::Public(error) => PayloadResult::Failed(bounded_error(error)),
            FnEnvelopeError::Helper(error) => match error.kind {
                HelperErrorKind::Rejected => PayloadResult::Rejected(error_tail(&error.message)),
                HelperErrorKind::Cancelled => PayloadResult::Cancelled(error_tail(&error.message)),
                _ => PayloadResult::Failed(PublicError::FnFailure {
                    message: error_tail(&error.message),
                }),
            },
        }),
        _ => Err(invalid("payload envelope and exit evidence disagree")),
    }
}
/// Payload-exec dispatch uses this helper with the composition root's FnHost.
pub async fn invoke_payload<H: FnHost>(host: &H, invocation: FnInvocation) -> (Vec<u8>, i32) {
    let (value, code) = match host.invoke(invocation).await {
        Ok(outputs) => (serde_json::json!({"ok":true,"outputs":outputs}), 0),
        Err(error) => (serde_json::json!({"ok":false,"error":error}), 1),
    };
    (
        serde_json::to_vec(&value).expect("typed fn result serializes"),
        code,
    )
}
impl PayloadInvocation for OsInvocation {
    async fn engine_hook(
        &mut self,
        request: EngineHookRequest,
    ) -> Result<EngineHookReply, PublicError> {
        let directory = self.run_dir.join("engine-hooks");
        fs::create_dir_all(&directory).map_err(|e| PublicError::Storage {
            message: e.to_string(),
        })?;
        if fs::read_dir(&directory)
            .map_err(|e| PublicError::Storage {
                message: e.to_string(),
            })?
            .count()
            >= 4096
        {
            return Err(PublicError::BadRequest {
                message: "engine hook journal bound exceeded".into(),
            });
        }
        let id = InvocationId::new().to_string();
        atomic_json(&directory, &format!("{id}.request.json"), &request).map_err(|e| {
            PublicError::Storage {
                message: e.to_string(),
            }
        })?;
        let path = directory.join(format!("{id}.reply.json"));
        loop {
            if let Some(reply) =
                read_json::<Result<EngineHookReply, PublicError>>(&path).map_err(|e| {
                    PublicError::Storage {
                        message: e.to_string(),
                    }
                })?
            {
                return reply;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn id(&self) -> InvocationId {
        self.id
    }
    fn executor(&self) -> &ProcessIdentity {
        self.payload.process().identity()
    }
    fn exit_evidence(&self) -> Option<ExitEvidence> {
        self.exit.clone()
    }
    async fn poll(&mut self) -> io::Result<Option<(PayloadResult, ExitEvidence)>> {
        if self.result.is_some() {
            return Ok(self.result.clone());
        }
        if self.output.as_ref().is_some_and(|t| t.is_finished()) {
            self.bytes = Some(
                self.output
                    .take()
                    .expect("output task")
                    .await
                    .map_err(io::Error::other)??,
            );
        }
        let Some(status) = self.payload.try_wait()? else {
            return Ok(None);
        };
        self.exit = Some(ExitEvidence {
            code: status.code(),
            signal: status.signal(),
            executor: Some(self.executor().clone()),
        });
        // Descendants may still hold stdout open. Cleanup precedes joining pipes.
        let proof = stop_invocation(&self.group, StopPolicy::default()).await?;
        if !proof.cgroup().ends_with(&self.id.to_string()) {
            return Err(invalid("wrong cleanup leaf"));
        }
        self.cleanup_evidence = Some(proof.into());
        if let Some(task) = self.output.take() {
            self.bytes = Some(task.await.map_err(io::Error::other)??);
        }
        let exit = ExitEvidence {
            code: status.code(),
            signal: status.signal(),
            executor: Some(self.executor().clone()),
        };
        let result = decode_result(self.bytes.as_deref().unwrap_or_default(), exit.code)
            .unwrap_or_else(|e| {
                PayloadResult::Failed(PublicError::FnFailure {
                    message: e.to_string(),
                })
            });
        self.result = Some((result.clone(), exit.clone()));
        Ok(Some((result, exit)))
    }
    async fn deliver(&mut self, messages: &[DeliveryMessage]) -> io::Result<()> {
        // This durable ordered view is available to helper/engine adapters. It
        // is an offer, never an acknowledgement of engine acceptance.
        let mut pending: Vec<DeliveryMessage> =
            read_json(&self.run_dir.join("live-messages.json"))?.unwrap_or_default();
        let acks: Vec<DeliveryAck> =
            read_json(&self.run_dir.join("delivery.json"))?.unwrap_or_default();
        pending.retain(|m| !acks.iter().any(|ack| ack.message == m.id));
        for m in messages {
            if !pending.iter().any(|old| old.id == m.id) {
                pending.push(m.clone());
            }
        }
        pending.sort_by_key(|m| m.id);
        atomic_json(&self.run_dir, "live-messages.json", &pending)
    }
    async fn cleanup(&mut self) -> io::Result<CleanupEvidence> {
        let proof = match self.cleanup_evidence.take() {
            Some(proof) => proof,
            None => stop_invocation(&self.group, StopPolicy::default())
                .await?
                .into(),
        };
        let status = self.payload.wait()?;
        self.exit = Some(ExitEvidence {
            code: status.code(),
            signal: status.signal(),
            executor: Some(self.executor().clone()),
        });
        if let Some(task) = self.output.take() {
            let _ = task.await.map_err(io::Error::other)?;
        }
        if let Some(task) = self.errors.take() {
            let tail = task.await.map_err(io::Error::other)??;
            std::fs::write(self.run_dir.join("stderr-tail.log"), tail)?;
        }
        Ok(proof)
    }
}

#[derive(Debug, Clone)]
pub struct AdoptionAttempt {
    pub identity: AttemptKey,
    pub guardian: Option<GuardianIdentity>,
    pub run_dir: PathBuf,
    pub unit: String,
    pub service_cgroup: Option<String>,
    pub capability: RunCapability,
}
#[derive(Debug, Clone)]
pub enum GuardianPresence {
    Live(GuardianIdentity),
    Gone(CleanupEvidence),
    Ambiguous(String),
}
pub trait AdoptionHost: Send + Sync {
    fn reconcile(
        &self,
        attempt: &AdoptionAttempt,
    ) -> impl Future<Output = io::Result<GuardianPresence>> + Send;
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdoptionOutcome {
    Reconnected,
    Imported(DurableAck),
    Lost(DurableAck),
    Pending(String),
}
/// A replacement coordinator calls this for each nonterminal attempt. No branch
/// admits execution. Busy leaves the journal for a later adoption pass.
pub async fn adopt_attempt<L: CoordinatorLink, H: AdoptionHost>(
    attempt: &AdoptionAttempt,
    link: &L,
    host: &H,
) -> io::Result<AdoptionOutcome> {
    if let Some(outcome) = import_checkpoint(attempt, link).await? {
        return Ok(outcome);
    }
    let presence = host.reconcile(attempt).await?;
    // Both journal stages can land during service reconciliation.
    if let Some(outcome) = import_checkpoint(attempt, link).await? {
        return Ok(outcome);
    }
    match presence {
        GuardianPresence::Live(g)
            if Some(&g) == attempt.guardian.as_ref() && g.identity == attempt.identity =>
        {
            Ok(AdoptionOutcome::Reconnected)
        }
        GuardianPresence::Live(_) => Ok(AdoptionOutcome::Pending(
            "guardian identity mismatch".into(),
        )),
        GuardianPresence::Ambiguous(reason) => Ok(AdoptionOutcome::Pending(reason)),
        GuardianPresence::Gone(proof) if proof.empty => {
            fs::create_dir_all(&attempt.run_dir)?;
            let journal = CompletionJournal {
                protocol: PROTOCOL_VERSION,
                identity: attempt.identity.clone(),
                completion_id: format!("lost-{}", attempt.identity.attempt),
                result: PayloadResult::Lost(
                    "guardian/service gone; automatic replay forbidden".into(),
                ),
                starts: Vec::new(),
                exits: Vec::new(),
                cleanup: vec![proof],
                submissions: JsonMap::default(),
                submission_version: None,
                delivery_acks: Vec::new(),
            };
            let mut journal = journal;
            match link
                .request(CoordinatorCommand::Submissions(attempt.identity.clone()))
                .await
            {
                Ok(CoordinatorReply::Submissions(snapshot)) => {
                    journal.submissions = snapshot.fields;
                    journal.submission_version = snapshot.version;
                }
                Err(PublicError::Busy { .. }) => {
                    atomic_json(&attempt.run_dir, "collected.json", &journal)?;
                    return Ok(AdoptionOutcome::Pending(
                        "submission snapshot unavailable".into(),
                    ));
                }
                _ => return Err(invalid("wrong lost submission reply")),
            }
            journal.write(&attempt.run_dir)?;
            import_once(&attempt.run_dir, &journal, link)
                .await
                .map(|r| match r {
                    Some(ack) => AdoptionOutcome::Lost(ack),
                    None => AdoptionOutcome::Pending("lost commit unavailable".into()),
                })
        }
        GuardianPresence::Gone(_) => Ok(AdoptionOutcome::Pending("cleanup not proven".into())),
    }
}
async fn import_checkpoint<L: CoordinatorLink>(
    attempt: &AdoptionAttempt,
    link: &L,
) -> io::Result<Option<AdoptionOutcome>> {
    let journal = match CompletionJournal::read(&attempt.run_dir, &attempt.identity)? {
        Some(journal) => journal,
        None => {
            let Some(mut journal) =
                read_json::<CompletionJournal>(&attempt.run_dir.join("collected.json"))?
            else {
                return Ok(None);
            };
            journal.validate(&attempt.identity)?;
            match link
                .request(CoordinatorCommand::Submissions(attempt.identity.clone()))
                .await
            {
                Ok(CoordinatorReply::Submissions(snapshot)) => {
                    journal.submissions = snapshot.fields;
                    journal.submission_version = snapshot.version;
                    journal.write(&attempt.run_dir)?;
                }
                Err(PublicError::Busy { .. }) => {
                    return Ok(Some(AdoptionOutcome::Pending(
                        "submission snapshot unavailable".into(),
                    )));
                }
                _ => return Err(invalid("wrong adoption submission reply")),
            }
            journal
        }
    };
    import_once(&attempt.run_dir, &journal, link)
        .await
        .map(|r| {
            Some(match r {
                Some(ack) => AdoptionOutcome::Imported(ack),
                None => AdoptionOutcome::Pending("completion commit unavailable".into()),
            })
        })
}
async fn import_once<L: CoordinatorLink>(
    dir: &Path,
    journal: &CompletionJournal,
    link: &L,
) -> io::Result<Option<DurableAck>> {
    for start in &journal.starts {
        match link
            .request(CoordinatorCommand::Started {
                identity: journal.identity.clone(),
                invocation: start.invocation,
                executor: start.executor.clone(),
            })
            .await
        {
            Ok(CoordinatorReply::Started) => {}
            Err(PublicError::Busy { .. }) => return Ok(None),
            other => return Err(io::Error::other(format!("start import refused: {other:?}"))),
        }
    }
    for ack in &journal.delivery_acks {
        match link
            .request(CoordinatorCommand::DeliverAck {
                identity: journal.identity.clone(),
                ack: ack.clone(),
            })
            .await
        {
            Ok(CoordinatorReply::Ack) => {}
            Err(PublicError::Busy { .. }) => return Ok(None),
            other => {
                return Err(io::Error::other(format!(
                    "delivery import refused: {other:?}"
                )));
            }
        }
    }
    match link
        .request(CoordinatorCommand::Complete(Box::new(journal.clone())))
        .await
    {
        Ok(CoordinatorReply::Completed(ack)) if ack.matches(journal) => {
            journal.remove(dir)?;
            Ok(Some(ack))
        }
        Err(PublicError::Busy { .. }) => Ok(None),
        other => Err(io::Error::other(format!(
            "completion import refused: {other:?}"
        ))),
    }
}
pub struct OsAdoptionHost {
    pub timeout: Duration,
}
impl AdoptionHost for OsAdoptionHost {
    async fn reconcile(&self, attempt: &AdoptionAttempt) -> io::Result<GuardianPresence> {
        use crate::{signals::stop_run, systemd::TransientService};
        let service = if attempt.unit == TransientService::for_run(attempt.identity.run).name() {
            TransientService::adopt(attempt.identity.run)
        } else if attempt.unit == TransientService::for_test(attempt.identity.run).name() {
            TransientService::adopt_test(attempt.identity.run)
        } else {
            return Err(invalid("unit is not bound to run"));
        };
        let state = service.query().await?;
        let group = match attempt
            .service_cgroup
            .as_deref()
            .or(state.cgroup.as_deref())
        {
            Some(path) => match Cgroup::open_service(path) {
                Ok(group) => Some(group),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(e),
            },
            None => None,
        };
        if let Some(g) = &attempt.guardian {
            let same_boot = crate::proc::boot_id()? == g.process.boot_id;
            if same_boot && g.process.matches_current()? && !state.stopped() {
                if state.main_pid != Some(g.process.pid)
                    || state
                        .cgroup
                        .as_deref()
                        .is_none_or(|root| g.process.cgroup != format!("{root}/control"))
                {
                    return Ok(GuardianPresence::Ambiguous(
                        "guardian is outside the recorded service".into(),
                    ));
                }
                let reply = call::<_, ControlReply>(
                    &attempt.run_dir.join("control.sock"),
                    &attempt.capability,
                    ControlCommand::Challenge(g.socket_challenge.clone()),
                )
                .await;
                return Ok(match reply {
                    Ok(ControlReply::Identity(identity)) if *identity == *g => {
                        GuardianPresence::Live(*identity)
                    }
                    _ => GuardianPresence::Ambiguous(
                        "live service has no verified guardian connection".into(),
                    ),
                });
            }
            if !state.stopped() && (!same_boot || state.main_pid != Some(g.process.pid)) {
                return Ok(GuardianPresence::Ambiguous(
                    "unit has a different process generation".into(),
                ));
            }
        } else if !state.stopped() {
            return Ok(GuardianPresence::Ambiguous(
                "unclaimed service may still be launching".into(),
            ));
        }
        if let Some(group) = group {
            if !group.path().ends_with(&format!("/{}", attempt.unit)) {
                return Err(invalid("wrong adoption cgroup"));
            }
            let proof = stop_run(&service, &group, self.timeout).await?;
            Ok(GuardianPresence::Gone(proof.into()))
        } else if state.stopped() && service.query().await?.stopped() {
            Ok(GuardianPresence::Gone(CleanupEvidence {
                cgroup: attempt
                    .service_cgroup
                    .clone()
                    .unwrap_or_else(|| attempt.unit.clone()),
                empty: true,
                escalated: false,
            }))
        } else {
            Ok(GuardianPresence::Ambiguous(
                "unit absence not proven".into(),
            ))
        }
    }
}
