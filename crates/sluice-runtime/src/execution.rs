//! Guardian handoff and executable entries. Only scratch/test service units are used.
use crate::calls::{AdmittedCall, CallGuardian};
use serde::{Deserialize, Serialize};
use sluice_model::{
    error::PublicError,
    ids::*,
    rpc::{FnInvocation, JsonMap, RunCapability, decode_json},
};
use sluice_process::{
    guardian::{self, FnHost, GuardianArgs, OsFnHost},
    journal::{AttemptKey, PayloadResult},
    socket::{
        AssignedRange, CoordinatorCommand, CoordinatorLink, CoordinatorReply, GuardianIdentity,
        UnixCoordinatorLink,
    },
    systemd::{ServiceCommand, StartOutcome, TransientService},
};
use std::{
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// Validated admission context, independent of later registry publications.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenPlan {
    pub revision: Revision,
    pub document: JsonMap,
    pub signatures: indexmap::IndexMap<String, FrozenSignature>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenSignature {
    pub inputs: indexmap::IndexMap<String, sluice_model::types::Type>,
    pub outputs: indexmap::IndexMap<String, sluice_model::types::Type>,
    pub submits: indexmap::IndexMap<String, FrozenDeclaration>,
    pub open: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenDeclaration {
    pub ty: sluice_model::types::Type,
    pub doc: Option<String>,
}
impl FrozenPlan {
    pub fn from_context(context: &sluice_store::plans::PlanContext) -> Self {
        Self {
            revision: context.revision,
            document: context.plan.document().clone(),
            signatures: context
                .plan
                .steps()
                .values()
                .map(|step| {
                    let sig = &step.signature;
                    (
                        step.run.clone(),
                        FrozenSignature {
                            inputs: sig.inputs.clone(),
                            outputs: sig.outputs.clone(),
                            open: sig.open,
                            submits: sig
                                .submits
                                .iter()
                                .map(|(n, d)| {
                                    (
                                        n.clone(),
                                        FrozenDeclaration {
                                            ty: d.ty.clone(),
                                            doc: d.doc.clone(),
                                        },
                                    )
                                })
                                .collect(),
                        },
                    )
                })
                .collect(),
        }
    }
    pub fn context(
        self,
        project: ProjectId,
    ) -> Result<sluice_store::plans::PlanContext, PublicError> {
        let signatures = self
            .signatures
            .into_iter()
            .map(|(n, s)| {
                (
                    n,
                    sluice_model::plan::FnSignature {
                        inputs: s.inputs,
                        outputs: s.outputs,
                        open: s.open,
                        submits: s
                            .submits
                            .into_iter()
                            .map(|(n, d)| {
                                (
                                    n,
                                    sluice_model::plan::Declaration {
                                        ty: d.ty,
                                        doc: d.doc,
                                    },
                                )
                            })
                            .collect(),
                    },
                )
            })
            .collect::<indexmap::IndexMap<_, _>>();
        let plan =
            sluice_model::plan::Plan::parse(&self.document, &signatures).map_err(|errors| {
                PublicError::Invalid {
                    message: "invalid frozen completion plan".into(),
                    errors: errors.into_iter().map(|e| e.to_string()).collect(),
                }
            })?;
        Ok(sluice_store::plans::PlanContext {
            project,
            revision: self.revision,
            plan,
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Launch {
    pub identity: AttemptKey,
    pub invocation: FnInvocation,
    pub assigned: AssignedRange,
    pub prev_run: Option<RunId>,
    pub capability: RunCapability,
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub context: Option<crate::compose::ReservationContext>,
}
#[derive(Debug)]
pub enum LaunchOutcome {
    Accepted,
    /// The host guarantees no process was created. Ambiguous starts never use this.
    Refused(PublicError),
    Uncertain(String),
}
pub trait ExecutionHost: FnHost + guardian::AdoptionHost + Send + Sync + 'static {
    fn composition_enabled(&self) -> bool {
        false
    }
    fn launch(
        &self,
        launch: Launch,
    ) -> impl Future<Output = Result<LaunchOutcome, PublicError>> + Send;
    fn cleanup_valid(
        &self,
        journal: &sluice_process::journal::CompletionJournal,
    ) -> impl Future<Output = Result<bool, PublicError>> + Send;
}
#[derive(Clone)]
pub struct OsHost {
    pub home: PathBuf,
    pub program: PathBuf,
}
impl guardian::AdoptionHost for OsHost {
    async fn reconcile(
        &self,
        attempt: &guardian::AdoptionAttempt,
    ) -> io::Result<guardian::GuardianPresence> {
        guardian::OsAdoptionHost {
            timeout: Duration::from_secs(5),
        }
        .reconcile(attempt)
        .await
    }
}
impl ExecutionHost for OsHost {
    fn composition_enabled(&self) -> bool {
        true
    }
    async fn cleanup_valid(
        &self,
        journal: &sluice_process::journal::CompletionJournal,
    ) -> Result<bool, PublicError> {
        let service = TransientService::adopt_test(journal.identity.run);
        let state = service.query().await.map_err(storage)?;
        if let Some(ref root) = state.cgroup {
            let group = sluice_process::cgroup::Cgroup::open_service(root).map_err(storage)?;
            let empty = if state.stopped() {
                !group.populated().map_err(storage)?
            } else {
                !group.payload_populated().map_err(storage)?
            };
            Ok(empty
                && journal.cleanup.iter().all(|p| {
                    p.cgroup == *root
                        || p.cgroup == format!("{root}/payload")
                        || p.cgroup.starts_with(&format!("{root}/payload/"))
                }))
        } else {
            Ok(state.stopped()
                && journal.cleanup.iter().all(|p| {
                    p.cgroup.ends_with(service.name())
                        || p.cgroup.ends_with(&format!("/{}/payload", service.name()))
                        || p.cgroup.contains(&format!("/{}/payload/", service.name()))
                }))
        }
    }
    async fn launch(&self, launch: Launch) -> Result<LaunchOutcome, PublicError> {
        let home = self.home.clone();
        let run = launch.identity.run;
        match tokio::task::spawn_blocking(move || write_launch(&home, &launch)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Ok(LaunchOutcome::Refused(storage(e))),
            Err(e) => return Ok(LaunchOutcome::Refused(storage(e))),
        }
        let mut unit = TransientService::for_test(run);
        let mut command = ServiceCommand::new(&self.program);
        command.args = vec![
            "guardian".into(),
            "--run".into(),
            run.to_string().into(),
            "--attempt".into(),
            launch_attempt(&self.home, run)?.to_string().into(),
            "--socket".into(),
            self.home.join("coordinator.sock").into_os_string(),
        ];
        // A run inherits its launch snapshot, including provider configuration
        // and secrets. systemd otherwise replaces it with the manager's environment.
        command.env = std::env::vars_os().collect();
        command
            .env
            .insert("SLUICE_HOME".into(), self.home.clone().into_os_string());
        match unit.start_once(&command).await {
            Ok(StartOutcome::Confirmed { .. }) => Ok(LaunchOutcome::Accepted),
            Ok(StartOutcome::Uncertain { message, .. }) => Ok(LaunchOutcome::Uncertain(message)),
            Err(e) => Ok(LaunchOutcome::Uncertain(e.to_string())),
        }
    }
}
fn launch_attempt(home: &Path, run: RunId) -> Result<AttemptId, PublicError> {
    Ok(read_launch(home, run)?.identity.attempt)
}
fn storage(e: impl std::fmt::Display) -> PublicError {
    PublicError::Storage {
        message: e.to_string(),
    }
}
pub fn write_launch(home: &Path, launch: &Launch) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let dir = home.join("runs").join(launch.identity.run.to_string());
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let path = dir.join("runtime.json");
    let bytes = serde_json::to_vec(launch)?;
    if path.exists() {
        if std::fs::read(&path)? != bytes {
            return Err(io::Error::other("frozen launch changed"));
        }
        return Ok(());
    }
    let temp = dir.join(format!(".runtime-{}", InvocationId::new()));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    f.write_all(&bytes)?;
    f.sync_all()?;
    std::fs::rename(temp, path)?;
    std::fs::File::open(&dir)?.sync_all()
}
pub fn read_launch(home: &Path, run: RunId) -> Result<Launch, PublicError> {
    decode_json(
        &std::fs::read(home.join("runs").join(run.to_string()).join("runtime.json"))
            .map_err(storage)?,
    )
}

pub struct CallLauncher<H> {
    pub host: Arc<H>,
    pub home: HomeId,
    pub writer: sluice_store::Writer,
}
impl<H> CallLauncher<H> {
    fn host_home(&self) -> PathBuf {
        self.writer.home().to_path_buf()
    }
}
impl<H: ExecutionHost> CallGuardian for CallLauncher<H> {
    async fn launch(&self, call: AdmittedCall) -> Result<(), PublicError> {
        let launch = Launch {
            identity: AttemptKey {
                home: self.home,
                project: call.project,
                step: None,
                generation: StepGeneration(1),
                work: WorkGeneration(1),
                run: call.call,
                attempt: call.attempt,
            },
            invocation: FnInvocation {
                project: call
                    .project
                    .unwrap_or(self.home.to_string().parse().expect("home UUID")),
                step: None,
                run: call.call,
                attempt: call.attempt,
                invocation: InvocationId::new(),
                name: call.function.name.clone(),
                inputs: call.inputs.clone(),
            },
            assigned: AssignedRange {
                after: MessageId(0),
                through: MessageId(0),
            },
            prev_run: None,
            capability: serde_json::from_value(
                serde_json::to_value(&call.function.bundle).map_err(storage)?["capability"].clone(),
            )
            .map_err(storage)?,
            timeout_seconds: call.function.timeout_seconds,
            context: crate::compose::ReservationContext::from_call(
                &self.writer,
                &self.host_home(),
                &call,
            )
            .await?,
        };
        let identity = launch.identity.clone();
        match self.host.launch(launch).await? {
            LaunchOutcome::Accepted => Ok(()),
            LaunchOutcome::Refused(e) => {
                crate::calls::complete(
                    &self.writer,
                    crate::calls::CallCompletion {
                        call: identity.run,
                        attempt: identity.attempt,
                        project: identity.project,
                        completion_id: format!("spawn-{}", identity.attempt),
                        outputs: JsonMap::default(),
                        error: Some(e),
                        processes_gone: true,
                    },
                )
                .await?;
                Ok(())
            }
            LaunchOutcome::Uncertain(message) => Err(PublicError::ProcessLost { message }),
        }
    }
}
impl FnHost for OsHost {
    async fn invoke(&self, invocation: FnInvocation) -> Result<JsonMap, PublicError> {
        if invocation.name.starts_with("core.") || invocation.name.starts_with("fixture.") {
            fixture_dispatch(&self.home, invocation).await
        } else {
            Err(PublicError::not_implemented("unreserved fn execution"))
        }
    }
}
pub async fn fixture_dispatch(
    home: &Path,
    invocation: FnInvocation,
) -> Result<JsonMap, PublicError> {
    if invocation.name.starts_with("core.") {
        return crate::builtins::core::dispatch(
            &invocation.name,
            &invocation.inputs,
            &crate::builtins::jev::BuiltinCtx::new([]),
        )
        .await
        .map_err(|e| PublicError::FnFailure {
            message: e.to_string(),
        });
    }
    let dir = home.join("runs").join(invocation.run.to_string());
    match invocation.name.as_str() {
        "fixture.capacity" => {
            let value = std::fs::read(home.join("fixture-capacity.json")).map_err(storage)?;
            decode_json(&value)
        }
        "fixture.echo" | "fixture.wait" | "fixture.fail" | "fixture.submit" => {
            use std::io::Write;
            let mut marker = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("dispatch-count"))
                .map_err(storage)?;
            marker.write_all(b"dispatch\n").map_err(storage)?;
            if invocation.name == "fixture.wait" {
                while !dir.join("finish").exists() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            if invocation.name == "fixture.fail" {
                return Err(PublicError::FnFailure {
                    message: "fixture failed".into(),
                });
            }
            if invocation.name == "fixture.submit" {
                let capability = read_launch(home, invocation.run)?.capability;
                let request = sluice_model::rpc::RpcRequest {
                    protocol: 1,
                    request_id: sluice_model::rpc::RequestId("fixture-submit".into()),
                    run_capability: Some(capability.clone()),
                    command: sluice_model::commands::CommandRequest::StepSubmit(
                        sluice_model::commands::StepSubmit {
                            project: invocation.project,
                            step: invocation
                                .step
                                .clone()
                                .ok_or_else(|| storage("submit needs a step"))?,
                            run: invocation.run,
                            outputs: decode_json(br#"{"submitted":true}"#)?,
                            author: Some("fixture".into()),
                        },
                    ),
                };
                sluice_process::socket::call::<_, sluice_process::socket::ControlReply>(
                    &dir.join("control.sock"),
                    &capability,
                    sluice_process::socket::ControlCommand::Callback(Box::new(request.command)),
                )
                .await?;
            }
            decode_json(
                &serde_json::to_vec(&serde_json::json!({"value":invocation.inputs.0.get("value")}))
                    .map_err(storage)?,
            )
        }
        _ => Err(PublicError::not_implemented("fn dispatcher")),
    }
}
pub async fn guardian_entry(
    home: PathBuf,
    run: RunId,
    attempt: AttemptId,
    socket: PathBuf,
) -> Result<(), PublicError> {
    let launch = read_launch(&home, run)?;
    if launch.identity.attempt != attempt {
        return Err(storage("guardian attempt mismatch"));
    }
    let process =
        sluice_process::identity::ProcessIdentity::read(std::process::id()).map_err(storage)?;
    let group = sluice_process::cgroup::Cgroup::open_service(&process.cgroup).map_err(storage)?;
    let host = OsFnHost::attach(
        OsHost {
            home: home.clone(),
            program: std::env::current_exe().map_err(storage)?,
        },
        group,
        std::env::current_exe().map_err(storage)?,
        vec!["runtime".into()],
        home.clone(),
    )
    .map_err(storage)?
    .with_admission_guard(|home| {
        crate::install::Installation::for_home(home)
            .and_then(|install| install.payload_guard(home))
            .map(|guard| Box::new(guard) as Box<dyn Send>)
            .map_err(io::Error::other)
    });
    let args = GuardianArgs {
        home_dir: home.clone(),
        run_dir: home.join("runs").join(run.to_string()),
        guardian: GuardianIdentity {
            identity: launch.identity,
            process: host.control_identity().map_err(storage)?,
            unit: TransientService::for_test(run).name().into(),
            socket_challenge: InvocationId::new().to_string(),
        },
        invocation: launch.invocation,
        assigned: launch.assigned,
        prev_run: launch.prev_run,
        capability: launch.capability.clone(),
        poll_interval: Duration::from_millis(50),
    };
    let link = UnixCoordinatorLink {
        path: socket,
        capability: launch.capability,
    };
    // Capacity observations are bounded at the payload dispatcher; guardian cleanup
    // and completion retain ownership after the timeout result.
    guardian::guardian_main(args, &link, &host)
        .await
        .map_err(storage)?;
    Ok(())
}
pub async fn payload_entry(home: PathBuf) -> io::Result<i32> {
    let dir = PathBuf::from(
        std::env::var_os("SLUICE_RUN_DIR").ok_or_else(|| io::Error::other("run dir absent"))?,
    );
    let invocation: FnInvocation =
        decode_json(&std::fs::read(dir.join("invocation.json"))?).map_err(io::Error::other)?;
    let launch = read_launch(&home, invocation.run).map_err(io::Error::other)?;
    if std::env::var_os("SLUICE_AGENT_SIDECAR").is_some() {
        return crate::sidecar::serve(home, launch)
            .await
            .map(|()| 0)
            .map_err(io::Error::other);
    }
    if launch.context.is_some() {
        // The barrier has persisted executor identity. Commit start before any
        // function callback can request a lease or submit its outputs.
        let link = UnixCoordinatorLink {
            path: home.join("coordinator.sock"),
            capability: launch.capability.clone(),
        };
        match link
            .request(CoordinatorCommand::Started {
                identity: launch.identity.clone(),
                invocation: invocation.invocation,
                executor: sluice_process::identity::ProcessIdentity::read(std::process::id())?,
            })
            .await
            .map_err(io::Error::other)?
        {
            CoordinatorReply::Started => {}
            _ => return Err(io::Error::other("executor start was not acknowledged")),
        }
    }
    let dispatcher = launch.context.clone().map(|context| {
        Arc::new(crate::compose::Dispatcher::new(
            home.clone(),
            launch.clone(),
            context,
        ))
    });
    let dispatch = async {
        if invocation.name.starts_with("fixture.") {
            return fixture_dispatch(&home, invocation).await;
        }
        match dispatcher {
            Some(d) => d.execute(invocation).await,
            None => fixture_dispatch(&home, invocation).await,
        }
    };
    let result = if let Some(seconds) = launch.timeout_seconds {
        match tokio::time::timeout(Duration::from_secs(seconds), dispatch).await {
            Ok(r) => r,
            Err(_) => Err(PublicError::FnFailure {
                message: "fn timed out".into(),
            }),
        }
    } else {
        dispatch.await
    };
    let result = match result {
        Ok(outputs) => PayloadResult::Succeeded(outputs),
        Err(e) => PayloadResult::Failed(e),
    };
    let (value, code) = match result {
        PayloadResult::Succeeded(outputs) => (serde_json::json!({"ok":true,"outputs":outputs}), 0),
        PayloadResult::Failed(e) => (serde_json::json!({"ok":false,"error":e}), 1),
        _ => unreachable!(),
    };
    println!("{value}");
    Ok(code)
}
