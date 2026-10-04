#![allow(dead_code)]
use sluice_agents::{ScriptedEngine, engines::*, supervisor::*};
use sluice_model::{ids::*, rpc::JsonValue};
use sluice_process::socket::{AssignedRange, DeliveryMessage};
use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub struct Scratch(pub PathBuf);
impl Scratch {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "sluice-test-acceptance-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[derive(Default)]
pub struct Host {
    pub submissions: BTreeMap<String, serde_json::Value>,
    /// What the agent submits in its turn: it lands once a checkpoint shows that state
    /// (`Idle`: its turn is over; `Busy`: mid-turn).
    pub submit_when: Option<(State, BTreeMap<String, serde_json::Value>)>,
    pub directory: Option<PathBuf>,
    pub acks: Vec<MessageId>,
    pub notes: Vec<String>,
    pub messages: Vec<DeliveryMessage>,
    pub cleanups: u32,
    pub snapshots: u32,
    pub submit_after_nudge: bool,
    pub cancel_on_backoff: Option<CancellationToken>,
    pub cancel_on_note: Option<CancellationToken>,
}
impl Host {
    /// An agent that submits `word` in its turn.
    pub fn submitted() -> Self {
        Self {
            submit_when: Some((
                State::Idle,
                BTreeMap::from([("word".into(), serde_json::json!("blue"))]),
            )),
            ..Self::default()
        }
    }
}
impl SupervisorHost for Host {
    async fn snapshot(&mut self, after: MessageId) -> io::Result<HostSnapshot> {
        self.snapshots += 1;
        if let Some(dir) = &self.directory {
            for file in [
                "fixture-submission.json",
                "submitted.json",
                "submission.json",
            ] {
                if let Ok(bytes) = fs::read(dir.join(file)) {
                    self.submissions = serde_json::from_slice(&bytes)?;
                }
            }
        }
        Ok(HostSnapshot {
            submissions: self.submissions.clone(),
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
        Ok("current acceptance run context".into())
    }
    async fn note(&mut self, text: &str) -> io::Result<()> {
        self.notes.push(text.into());
        if let Some(cancel) = &self.cancel_on_note {
            cancel.cancel();
        }
        Ok(())
    }
    async fn checkpoint(&mut self, cp: &Checkpoint) -> io::Result<()> {
        if self
            .submit_when
            .as_ref()
            .is_some_and(|(at, _)| *at == cp.state)
        {
            self.submissions = self.submit_when.take().unwrap().1;
        }
        if self.submit_after_nudge && cp.nudges > 0 {
            self.submissions
                .insert("word".into(), serde_json::json!("blue"));
        }
        if cp.state == State::Backoff
            && let Some(cancel) = &self.cancel_on_backoff
        {
            cancel.cancel();
        }
        Ok(())
    }
    async fn cleanup(&mut self) -> io::Result<()> {
        self.cleanups += 1;
        Ok(())
    }
}
pub fn config(root: &Path, engine: &str) -> SupervisorConfig {
    let cwd = root.join("work");
    fs::create_dir_all(&cwd).unwrap();
    SupervisorConfig {
        run: RunId::new(),
        attempt: AttemptId::new(),
        invocation: InvocationId::new(),
        project: "acceptance".into(),
        home: root.into(),
        run_dir: root.join("run"),
        cwd,
        engine: engine.into(),
        task: "Labelled scratch acceptance task".into(),
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
pub fn observation(status: EngineStatus, started: u64, completed: u64) -> EngineObservation {
    EngineObservation {
        status,
        turns_started: started,
        turns_completed: completed,
        session_id: Some("acceptance-session".into()),
        final_text: "blue".into(),
        progress: started + completed,
        ..EngineObservation::default()
    }
}
pub fn frame(command: Option<EngineCommand>, observation: EngineObservation) -> ScriptFrame {
    ScriptFrame {
        command,
        observation,
        outcome: DeliveryOutcome::Acknowledged,
        error: None,
        delay_ms: 0,
    }
}
pub fn input(id: InputId) -> EngineCommand {
    EngineCommand::DeliverText {
        id,
        text: "*".into(),
    }
}
pub fn happy() -> Vec<ScriptFrame> {
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
pub fn scripted(engine: &str, frames: Vec<ScriptFrame>) -> ScriptedEngine {
    let mut fake = ScriptedEngine::new(frames);
    fake.profile = match engine {
        "codex" => codex::profile::profile(),
        "claude" => claude::profile::profile(),
        "devin" => devin::profile::profile(),
        _ => fake.profile,
    };
    // The scripted fixture models explicit reprime delivery, independent of hook-inline support.
    fake.profile
        .required_capabilities
        .retain(|s| s != INLINE_COMPACTION_CONTEXT);
    fake
}
pub async fn run(
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
pub fn message(id: i64) -> DeliveryMessage {
    DeliveryMessage {
        id: MessageId(id),
        body: JsonValue::try_from(serde_json::json!({"body":"addressed acceptance feedback"}))
            .unwrap(),
    }
}

pub async fn scenario(name: &str, engine_name: &str) {
    let root = Scratch::new();
    let mut cfg = config(&root.0, engine_name);
    let mut host = Host::submitted();
    let mut frames = happy();
    match name {
        "busy_submitted" | "quiet" => {
            cfg.limits.wall = Duration::from_millis(300);
            cfg.limits.quiet = Duration::from_millis(20);
            frames.truncate(2);
            if name == "quiet" {
                host.submit_when = None;
                repo(&cfg.cwd);
            } else if let Some((at, _)) = &mut host.submit_when {
                // Submitted mid-turn: the session ends though the engine is still busy,
                // long before the wall cap.
                *at = State::Busy;
                cfg.limits.wall = Duration::from_secs(30);
            }
        }
        "background" => {
            frames[2].observation.background_work = vec!["owned shell".into()];
            frames.push(ScriptFrame {
                delay_ms: 40,
                ..frame(None, observation(EngineStatus::Idle, 1, 1))
            });
        }
        "compaction" => {
            frames[2].observation.compactions = 1;
            frames.push(frame(
                Some(input(InputId::Reprime { ordinal: 1 })),
                observation(EngineStatus::Busy, 2, 1),
            ));
            frames.push(frame(None, observation(EngineStatus::Idle, 2, 2)));
        }
        "addressed_live_message" => {
            host.messages = vec![message(1)];
            frames.insert(
                2,
                frame(
                    Some(EngineCommand::Steer {
                        id: InputId::Message { id: MessageId(1) },
                        text: "*".into(),
                    }),
                    observation(EngineStatus::Busy, 1, 0),
                ),
            );
        }
        "feedback_resume" => {
            cfg.previous = Some(PreviousSession {
                engine: engine_name.into(),
                cwd: cfg.cwd.clone(),
                session: Some("acceptance-session".into()),
            });
            cfg.assigned.through = MessageId(1);
            cfg.messages = vec![message(1)];
            frames[0].command = Some(EngineCommand::Resume {
                session: "acceptance-session".into(),
            });
            frames.insert(
                2,
                frame(
                    Some(EngineCommand::Steer {
                        id: InputId::Message { id: MessageId(1) },
                        text: "*".into(),
                    }),
                    observation(EngineStatus::Busy, 1, 0),
                ),
            );
        }
        "missing_outputs" | "nudge" => {
            host.submit_when = None;
            host.submit_after_nudge = name == "nudge";
            for ordinal in 1..=2 {
                frames.push(frame(
                    Some(input(InputId::Nudge { ordinal })),
                    observation(EngineStatus::Busy, ordinal as u64 + 1, ordinal as u64),
                ));
                frames.push(frame(
                    None,
                    observation(EngineStatus::Idle, ordinal as u64 + 1, ordinal as u64 + 1),
                ));
            }
        }
        "unknown_acceptance" => {
            frames[1].outcome = DeliveryOutcome::Uncertain;
            host.submit_when = None;
        }
        "cancel_backoff" | "retry_exhaustion" => {
            frames[2].observation.error = Some(EngineError {
                kind: EngineErrorKind::Transient,
                message: "fixture capacity".into(),
            });
            host.submit_when = None;
            if name == "retry_exhaustion" {
                cfg.retry.additional_tries = 0;
            }
        }
        "session_cwd_mismatch" => cfg.session = Some("acceptance-session".into()),
        "engine_mismatch" | "predecessor_cwd_mismatch" => {
            cfg.previous = Some(PreviousSession {
                engine: if name == "engine_mismatch" {
                    "other"
                } else {
                    engine_name
                }
                .into(),
                cwd: if name == "predecessor_cwd_mismatch" {
                    root.0.clone()
                } else {
                    cfg.cwd.clone()
                },
                session: Some("acceptance-session".into()),
            });
            cfg.assigned.through = MessageId(1);
            cfg.messages = vec![message(1)];
            frames.insert(
                2,
                frame(
                    Some(EngineCommand::Steer {
                        id: InputId::Message { id: MessageId(1) },
                        text: "*".into(),
                    }),
                    observation(EngineStatus::Busy, 1, 0),
                ),
            );
        }
        "fresh_required_submit" => (),
        _ => panic!("unknown scenario {name}"),
    }
    let mut engine = scripted(engine_name, frames);
    if name == "feedback_resume" {
        engine.sessions.insert(
            "acceptance-session".into(),
            SessionMetadata {
                id: "acceptance-session".into(),
                cwd: cfg.cwd.clone(),
            },
        );
    }
    if name == "session_cwd_mismatch" {
        engine.sessions.insert(
            "acceptance-session".into(),
            SessionMetadata {
                id: "acceptance-session".into(),
                cwd: root.0.clone(),
            },
        );
    }
    let cancel = CancellationToken::new();
    if name == "quiet" {
        cfg.limits.wall = Duration::from_secs(5);
        host.cancel_on_note = Some(cancel.clone());
    }
    if name == "cancel_backoff" {
        cfg.retry.backoff = Duration::from_secs(600);
        host.cancel_on_backoff = Some(cancel.clone());
    }
    let dir = cfg.run_dir.clone();
    let result = supervise(
        cfg,
        &mut engine,
        &mut host,
        &mut SessionGuard::default(),
        None,
        &cancel,
    )
    .await;
    match name {
        "busy_submitted" => {
            assert_eq!(result.unwrap().session, "acceptance-session");
            assert_eq!(engine.commands.last(), Some(&EngineCommand::RequestExit));
        }
        "quiet" => {
            assert_eq!(result.unwrap_err().kind, FailureKind::Cancelled);
            assert!(!host.notes.is_empty());
        }
        "missing_outputs" => assert_eq!(result.unwrap_err().kind, FailureKind::ExitedWithoutSubmit),
        "unknown_acceptance" => {
            assert_eq!(result.unwrap_err().kind, FailureKind::UnknownAcceptance)
        }
        "cancel_backoff" => assert_eq!(result.unwrap_err().kind, FailureKind::Cancelled),
        "retry_exhaustion" => assert_eq!(result.unwrap_err().kind, FailureKind::Transient),
        "session_cwd_mismatch" => {
            assert_eq!(result.unwrap_err().kind, FailureKind::SessionCwd);
            assert!(engine.commands.is_empty());
            return;
        }
        _ => {
            assert_eq!(result.unwrap().session, "acceptance-session");
            assert_eq!(Checkpoint::read(&dir).unwrap().unwrap().state, State::Done);
        }
    }
    assert!(host.cleanups > 0);
    if name == "feedback_resume" {
        assert!(matches!(engine.commands[0], EngineCommand::Resume { .. }));
    }
    if name == "engine_mismatch" || name == "predecessor_cwd_mismatch" {
        assert_eq!(engine.commands[0], EngineCommand::StartFresh);
    }
    if name == "addressed_live_message" || name == "feedback_resume" {
        assert_eq!(host.acks, vec![MessageId(1)]);
    }
    if name == "compaction" {
        assert!(engine.commands.iter().any(|c| matches!(
            c,
            EngineCommand::DeliverText {
                id: InputId::Reprime { .. },
                ..
            }
        )));
    }
    if name == "unknown_acceptance" {
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

pub fn git(cwd: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
pub fn repo(cwd: &Path) {
    git(cwd, &["init", "-q"]);
    git(cwd, &["config", "user.name", "Acceptance fixture"]);
    git(cwd, &["config", "user.email", "acceptance@example.invalid"]);
    git(
        cwd,
        &[
            "commit",
            "--allow-empty",
            "-qm",
            "Create the fixture baseline.",
        ],
    );
}

pub async fn session_policy(engine_name: &str) {
    for case in ["gone", "fatal", "lock", "cwd"] {
        let root = Scratch::new();
        let mut cfg = config(&root.0, engine_name);
        cfg.session = Some("old-session".into());
        let mut frames = happy();
        let mut initial = frame(
            Some(EngineCommand::Resume {
                session: "old-session".into(),
            }),
            observation(EngineStatus::Starting, 0, 0),
        );
        initial.error = Some(EngineError {
            kind: if case == "gone" {
                EngineErrorKind::MissingSession
            } else {
                EngineErrorKind::Fatal
            },
            message: "recorded session gone".into(),
        });
        if case == "gone" || case == "fatal" {
            frames.insert(0, initial);
        }
        let mut engine = scripted(engine_name, frames);
        let mut host = Host::submitted();
        let mut holder = SessionGuard::default();
        if case == "lock" {
            holder
                .acquire(&root.0, engine_name, "old-session", "other", RunId::new())
                .unwrap();
        }
        if case == "cwd" {
            fs::remove_dir(&cfg.cwd).unwrap();
        }
        let result = run(cfg, &mut engine, &mut host).await;
        match case {
            "gone" => {
                assert!(
                    result
                        .unwrap()
                        .notes
                        .iter()
                        .any(|n| n.contains("starting fresh"))
                );
                assert!(engine.commands.contains(&EngineCommand::StartFresh));
            }
            "fatal" => {
                assert_eq!(result.unwrap_err().kind, FailureKind::EngineExited);
                assert!(!engine.commands.contains(&EngineCommand::StartFresh));
            }
            "lock" => {
                assert_eq!(result.unwrap_err().kind, FailureKind::LockConflict);
                assert!(engine.commands.is_empty());
            }
            _ => {
                assert_eq!(result.unwrap_err().kind, FailureKind::SessionCwd);
                assert!(engine.commands.is_empty());
            }
        }
    }
}
struct CommitEngine {
    fake: ScriptedEngine,
}
impl EngineAdapter for CommitEngine {
    fn profile(&self) -> EngineProfile {
        self.fake.profile()
    }
    async fn models(&mut self) -> Result<Vec<String>, EngineError> {
        self.fake.models().await
    }
    async fn session(&mut self, s: &str) -> Result<Option<SessionMetadata>, EngineError> {
        self.fake.session(s).await
    }
    async fn prepare(
        &mut self,
        c: &EngineContext,
        s: Option<&str>,
    ) -> Result<Option<EngineLaunch>, EngineError> {
        self.fake.prepare(c, s).await
    }
    async fn execute(
        &mut self,
        c: &EngineContext,
        command: EngineCommand,
    ) -> Result<DeliveryOutcome, EngineError> {
        let name = match &command {
            EngineCommand::DeliverText {
                id: InputId::Task, ..
            } => Some("first"),
            EngineCommand::DeliverText {
                id: InputId::Continue { .. },
                ..
            } => Some("second"),
            _ => None,
        };
        if let Some(name) = name {
            fs::write(c.cwd.join(name), name).unwrap();
            git(&c.cwd, &["add", name]);
            git(&c.cwd, &["commit", "-qm", name]);
            if name == "second" {
                fs::write(c.run_dir.join("submitted.json"), br#"{"word":"blue"}"#).unwrap();
            }
        }
        self.fake.execute(c, command).await
    }
    async fn observe(&mut self, c: &EngineContext) -> Result<EngineObservation, EngineError> {
        self.fake.observe(c).await
    }
    async fn close(&mut self) -> io::Result<()> {
        self.fake.close().await
    }
}
pub async fn transient_commits(engine_name: &str) {
    let root = Scratch::new();
    let cfg = config(&root.0, engine_name);
    repo(&cfg.cwd);
    let baseline = git(&cfg.cwd, &["rev-parse", "HEAD"]);
    let run_id = cfg.run;
    let attempt = cfg.attempt;
    let dir = cfg.run_dir.clone();
    let mut frames = happy();
    frames[2].observation.error = Some(EngineError {
        kind: EngineErrorKind::Transient,
        message: "capacity after commit".into(),
    });
    frames.push(frame(
        Some(EngineCommand::Resume {
            session: "acceptance-session".into(),
        }),
        observation(EngineStatus::Idle, 0, 0),
    ));
    frames.push(frame(
        Some(input(InputId::Continue { attempt: 2 })),
        observation(EngineStatus::Busy, 1, 0),
    ));
    frames.push(frame(None, observation(EngineStatus::Idle, 1, 1)));
    let mut engine = CommitEngine {
        fake: scripted(engine_name, frames),
    };
    let mut host = Host {
        directory: Some(dir.clone()),
        ..Default::default()
    };
    let result = run(cfg, &mut engine, &mut host).await.unwrap();
    let facts = result.git.unwrap();
    assert_eq!(facts.head_before, baseline);
    assert_eq!(facts.commits, 2);
    assert!(!facts.dirty);
    let cp = Checkpoint::read(&dir).unwrap().unwrap();
    assert_eq!(cp.run, run_id);
    assert_eq!(cp.attempt, attempt);
    assert_eq!(cp.internal_attempt, 2);
    assert_eq!(cp.session.as_deref(), Some("acceptance-session"));
    assert_eq!(
        engine
            .fake
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
    assert_eq!(engine.fake.closed, 2);
    assert_eq!(host.cleanups, 2);
}
