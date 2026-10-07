use sluice_agents::{ScriptedEngine, engines::*, prompt, supervisor::*};
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
        let path = std::env::temp_dir().join(format!(
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

#[test]
fn limits_reject_malformed_nonfinite_nonpositive_and_fractional_counts() {
    for value in ["bad", "NaN", "inf", "0", "-1", "1e300"] {
        assert!(
            Limits::parse(|name| Ok((name == "SLUICE_AGENT_MAX_MIN").then(|| value.into())))
                .is_err()
        );
    }
    for value in ["0", "-1", "1.5", "inf"] {
        assert!(
            Limits::parse(|name| Ok((name == "SLUICE_AGENT_NUDGES").then(|| value.into())))
                .is_err()
        );
    }
    let limits =
        Limits::parse(|name| Ok((name == "SLUICE_AGENT_QUIET_MIN").then(|| "20".into()))).unwrap();
    assert_eq!(limits.quiet, Duration::from_secs(1200));
    assert_eq!(Limits::default().quiet, Duration::from_secs(2700));
}
#[test]
fn long_tasks_use_a_file_and_short_tasks_keep_shell_characters_literal() {
    let scratch = Scratch::new();
    let path = scratch.0.join("task.md");
    for text in ["two\nlines".into(), "x".repeat(501)] {
        assert!(
            prompt::hand_over(&text, &path, "Your task")
                .unwrap()
                .contains("read it fully")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }
    assert_eq!(
        prompt::hand_over("`ls $HOME` $(date) \\", &path, "Your task").unwrap(),
        "`ls $HOME` $(date) \\"
    );
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
