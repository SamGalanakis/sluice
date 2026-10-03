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
#[derive(Clone)]
pub struct CoordinatorClient {
    pub path: PathBuf,
}
impl CoordinatorClient {
    pub fn new(home: &Path) -> Self {
        Self {
            path: home.join("coordinator.sock"),
        }
    }
    /// The returned connection owns the lease. EOF releases it at the broker.
    pub async fn acquire_scheduler(&self) -> Result<UnixStream, PublicError> {
        let mut stream = UnixStream::connect(&self.path).await.map_err(storage)?;
        let request_id = RequestId(InvocationId::new().to_string());
        socket::write_frame(
            &mut stream,
            &Request {
                protocol: PROTOCOL_VERSION,
                request_id: request_id.clone(),
                run_capability: None,
                command: RuntimeCommand::AcquireScheduler {
                    owner: InvocationId::new().to_string(),
                },
            },
        )
        .await
        .map_err(storage)?;
        let reply: Reply<CommandReply> = socket::read_frame(&mut stream).await.map_err(storage)?;
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
        let mut stream = UnixStream::connect(&self.path).await.map_err(storage)?;
        socket::write_frame(
            &mut stream,
            &RpcRequest {
                protocol: PROTOCOL_VERSION,
                request_id: request_id.clone(),
                run_capability: None,
                command,
            },
        )
        .await
        .map_err(storage)?;
        let reply: RpcReply = socket::read_frame(&mut stream).await.map_err(storage)?;
        if reply.protocol != PROTOCOL_VERSION || reply.request_id != request_id {
            return Err(storage("RPC reply identity mismatch"));
        }
        match reply.result {
            RpcResult::Ok(reply) => Ok(*reply),
            RpcResult::Error(e) => Err(e),
        }
    }
    async fn changes(&self, cursor: ChangeCursor) -> Result<ChangeBatch, PublicError> {
        let mut stream = UnixStream::connect(&self.path).await.map_err(storage)?;
        let request_id = RequestId(InvocationId::new().to_string());
        socket::write_frame(
            &mut stream,
            &Request {
                protocol: PROTOCOL_VERSION,
                request_id: request_id.clone(),
                run_capability: None,
                command: RuntimeCommand::Changes(cursor),
            },
        )
        .await
        .map_err(storage)?;
        let reply: Reply<ChangeBatch> = socket::read_frame(&mut stream).await.map_err(storage)?;
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
    if home.join("sluice.db").exists() {
        let reads = sluice_store::ReadPool::open(home, 1).map_err(|e| e.into_public(true))?;
        let fenced = reads
            .snapshot(|sql| {
                Ok(sql.query_row(
                    "SELECT mode='cutover' FROM maintenance WHERE singleton=1",
                    [],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .await
            .map_err(|e| e.into_public(true))?;
        if fenced {
            return Err(PublicError::Busy {
                message: "cutover forbids coordinator activation".into(),
                retryable: false,
            });
        }
    }
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
        .arg(format!("sluice-test-coordinator-{}", &digest[..16]))
        .arg(format!("--setenv=SLUICE_HOME={}", home.display()))
        .args(if std::env::var_os("SLUICE_FIXTURE").is_some() {
            vec!["--setenv=SLUICE_FIXTURE=1"]
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
    let client = CoordinatorClient::new(&home);
    if UnixStream::connect(&client.path).await.is_ok() {
        let _lease = if scheduling {
            Some(client.acquire_scheduler().await?)
        } else {
            None
        };
        return wait_for_signal().await;
    }
    let program = std::env::current_exe().map_err(storage)?;
    let catalog = if std::env::var_os("SLUICE_FIXTURE").is_some() {
        crate::dispatch::Catalog::fixtures()
    } else {
        crate::dispatch::Catalog::core()
    };
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
    let stop = tokio_util::sync::CancellationToken::new();
    let signal_stop = stop.clone();
    let signal = tokio::spawn(async move {
        let result = wait_for_signal().await;
        signal_stop.cancel();
        result
    });
    let result = broker.serve(stop.clone()).await;
    stop.cancel();
    signal.abort();
    let _ = signal.await;
    result
}
