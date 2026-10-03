//! Bounded, authenticated Unix RPC. Each reconnect resolves the same endpoint.
use crate::{
    identity::ProcessIdentity,
    journal::{AttemptKey, CompletionJournal, DeliveryAck},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sluice_model::{
    commands::{CommandReply, CommandRequest},
    error::PublicError,
    ids::*,
    rpc::*,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot},
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

pub const RPC_TIMEOUT: Duration = Duration::from_secs(5);
pub const MAX_CONNECTIONS: usize = 8;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardianIdentity {
    pub identity: AttemptKey,
    pub process: ProcessIdentity,
    pub unit: String,
    pub socket_challenge: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssignedRange {
    pub after: MessageId,
    pub through: MessageId,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryMessage {
    pub id: MessageId,
    pub body: JsonValue,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmissionSnapshot {
    pub version: Option<u64>,
    pub fields: JsonMap,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DurableAck {
    pub run: RunId,
    pub completion_id: String,
}
impl DurableAck {
    pub fn matches(&self, journal: &CompletionJournal) -> bool {
        self.run == journal.identity.run && self.completion_id == journal.completion_id
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CoordinatorCommand {
    Claim(GuardianIdentity),
    Started {
        identity: AttemptKey,
        invocation: InvocationId,
        executor: ProcessIdentity,
    },
    CancelIntent(AttemptKey),
    Messages {
        identity: AttemptKey,
        after: MessageId,
        through: Option<MessageId>,
        limit: u16,
    },
    DeliverAck {
        identity: AttemptKey,
        ack: DeliveryAck,
    },
    Submissions(AttemptKey),
    Complete(Box<CompletionJournal>),
    Callback {
        identity: AttemptKey,
        request: Box<RpcRequest>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CoordinatorReply {
    Claimed(bool),
    Started,
    CancelIntent(bool),
    Messages(Vec<DeliveryMessage>),
    Ack,
    Submissions(SubmissionSnapshot),
    Completed(DurableAck),
    Callback(Box<CommandReply>),
}
/// Implementations reconnect without changing admission identity. Every mutation
/// is idempotent by its IDs; Complete replies only after the writer commits.
pub trait CoordinatorLink: Send + Sync {
    fn request(
        &self,
        command: CoordinatorCommand,
    ) -> impl Future<Output = Result<CoordinatorReply, PublicError>> + Send;
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request<T> {
    pub protocol: u16,
    pub request_id: RequestId,
    pub run_capability: Option<RunCapability>,
    pub command: T,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply<T> {
    pub protocol: u16,
    pub request_id: RequestId,
    pub result: Result<T, PublicError>,
}

pub async fn read_frame<T: DeserializeOwned>(stream: &mut UnixStream) -> io::Result<T> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid RPC frame length",
        ));
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await?;
    decode_json(&body).map_err(io::Error::other)
}
pub async fn write_frame<T: Serialize>(stream: &mut UnixStream, value: &T) -> io::Result<()> {
    let bytes = encode_frame(value).map_err(io::Error::other)?;
    stream.write_all(&bytes).await
}
pub async fn read_request<T: DeserializeOwned>(
    stream: &mut UnixStream,
    capability: &RunCapability,
) -> io::Result<Request<T>> {
    let request: Request<T> = read_frame(stream).await?;
    if request.protocol != PROTOCOL_VERSION || request.run_capability.as_ref() != Some(capability) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "wrong protocol or run capability",
        ));
    }
    Ok(request)
}
pub async fn call<T: Serialize, R: DeserializeOwned>(
    path: &Path,
    capability: &RunCapability,
    command: T,
) -> Result<R, PublicError> {
    let request_id = RequestId(InvocationId::new().to_string());
    let exchange = async {
        let mut stream = UnixStream::connect(path).await?;
        write_frame(
            &mut stream,
            &Request {
                protocol: PROTOCOL_VERSION,
                request_id: request_id.clone(),
                run_capability: Some(capability.clone()),
                command,
            },
        )
        .await?;
        let reply: Reply<R> = read_frame(&mut stream).await?;
        if reply.protocol != PROTOCOL_VERSION || reply.request_id != request_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "RPC reply identity mismatch",
            ));
        }
        Ok(reply.result)
    };
    match tokio::time::timeout(RPC_TIMEOUT, exchange).await {
        Ok(Ok(reply)) => reply,
        _ => Err(PublicError::Busy {
            message: "coordinator unavailable; acceptance may be unknown".into(),
            retryable: true,
        }),
    }
}
#[derive(Clone)]
pub struct UnixCoordinatorLink {
    pub path: PathBuf,
    pub capability: RunCapability,
}
impl CoordinatorLink for UnixCoordinatorLink {
    async fn request(&self, command: CoordinatorCommand) -> Result<CoordinatorReply, PublicError> {
        call(&self.path, &self.capability, command).await
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "method",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ControlCommand {
    Challenge(String),
    EngineHook {
        engine: String,
        run: RunId,
        event: String,
        payload: JsonValue,
    },
    Cancel,
    DeliveryAck(DeliveryAck),
    Callback(Box<CommandRequest>),
    Retry {
        checkpoint: JsonMap,
        backoff_ms: u64,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ControlReply {
    Identity(Box<GuardianIdentity>),
    EngineHook(EngineHookReply),
    Ack,
    Callback(Box<CommandReply>),
}
pub struct ControlEvent {
    pub request: Request<ControlCommand>,
    pub reply: oneshot::Sender<Result<ControlReply, PublicError>>,
}
/// The server owns all accepted tasks and stops them before unlinking its socket.
pub struct ControlServer {
    stop: CancellationToken,
    task: Option<JoinHandle<()>>,
    path: PathBuf,
}
impl ControlServer {
    pub fn bind(
        path: PathBuf,
        capability: RunCapability,
        events: mpsc::Sender<ControlEvent>,
    ) -> io::Result<Self> {
        let listener = UnixListener::bind(&path)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let stop = CancellationToken::new();
        let cancelled = stop.clone();
        let task = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    _ = cancelled.cancelled() => break,
                    Some(_) = clients.join_next(), if !clients.is_empty() => {},
                    accepted = listener.accept(), if clients.len() < MAX_CONNECTIONS => {
                        let Ok((mut stream, _)) = accepted else { break };
                        let capability = capability.clone(); let events = events.clone();
                        clients.spawn(async move {
                            let _ = tokio::time::timeout(RPC_TIMEOUT, async {
                                let raw: JsonValue = read_frame(&mut stream).await?;
                                let bytes = serde_json::to_vec(&raw).map_err(io::Error::other)?;
                                let (request, public_callback) = match decode_json::<Request<ControlCommand>>(&bytes) {
                                    Ok(request) => (request, false),
                                    Err(_) => {
                                        let request: RpcRequest = decode_json(&bytes).map_err(io::Error::other)?;
                                        (Request { protocol: request.protocol, request_id: request.request_id, run_capability: request.run_capability, command: ControlCommand::Callback(Box::new(request.command)) }, true)
                                    }
                                };
                                if request.protocol != PROTOCOL_VERSION || request.run_capability.as_ref() != Some(&capability) {
                                    return Err(io::Error::new(io::ErrorKind::PermissionDenied, "wrong protocol or run capability"));
                                }
                                let (reply, response) = oneshot::channel();
                                let request_id = request.request_id.clone();
                                events.send(ControlEvent { request, reply }).await.map_err(io::Error::other)?;
                                let result = response.await.map_err(io::Error::other)?;
                                if public_callback {
                                    let result = match result {
                                        Ok(ControlReply::Callback(value)) => RpcResult::Ok(value),
                                        Err(error) => RpcResult::Error(error),
                                        _ => RpcResult::Error(PublicError::BadRequest { message: "wrong callback response".into() }),
                                    };
                                    write_frame(&mut stream, &RpcReply { protocol: PROTOCOL_VERSION, request_id, result }).await
                                } else {
                                    write_frame(&mut stream, &Reply { protocol: PROTOCOL_VERSION, request_id, result }).await
                                }
                            }).await;
                        });
                    }
                }
            }
            clients.abort_all();
            while clients.join_next().await.is_some() {}
        });
        Ok(Self {
            stop,
            task: Some(task),
            path,
        })
    }
    pub async fn close(mut self) {
        self.stop.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}
impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Shared state survives dropping a client, exactly as a committed store would.
#[derive(Clone, Default)]
pub struct MemoryCoordinator {
    state: Arc<Mutex<MemoryState>>,
}
#[derive(Default)]
pub struct MemoryState {
    pub offline: bool,
    pub busy_completions: usize,
    pub attempts: BTreeMap<RunId, MemoryAttempt>,
}
pub struct MemoryAttempt {
    pub identity: AttemptKey,
    pub claimed: Option<GuardianIdentity>,
    pub cancelled: bool,
    pub started: BTreeSet<InvocationId>,
    pub range: AssignedRange,
    pub cursor: MessageId,
    pub messages: Vec<DeliveryMessage>,
    pub acknowledgements: Vec<DeliveryAck>,
    pub submissions: SubmissionSnapshot,
    pub completion: Option<CompletionJournal>,
    pub releases: usize,
}
impl MemoryCoordinator {
    pub fn with_state<T>(&self, f: impl FnOnce(&mut MemoryState) -> T) -> T {
        f(&mut self.state.lock().expect("fake state poisoned"))
    }
    pub fn reserve(
        &self,
        identity: AttemptKey,
        range: AssignedRange,
        messages: Vec<DeliveryMessage>,
    ) {
        self.with_state(|s| {
            s.attempts.entry(identity.run).or_insert(MemoryAttempt {
                identity,
                claimed: None,
                cancelled: false,
                started: BTreeSet::new(),
                range,
                cursor: range.after,
                messages,
                acknowledgements: Vec::new(),
                submissions: SubmissionSnapshot {
                    version: None,
                    fields: JsonMap::default(),
                },
                completion: None,
                releases: 0,
            });
        });
    }
}
impl CoordinatorLink for MemoryCoordinator {
    async fn request(&self, command: CoordinatorCommand) -> Result<CoordinatorReply, PublicError> {
        self.with_state(|s| {
            if s.offline {
                return Err(unavailable());
            }
            if matches!(command, CoordinatorCommand::Complete(_)) && s.busy_completions > 0 {
                s.busy_completions -= 1;
                return Err(unavailable());
            }
            let identity = match &command {
                CoordinatorCommand::Claim(g) => &g.identity,
                CoordinatorCommand::Started { identity, .. }
                | CoordinatorCommand::Messages { identity, .. }
                | CoordinatorCommand::DeliverAck { identity, .. }
                | CoordinatorCommand::Callback { identity, .. } => identity,
                CoordinatorCommand::CancelIntent(id) | CoordinatorCommand::Submissions(id) => id,
                CoordinatorCommand::Complete(j) => &j.identity,
            };
            let a = s
                .attempts
                .get_mut(&identity.run)
                .filter(|a| a.identity == *identity)
                .ok_or_else(|| PublicError::Conflict {
                    message: "attempt identity changed".into(),
                    current_rev: None,
                })?;
            match command {
                CoordinatorCommand::Claim(g) => {
                    let accepted =
                        a.completion.is_none() && a.claimed.as_ref().is_none_or(|old| old == &g);
                    if accepted {
                        a.claimed = Some(g);
                    }
                    Ok(CoordinatorReply::Claimed(accepted))
                }
                CoordinatorCommand::Started { invocation, .. } => {
                    a.started.insert(invocation);
                    a.cursor = a.range.through;
                    Ok(CoordinatorReply::Started)
                }
                CoordinatorCommand::CancelIntent(_) => {
                    Ok(CoordinatorReply::CancelIntent(a.cancelled))
                }
                CoordinatorCommand::Messages {
                    after,
                    through,
                    limit,
                    ..
                } => Ok(CoordinatorReply::Messages(
                    a.messages
                        .iter()
                        .filter(|m| m.id > after && through.is_none_or(|end| m.id <= end))
                        .take(limit as usize)
                        .cloned()
                        .collect(),
                )),
                CoordinatorCommand::DeliverAck { ack, .. } => {
                    if !a.acknowledgements.contains(&ack) {
                        a.acknowledgements.push(ack);
                    }
                    Ok(CoordinatorReply::Ack)
                }
                CoordinatorCommand::Submissions(_) => {
                    Ok(CoordinatorReply::Submissions(a.submissions.clone()))
                }
                CoordinatorCommand::Complete(j) => {
                    if j.submission_version != a.submissions.version
                        || j.submissions != a.submissions.fields
                    {
                        return Err(PublicError::Conflict {
                            message: "submission version differs".into(),
                            current_rev: None,
                        });
                    }
                    j.validate(&a.identity)
                        .map_err(|e| PublicError::BadRequest {
                            message: e.to_string(),
                        })?;
                    if let Some(old) = &a.completion {
                        if old != j.as_ref() {
                            return Err(PublicError::Conflict {
                                message: "completion differs".into(),
                                current_rev: None,
                            });
                        }
                    } else {
                        a.completion = Some(*j);
                        a.releases += 1;
                    }
                    let journal = a.completion.as_ref().expect("completion installed");
                    Ok(CoordinatorReply::Completed(DurableAck {
                        run: a.identity.run,
                        completion_id: journal.completion_id.clone(),
                    }))
                }
                CoordinatorCommand::Callback { .. } => {
                    Err(PublicError::not_implemented("fake callback"))
                }
            }
        })
    }
}
fn unavailable() -> PublicError {
    PublicError::Busy {
        message: "fake coordinator unavailable".into(),
        retryable: true,
    }
}

/// Engine-independent hook wire result; process must not depend on sluice-agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineHookReply {
    pub stdout: Option<JsonValue>,
    pub exit_code: i32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineHookRequest {
    pub engine: String,
    pub run: RunId,
    pub event: String,
    pub payload: JsonValue,
}
