use sluice_agents::{FixtureEngine, ScriptedEngine, engines::*, prompt, supervisor::*};
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
        effort: None,
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
async fn run(
    config: SupervisorConfig,
    engine: &mut impl EngineAdapter,
    host: &mut Host,
) -> Result<AgentResult, AgentFailure> {
    supervise(
        config,
        engine,
        host,
        &mut SessionGuard::default(),
        None,
        &CancellationToken::new(),
    )
    .await
}
#[tokio::test]
async fn done_requires_a_completed_turn_and_cleanup() {
    let scratch = Scratch::new();
    let config = config(&scratch);
    let directory = config.run_dir.clone();
    let mut engine = ScriptedEngine::new(happy());
    let mut host = Host::new();
    let result = run(config, &mut engine, &mut host).await.unwrap();
    assert_eq!(result.final_text, "reply 1");
    assert_eq!(result.session, "s-1");
    assert!(result.git.is_none());
    assert!(engine.commands.contains(&EngineCommand::RequestExit));
    assert_eq!(host.cleanup, 1);
    assert_eq!(
        Checkpoint::read(&directory).unwrap().unwrap().state,
        State::Done
    );
}
#[tokio::test]
async fn busy_with_outputs_submitted_obeys_wall_cap() {
    let scratch = Scratch::new();
    let mut config = config(&scratch);
    config.limits.wall = Duration::from_millis(80);
    let mut engine = ScriptedEngine::new(happy()[..2].to_vec());
    let mut host = Host::new();
    let error = run(config, &mut engine, &mut host).await.unwrap_err();
    assert_eq!(error.kind, FailureKind::WallCap);
    assert!(!engine.commands.contains(&EngineCommand::RequestExit));
    assert_eq!(host.cleanup, 1);
}
#[tokio::test]
async fn missing_outputs_get_bounded_nudges_then_fail() {
    let scratch = Scratch::new();
    let config = config(&scratch);
    let mut frames = happy();
    for n in 1..=2 {
        frames.push(frame(
            Some(input(InputId::Nudge { ordinal: n })),
            observation(EngineStatus::Busy, u64::from(n) + 1, u64::from(n)),
        ));
        frames.push(frame(
            None,
            observation(EngineStatus::Idle, u64::from(n) + 1, u64::from(n) + 1),
        ));
    }
    let mut engine = ScriptedEngine::new(frames);
    let mut host = Host::new();
    host.outputs.clear();
    let error = run(config, &mut engine, &mut host).await.unwrap_err();
    assert_eq!(error.kind, FailureKind::MissingOutputs);
    assert_eq!(
        engine
            .commands
            .iter()
            .filter(|c| matches!(
                c,
                EngineCommand::DeliverText {
                    id: InputId::Nudge { .. },
                    ..
                }
            ))
            .count(),
        2
    );
    assert!(error.message.contains("reply 3"));
    assert_eq!(error.session.as_deref(), Some("s-1"));
}
#[tokio::test]
async fn a_nudge_that_gets_outputs_finishes() {
    let scratch = Scratch::new();
    let config = config(&scratch);
    let mut frames = happy();
    frames.push(frame(
        Some(input(InputId::Nudge { ordinal: 1 })),
        observation(EngineStatus::Busy, 2, 1),
    ));
    frames.push(frame(None, observation(EngineStatus::Idle, 2, 2)));
    let mut engine = ScriptedEngine::new(frames);
    let mut host = Host::new();
    host.complete_after_nudge = true;
    let result = run(config, &mut engine, &mut host).await.unwrap();
    assert_eq!(result.final_text, "reply 2");
}
#[tokio::test]
async fn unacknowledged_input_and_unknown_acceptance_never_finish_or_replay() {
    for outcome in [DeliveryOutcome::Pending, DeliveryOutcome::Uncertain] {
        let scratch = Scratch::new();
        let config = config(&scratch);
        let mut frames = happy();
        frames[1].outcome = outcome;
        let mut engine = ScriptedEngine::new(frames);
        let mut host = Host::new();
        let error = run(config, &mut engine, &mut host).await.unwrap_err();
        assert!(matches!(
            error.kind,
            FailureKind::UnknownAcceptance | FailureKind::WallCap
        ));
        assert_eq!(
            engine
                .commands
                .iter()
                .filter(|c| matches!(
                    c,
                    EngineCommand::DeliverText {
                        id: InputId::Task,
                        ..
                    }
                ))
                .count(),
            1
        );
    }
}
#[tokio::test]
async fn verified_nonacceptance_permits_only_one_redelivery() {
    let scratch = Scratch::new();
    let config = config(&scratch);
    let mut frames = happy();
    frames[1].outcome = DeliveryOutcome::NotAccepted;
    frames[1].observation = observation(EngineStatus::Idle, 0, 0);
    frames.insert(
        2,
        frame(
            Some(input(InputId::Task)),
            observation(EngineStatus::Busy, 1, 0),
        ),
    );
    let mut engine = ScriptedEngine::new(frames);
    let mut host = Host::new();
    run(config, &mut engine, &mut host).await.unwrap();
    assert_eq!(
        engine
            .commands
            .iter()
            .filter(|c| matches!(
                c,
                EngineCommand::DeliverText {
                    id: InputId::Task,
                    ..
                }
            ))
            .count(),
        2
    );
}
#[tokio::test]
async fn waiting_suppresses_missing_output_nudges_and_background_settles() {
    let scratch = Scratch::new();
    let config = config(&scratch);
    let mut frames = happy();
    let mut waiting = observation(EngineStatus::Idle, 1, 1);
    waiting.waiting = Some("a background shell".into());
    frames[2] = frame(None, waiting.clone());
    frames.push(ScriptFrame {
        delay_ms: 40,
        ..frame(None, waiting)
    });
    frames.push(frame(None, observation(EngineStatus::Idle, 1, 1)));
    let mut engine = ScriptedEngine::new(frames);
    let mut host = Host::new();
    let started = std::time::Instant::now();
    run(config, &mut engine, &mut host).await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(40));
    assert_eq!(engine.commands.len(), 3);
}
#[tokio::test]
async fn submitted_outputs_with_background_work_still_obey_wall_cap() {
    let scratch = Scratch::new();
    let mut config = config(&scratch);
    config.limits.wall = Duration::from_millis(70);
    config.limits.work = Duration::from_secs(1);
    let mut frames = happy();
    frames[2].observation.background_work = vec!["shell".into()];
    let mut engine = ScriptedEngine::new(frames);
    let mut host = Host::new();
    assert_eq!(
        run(config, &mut engine, &mut host).await.unwrap_err().kind,
        FailureKind::WallCap
    );
}
#[tokio::test]
async fn completion_before_transient_wins_only_after_background_settles() {
    for work in [false, true] {
        let scratch = Scratch::new();
        let config = config(&scratch);
        let mut frames = happy();
        frames[2].observation.error = Some(EngineError {
            kind: EngineErrorKind::Transient,
            message: "capacity".into(),
        });
        if work {
            frames[2].observation.waiting = Some("shell".into());
        }
        let mut engine = ScriptedEngine::new(frames);
        let mut host = Host::new();
        let mut config = config;
        config.retry.additional_tries = 0;
        let result = run(config, &mut engine, &mut host).await;
        if work {
            assert_eq!(result.unwrap_err().kind, FailureKind::Transient);
        } else {
            result.unwrap();
        }
    }
}
#[tokio::test]
async fn assigned_messages_precede_live_feed_and_acknowledge_each_id() {
    let scratch = Scratch::new();
    let mut config = config(&scratch);
    config.assigned.through = MessageId(2);
    let message = |id| DeliveryMessage {
        id: MessageId(id),
        body: JsonValue::try_from(serde_json::json!({"body":format!("message {id}")})).unwrap(),
    };
    config.messages = vec![message(2), message(1)];
    let mut frames = happy()[..2].to_vec();
    for id in 1..=3 {
        frames.push(frame(
            Some(EngineCommand::Steer {
                id: InputId::Message { id: MessageId(id) },
                text: "*".into(),
            }),
            observation(EngineStatus::Busy, id as u64 + 1, 0),
        ));
    }
    frames.push(frame(None, observation(EngineStatus::Idle, 4, 1)));
    let mut engine = ScriptedEngine::new(frames);
    let mut host = Host::new();
    host.messages = vec![message(3)];
    run(config, &mut engine, &mut host).await.unwrap();
    let ids: Vec<_> = engine
        .commands
        .iter()
        .filter_map(|c| match c {
            EngineCommand::Steer {
                id: InputId::Message { id },
                ..
            } => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(ids, vec![MessageId(1), MessageId(2), MessageId(3)]);
    assert_eq!(host.acks, ids);
}
#[tokio::test]
async fn compaction_gets_current_me_snapshot_once() {
    let scratch = Scratch::new();
    let config = config(&scratch);
    let compact_path = config.run_dir.join("messages/compact-1.md");
    let mut frames = happy();
    frames[2].observation.compactions = 1;
    frames.push(frame(
        Some(input(InputId::Reprime { ordinal: 1 })),
        observation(EngineStatus::Busy, 2, 1),
    ));
    let mut final_o = observation(EngineStatus::Idle, 2, 2);
    final_o.compactions = 1;
    frames.push(frame(None, final_o));
    let mut engine = ScriptedEngine::new(frames);
    let mut host = Host::new();
    run(config, &mut engine, &mut host).await.unwrap();
    let text = engine
        .commands
        .iter()
        .find_map(|c| match c {
            EngineCommand::DeliverText {
                id: InputId::Reprime { .. },
                text,
            } => Some(text),
            _ => None,
        })
        .unwrap();
    assert!(text.contains(&compact_path.to_string_lossy().to_string()));
    assert!(
        fs::read_to_string(compact_path)
            .unwrap()
            .contains("step stands here")
    );
    assert_eq!(engine.commands.len(), 4);
}
#[tokio::test]
async fn inline_compaction_hook_does_not_deliver_a_second_reprime() {
    let scratch = Scratch::new();
    let config = config(&scratch);
    let mut frames = happy();
    frames[2].observation.compactions = 1;
    let mut engine = ScriptedEngine::new(frames);
    engine
        .profile
        .required_capabilities
        .push(INLINE_COMPACTION_CONTEXT.into());
    run(config, &mut engine, &mut Host::new()).await.unwrap();
    assert_eq!(engine.commands.len(), 3);
}
#[tokio::test]
async fn cleanup_failure_prevents_success() {
    let scratch = Scratch::new();
    let config = config(&scratch);
    let mut engine = ScriptedEngine::new(happy());
    let mut host = Host::new();
    host.fail_cleanup = true;
    assert_eq!(
        run(config, &mut engine, &mut host).await.unwrap_err().kind,
        FailureKind::Cleanup
    );
}
#[tokio::test]
async fn cancellation_during_backoff_is_immediate_and_never_resumes() {
    let scratch = Scratch::new();
    let mut config = config(&scratch);
    config.retry.backoff = Duration::from_secs(30);
    let directory = config.run_dir.clone();
    let mut frames = happy();
    frames[2].observation.error = Some(EngineError {
        kind: EngineErrorKind::Transient,
        message: "rate limit".into(),
    });
    let mut engine = ScriptedEngine::new(frames);
    let mut host = Host::new();
    host.outputs.clear();
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let killer = tokio::spawn(async move {
        loop {
            if Checkpoint::read(&directory)
                .ok()
                .flatten()
                .is_some_and(|cp| cp.state == State::Backoff)
            {
                trigger.cancel();
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    });
    let result = supervise(
        config,
        &mut engine,
        &mut host,
        &mut SessionGuard::default(),
        None,
        &cancel,
    )
    .await;
    killer.await.unwrap();
    assert_eq!(result.unwrap_err().kind, FailureKind::Cancelled);
    assert!(
        !engine
            .commands
            .iter()
            .any(|c| matches!(c, EngineCommand::Resume { .. }))
    );
}
#[tokio::test]
async fn script_fixture_executable_drives_real_supervision() {
    let scratch = Scratch::new();
    let config = config(&scratch);
    let script = scratch.0.join("events.json");
    fs::write(&script, serde_json::to_vec(&happy()).unwrap()).unwrap();
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("fixture");
    assert!(
        binary.exists(),
        "workspace gate must build the fixture binary"
    );
    let mut engine = FixtureEngine::new(binary, script, scratch.0.clone());
    let result = run(config, &mut engine, &mut Host::new()).await.unwrap();
    assert_eq!(result.final_text, "reply 1");
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

struct PaneEngine {
    scripted: ScriptedEngine,
    launch: Option<EngineLaunch>,
}
impl EngineAdapter for PaneEngine {
    fn profile(&self) -> EngineProfile {
        self.scripted.profile()
    }
    async fn session(&mut self, session: &str) -> Result<Option<SessionMetadata>, EngineError> {
        self.scripted.session(session).await
    }
    async fn prepare(
        &mut self,
        _: &EngineContext,
        _: Option<&str>,
    ) -> Result<Option<EngineLaunch>, EngineError> {
        Ok(self.launch.take())
    }
    async fn execute(
        &mut self,
        context: &EngineContext,
        command: EngineCommand,
    ) -> Result<DeliveryOutcome, EngineError> {
        self.scripted.execute(context, command).await
    }
    async fn observe(&mut self, context: &EngineContext) -> Result<EngineObservation, EngineError> {
        self.scripted.observe(context).await
    }
    async fn close(&mut self) -> io::Result<()> {
        self.scripted.close().await
    }
}
#[tokio::test]
#[ignore = "approved release-local private tmux required"]
async fn private_tmux_pane_receives_explicit_private_home_and_run_identity() {
    use sluice_process::tmux::ApprovedTmux;
    let scratch = Scratch::new();
    let mut config = config(&scratch);
    config.limits.settle = Duration::from_millis(200);
    let prefix = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/private-tmux");
    let artifact = ApprovedTmux::load(&prefix).await.unwrap();
    let private_home = scratch.0.join("engine-home");
    fs::create_dir(&private_home).unwrap();
    let script = scratch.0.join("env-check.sh");
    fs::write(&script, "printf '%s\\n' \"$SLUICE_RUN_ID\" \"$SLUICE_RUN_DIR\" \"$HOME\" \"$GIT_TERMINAL_PROMPT\" > \"$SLUICE_ENV_OUTPUT\"\nexec /usr/bin/sleep 10\n").unwrap();
    let output = scratch.0.join("pane-env");
    let mut engine = PaneEngine {
        scripted: ScriptedEngine::new(happy()),
        launch: Some(EngineLaunch {
            argv: vec!["/bin/sh".into(), script.to_string_lossy().into_owned()],
            env: BTreeMap::from([
                ("SLUICE_RUN_ID".into(), "stale-adapter-run".into()),
                ("SLUICE_RUN_DIR".into(), "/stale-adapter-dir".into()),
                ("HOME".into(), private_home.to_string_lossy().into_owned()),
                (
                    "SLUICE_ENV_OUTPUT".into(),
                    output.to_string_lossy().into_owned(),
                ),
            ]),
        }),
    };
    let result = supervise(
        config.clone(),
        &mut engine,
        &mut Host::new(),
        &mut SessionGuard::default(),
        Some(&artifact),
        &CancellationToken::new(),
    )
    .await;
    result.unwrap();
    assert_eq!(
        fs::read_to_string(&output).unwrap(),
        format!(
            "{}\n{}\n{}\n0\n",
            config.run,
            fs::canonicalize(&config.run_dir).unwrap().display(),
            private_home.display()
        )
    );
    assert!(std::os::unix::net::UnixStream::connect(config.run_dir.join("tmux.sock")).is_err());
}
#[tokio::test]
async fn quiet_note_leaves_busy_run_alive_until_wall_cap() {
    let scratch = Scratch::new();
    let mut config = config(&scratch);
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "Baseline",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(&config.cwd)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    config.limits.wall = Duration::from_secs(1);
    config.limits.stall = Duration::from_secs(2);
    config.limits.quiet = Duration::from_millis(40);
    let mut engine = ScriptedEngine::new(happy()[..2].to_vec());
    let mut host = Host::new();
    let error = run(config, &mut engine, &mut host).await.unwrap_err();
    assert_eq!(error.kind, FailureKind::WallCap);
    assert!(
        host.notes
            .iter()
            .any(|n| n.contains("no change to the worktree"))
    );
    assert!(!engine.commands.contains(&EngineCommand::RequestExit));
    assert_eq!(host.cleanup, 1);
}
#[tokio::test]
async fn readiness_stall_and_turn_start_have_distinct_caps() {
    for kind in [
        FailureKind::ReadyTimeout,
        FailureKind::StallCap,
        FailureKind::TurnStartTimeout,
    ] {
        let scratch = Scratch::new();
        let mut config = config(&scratch);
        config.limits.ready = Duration::from_millis(30);
        config.limits.stall = Duration::from_millis(40);
        config.limits.turn_start = Duration::from_millis(50);
        let frames = match kind {
            FailureKind::ReadyTimeout => vec![frame(
                Some(EngineCommand::StartFresh),
                observation(EngineStatus::Starting, 0, 0),
            )],
            FailureKind::StallCap => happy()[..2].to_vec(),
            _ => vec![
                frame(
                    Some(EngineCommand::StartFresh),
                    observation(EngineStatus::Idle, 0, 0),
                ),
                frame(
                    Some(input(InputId::Task)),
                    observation(EngineStatus::Idle, 0, 0),
                ),
            ],
        };
        let mut engine = ScriptedEngine::new(frames);
        assert_eq!(
            run(config, &mut engine, &mut Host::new())
                .await
                .unwrap_err()
                .kind,
            kind
        );
    }
}

fn policy(required: Vec<String>, reports_waiting: bool) -> Machine {
    let mut delivery = sluice_agents::delivery::DeliveryLedger::default();
    delivery.enqueue(InputId::Task, "task".into()).unwrap();
    delivery.offer(&InputId::Task).unwrap();
    delivery
        .outcome(&InputId::Task, DeliveryOutcome::Acknowledged)
        .unwrap();
    let mut limits = Limits::test_profile();
    if !reports_waiting {
        limits.grace = Duration::from_secs(1);
    }
    Machine::new(
        Checkpoint {
            version: 1,
            run: RunId::new(),
            attempt: AttemptId::new(),
            invocation: InvocationId::new(),
            engine: "fake".into(),
            cwd: "/scratch".into(),
            internal_attempt: 1,
            session: None,
            head_before: None,
            started_ms: 0,
            state: State::Busy,
            delivery,
            submissions: BTreeMap::new(),
            reminded: false,
            nudges: 0,
            compactions: 0,
            live_after: MessageId(0),
            final_text: String::new(),
            notes: vec![],
        },
        limits,
        required,
        reports_waiting,
        Duration::ZERO,
    )
}
#[test]
fn no_required_outputs_still_obey_background_bound_and_unsupported_waiting_grace() {
    let o = observation(EngineStatus::Idle, 1, 1);
    let mut machine = policy(vec![], true);
    assert_eq!(
        machine
            .update(Duration::ZERO, &o, &["detached shell".into()], "")
            .unwrap(),
        Action::None
    );
    assert_eq!(machine.checkpoint.state, State::Finishing);
    assert_eq!(
        machine
            .update(
                Duration::from_millis(100),
                &o,
                &["detached shell".into()],
                ""
            )
            .unwrap(),
        Action::None
    );
    assert_eq!(
        machine
            .update(
                Duration::from_millis(125),
                &o,
                &["detached shell".into()],
                ""
            )
            .unwrap(),
        Action::Finish
    );
    assert_eq!(machine.checkpoint.notes.len(), 1);
    let mut machine = policy(vec![], false);
    assert_eq!(
        machine.update(Duration::ZERO, &o, &[], "").unwrap(),
        Action::None
    );
    assert_eq!(
        machine
            .update(Duration::from_millis(999), &o, &[], "")
            .unwrap(),
        Action::None
    );
    assert_eq!(
        machine.update(Duration::from_secs(1), &o, &[], "").unwrap(),
        Action::Finish
    );
}
#[test]
fn waiting_cap_names_work_and_dialog_nudge_is_bounded() {
    let mut machine = policy(vec!["word".into()], true);
    let mut o = observation(EngineStatus::Idle, 1, 1);
    o.waiting = Some("formatter".into());
    assert_eq!(
        machine.update(Duration::ZERO, &o, &[], "").unwrap(),
        Action::None
    );
    assert!(
        matches!(machine.update(Duration::from_millis(100), &o, &[], "").unwrap(), Action::Send { text, .. } if text.contains("formatter") && text.contains("word"))
    );
    let mut machine = policy(vec!["word".into()], true);
    let o = observation(EngineStatus::Blocked, 1, 0);
    assert_eq!(
        machine.update(Duration::ZERO, &o, &[], "").unwrap(),
        Action::None
    );
    assert!(
        matches!(machine.update(Duration::from_millis(21), &o, &[], "").unwrap(), Action::Send { text, .. } if text.contains("Nobody can answer"))
    );
    assert_eq!(machine.checkpoint.nudges, 1);
}
#[test]
fn engine_error_precedes_queued_live_delivery_and_early_exit_never_finishes() {
    let mut machine = policy(vec![], true);
    machine
        .checkpoint
        .delivery
        .enqueue(InputId::Message { id: MessageId(1) }, "feedback".into())
        .unwrap();
    let mut o = observation(EngineStatus::Busy, 1, 0);
    o.error = Some(EngineError {
        kind: EngineErrorKind::Transient,
        message: "capacity".into(),
    });
    assert_eq!(
        machine.update(Duration::ZERO, &o, &[], "").unwrap(),
        Action::Retry
    );
    assert_eq!(machine.checkpoint.delivery.entries[1].tries, 0);
    let mut machine = policy(vec![], true);
    assert_eq!(
        machine
            .update(
                Duration::ZERO,
                &observation(EngineStatus::Exited, 1, 0),
                &[],
                ""
            )
            .unwrap_err()
            .kind,
        FailureKind::EngineExited
    );
}
