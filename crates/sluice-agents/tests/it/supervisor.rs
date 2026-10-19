use sluice_agents::{ScriptedEngine, engines::*, supervisor::*};
use sluice_model::{ids::*, rpc::JsonValue};
use sluice_process::socket::{AssignedRange, DeliveryMessage};
use std::{
    collections::BTreeMap,
    fs, io,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = crate::scratch::root().join(format!(
            "sluice-test-supervisor-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Host {
    outputs: BTreeMap<String, serde_json::Value>,
    snapshots: u32,
    complete_after: u32,
    complete_after_nudge: bool,
    has_nudged: bool,
    /// The agent submits once a checkpoint shows this state (`Idle`: its turn is over);
    /// `None` submits from the start.
    submit_at: Option<State>,
    submitted: bool,
    cleanup: u32,
    fail_cleanup: bool,
    acks: Vec<MessageId>,
    notes: Vec<String>,
    messages: Vec<DeliveryMessage>,
    me: String,
}
impl Host {
    fn new() -> Self {
        Self {
            outputs: BTreeMap::from([("word".into(), serde_json::json!("blue"))]),
            snapshots: 0,
            complete_after: 0,
            complete_after_nudge: false,
            has_nudged: false,
            submit_at: Some(State::Idle),
            submitted: false,
            cleanup: 0,
            fail_cleanup: false,
            acks: vec![],
            notes: vec![],
            messages: vec![],
            me: "step stands here".into(),
        }
    }
}
impl SupervisorHost for Host {
    async fn snapshot(&mut self, after: MessageId) -> io::Result<HostSnapshot> {
        self.snapshots += 1;
        Ok(HostSnapshot {
            submissions: if self.snapshots >= self.complete_after
                && (self.submit_at.is_none() || self.submitted)
                && (!self.complete_after_nudge || self.has_nudged)
            {
                self.outputs.clone()
            } else {
                BTreeMap::new()
            },
            messages: self
                .messages
                .iter()
                .filter(|m| m.id > after)
                .cloned()
                .collect(),
            ..HostSnapshot::default()
        })
    }
    async fn acknowledge(&mut self, ids: &[MessageId]) -> io::Result<()> {
        for id in ids {
            if !self.acks.contains(id) {
                self.acks.push(*id);
            }
        }
        Ok(())
    }
    async fn me(&mut self) -> io::Result<String> {
        Ok(self.me.clone())
    }
    async fn note(&mut self, body: &str) -> io::Result<()> {
        self.notes.push(body.into());
        Ok(())
    }
    async fn checkpoint(&mut self, checkpoint: &Checkpoint) -> io::Result<()> {
        self.has_nudged = checkpoint.nudges > 0;
        self.submitted |= self.submit_at == Some(checkpoint.state);
        Ok(())
    }
    async fn cleanup(&mut self) -> io::Result<()> {
        self.cleanup += 1;
        if self.fail_cleanup {
            Err(io::Error::other("not empty"))
        } else {
            Ok(())
        }
    }
}
fn config(scratch: &Scratch) -> SupervisorConfig {
    let cwd = scratch.0.join("work");
    fs::create_dir(&cwd).unwrap();
    SupervisorConfig {
        run: RunId::new(),
        attempt: AttemptId::new(),
        invocation: InvocationId::new(),
        project: "p".into(),
        home: scratch.0.clone(),
        run_dir: scratch.0.join("run"),
        cwd,
        workspace: None,
        engine: "fake".into(),
        task: "the task".into(),
        required: vec!["word".into()],
        session: None,
        previous: None,
        assigned: AssignedRange {
            after: MessageId(0),
            through: MessageId(0),
        },
        messages: vec![],
        model: None,
        limits: Limits::test_profile(),
        retry: RetryPolicy {
            backoff: Duration::from_millis(5),
            ..RetryPolicy::agent()
        },
        internal_attempt: 1,
    }
}
fn observation(status: EngineStatus, starts: u64, completed: u64) -> EngineObservation {
    EngineObservation {
        status,
        turns_started: starts,
        turns_completed: completed,
        session_id: Some("s-1".into()),
        final_text: format!("reply {completed}"),
        progress: starts + completed,
        ..EngineObservation::default()
    }
}
fn frame(command: Option<EngineCommand>, o: EngineObservation) -> ScriptFrame {
    ScriptFrame {
        command,
        outcome: DeliveryOutcome::Acknowledged,
        observation: o,
        error: None,
        delay_ms: 0,
    }
}
fn input(id: InputId) -> EngineCommand {
    EngineCommand::DeliverText {
        id,
        text: "*".into(),
    }
}
fn happy() -> Vec<ScriptFrame> {
    vec![
        frame(
            Some(EngineCommand::StartFresh),
            observation(EngineStatus::Idle, 0, 0),
        ),
        frame(
            Some(input(InputId::Task)),
            observation(EngineStatus::Busy, 1, 0),
        ),
        frame(None, observation(EngineStatus::Idle, 1, 1)),
    ]
}

/// A host whose peer answers `busy` for `busy_for` from the first acknowledgement and note,
/// as a guardian or coordinator stalled behind plan edits does, retried as `RunHost` retries.
struct StallingHost {
    host: Host,
    busy_for: Duration,
    patience: Duration,
    since: Option<tokio::time::Instant>,
    tries: std::sync::Arc<AtomicU64>,
    cancel: CancellationToken,
}
impl StallingHost {
    async fn stalled(&mut self) -> io::Result<()> {
        let since = *self.since.get_or_insert_with(tokio::time::Instant::now);
        let (busy_for, tries) = (self.busy_for, self.tries.clone());
        patiently(&self.cancel, Some(self.patience), || {
            let tries = tries.clone();
            async move {
                tries.fetch_add(1, Ordering::Relaxed);
                if since.elapsed() < busy_for {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    Err(sluice_model::error::PublicError::Busy {
                        message: "the run's guardian did not answer within 5s".into(),
                        retryable: true,
                    })
                } else {
                    Ok(())
                }
            }
        })
        .await
    }
}
impl SupervisorHost for StallingHost {
    async fn snapshot(&mut self, after: MessageId) -> io::Result<HostSnapshot> {
        self.host.snapshot(after).await
    }
    async fn acknowledge(&mut self, ids: &[MessageId]) -> io::Result<()> {
        if !ids.is_empty() {
            self.stalled().await?;
        }
        self.host.acknowledge(ids).await
    }
    async fn me(&mut self) -> io::Result<String> {
        self.host.me().await
    }
    async fn note(&mut self, body: &str) -> io::Result<()> {
        self.stalled().await?;
        self.host.note(body).await
    }
    async fn checkpoint(&mut self, checkpoint: &Checkpoint) -> io::Result<()> {
        self.host.checkpoint(checkpoint).await
    }
    async fn cleanup(&mut self) -> io::Result<()> {
        self.host.cleanup().await
    }
}
/// One live message steered into the running turn, then the turn completes.
fn live_message_frames() -> (Vec<ScriptFrame>, Host) {
    let mut frames = happy()[..2].to_vec();
    frames.push(frame(
        Some(EngineCommand::Steer {
            id: InputId::Message { id: MessageId(1) },
            text: "*".into(),
        }),
        observation(EngineStatus::Busy, 2, 0),
    ));
    frames.push(frame(None, observation(EngineStatus::Idle, 2, 1)));
    let mut host = Host::new();
    host.messages = vec![DeliveryMessage {
        id: MessageId(1),
        body: JsonValue::try_from(serde_json::json!({"body":"live"})).unwrap(),
    }];
    (frames, host)
}
#[tokio::test]
async fn a_stalled_peer_delays_the_acknowledgement_but_never_fails_the_run() {
    let scratch = Scratch::new();
    let config = config(&scratch);
    let (frames, host) = live_message_frames();
    let mut engine = ScriptedEngine::new(frames);
    let cancel = CancellationToken::new();
    let mut stalling = StallingHost {
        host,
        busy_for: Duration::from_millis(300),
        patience: HOST_PATIENCE,
        since: None,
        tries: Default::default(),
        cancel: cancel.clone(),
    };
    supervise(
        config,
        &mut engine,
        &mut stalling,
        &mut SessionGuard::default(),
        None,
        &cancel,
    )
    .await
    .unwrap();
    assert!(stalling.tries.load(Ordering::Relaxed) > 2);
    assert_eq!(stalling.host.acks, vec![MessageId(1)]);
}

#[tokio::test]
async fn a_cancelled_run_stops_waiting_on_a_stalled_peer() {
    let cancel = CancellationToken::new();
    let waiting = patiently(&cancel, None, || async {
        Err::<(), _>(sluice_model::error::PublicError::Busy {
            message: "the coordinator did not answer within 5s".into(),
            retryable: true,
        })
    });
    cancel.cancel();
    let error = waiting.await.unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    // A definite refusal is never retried.
    let tries = AtomicU64::new(0);
    let error = patiently(&CancellationToken::new(), None, || {
        tries.fetch_add(1, Ordering::Relaxed);
        async {
            Err::<(), _>(sluice_model::error::PublicError::BadRequest {
                message: "wrong invocation acknowledgement".into(),
            })
        }
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("wrong invocation"));
    assert_eq!(tries.load(Ordering::Relaxed), 1);
}

/// The guardian answers control requests in order: a hook it accepted first must be
/// answered by the supervisor before a DeliveryAck queued behind it can return.
struct SerialGuardian {
    host: Host,
    hooks: PathBuf,
    run: RunId,
    engine: String,
    held: bool,
}
impl SupervisorHost for SerialGuardian {
    async fn snapshot(&mut self, after: MessageId) -> io::Result<HostSnapshot> {
        self.host.snapshot(after).await
    }
    async fn acknowledge(&mut self, ids: &[MessageId]) -> io::Result<()> {
        if !ids.is_empty() && !self.held {
            self.held = true;
            fs::create_dir_all(&self.hooks)?;
            let request = serde_json::json!({"engine":self.engine,"run":self.run,"event":"Stop","payload":{"hook_event_name":"Stop"}});
            fs::write(self.hooks.join("held.tmp"), request.to_string())?;
            fs::rename(
                self.hooks.join("held.tmp"),
                self.hooks.join("held.request.json"),
            )?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            while !self.hooks.join("held.reply.json").exists() {
                if tokio::time::Instant::now() >= deadline {
                    return Err(io::Error::other("engine hook decision timed out"));
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        self.host.acknowledge(ids).await
    }
    async fn me(&mut self) -> io::Result<String> {
        self.host.me().await
    }
    async fn note(&mut self, body: &str) -> io::Result<()> {
        self.host.note(body).await
    }
    async fn checkpoint(&mut self, checkpoint: &Checkpoint) -> io::Result<()> {
        self.host.checkpoint(checkpoint).await
    }
    async fn cleanup(&mut self) -> io::Result<()> {
        self.host.cleanup().await
    }
}
#[tokio::test]
async fn hooks_are_answered_while_a_delivery_ack_waits_on_the_guardian() {
    let scratch = Scratch::new();
    let config = config(&scratch);
    let mut frames = happy()[..2].to_vec();
    frames.push(frame(
        Some(EngineCommand::Steer {
            id: InputId::Message { id: MessageId(1) },
            text: "*".into(),
        }),
        observation(EngineStatus::Busy, 2, 0),
    ));
    frames.push(frame(None, observation(EngineStatus::Idle, 2, 1)));
    let mut engine = ScriptedEngine::new(frames);
    engine.hook_reply = Some(HookReply {
        stdout: None,
        exit_code: 0,
    });
    let mut host = Host::new();
    host.messages = vec![DeliveryMessage {
        id: MessageId(1),
        body: JsonValue::try_from(serde_json::json!({"body":"live"})).unwrap(),
    }];
    let mut guardian = SerialGuardian {
        host,
        hooks: config.run_dir.join("engine-hooks"),
        run: config.run,
        engine: engine.profile.engine.clone(),
        held: false,
    };
    let started = std::time::Instant::now();
    supervise(
        config,
        &mut engine,
        &mut guardian,
        &mut SessionGuard::default(),
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(engine.hooks.len(), 1);
    assert_eq!(guardian.host.acks, vec![MessageId(1)]);
}

/// Queued gates used to get told to stop after WAIT_MIN; once idle, an agent could be
/// reminded again seconds later. Drive the public supervision policy with elapsed time.
#[test]
fn background_jobs_block_nudges_and_idle_reminders_are_at_most_hourly() {
    let checkpoint = || {
        serde_json::from_value::<Checkpoint>(serde_json::json!({
            "version":1, "run":RunId::new(), "attempt":AttemptId::new(),
            "invocation":InvocationId::new(), "engine":"codex", "cwd":"/tmp",
            "internal_attempt":1, "session":"s-1", "head_before":null, "started_ms":0,
            "state":"Busy", "delivery":{"entries":[]}, "submissions":{}, "nudges":0,
            "compactions":0, "live_after":0, "final_text":"", "notes":[]
        }))
        .unwrap()
    };
    let limits = Limits {
        wall: Duration::from_secs(24 * 3600),
        ..Limits::default()
    };
    let mut machine = Machine::new(
        checkpoint(),
        limits.clone(),
        vec!["word".into()],
        false,
        Duration::ZERO,
    );
    let mut idle = observation(EngineStatus::Busy, 1, 1);
    let background = vec!["live gate child".into()];
    assert_eq!(
        machine
            .update(Duration::ZERO, &idle, &background, false)
            .unwrap(),
        Action::None
    );
    assert_eq!(
        machine
            .update(Duration::from_secs(2 * 3600), &idle, &background, false)
            .unwrap(),
        Action::None
    );
    // Engine-reported waiting work receives the same protection.
    idle.status = EngineStatus::Blocked;
    idle.waiting = Some("gate queue".into());
    assert_eq!(
        machine
            .update(Duration::from_secs(3 * 3600), &idle, &[], false)
            .unwrap(),
        Action::None
    );
    idle.waiting = None;
    idle.status = EngineStatus::Idle;
    assert_eq!(
        machine
            .update(Duration::from_secs(3 * 3600), &idle, &[], true)
            .unwrap(),
        Action::None
    );
    let now = Duration::from_secs(3 * 3600);
    assert_eq!(
        machine.update(now, &idle, &[], false).unwrap(),
        Action::None
    );
    let first = now + limits.grace;
    let Action::Send { id, text, .. } = machine.update(first, &idle, &[], false).unwrap() else {
        panic!("an idle agent that forgot to submit must still be reminded");
    };
    assert!(
        text.contains("if you are waiting on a job, keep waiting"),
        "{text}"
    );
    machine.offered(&id, &idle, first).unwrap();
    idle.acknowledged = vec![id];
    idle.turns_started += 1;
    idle.turns_completed += 1;
    assert_eq!(
        machine
            .update(first + Duration::from_secs(1), &idle, &[], false)
            .unwrap(),
        Action::None
    );
    assert_eq!(
        machine
            .update(first + Duration::from_secs(3599), &idle, &[], false)
            .unwrap(),
        Action::None
    );
    let reentry = first + Duration::from_secs(3599);
    let mut resumed = Machine::new(
        machine.checkpoint.clone(),
        limits,
        vec!["word".into()],
        false,
        reentry,
    );
    assert_eq!(
        resumed.update(reentry, &idle, &[], false).unwrap(),
        Action::None
    );
    assert_eq!(
        resumed
            .update(reentry + Duration::from_secs(3599), &idle, &[], false)
            .unwrap(),
        Action::None
    );
    assert!(matches!(
        machine
            .update(first + Duration::from_secs(3600), &idle, &[], false)
            .unwrap(),
        Action::Send { .. }
    ));
}

#[test]
fn a_detached_live_job_is_observed_until_it_exits() {
    let output = std::process::Command::new("systemd-run")
        .args([
            "--user",
            "--quiet",
            "--wait",
            "--pipe",
            "--collect",
            "--service-type=exec",
            "--setenv=SLUICE_TEST=1",
            "-p",
            "Delegate=yes",
            "--unit",
        ])
        .arg(format!("sluice-test-{}", RunId::new()))
        .arg(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "supervisor::background_job_worker",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "internal cgroup observation worker"]
fn background_job_worker() {
    use sluice_process::{
        cgroup::{Cgroup, RunCgroups},
        identity::{OwnedProcess, ProcessIdentity},
    };
    use std::{
        io::{BufRead, BufReader},
        os::unix::process::CommandExt,
        process::{Command, Stdio},
    };
    let mut me = OwnedProcess::capture(std::process::id()).unwrap();
    let service = Cgroup::open_service(&me.identity().cgroup).unwrap();
    let limits = sluice_model::ResourcePolicy::for_host(64 << 30, 32000)
        .unwrap()
        .resolve(None, None)
        .unwrap();
    let groups = RunCgroups::create(service, &mut me, limits, 256 << 20).unwrap();
    let leaf = groups.invocation(InvocationId::new()).unwrap();
    leaf.move_process(&mut me).unwrap();
    let root = ProcessIdentity::read(std::process::id()).unwrap();
    let mut agent = Command::new("/bin/sh")
        .arg0("codex")
        .args(["-c", "printf 'ready\\n'; read value"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(agent.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "ready\n");
    let mut gate = Command::new("setsid")
        .args(["/bin/sh", "-c", "printf 'ready\\n'; read value"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    ready.clear();
    BufReader::new(gate.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "ready\n");
    let work = sluice_agents::quiet::background_work(&root, &[]).unwrap();
    assert_eq!(work.len(), 1, "{work:?}");
    assert!(work[0].contains(&format!("pid {} ", gate.id())), "{work:?}");
    gate.kill().unwrap();
    gate.wait().unwrap();
    assert!(
        sluice_agents::quiet::background_work(&root, &[])
            .unwrap()
            .is_empty()
    );
    agent.kill().unwrap();
    agent.wait().unwrap();
}

/// The coordinator's side of a park, in memory: the agent asked to park from its first turn.
struct ParkHost {
    host: Host,
    park: Option<sluice_model::park::ParkRequest>,
    entered: u32,
    woke: Vec<String>,
}
impl SupervisorHost for ParkHost {
    async fn snapshot(&mut self, after: MessageId) -> io::Result<HostSnapshot> {
        let mut snapshot = self.host.snapshot(after).await?;
        snapshot.park = self.park.clone();
        Ok(snapshot)
    }
    async fn acknowledge(&mut self, ids: &[MessageId]) -> io::Result<()> {
        self.host.acknowledge(ids).await
    }
    async fn me(&mut self) -> io::Result<String> {
        self.host.me().await
    }
    async fn note(&mut self, body: &str) -> io::Result<()> {
        self.host.note(body).await
    }
    async fn checkpoint(&mut self, checkpoint: &Checkpoint) -> io::Result<()> {
        // The agent submits in the turn its wake starts.
        self.host.submitted |= checkpoint.delivery.entries.iter().any(|entry| {
            matches!(entry.id, InputId::Wake { .. })
                && entry.state == sluice_agents::delivery::DeliveryState::Acknowledged
        });
        self.host.checkpoint(checkpoint).await
    }
    async fn cleanup(&mut self) -> io::Result<()> {
        self.host.cleanup().await
    }
    async fn park(
        &mut self,
        op: sluice_model::park::ParkOp,
    ) -> io::Result<sluice_model::park::ParkView> {
        use sluice_model::park::{ParkOp, ParkPhase, ParkView};
        let woke = match op {
            ParkOp::Read => false,
            ParkOp::Enter => {
                // Only the parked stretch runs on a paused clock, which skips its waits.
                tokio::time::pause();
                self.entered += 1;
                if let Some(park) = &mut self.park {
                    park.phase = ParkPhase::Parked;
                }
                false
            }
            ParkOp::Wake { reason } => {
                tokio::time::resume();
                self.woke.push(reason);
                true
            }
        };
        let view = ParkView {
            park: self.park.clone(),
            woke,
            ..ParkView::default()
        };
        if woke {
            self.park = None;
        }
        Ok(view)
    }
}

/// Parked time is left out of the wall cap: a run parked well past its cap still finishes,
/// in the session it parked in, after one "woke:" message. Tokio's clock is paused while it
/// is parked, so the park's ten-second timeout passes at once.
#[tokio::test]
async fn a_parked_run_outlasts_its_wall_cap_and_resumes_in_the_same_session() {
    use sluice_model::park::{ParkCondition, ParkPhase, ParkRequest};
    let scratch = Scratch::new();
    let mut config = config(&scratch);
    config.limits.poll = Duration::from_millis(500);
    let wall = config.limits.wall;
    let never = scratch.0.join("never");
    let mut frames = happy();
    frames.push(frame(
        Some(input(InputId::Wake { ordinal: 1 })),
        observation(EngineStatus::Busy, 2, 1),
    ));
    frames.push(frame(None, observation(EngineStatus::Idle, 2, 2)));
    let mut engine = ScriptedEngine::new(frames);
    let mut host = ParkHost {
        host: Host::new(),
        park: Some(ParkRequest {
            condition: ParkCondition::File {
                path: never.display().to_string(),
                marker: None,
            },
            timeout_seconds: Some(10),
            lease: None,
            phase: ParkPhase::Requested,
            requested_at: "now".into(),
            parked_at: None,
            woke: None,
        }),
        entered: 0,
        woke: vec![],
    };
    host.host.submit_at = Some(State::Done);
    let run_dir = config.run_dir.clone();
    let started = tokio::time::Instant::now();
    supervise(
        config,
        &mut engine,
        &mut host,
        &mut SessionGuard::default(),
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(started.elapsed() > wall + Duration::from_secs(5));
    assert_eq!(host.entered, 1);
    let reason = format!("timeout after 10s waiting for file:{}", never.display());
    assert_eq!(host.woke, vec![reason.clone()]);
    let woke: Vec<_> = engine
        .commands
        .iter()
        .filter_map(|c| match c {
            EngineCommand::DeliverText {
                id: InputId::Wake { .. },
                text,
            } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(woke, vec![format!("woke: {reason}")]);
    // The same session carried on: started once, never resumed or restarted.
    assert_eq!(
        engine
            .commands
            .iter()
            .filter(|c| matches!(c, EngineCommand::StartFresh | EngineCommand::Resume { .. }))
            .count(),
        1
    );
    let checkpoint = Checkpoint::read(&run_dir).unwrap().unwrap();
    assert!(checkpoint.parked_ms >= 10_000, "{}", checkpoint.parked_ms);
}
