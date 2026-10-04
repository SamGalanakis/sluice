use sluice_agents::{ScriptedEngine, delivery::*, engines::*, git, quiet::*, supervisor::*};
use sluice_model::{ids::*, rpc::JsonValue};
use sluice_process::{identity::ProcessIdentity, proc::ProcessObservation, socket::AssignedRange};
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("sluice-test-session-{}", RunId::new()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn git_cmd(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(path)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
fn repo(path: &Path) {
    fs::create_dir(path).unwrap();
    git_cmd(path, &["init", "-q", "-b", "main"]);
    git_cmd(path, &["config", "user.name", "Test"]);
    git_cmd(path, &["config", "user.email", "test@example.com"]);
    fs::write(path.join("seed"), "seed").unwrap();
    git_cmd(path, &["add", "seed"]);
    git_cmd(path, &["commit", "-q", "-m", "Create seed"]);
}
fn config(scratch: &Scratch) -> SupervisorConfig {
    SupervisorConfig {
        run: RunId::new(),
        attempt: AttemptId::new(),
        invocation: InvocationId::new(),
        project: "p".into(),
        home: scratch.0.clone(),
        run_dir: scratch.0.join("run"),
        cwd: scratch.0.join("work"),
        engine: "fake".into(),
        task: "task".into(),
        required: vec!["word".into()],
        session: Some("".into()),
        previous: None,
        assigned: AssignedRange {
            after: MessageId(0),
            through: MessageId(0),
        },
        messages: vec![],
        model: None,
        limits: Limits::test_profile(),
        retry: RetryPolicy {
            backoff: Duration::from_millis(2),
            ..RetryPolicy::agent()
        },
        internal_attempt: 1,
    }
}
struct Host {
    submissions: Arc<Mutex<BTreeMap<String, serde_json::Value>>>,
    clean: u32,
}
impl SupervisorHost for Host {
    async fn snapshot(&mut self, _after: MessageId) -> io::Result<HostSnapshot> {
        Ok(HostSnapshot {
            submissions: self.submissions.lock().unwrap().clone(),
            ..HostSnapshot::default()
        })
    }
    async fn acknowledge(&mut self, _ids: &[MessageId]) -> io::Result<()> {
        Ok(())
    }
    async fn me(&mut self) -> io::Result<String> {
        Ok("current step".into())
    }
    async fn note(&mut self, _body: &str) -> io::Result<()> {
        Ok(())
    }
    async fn checkpoint(&mut self, _checkpoint: &Checkpoint) -> io::Result<()> {
        Ok(())
    }
    async fn cleanup(&mut self) -> io::Result<()> {
        self.clean += 1;
        Ok(())
    }
}
fn frame(
    command: Option<EngineCommand>,
    status: EngineStatus,
    starts: u64,
    completed: u64,
    error: Option<EngineError>,
) -> ScriptFrame {
    ScriptFrame {
        command,
        outcome: DeliveryOutcome::Acknowledged,
        observation: EngineObservation {
            status,
            turns_started: starts,
            turns_completed: completed,
            session_id: Some("own-session".into()),
            error,
            ..EngineObservation::default()
        },
        error: None,
        delay_ms: 0,
    }
}
fn deliver(id: InputId) -> EngineCommand {
    EngineCommand::DeliverText {
        id,
        text: "*".into(),
    }
}
struct CommitEngine {
    fake: ScriptedEngine,
    cwd: PathBuf,
    submissions: Arc<Mutex<BTreeMap<String, serde_json::Value>>>,
}
impl EngineAdapter for CommitEngine {
    fn profile(&self) -> EngineProfile {
        self.fake.profile()
    }
    async fn models(&mut self) -> Result<Vec<String>, EngineError> {
        self.fake.models().await
    }
    async fn session(&mut self, session: &str) -> Result<Option<SessionMetadata>, EngineError> {
        self.fake.session(session).await
    }
    async fn prepare(
        &mut self,
        context: &EngineContext,
        session: Option<&str>,
    ) -> Result<Option<EngineLaunch>, EngineError> {
        self.fake.prepare(context, session).await
    }
    async fn execute(
        &mut self,
        context: &EngineContext,
        command: EngineCommand,
    ) -> Result<DeliveryOutcome, EngineError> {
        let name = match &command {
            EngineCommand::DeliverText {
                id: InputId::Task, ..
            } => Some("one"),
            EngineCommand::DeliverText {
                id: InputId::Continue { .. },
                ..
            } => Some("two"),
            _ => None,
        };
        if let Some(name) = name {
            fs::write(self.cwd.join(name), name).unwrap();
            git_cmd(&self.cwd, &["add", name]);
            git_cmd(&self.cwd, &["commit", "-q", "-m", name]);
            if name == "two" {
                self.submissions
                    .lock()
                    .unwrap()
                    .insert("word".into(), serde_json::json!("built"));
            }
        }
        self.fake.execute(context, command).await
    }
    async fn observe(&mut self, context: &EngineContext) -> Result<EngineObservation, EngineError> {
        self.fake.observe(context).await
    }
    async fn close(&mut self) -> io::Result<()> {
        self.fake.close().await
    }
}
#[tokio::test]
async fn commit_transient_commit_retains_run_session_baseline_and_delivery_once_without_messages() {
    let scratch = Scratch::new();
    let config = config(&scratch);
    repo(&config.cwd);
    let before = git_cmd(&config.cwd, &["rev-parse", "HEAD"]);
    let run = config.run;
    let attempt = config.attempt;
    let directory = config.run_dir.clone();
    let frames = vec![
        frame(
            Some(EngineCommand::StartFresh),
            EngineStatus::Idle,
            0,
            0,
            None,
        ),
        frame(Some(deliver(InputId::Task)), EngineStatus::Busy, 1, 0, None),
        frame(
            None,
            EngineStatus::Idle,
            1,
            1,
            Some(EngineError {
                kind: EngineErrorKind::Transient,
                message: "capacity".into(),
            }),
        ),
        frame(
            Some(EngineCommand::Resume {
                session: "own-session".into(),
            }),
            EngineStatus::Idle,
            0,
            0,
            None,
        ),
        frame(
            Some(deliver(InputId::Continue { attempt: 2 })),
            EngineStatus::Busy,
            1,
            0,
            None,
        ),
        frame(None, EngineStatus::Idle, 1, 1, None),
    ];
    let submissions = Arc::new(Mutex::new(BTreeMap::new()));
    let mut engine = CommitEngine {
        fake: ScriptedEngine::new(frames),
        cwd: config.cwd.clone(),
        submissions: submissions.clone(),
    };
    let mut host = Host {
        submissions,
        clean: 0,
    };
    let result = supervise(
        config,
        &mut engine,
        &mut host,
        &mut SessionGuard::default(),
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let facts = result.git.unwrap();
    assert_eq!(facts.head_before, before);
    assert_eq!(facts.commits, 2);
    assert!(!facts.dirty);
    assert_eq!(result.session, "own-session");
    let checkpoint = Checkpoint::read(&directory).unwrap().unwrap();
    assert_eq!(checkpoint.run, run);
    assert_eq!(checkpoint.attempt, attempt);
    assert_eq!(checkpoint.internal_attempt, 2);
    assert_eq!(host.clean, 2);
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
    assert!(engine.fake.commands.contains(&EngineCommand::Resume {
        session: "own-session".into()
    }));
}
#[test]
fn explicit_empty_and_previous_resume_follow_assigned_message_policy() {
    let previous = PreviousSession {
        engine: "fake".into(),
        cwd: "/same".into(),
        session: Some("prev".into()),
    };
    let assigned = AssignedRange {
        after: MessageId(1),
        through: MessageId(2),
    };
    assert_eq!(
        select_session(
            Some("explicit"),
            Some(&previous),
            "fake",
            Path::new("/same"),
            assigned
        ),
        Some("explicit".into())
    );
    assert_eq!(
        select_session(
            Some(""),
            Some(&previous),
            "fake",
            Path::new("/same"),
            assigned
        ),
        None
    );
    assert_eq!(
        select_session(None, Some(&previous), "fake", Path::new("/same"), assigned),
        Some("prev".into())
    );
    assert!(select_session(None, Some(&previous), "other", Path::new("/same"), assigned).is_none());
    assert!(select_session(None, Some(&previous), "fake", Path::new("/other"), assigned).is_none());
    assert!(
        select_session(
            None,
            Some(&previous),
            "fake",
            Path::new("/same"),
            AssignedRange {
                after: MessageId(2),
                through: MessageId(2)
            }
        )
        .is_none()
    );
}
#[tokio::test]
async fn lock_conflict_fails_without_fresh_fallback() {
    let scratch = Scratch::new();
    let mut config = config(&scratch);
    fs::create_dir(&config.cwd).unwrap();
    config.session = Some("held".into());
    let mut owner = SessionGuard::default();
    owner
        .acquire(&config.home, "fake", "held", "holder", RunId::new())
        .unwrap();
    let mut engine = ScriptedEngine::new(vec![]);
    engine.sessions.insert(
        "held".into(),
        SessionMetadata {
            id: "held".into(),
            cwd: config.cwd.clone(),
        },
    );
    let mut host = Host {
        submissions: Arc::new(Mutex::new(BTreeMap::new())),
        clean: 0,
    };
    let error = supervise(
        config,
        &mut engine,
        &mut host,
        &mut SessionGuard::default(),
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, FailureKind::LockConflict);
    assert!(error.message.contains("holder"));
    assert!(engine.commands.is_empty());
}
#[tokio::test]
async fn missing_or_different_cwd_fails_before_launch() {
    for missing in [false, true] {
        let scratch = Scratch::new();
        let mut config = config(&scratch);
        if !missing {
            fs::create_dir(&config.cwd).unwrap();
        }
        config.session = Some("old".into());
        let mut engine = ScriptedEngine::new(vec![]);
        engine.sessions.insert(
            "old".into(),
            SessionMetadata {
                id: "old".into(),
                cwd: scratch.0.clone(),
            },
        );
        let mut host = Host {
            submissions: Arc::new(Mutex::new(BTreeMap::new())),
            clean: 0,
        };
        let error = supervise(
            config,
            &mut engine,
            &mut host,
            &mut SessionGuard::default(),
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, FailureKind::SessionCwd);
        assert!(engine.commands.is_empty());
    }
}
#[tokio::test]
async fn only_reported_missing_session_allows_fresh_fallback_and_records_note() {
    for kind in [EngineErrorKind::MissingSession, EngineErrorKind::Fatal] {
        let scratch = Scratch::new();
        let mut config = config(&scratch);
        fs::create_dir(&config.cwd).unwrap();
        config.session = Some("old".into());
        let mut initial = frame(
            Some(EngineCommand::Resume {
                session: "old".into(),
            }),
            EngineStatus::Starting,
            0,
            0,
            None,
        );
        initial.error = Some(EngineError {
            kind,
            message: "session not found".into(),
        });
        let frames = vec![
            initial,
            frame(
                Some(EngineCommand::StartFresh),
                EngineStatus::Idle,
                0,
                0,
                None,
            ),
            frame(Some(deliver(InputId::Task)), EngineStatus::Busy, 1, 0, None),
            frame(None, EngineStatus::Idle, 1, 1, None),
        ];
        let mut engine = ScriptedEngine::new(frames);
        let mut host = Host {
            submissions: Arc::new(Mutex::new(BTreeMap::from([(
                "word".into(),
                serde_json::json!("ok"),
            )]))),
            clean: 0,
        };
        let result = supervise(
            config,
            &mut engine,
            &mut host,
            &mut SessionGuard::default(),
            None,
            &CancellationToken::new(),
        )
        .await;
        if kind == EngineErrorKind::MissingSession {
            assert!(result.unwrap().notes[0].contains("starting fresh"));
        } else {
            assert_eq!(result.unwrap_err().kind, FailureKind::EngineExited);
            assert_eq!(engine.commands.len(), 1);
        }
    }
}
#[tokio::test]
async fn a_submission_ends_the_session_without_a_commit_reminder_and_reports_the_dirty_tree() {
    for tracked in [true, false] {
        let scratch = Scratch::new();
        let config = config(&scratch);
        repo(&config.cwd);
        fs::write(
            config.cwd.join(if tracked { "seed" } else { "untracked" }),
            "edited",
        )
        .unwrap();
        let frames = vec![
            frame(
                Some(EngineCommand::StartFresh),
                EngineStatus::Idle,
                0,
                0,
                None,
            ),
            frame(Some(deliver(InputId::Task)), EngineStatus::Busy, 1, 0, None),
            frame(None, EngineStatus::Idle, 1, 1, None),
        ];
        let submissions = Arc::new(Mutex::new(BTreeMap::new()));
        // The agent submits in its task turn and leaves its edit uncommitted.
        let mut engine = SubmitEngine {
            fake: ScriptedEngine::new(frames),
            submissions: submissions.clone(),
        };
        let mut host = Host {
            submissions,
            clean: 0,
        };
        let result = supervise(
            config,
            &mut engine,
            &mut host,
            &mut SessionGuard::default(),
            None,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(!engine.fake.commands.iter().any(|c| matches!(
            c,
            EngineCommand::DeliverText {
                id: InputId::Reminder,
                ..
            }
        )));
        assert_eq!(
            engine.fake.commands.last(),
            Some(&EngineCommand::RequestExit)
        );
        let facts = result.git.unwrap();
        assert_eq!(facts.commits, 0);
        assert_eq!(facts.dirty, tracked);
    }
}
/// Submits `word` when its task is delivered.
struct SubmitEngine {
    fake: ScriptedEngine,
    submissions: Arc<Mutex<BTreeMap<String, serde_json::Value>>>,
}
impl EngineAdapter for SubmitEngine {
    fn profile(&self) -> EngineProfile {
        self.fake.profile()
    }
    async fn models(&mut self) -> Result<Vec<String>, EngineError> {
        self.fake.models().await
    }
    async fn session(&mut self, session: &str) -> Result<Option<SessionMetadata>, EngineError> {
        self.fake.session(session).await
    }
    async fn prepare(
        &mut self,
        context: &EngineContext,
        session: Option<&str>,
    ) -> Result<Option<EngineLaunch>, EngineError> {
        self.fake.prepare(context, session).await
    }
    async fn execute(
        &mut self,
        context: &EngineContext,
        command: EngineCommand,
    ) -> Result<DeliveryOutcome, EngineError> {
        if matches!(
            command,
            EngineCommand::DeliverText {
                id: InputId::Task,
                ..
            }
        ) {
            self.submissions
                .lock()
                .unwrap()
                .insert("word".into(), serde_json::json!("ok"));
        }
        self.fake.execute(context, command).await
    }
    async fn observe(&mut self, context: &EngineContext) -> Result<EngineObservation, EngineError> {
        self.fake.observe(context).await
    }
    async fn close(&mut self) -> io::Result<()> {
        self.fake.close().await
    }
}
#[test]
fn delivery_checkpoint_marks_interrupted_offers_uncertain_and_preserves_acks() {
    let mut ledger = DeliveryLedger::default();
    ledger.enqueue(InputId::Task, "task".into()).unwrap();
    ledger
        .enqueue(InputId::Message { id: MessageId(1) }, "msg".into())
        .unwrap();
    ledger.offer(&InputId::Task).unwrap();
    ledger
        .outcome(&InputId::Task, DeliveryOutcome::Acknowledged)
        .unwrap();
    ledger
        .offer(&InputId::Message { id: MessageId(1) })
        .unwrap();
    let bytes = serde_json::to_vec(&ledger).unwrap();
    let mut resumed: DeliveryLedger = serde_json::from_slice(&bytes).unwrap();
    resumed.recover();
    assert!(resumed.uncertain());
    assert!(resumed.next().is_none());
    assert_eq!(resumed.entries[0].state, DeliveryState::Acknowledged);
}
#[tokio::test]
async fn untracked_fingerprint_and_head_reset_quiet_without_triggering_reminder() {
    let scratch = Scratch::new();
    repo(&scratch.0.join("work"));
    let work = scratch.0.join("work");
    fs::write(work.join("scratch"), "x").unwrap();
    let first = git::sample(&work).await.unwrap().unwrap();
    fs::write(work.join("scratch"), "xx").unwrap();
    let second = git::sample(&work).await.unwrap().unwrap();
    assert_ne!(first.fingerprint, second.fingerprint);
    assert!(first.tracked.is_empty());
    let mut quiet = QuietMonitor::default();
    let period = Duration::from_secs(10);
    assert!(
        quiet
            .observe(Duration::ZERO, period, Some(first), BTreeMap::new())
            .is_none()
    );
    assert!(
        quiet
            .observe(
                Duration::from_secs(9),
                period,
                Some(second.clone()),
                BTreeMap::new()
            )
            .is_none()
    );
    assert!(
        quiet
            .observe(
                Duration::from_secs(10),
                period,
                Some(second.clone()),
                BTreeMap::new()
            )
            .is_none()
    );
    assert!(
        quiet
            .observe(
                Duration::from_secs(19),
                period,
                Some(second),
                BTreeMap::new()
            )
            .is_some()
    );
}
#[test]
fn descendant_cpu_ignores_servers_but_counts_tools_and_process_generations() {
    let make = |pid, argv: Vec<String>| ProcessObservation {
        identity: ProcessIdentity {
            pid,
            start_time: 10,
            boot_id: "boot".into(),
            cgroup: "group".into(),
        },
        parent: 1,
        session: 1,
        state: 'S',
        command: "proc".into(),
        argv,
        user_ticks: 10,
        system_ticks: 2,
        waited_user_ticks: 30,
        waited_system_ticks: 0,
    };
    let servers = [
        vec!["codex".into(), "app-server".into()],
        vec!["tmux".into()],
        vec!["python".into(), "-m".into(), "mcp.server".into()],
        vec!["node".into(), "/tools/figments-mcp/src/index.js".into()],
    ];
    let mut processes: Vec<_> = servers
        .into_iter()
        .enumerate()
        .map(|(i, argv)| make(i as u32, argv))
        .collect();
    processes.push(make(50, vec!["cargo".into(), "test".into()]));
    processes.push(make(
        51,
        vec![
            "python".into(),
            "/work/mcp-refactor/tests/test_mcp.py".into(),
        ],
    ));
    let cpu = cpu_sample(&processes);
    assert_eq!(cpu.len(), 2);
    assert_eq!(cpu[&(50, 10, "boot".into())], 42);
    let sample = sluice_agents::git::GitSample {
        head: "abcdefg".into(),
        status: vec![],
        fingerprint: "same".into(),
        tracked: String::new(),
    };
    let mut monitor = QuietMonitor::default();
    let period = Duration::from_secs(10);
    monitor.observe(Duration::ZERO, period, Some(sample.clone()), cpu.clone());
    let mut more = cpu.clone();
    *more.get_mut(&(50, 10, "boot".into())).unwrap() += 1;
    assert!(
        monitor
            .observe(
                Duration::from_secs(10),
                period,
                Some(sample.clone()),
                more.clone()
            )
            .is_none()
    );
    assert!(
        monitor
            .observe(
                Duration::from_secs(20),
                period,
                Some(sample.clone()),
                more.clone()
            )
            .is_some()
    );
    assert!(
        monitor
            .observe(
                Duration::from_secs(21),
                period,
                Some(sample.clone()),
                more.clone()
            )
            .is_none()
    );
    assert!(
        monitor
            .observe(Duration::from_secs(30), period, Some(sample), more)
            .is_some()
    );
}
#[test]
fn session_lock_keys_are_hashes_not_lossy_path_sanitization() {
    let scratch = Scratch::new();
    let run = RunId::new();
    let mut guard = SessionGuard::default();
    guard.acquire(&scratch.0, "fake", "a/b", "p", run).unwrap();
    guard.acquire(&scratch.0, "fake", "a_b", "p", run).unwrap();
    assert_eq!(fs::read_dir(scratch.0.join("locks")).unwrap().count(), 4);
    let value = JsonValue::try_from(serde_json::json!({"ok":true})).unwrap();
    assert!(value.as_value()["ok"].as_bool().unwrap());
}
#[tokio::test]
async fn git_sampling_never_rewrites_the_index_under_an_agent() {
    let scratch = Scratch::new();
    let work = scratch.0.join("work");
    repo(&work);
    // Same content, newer stat: a locking `git status` refreshes and rewrites the index.
    let seed = fs::File::options()
        .write(true)
        .open(work.join("seed"))
        .unwrap();
    seed.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    let index = fs::read(work.join(".git/index")).unwrap();
    git::sample(&work).await.unwrap().unwrap();
    assert_eq!(fs::read(work.join(".git/index")).unwrap(), index);
}
