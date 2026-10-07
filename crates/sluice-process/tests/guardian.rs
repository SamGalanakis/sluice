use sluice_model::{error::PublicError, ids::*, rpc::*};
use sluice_process::{guardian::*, identity::ProcessIdentity, journal::*, socket::*};
use std::{
    io,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct FixtureHost {
    state: Arc<FixtureState>,
}
#[derive(Default)]
struct FixtureState {
    starts: AtomicUsize,
    polls: AtomicUsize,
    cleaned: AtomicUsize,
    done: AtomicBool,
    delay_spawn: AtomicBool,
    delivered: Mutex<Vec<MessageId>>,
    checkpoints: Mutex<Vec<JsonMap>>,
}
impl FnHost for FixtureHost {
    async fn invoke(&self, _: FnInvocation) -> Result<JsonMap, PublicError> {
        Ok(JsonMap::default())
    }
}
impl PayloadHost for FixtureHost {
    type Invocation = FixtureInvocation;
    async fn start(
        &self,
        request: LaunchRequest,
        cancel: CancellationToken,
    ) -> io::Result<FixtureInvocation> {
        if self.state.delay_spawn.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        if cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        self.state.starts.fetch_add(1, Ordering::SeqCst);
        self.state
            .checkpoints
            .lock()
            .unwrap()
            .push(request.checkpoint);
        let backlog: Vec<DeliveryMessage> =
            decode_json(&std::fs::read(request.run_dir.join("messages.json"))?).unwrap();
        self.state
            .delivered
            .lock()
            .unwrap()
            .extend(backlog.iter().map(|m| m.id));
        Ok(FixtureInvocation {
            id: request.invocation.invocation,
            host: self.clone(),
            identity: ProcessIdentity::read(std::process::id())?,
        })
    }
    fn empty_without_payload(&self) -> io::Result<CleanupEvidence> {
        Ok(proof())
    }
}
struct FixtureInvocation {
    id: InvocationId,
    host: FixtureHost,
    identity: ProcessIdentity,
}
impl PayloadInvocation for FixtureInvocation {
    fn id(&self) -> InvocationId {
        self.id
    }
    fn executor(&self) -> &ProcessIdentity {
        &self.identity
    }
    async fn poll(&mut self) -> io::Result<Option<(PayloadResult, ExitEvidence)>> {
        self.host.state.polls.fetch_add(1, Ordering::SeqCst);
        if !self.host.state.done.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let outputs = self
            .host
            .invoke(FnInvocation {
                project: ProjectId::new(),
                step: None,
                run: RunId::new(),
                attempt: AttemptId::new(),
                invocation: self.id,
                name: "fixture".into(),
                inputs: JsonMap::default(),
            })
            .await
            .unwrap();
        Ok(Some((
            PayloadResult::Succeeded(outputs),
            ExitEvidence {
                code: Some(0),
                signal: None,
                executor: Some(self.identity.clone()),
            },
        )))
    }
    async fn deliver(&mut self, messages: &[DeliveryMessage]) -> io::Result<()> {
        self.host
            .state
            .delivered
            .lock()
            .unwrap()
            .extend(messages.iter().map(|m| m.id));
        Ok(())
    }
    async fn cleanup(&mut self) -> io::Result<CleanupEvidence> {
        self.host.state.cleaned.fetch_add(1, Ordering::SeqCst);
        Ok(proof())
    }
}
fn proof() -> CleanupEvidence {
    CleanupEvidence {
        cgroup: "fixture/payload".into(),
        empty: true,
        escalated: false,
    }
}
fn setup(home: &Path) -> (GuardianArgs, MemoryCoordinator, FixtureHost) {
    let project = ProjectId::new();
    let run = RunId::new();
    let attempt = AttemptId::new();
    let identity = AttemptKey {
        home: HomeId::new(),
        project: Some(project),
        step: Some("work".parse().unwrap()),
        generation: StepGeneration(1),
        work: WorkGeneration(1),
        run,
        attempt,
    };
    let assigned = AssignedRange {
        after: MessageId(0),
        through: MessageId(2),
    };
    let args = GuardianArgs {
        home_dir: home.into(),
        run_dir: home.join("run"),
        guardian: GuardianIdentity {
            identity,
            process: ProcessIdentity::read(std::process::id()).unwrap(),
            unit: format!("sluice-test-{run}.service"),
            socket_challenge: "private-challenge".into(),
        },
        invocation: FnInvocation {
            project,
            step: Some("work".parse().unwrap()),
            run,
            attempt,
            invocation: InvocationId::new(),
            name: "fixture".into(),
            inputs: JsonMap::default(),
        },
        assigned,
        prev_run: None,
        capability: RunCapability::new("fixture-secret"),
        poll_interval: Duration::from_millis(2),
    };
    let link = MemoryCoordinator::default();
    link.reserve(
        args.guardian.identity.clone(),
        assigned,
        vec![message(1), message(2), message(3)],
    );
    (args, link, FixtureHost::default())
}
fn message(id: i64) -> DeliveryMessage {
    DeliveryMessage {
        id: MessageId(id),
        body: JsonValue::try_from(serde_json::json!({"text":format!("message {id}")})).unwrap(),
    }
}
async fn until(mut f: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !f() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
async fn control(
    args: &GuardianArgs,
    command: ControlCommand,
) -> Result<ControlReply, PublicError> {
    call(
        &args.run_dir.join("control.sock"),
        &args.capability,
        command,
    )
    .await
}
fn launch(
    args: &GuardianArgs,
    link: &MemoryCoordinator,
    host: &FixtureHost,
) -> tokio::task::JoinHandle<io::Result<GuardianOutcome>> {
    let (args, link, host) = (args.clone(), link.clone(), host.clone());
    tokio::spawn(async move { guardian_main(args, &link, &host).await })
}

#[tokio::test]
async fn busy_completion_keeps_the_exact_journal_and_never_runs_again() {
    let home = tempfile::tempdir().unwrap();
    let (args, link, host) = setup(home.path());
    link.with_state(|s| s.busy_completions = usize::MAX);
    host.state.done.store(true, Ordering::SeqCst);
    let task = launch(&args, &link, &host);
    until(|| args.run_dir.join("completion.json").exists()).await;
    let journal = CompletionJournal::read(&args.run_dir, &args.guardian.identity)
        .unwrap()
        .unwrap();
    assert_eq!(host.state.cleaned.load(Ordering::SeqCst), 1);
    link.with_state(|s| assert_eq!(s.attempts[&args.invocation.run].releases, 0));
    link.with_state(|s| s.busy_completions = 0);
    task.await.unwrap().unwrap();
    link.request(CoordinatorCommand::Complete(Box::new(journal.clone())))
        .await
        .unwrap();
    link.request(CoordinatorCommand::Complete(Box::new(journal)))
        .await
        .unwrap();
    assert_eq!(host.state.starts.load(Ordering::SeqCst), 1);
    link.with_state(|s| assert_eq!(s.attempts[&args.invocation.run].releases, 1));
}
#[tokio::test]
async fn unknown_or_undelivered_acknowledgements_and_wrong_capabilities_are_refused() {
    let home = tempfile::tempdir().unwrap();
    let (args, link, host) = setup(home.path());
    let task = launch(&args, &link, &host);
    until(|| host.state.starts.load(Ordering::SeqCst) == 1).await;
    assert!(
        control(
            &args,
            ControlCommand::DeliveryAck(DeliveryAck {
                invocation: args.invocation.invocation,
                message: MessageId(99)
            })
        )
        .await
        .is_err()
    );
    assert!(
        call::<_, ControlReply>(
            &args.run_dir.join("control.sock"),
            &RunCapability::new("wrong"),
            ControlCommand::Cancel
        )
        .await
        .is_err()
    );
    assert!(control(&args, ControlCommand::Cancel).await.is_err());
    link.with_state(|s| s.attempts.get_mut(&args.invocation.run).unwrap().cancelled = true);
    // The guardian's watch sees the committed intent at once, so it may have
    // closed its control socket before this request arrives.
    match control(&args, ControlCommand::Cancel).await {
        Ok(_) | Err(PublicError::Busy { .. }) => {}
        Err(e) => panic!("cancel after intent: {e:?}"),
    }
    task.await.unwrap().unwrap();
}

#[test]
fn result_envelope_rejects_raw_exit_75_trailing_json_and_duplicate_keys() {
    for bytes in [
        br#"{"ok":true,"outputs":{}} {}"#.as_slice(),
        br#"{"ok":true,"ok":true,"outputs":{}}"#,
        br#"{"ok":false,"outputs":{}}"#,
    ] {
        assert!(decode_result(bytes, Some(0)).is_err());
    }
    assert!(decode_result(br#"{"ok":true,"outputs":{}}"#, Some(75)).is_err());
    assert!(matches!(
        decode_result(
            br#"{"ok":false,"error":{"error":"fn_failure","message":"boom"}}"#,
            Some(1)
        )
        .unwrap(),
        PayloadResult::Failed(_)
    ));
}
#[tokio::test]
async fn framing_rejects_protocol_length_and_duplicate_keys_before_dispatch() {
    use tokio::io::AsyncWriteExt;
    for raw in [serde_json::to_vec(&Request {protocol:2,request_id:RequestId("x".into()),run_capability:Some(RunCapability::new("secret")),command:ControlCommand::Cancel}).unwrap(),br#"{"protocol":1,"protocol":1,"request_id":"x","run_capability":"secret","command":{"method":"cancel"}}"#.to_vec()] {
        let (mut tx,mut rx)=tokio::net::UnixStream::pair().unwrap();
        tx.write_u32(raw.len() as u32).await.unwrap();tx.write_all(&raw).await.unwrap();
        assert!(read_request::<ControlCommand>(&mut rx,&RunCapability::new("secret")).await.is_err());
    }
    let (mut tx, mut rx) = tokio::net::UnixStream::pair().unwrap();
    tx.write_u32(MAX_FRAME_BYTES as u32 + 1).await.unwrap();
    assert!(read_frame::<ControlCommand>(&mut rx).await.is_err());
}

fn fixture_binary() -> std::path::PathBuf {
    std::env::var_os("SLUICE_FIXTURE_BINARY")
        .map(Into::into)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("fixture")
        })
}
fn coordinator_server(
    path: &Path,
    link: MemoryCoordinator,
    capability: RunCapability,
) -> tokio::task::JoinHandle<()> {
    let listener = tokio::net::UnixListener::bind(path).unwrap();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            if let Ok(request) = read_request::<CoordinatorCommand>(&mut stream, &capability).await
            {
                let result = link.request(request.command).await;
                let _ = write_frame(
                    &mut stream,
                    &Reply {
                        protocol: PROTOCOL_VERSION,
                        request_id: request.request_id,
                        result,
                    },
                )
                .await;
            }
        }
    })
}
struct UnitGuard(sluice_process::systemd::TransientService);
impl Drop for UnitGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new("/usr/bin/systemctl")
            .args(["--user", "stop", self.0.name()])
            .output();
        let _ = std::process::Command::new("/usr/bin/systemctl")
            .args(["--user", "reset-failed", self.0.name()])
            .output();
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real delegated service and compiled fixture binary"]
async fn real_guardian_survives_socket_coordinator_restart_and_busy_completion() {
    use sluice_process::{
        cgroup::Cgroup,
        signals::stop_run,
        systemd::{ServiceCommand, StartOutcome, TransientService},
    };
    let home = tempfile::tempdir().unwrap();
    let (args, link, _) = setup(home.path());
    std::fs::create_dir_all(&args.run_dir).unwrap();
    std::fs::write(
        home.path().join("args.json"),
        serde_json::to_vec(&args).unwrap(),
    )
    .unwrap();
    let path = home.path().join("coordinator.sock");
    let server = coordinator_server(&path, link.clone(), args.capability.clone());
    let mut guard = UnitGuard(TransientService::for_test(args.invocation.run));
    let mut spec = ServiceCommand::new(std::env::current_exe().unwrap());
    spec.args = [
        "--exact",
        "real_guardian_worker",
        "--ignored",
        "--nocapture",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    spec.env.insert(
        "SLUICE_P302_HOME".into(),
        home.path().as_os_str().to_owned(),
    );
    spec.env.insert(
        "SLUICE_FIXTURE_BINARY".into(),
        fixture_binary().into_os_string(),
    );
    let StartOutcome::Confirmed { state, .. } = guard.0.start_once(&spec).await.unwrap() else {
        panic!("service start uncertain")
    };
    let group = Cgroup::open_service(state.cgroup.as_deref().unwrap()).unwrap();
    until(|| args.run_dir.join("dispatch-count").exists()).await;
    server.abort();
    let _ = server.await;
    std::fs::remove_file(&path).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let guardian = link.with_state(|s| s.attempts[&args.invocation.run].claimed.clone().unwrap());
    assert!(guardian.process.matches_current().unwrap());
    let adoption = AdoptionAttempt {
        identity: args.guardian.identity.clone(),
        guardian: Some(guardian),
        run_dir: args.run_dir.clone(),
        unit: guard.0.name().into(),
        service_cgroup: Some(group.path().into()),
        capability: args.capability.clone(),
    };
    assert_eq!(
        adopt_attempt(
            &adoption,
            &link,
            &OsAdoptionHost {
                timeout: Duration::from_secs(10)
            }
        )
        .await
        .unwrap(),
        AdoptionOutcome::Reconnected
    );
    // Finish while no coordinator socket exists. The collected checkpoint must
    // precede reconnect, and the executor is never launched again.
    std::fs::write(args.run_dir.join("finish"), b"").unwrap();
    until(|| args.run_dir.join("collected.json").exists()).await;
    link.with_state(|s| s.busy_completions = usize::MAX);
    let server = coordinator_server(&path, link.clone(), args.capability.clone());
    until(|| args.run_dir.join("completion.json").exists()).await;
    assert_eq!(
        std::fs::read_to_string(args.run_dir.join("dispatch-count")).unwrap(),
        "dispatch\n"
    );
    link.with_state(|s| assert_eq!(s.attempts[&args.invocation.run].releases, 0));
    link.with_state(|s| s.busy_completions = 0);
    until(|| args.run_dir.join("worker-done").exists()).await;
    assert!(!args.run_dir.join("completion.json").exists());
    stop_run(&guard.0, &group, Duration::from_secs(10))
        .await
        .unwrap();
    assert!(!group.populated().unwrap());
    link.with_state(|s| assert_eq!(s.attempts[&args.invocation.run].releases, 1));
    server.abort();
    let _ = server.await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "real delegated service and compiled fixture binary"]
async fn real_guardian_death_is_lost_only_after_service_emptiness() {
    use sluice_process::{
        cgroup::Cgroup,
        identity::OwnedProcess,
        systemd::{ServiceCommand, StartOutcome, TransientService},
    };
    let home = tempfile::tempdir().unwrap();
    let (args, link, _) = setup(home.path());
    std::fs::create_dir_all(&args.run_dir).unwrap();
    std::fs::write(
        home.path().join("args.json"),
        serde_json::to_vec(&args).unwrap(),
    )
    .unwrap();
    let path = home.path().join("coordinator.sock");
    let server = coordinator_server(&path, link.clone(), args.capability.clone());
    let mut guard = UnitGuard(TransientService::for_test(args.invocation.run));
    let mut spec = ServiceCommand::new(std::env::current_exe().unwrap());
    spec.args = [
        "--exact",
        "real_guardian_worker",
        "--ignored",
        "--nocapture",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    spec.env.insert(
        "SLUICE_P302_HOME".into(),
        home.path().as_os_str().to_owned(),
    );
    spec.env.insert(
        "SLUICE_FIXTURE_BINARY".into(),
        fixture_binary().into_os_string(),
    );
    let StartOutcome::Confirmed { state, .. } = guard.0.start_once(&spec).await.unwrap() else {
        panic!("uncertain")
    };
    let group = Cgroup::open_service(state.cgroup.as_deref().unwrap()).unwrap();
    until(|| args.run_dir.join("dispatch-count").exists()).await;
    let guardian = link.with_state(|s| s.attempts[&args.invocation.run].claimed.clone().unwrap());
    let owned = OwnedProcess::open(&guardian.process).unwrap();
    owned.signal(rustix::process::Signal::KILL).unwrap();
    group
        .wait_populated(false, Duration::from_secs(10))
        .await
        .unwrap();
    assert!(owned.exited().unwrap());
    let a = AdoptionAttempt {
        identity: args.guardian.identity.clone(),
        guardian: Some(guardian),
        run_dir: args.run_dir.clone(),
        unit: guard.0.name().into(),
        service_cgroup: Some(group.path().into()),
        capability: args.capability.clone(),
    };
    assert!(matches!(
        adopt_attempt(
            &a,
            &link,
            &OsAdoptionHost {
                timeout: Duration::from_secs(10)
            }
        )
        .await
        .unwrap(),
        AdoptionOutcome::Lost(_)
    ));
    link.with_state(|s| {
        let r = &s.attempts[&args.invocation.run];
        assert_eq!(r.releases, 1);
        assert!(matches!(
            r.completion.as_ref().unwrap().result,
            PayloadResult::Lost(_)
        ));
    });
    assert_eq!(
        std::fs::read_to_string(args.run_dir.join("dispatch-count")).unwrap(),
        "dispatch\n"
    );
    server.abort();
    let _ = server.await;
}
#[tokio::test]
#[ignore = "internal guardian service worker"]
async fn real_guardian_worker() {
    let Some(home) = std::env::var_os("SLUICE_P302_HOME").map(std::path::PathBuf::from) else {
        return;
    };
    sluice_process::host::guard_scratch_home(&home).unwrap();
    let mut args: GuardianArgs =
        decode_json(&std::fs::read(home.join("args.json")).unwrap()).unwrap();
    let current = ProcessIdentity::read(std::process::id()).unwrap();
    let group = sluice_process::cgroup::Cgroup::open_service(&current.cgroup).unwrap();
    let host = OsFnHost::attach(
        UnimplementedFnHost,
        group,
        fixture_binary(),
        vec!["fn-result".into()],
        home.clone(),
    )
    .unwrap();
    args.guardian.process = host.control_identity().unwrap();
    let link = UnixCoordinatorLink {
        path: home.join("coordinator.sock"),
        capability: args.capability.clone(),
    };
    guardian_main(args.clone(), &link, &host).await.unwrap();
    std::fs::write(args.run_dir.join("worker-done"), b"done").unwrap();
}

#[tokio::test]
async fn completion_freezes_the_durable_submission_version() {
    let home = tempfile::tempdir().unwrap();
    let (args, link, host) = setup(home.path());
    link.with_state(|s| {
        s.attempts
            .get_mut(&args.invocation.run)
            .unwrap()
            .submissions = SubmissionSnapshot {
            version: Some(2),
            fields: decode_json(br#"{"submitted":true}"#).unwrap(),
        }
    });
    host.state.done.store(true, Ordering::SeqCst);
    guardian_main(args.clone(), &link, &host).await.unwrap();
    link.with_state(|s| {
        let j = s.attempts[&args.invocation.run]
            .completion
            .as_ref()
            .unwrap();
        assert_eq!(j.submission_version, Some(2));
        assert_eq!(j.submissions.0["submitted"].as_value(), true);
    });
}

/// A coordinator that stops answering while `stalled` holds, as one applying a burst of plan
/// edits does, then answers everything it was holding.
#[derive(Clone)]
struct StallLink {
    inner: MemoryCoordinator,
    gate: Arc<tokio::sync::watch::Sender<bool>>,
    held: Arc<Mutex<Vec<&'static str>>>,
}
impl StallLink {
    fn stall(&self, stalled: bool) {
        self.gate.send_replace(stalled);
    }
}
impl CoordinatorLink for StallLink {
    async fn request(&self, command: CoordinatorCommand) -> Result<CoordinatorReply, PublicError> {
        let mut gate = self.gate.subscribe();
        if *gate.borrow() {
            self.held.lock().unwrap().push(match command {
                CoordinatorCommand::Started { .. } => "started",
                CoordinatorCommand::DeliverAck { .. } => "deliver_ack",
                _ => "other",
            });
            gate.wait_for(|stalled| !stalled).await.unwrap();
        }
        self.inner.request(command).await
    }
}
#[tokio::test]
async fn delivery_acks_are_answered_while_the_coordinator_stalls_and_reported_after() {
    let home = tempfile::tempdir().unwrap();
    let (args, link, host) = setup(home.path());
    let stall = StallLink {
        inner: link.clone(),
        gate: Arc::new(tokio::sync::watch::channel(false).0),
        held: Arc::default(),
    };
    let task = {
        let (args, host, stall) = (args.clone(), host.clone(), stall.clone());
        tokio::spawn(async move { guardian_main(args, &stall, &host).await })
    };
    until(|| host.state.starts.load(Ordering::SeqCst) == 1).await;
    until(|| link.with_state(|s| s.requests.contains(&"started"))).await;
    until(|| host.state.delivered.lock().unwrap().len() == 3).await;
    stall.stall(true);
    let ack = |id| {
        control(
            &args,
            ControlCommand::DeliveryAck(DeliveryAck {
                invocation: args.invocation.invocation,
                message: MessageId(id),
            }),
        )
    };
    // The answer waits only for delivery.json, never for the coordinator.
    tokio::time::timeout(Duration::from_secs(1), ack(1))
        .await
        .expect("answered while the coordinator stalls")
        .unwrap();
    let durable: Vec<DeliveryAck> =
        decode_json(&std::fs::read(args.run_dir.join("delivery.json")).unwrap()).unwrap();
    assert_eq!(durable.len(), 1);
    // The tick's report of that ack is now held by the stalled coordinator; the guardian
    // still answers the next acknowledgement at once.
    until(|| stall.held.lock().unwrap().contains(&"deliver_ack")).await;
    tokio::time::timeout(Duration::from_secs(1), ack(2))
        .await
        .expect("answered while a report is held")
        .unwrap();
    link.with_state(|s| assert!(s.attempts[&args.invocation.run].acknowledgements.is_empty()));
    // Once the coordinator answers again, it hears of both.
    stall.stall(false);
    until(|| link.with_state(|s| s.attempts[&args.invocation.run].acknowledgements.len() == 2))
        .await;
    host.state.done.store(true, Ordering::SeqCst);
    assert!(matches!(
        task.await.unwrap().unwrap(),
        GuardianOutcome::Completed(_)
    ));
}
