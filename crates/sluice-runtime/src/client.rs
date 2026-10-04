//! CLI socket client and bounded user-manager activation without scheduling.
use crate::coordinator::RuntimeCommand;
use sluice_model::{
    RuntimeApi,
    commands::{CommandReply, CommandRequest},
    error::PublicError,
    events::{ChangeBatch, ChangeCursor},
    ids::InvocationId,
    rpc::{PROTOCOL_VERSION, RequestId, RpcReply, RpcRequest, RpcResult},
};
use sluice_process::socket::{self, Reply, Request};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::net::UnixStream;
/// How long a client waits for the coordinator to answer an ordinary request.
/// Long polls (`log_wait`, `step_wait`, `next`), waiting `fn_call`s and `backup` get their own
/// wait on top ([`reply_limit`]).
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(120);
/// The longest a `backup` may copy the database before its client gives up.
const BACKUP_SECONDS: u64 = 3600;
#[derive(Clone)]
pub struct CoordinatorClient {
    pub path: PathBuf,
    /// The wait for an ordinary reply; [`REPLY_TIMEOUT`] unless a caller changes it.
    pub reply_timeout: Duration,
}
/// How long a client waits for the reply to `command`: the ordinary limit plus the
/// time the command itself may legitimately hold the request.
pub fn reply_limit(base: Duration, command: &CommandRequest) -> Duration {
    let held = match command {
        CommandRequest::LogWait(wait) => wait.timeout_seconds,
        CommandRequest::StepWait(wait) => wait.timeout_seconds,
        CommandRequest::Next(next) => next.timeout_seconds.saturating_add(next.settle_max_seconds),
        CommandRequest::FnCall(call) => call
            .wait_seconds
            .unwrap_or(if call.direct {
                crate::calls::MAX_WAIT_SECONDS
            } else {
                0
            })
            .min(crate::calls::MAX_WAIT_SECONDS),
        CommandRequest::Backup { .. } => BACKUP_SECONDS,
        _ => 0,
    };
    base.saturating_add(Duration::from_secs(held))
}
impl CoordinatorClient {
    pub fn new(home: &Path) -> Self {
        Self {
            path: home.join("coordinator.sock"),
            reply_timeout: REPLY_TIMEOUT,
        }
    }
    /// One request and its reply within `limit`, connect included. A coordinator
    /// that stays silent (still starting, adopting runs, or wedged) is a clear
    /// error, never an endless wait.
    async fn exchange<T: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        request: &T,
        limit: Duration,
    ) -> Result<(UnixStream, R), PublicError> {
        let exchange = async {
            let mut stream = UnixStream::connect(&self.path).await.map_err(storage)?;
            socket::write_frame(&mut stream, request)
                .await
                .map_err(storage)?;
            let reply: R = socket::read_frame(&mut stream).await.map_err(storage)?;
            Ok((stream, reply))
        };
        tokio::time::timeout(limit, exchange)
            .await
            .unwrap_or_else(|_| {
                Err(PublicError::Busy {
                    message: format!(
                        "the coordinator at {} did not answer within {} s; it may still be \
                         starting or adopting runs, and the request may still take effect",
                        self.path.display(),
                        limit.as_secs()
                    ),
                    retryable: true,
                })
            })
    }
    /// The returned connection owns the lease. EOF releases it at the broker.
    pub async fn acquire_scheduler(&self) -> Result<UnixStream, PublicError> {
        let request_id = RequestId(InvocationId::new().to_string());
        let (stream, reply): (_, Reply<CommandReply>) = self
            .exchange(
                &Request {
                    protocol: PROTOCOL_VERSION,
                    request_id: request_id.clone(),
                    run_capability: None,
                    command: RuntimeCommand::AcquireScheduler {
                        owner: InvocationId::new().to_string(),
                    },
                },
                self.reply_timeout,
            )
            .await?;
        if reply.request_id != request_id || reply.protocol != PROTOCOL_VERSION {
            return Err(storage("lease reply identity mismatch"));
        }
        reply.result?;
        Ok(stream)
    }
}
fn storage(e: impl std::fmt::Display) -> PublicError {
    PublicError::Storage {
        message: e.to_string(),
    }
}
impl RuntimeApi for CoordinatorClient {
    async fn command(&self, command: CommandRequest) -> Result<CommandReply, PublicError> {
        let request_id = RequestId(InvocationId::new().to_string());
        let limit = reply_limit(self.reply_timeout, &command);
        let (_, reply): (_, RpcReply) = self
            .exchange(
                &RpcRequest {
                    protocol: PROTOCOL_VERSION,
                    request_id: request_id.clone(),
                    run_capability: None,
                    command,
                },
                limit,
            )
            .await?;
        if reply.protocol != PROTOCOL_VERSION || reply.request_id != request_id {
            return Err(storage("RPC reply identity mismatch"));
        }
        match reply.result {
            RpcResult::Ok(reply) => Ok(*reply),
            RpcResult::Error(e) => Err(e),
        }
    }
    async fn changes(&self, cursor: ChangeCursor) -> Result<ChangeBatch, PublicError> {
        let request_id = RequestId(InvocationId::new().to_string());
        let (_, reply): (_, Reply<ChangeBatch>) = self
            .exchange(
                &Request {
                    protocol: PROTOCOL_VERSION,
                    request_id: request_id.clone(),
                    run_capability: None,
                    command: RuntimeCommand::Changes(cursor),
                },
                self.reply_timeout,
            )
            .await?;
        if reply.protocol != PROTOCOL_VERSION || reply.request_id != request_id {
            return Err(storage("changes reply identity mismatch"));
        }
        reply.result
    }
}
pub async fn ensure_coordinator(
    home: &Path,
    program: &Path,
) -> Result<CoordinatorClient, PublicError> {
    sluice_process::host::guard_scratch_home(home)?;
    let client = CoordinatorClient::new(home);
    if UnixStream::connect(&client.path).await.is_ok() {
        return Ok(client);
    }
    let installation = crate::install::Installation::for_home(home)?;
    let activation_home = home.to_path_buf();
    let _activation =
        tokio::task::spawn_blocking(move || installation.admission_guard(&activation_home, false))
            .await
            .map_err(storage)??;
    let digest = sluice_store::artifacts::fingerprint(home.as_os_str().as_encoded_bytes());
    let mut command = tokio::process::Command::new("/usr/bin/systemd-run");
    command
        .args([
            "--user",
            "--quiet",
            "--collect",
            "--service-type=exec",
            "--property=Restart=no",
            "--unit",
        ])
        .arg(format!(
            "{}coordinator-{}",
            sluice_process::systemd::unit_prefix(),
            &digest[..16]
        ))
        .arg(format!("--setenv=SLUICE_HOME={}", home.display()))
        .arg(format!(
            "--setenv=SLUICE_INSTALL_DIR={}",
            crate::install::Installation::for_home(home)?.dir.display()
        ))
        .args(if std::env::var_os("SLUICE_FIXTURE").is_some() {
            vec!["--setenv=SLUICE_FIXTURE=1"]
        } else {
            vec![]
        })
        // The coordinator names the units it launches by the same mode.
        .args(if sluice_process::host::test_mode() {
            vec!["--setenv=SLUICE_TEST=1"]
        } else {
            vec![]
        })
        .arg("--")
        .arg(program)
        .arg("coordinator")
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .map_err(storage)?
        .map_err(storage)?;
    // Concurrent clients can race activation; probe the deterministic endpoint even
    // when the second manager request says the unit already exists.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if UnixStream::connect(&client.path).await.is_ok() {
            return Ok(client);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(storage(format!(
                "coordinator activation failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
pub async fn wait_for_signal() -> Result<(), PublicError> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(storage)?;
    tokio::select! {r=tokio::signal::ctrl_c()=>r.map_err(storage),_=terminate.recv()=>Ok(())}
}
pub async fn run_home(home: PathBuf, scheduling: bool) -> Result<(), PublicError> {
    run_home_maintenance(home, scheduling, false).await
}
pub async fn run_home_maintenance(
    home: PathBuf,
    scheduling: bool,
    maintenance: bool,
) -> Result<(), PublicError> {
    let client = CoordinatorClient::new(&home);
    if UnixStream::connect(&client.path).await.is_ok() {
        let _lease = if scheduling {
            Some(client.acquire_scheduler().await?)
        } else {
            None
        };
        return wait_for_signal().await;
    }
    let installation = crate::install::Installation::for_home(&home)?;
    let activation_home = home.clone();
    let activation = tokio::task::spawn_blocking(move || {
        installation.admission_guard(&activation_home, maintenance)
    })
    .await
    .map_err(storage)??;
    let program = std::env::current_exe().map_err(storage)?;
    let catalog = if std::env::var_os("SLUICE_FIXTURE").is_some() {
        crate::dispatch::Catalog::fixtures()
    } else {
        crate::dispatch::Catalog::core()
    };
    let home_root = home.clone();
    let broker = crate::coordinator::Coordinator::open(
        home.clone(),
        catalog,
        crate::execution::OsHost { home, program },
    )
    .await?;
    if scheduling {
        broker
            .acquire_scheduler(InvocationId::new().to_string())
            .await?;
    }
    drop(activation);
    let stop = tokio_util::sync::CancellationToken::new();
    let signal_stop = stop.clone();
    let signal = tokio::spawn(async move {
        let result = wait_for_signal().await;
        signal_stop.cancel();
        result
    });
    let watcher = tokio::spawn(stop_when_home_disappears(
        home_root,
        stop.clone(),
        Duration::from_secs(1),
    ));
    let result = broker.serve(stop.clone()).await;
    stop.cancel();
    signal.abort();
    watcher.abort();
    let _ = signal.await;
    let _ = watcher.await;
    result
}
/// A broker whose home directory was removed (or replaced by another directory
/// at the same path) owns nothing any more; it stops instead of outliving it.
pub async fn stop_when_home_disappears(
    home: PathBuf,
    stop: tokio_util::sync::CancellationToken,
    period: Duration,
) {
    use std::os::unix::fs::MetadataExt;
    let identity = |path: &Path| std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()));
    let Some(original) = identity(&home) else {
        stop.cancel();
        return;
    };
    loop {
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep(period) => {}
        }
        if identity(&home) != Some(original) {
            tracing::warn!(home = %home.display(), "home directory disappeared; coordinator stopping");
            stop.cancel();
            return;
        }
    }
}
