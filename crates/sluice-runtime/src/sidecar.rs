//! A contained agent host retained across the outer helper's retry loop.
use crate::{
    compose::{Dispatcher, failure},
    execution::Launch,
    python::{HelperCommand, HelperRequest},
};
use sluice_model::{
    commands::{CommandReply, CommandRequest},
    error::PublicError,
    ids::InvocationId,
    rpc::*,
};
use sluice_process::{
    cgroup::Cgroup,
    identity::ProcessIdentity,
    signals::{StopPolicy, stop_invocation},
    socket::{self},
    spawn::{PreparedLaunch, RunningPayload},
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::net::{UnixListener, UnixStream};

pub struct Sidecar {
    payload: RunningPayload,
    group: Arc<Cgroup>,
    path: PathBuf,
}
impl Sidecar {
    pub async fn start(dispatcher: &Dispatcher) -> Result<Self, PublicError> {
        let process = ProcessIdentity::read(std::process::id()).map_err(failure)?;
        let (root, _) = process
            .cgroup
            .split_once("/payload/")
            .ok_or_else(|| failure("composition requires an admitted payload leaf"))?;
        let group = Arc::new(
            Cgroup::open_service(root)
                .and_then(|root| root.payload_invocation(InvocationId::new(), true))
                .map_err(failure)?,
        );
        let directory = dispatcher
            .home
            .join("runs")
            .join(dispatcher.launch.identity.run.to_string())
            .join("agent-host");
        std::fs::create_dir_all(&directory).map_err(failure)?;
        std::fs::write(
            directory.join("invocation.json"),
            serde_json::to_vec(&dispatcher.launch.invocation).map_err(failure)?,
        )
        .map_err(failure)?;
        let path = directory.join("agent.sock");
        let leaf = group.clone();
        let cancel = dispatcher.cancel.clone();
        let program = std::env::current_exe().map_err(failure)?;
        let payload = tokio::task::spawn_blocking(move || {
            let mut command = std::process::Command::new(program);
            command
                .env("SLUICE_RUN_DIR", &directory)
                .env("SLUICE_AGENT_SIDECAR", "1")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit());
            PreparedLaunch::spawn(&mut command, &["runtime".into()], Duration::from_secs(5))?
                .continue_in(&leaf, &cancel, |identity| {
                    std::fs::write(
                        directory.join("executor.json"),
                        serde_json::to_vec(identity)?,
                    )?;
                    std::fs::File::open(&directory)?.sync_all()
                })
        })
        .await
        .map_err(failure)?
        .map_err(failure)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut sidecar = Self {
            payload,
            group,
            path,
        };
        while !sidecar.path.exists() {
            if sidecar.payload.try_wait().map_err(failure)?.is_some()
                || tokio::time::Instant::now() >= deadline
            {
                sidecar.close().await?;
                return Err(failure("agent host did not start"));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok(sidecar)
    }
    pub async fn invoke(&mut self, request: HelperRequest) -> Result<CommandReply, PublicError> {
        let mut stream = UnixStream::connect(&self.path).await.map_err(failure)?;
        socket::write_frame(&mut stream, &request)
            .await
            .map_err(failure)?;
        // An agent can run for hours. Its own policy and parent cancellation own this wait.
        let reply: RpcReply = socket::read_frame(&mut stream).await.map_err(failure)?;
        if reply.request_id != request.request_id || reply.protocol != PROTOCOL_VERSION {
            return Err(failure("agent host reply identity mismatch"));
        }
        match reply.result {
            RpcResult::Ok(v) => Ok(*v),
            RpcResult::Error(e) => Err(e),
        }
    }
    pub async fn close(mut self) -> Result<(), PublicError> {
        stop_invocation(&self.group, StopPolicy::default())
            .await
            .map_err(failure)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if self.payload.try_wait().map_err(failure)?.is_some() {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(failure("agent host was not reaped"));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _ = std::fs::remove_file(self.path);
        Ok(())
    }
}
pub async fn serve(home: PathBuf, launch: Launch) -> Result<(), PublicError> {
    let directory = PathBuf::from(
        std::env::var_os("SLUICE_RUN_DIR").ok_or_else(|| failure("sidecar directory absent"))?,
    );
    let listener = UnixListener::bind(directory.join("agent.sock")).map_err(failure)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        directory.join("agent.sock"),
        std::fs::Permissions::from_mode(0o600),
    )
    .map_err(failure)?;
    let context = launch
        .context
        .clone()
        .ok_or_else(|| failure("sidecar reservation context absent"))?;
    let agents = crate::agent_factory::RunAgents::new(
        home,
        launch.clone(),
        context,
        tokio_util::sync::CancellationToken::new(),
    );
    loop {
        let (mut stream, _) = listener.accept().await.map_err(failure)?;
        let request: HelperRequest = socket::read_frame(&mut stream).await.map_err(failure)?;
        let result = match request.command {
            HelperCommand::Runtime(CommandRequest::Builtin { invocation })
                if request.protocol == PROTOCOL_VERSION
                    && request.run_capability.as_ref() == Some(&launch.capability)
                    && invocation.run == launch.identity.run
                    && invocation.attempt == launch.identity.attempt
                    && invocation.project == launch.invocation.project
                    && invocation.step == launch.identity.step =>
            {
                agents.compose(invocation).await.and_then(|v| {
                    Ok(CommandReply::Data(
                        serde_json::to_value(v).map_err(failure)?.try_into()?,
                    ))
                })
            }
            _ => Err(failure("invalid sidecar invocation")),
        };
        let result = match result {
            Ok(v) => RpcResult::Ok(Box::new(v)),
            Err(e) => RpcResult::Error(e),
        };
        socket::write_frame(
            &mut stream,
            &RpcReply {
                protocol: 1,
                request_id: request.request_id,
                result,
            },
        )
        .await
        .map_err(failure)?;
    }
}
