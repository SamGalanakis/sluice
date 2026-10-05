//! Shared state machine and admitted-invocation driver.
use crate::{
    delivery::DeliveryLedger,
    engines::*,
    git::{self, GitFacts},
    prompt,
    quiet::QuietMonitor,
    reprime,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sluice_model::{
    hash::ExecutionProvenance,
    ids::{AttemptId, InvocationId, MessageId, RunId},
    rpc::decode_json,
};
use sluice_process::{
    hook_journal,
    identity::ProcessIdentity,
    locks::{FileLock, LockAttempt},
    socket::{AssignedRange, DeliveryMessage},
    tmux::ApprovedTmux,
};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    future::Future,
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct Limits {
    pub nudges: u32,
    pub wall: Duration,
    pub stall: Duration,
    /// Idle time before a session with nothing to submit ends, or an unfinished one is nudged.
    pub settle: Duration,
    /// The longer idle time before the first nudge of an engine that does not report
    /// background work, which may still be running toward the submission.
    pub grace: Duration,
    pub poll: Duration,
    pub ready: Duration,
    pub turn_start: Duration,
    pub wait: Duration,
    pub dialog: Duration,
    pub quiet: Duration,
    pub work: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            nudges: 3,
            wall: Duration::from_secs(600 * 60),
            stall: Duration::from_secs(30 * 60),
            settle: Duration::from_secs(10),
            grace: Duration::from_secs(10 * 60),
            poll: Duration::from_millis(500),
            ready: Duration::from_secs(180),
            turn_start: Duration::from_secs(60),
            wait: Duration::from_secs(90 * 60),
            dialog: Duration::from_secs(60),
            quiet: Duration::from_secs(45 * 60),
            work: Duration::from_secs(10 * 60),
        }
    }
}
impl Limits {
    pub fn from_env() -> io::Result<Self> {
        Self::parse(|name| {
            std::env::var(name).map(Some).or_else(|e| match e {
                std::env::VarError::NotPresent => Ok(None),
                _ => Err(io::Error::other(format!("{name} is not UTF-8"))),
            })
        })
    }
    /// Inject settings directly in tests, without changing the process environment.
    pub fn parse(mut get: impl FnMut(&str) -> io::Result<Option<String>>) -> io::Result<Self> {
        let mut limits = Self::default();
        for (name, duration, scale) in [
            ("SLUICE_AGENT_MAX_MIN", &mut limits.wall, 60.0),
            ("SLUICE_AGENT_STALL_MIN", &mut limits.stall, 60.0),
            ("SLUICE_AGENT_SETTLE_S", &mut limits.settle, 1.0),
            ("SLUICE_AGENT_GRACE_MIN", &mut limits.grace, 60.0),
            ("SLUICE_AGENT_POLL_S", &mut limits.poll, 1.0),
            ("SLUICE_AGENT_READY_S", &mut limits.ready, 1.0),
            ("SLUICE_AGENT_TURN_START_S", &mut limits.turn_start, 1.0),
            ("SLUICE_AGENT_WAIT_MIN", &mut limits.wait, 60.0),
            ("SLUICE_AGENT_DIALOG_S", &mut limits.dialog, 1.0),
            ("SLUICE_AGENT_QUIET_MIN", &mut limits.quiet, 60.0),
            ("SLUICE_AGENT_WORK_MIN", &mut limits.work, 60.0),
        ] {
            if let Some(raw) = get(name)? {
                let number: f64 = raw.parse().map_err(|_| {
                    io::Error::other(format!("{name}: expected finite positive duration"))
                })?;
                if !number.is_finite() || number <= 0.0 {
                    return Err(io::Error::other(format!(
                        "{name}: expected finite positive duration"
                    )));
                }
                *duration = Duration::try_from_secs_f64(number * scale)
                    .map_err(|_| io::Error::other(format!("{name}: duration out of range")))?;
                if duration.is_zero() {
                    return Err(io::Error::other(format!(
                        "{name}: duration below timer resolution"
                    )));
                }
            }
        }
        if let Some(raw) = get("SLUICE_AGENT_NUDGES")? {
            limits.nudges = raw
                .parse()
                .map_err(|_| io::Error::other("SLUICE_AGENT_NUDGES: expected positive integer"))?;
            if limits.nudges == 0 {
                return Err(io::Error::other(
                    "SLUICE_AGENT_NUDGES: expected positive integer",
                ));
            }
        }
        Ok(limits)
    }
    pub fn test_profile() -> Self {
        let short = Duration::from_millis(20);
        Self {
            nudges: 2,
            wall: Duration::from_secs(3),
            stall: Duration::from_secs(1),
            settle: short,
            grace: short,
            poll: Duration::from_millis(2),
            ready: Duration::from_secs(1),
            turn_start: Duration::from_millis(100),
            wait: Duration::from_millis(100),
            dialog: short,
            quiet: Duration::from_millis(100),
            work: Duration::from_millis(100),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Boot,
    Delivering,
    Busy,
    Waiting,
    Idle,
    Finishing,
    Backoff,
    Exiting,
    Done,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureKind {
    Transient,
    MissingSession,
    /// The agent exited, or stopped after its nudges, without a valid step_submit.
    ExitedWithoutSubmit,
    WallCap,
    StallCap,
    ReadyTimeout,
    TurnStartTimeout,
    LockConflict,
    SessionCwd,
    CapabilityMismatch,
    UnknownAcceptance,
    EngineExited,
    Cancelled,
    Cleanup,
    Invalid,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentFailure {
    pub kind: FailureKind,
    pub message: String,
    pub session: Option<String>,
}
impl std::fmt::Display for AgentFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)?;
        if let Some(session) = &self.session {
            write!(
                f,
                "\nsession: {session}. Bind the step's session input to it, then step_retry."
            )?;
        }
        Ok(())
    }
}
impl std::error::Error for AgentFailure {}
fn failure(kind: FailureKind, message: impl Into<String>) -> AgentFailure {
    AgentFailure {
        kind,
        message: message.into(),
        session: None,
    }
}
fn invalid(error: impl std::fmt::Display) -> AgentFailure {
    failure(FailureKind::Invalid, error.to_string())
}
impl From<EngineError> for AgentFailure {
    fn from(e: EngineError) -> Self {
        failure(
            match e.kind {
                EngineErrorKind::Transient => FailureKind::Transient,
                EngineErrorKind::MissingSession => FailureKind::MissingSession,
                EngineErrorKind::CapabilityMismatch => FailureKind::CapabilityMismatch,
                EngineErrorKind::UnknownAcceptance => FailureKind::UnknownAcceptance,
                _ => FailureKind::EngineExited,
            },
            e.message,
        )
    }
}
#[derive(Debug, Clone, Copy)]
pub enum RetryOwner {
    Standalone,
    OuterHelper,
}
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub additional_tries: u32,
    pub backoff: Duration,
    pub owner: RetryOwner,
}
impl RetryPolicy {
    pub fn agent() -> Self {
        Self {
            additional_tries: 3,
            backoff: Duration::from_secs(600),
            owner: RetryOwner::Standalone,
        }
    }
    pub fn decision() -> Self {
        Self {
            additional_tries: 2,
            backoff: Duration::from_secs(30),
            owner: RetryOwner::Standalone,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviousSession {
    pub engine: String,
    pub cwd: PathBuf,
    pub session: Option<String>,
}
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub run: RunId,
    pub attempt: AttemptId,
    pub invocation: InvocationId,
    pub project: String,
    pub home: PathBuf,
    pub run_dir: PathBuf,
    pub cwd: PathBuf,
    pub engine: String,
    pub task: String,
    pub required: Vec<String>,
    pub session: Option<String>,
    pub previous: Option<PreviousSession>,
    pub assigned: AssignedRange,
    pub messages: Vec<DeliveryMessage>,
    /// The `model` input; `None` runs the engine default.
    pub model: Option<crate::model::ModelChoice>,
    pub limits: Limits,
    pub retry: RetryPolicy,
    pub internal_attempt: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub version: u16,
    pub run: RunId,
    pub attempt: AttemptId,
    pub invocation: InvocationId,
    pub engine: String,
    pub cwd: PathBuf,
    pub internal_attempt: u32,
    pub session: Option<String>,
    pub head_before: Option<String>,
    pub started_ms: u64,
    pub state: State,
    pub delivery: DeliveryLedger,
    pub submissions: BTreeMap<String, Value>,
    /// Retired (a reminder to commit before finishing); checkpoints of earlier releases
    /// still carry it.
    #[serde(default)]
    pub reminded: bool,
    pub nudges: u32,
    pub compactions: u64,
    pub live_after: MessageId,
    pub final_text: String,
    pub notes: Vec<String>,
}
impl Checkpoint {
    pub fn save(&self, directory: &Path) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(directory.join("native.json.tmp"))?;
        file.write_all(&serde_json::to_vec(self).map_err(io::Error::other)?)?;
        file.sync_all()?;
        fs::rename(
            directory.join("native.json.tmp"),
            directory.join("native.json"),
        )?;
        fs::File::open(directory)?.sync_all()
    }
    pub fn read(directory: &Path) -> io::Result<Option<Self>> {
        match fs::read(directory.join("native.json")) {
            Ok(bytes) => decode_json(&bytes).map(Some).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentResult {
    #[serde(rename = "final")]
    pub final_text: String,
    pub report: Option<String>,
    pub session: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitFacts>,
    pub notes: Vec<String>,
    /// The model id the run resolved and launched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}
#[derive(Debug, Clone, Default)]
pub struct HostSnapshot {
    pub submissions: BTreeMap<String, Value>,
    pub messages: Vec<DeliveryMessage>,
    pub background_work: Vec<String>,
    pub roots: Vec<ProcessIdentity>,
}
/// The guardian supplies durable observations and holds invocation containment authority.
/// cleanup must prove all engine-owned descendants gone, including detached work. It must
/// retain the supervisor/control process and remain callable after an interrupted adapter.
pub trait SupervisorHost: Send {
    fn snapshot(
        &mut self,
        after: MessageId,
    ) -> impl Future<Output = io::Result<HostSnapshot>> + Send;
    fn acknowledge(&mut self, ids: &[MessageId]) -> impl Future<Output = io::Result<()>> + Send;
    fn me(&mut self) -> impl Future<Output = io::Result<String>> + Send;
    fn note(&mut self, body: &str) -> impl Future<Output = io::Result<()>> + Send;
    fn checkpoint(
        &mut self,
        checkpoint: &Checkpoint,
    ) -> impl Future<Output = io::Result<()>> + Send;
    fn cleanup(&mut self) -> impl Future<Output = io::Result<()>> + Send;
}
/// Held by the guardian across outer-helper retries. Never unlink flock inodes.
#[derive(Default)]
pub struct SessionGuard {
    locks: BTreeMap<String, FileLock>,
}
impl SessionGuard {
    pub fn acquire(
        &mut self,
        home: &Path,
        engine: &str,
        session: &str,
        project: &str,
        run: RunId,
    ) -> Result<(), AgentFailure> {
        let key = ExecutionProvenance::fingerprint(
            &serde_json::to_vec(&(engine, session)).map_err(invalid)?,
        )
        .to_string();
        if let Some(lock) = self.locks.get(&key) {
            if lock.holder().run != Some(run) {
                return Err(failure(
                    FailureKind::LockConflict,
                    "session guard belongs to another run",
                ));
            }
            return Ok(());
        }
        fs::create_dir_all(home.join("locks")).map_err(invalid)?;
        match FileLock::try_acquire(
            &home.join("locks").join(format!("session-{key}.lock")),
            Some(project.into()),
            Some(run),
        )
        .map_err(invalid)?
        {
            LockAttempt::Acquired(lock) => {
                self.locks.insert(key, lock);
                Ok(())
            }
            LockAttempt::Conflict(holder) => Err(failure(
                FailureKind::LockConflict,
                format!("{engine} session {session} is held by {holder:?}"),
            )),
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageContinuation {
    pub run: RunId,
    pub delivery: DeliveryLedger,
    pub live_after: MessageId,
}
pub fn select_session(
    explicit: Option<&str>,
    previous: Option<&PreviousSession>,
    engine: &str,
    cwd: &Path,
    assigned: AssignedRange,
) -> Option<String> {
    match explicit {
        Some("") => None,
        Some(s) => Some(s.into()),
        None => previous
            .filter(|p| p.engine == engine && p.cwd == cwd && assigned.through > assigned.after)
            .and_then(|p| p.session.clone())
            .filter(|s| !s.is_empty()),
    }
}

/// Pure policy driven by elapsed run time. Actions are persisted before execution.
pub struct Machine {
    pub checkpoint: Checkpoint,
    limits: Limits,
    required: Vec<String>,
    reports_waiting: bool,
    base_turns: u64,
    base_starts: u64,
    awaiting: Option<Duration>,
    /// Input steered into a busy turn rides that turn, so it starts no turn-start wait.
    steered: bool,
    idle_since: Option<Duration>,
    wait_since: Option<Duration>,
    work_since: Option<Duration>,
    dialog_since: Option<Duration>,
    moved: Duration,
    progress: Option<u64>,
    boot_at: Duration,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Send {
        id: InputId,
        text: String,
        steer: bool,
    },
    Reprime {
        ordinal: u64,
    },
    Finish,
    Retry,
    None,
}
impl Machine {
    pub fn new(
        checkpoint: Checkpoint,
        limits: Limits,
        required: Vec<String>,
        reports_waiting: bool,
        now: Duration,
    ) -> Self {
        Self {
            checkpoint,
            limits,
            required,
            reports_waiting,
            base_turns: 0,
            base_starts: 0,
            awaiting: None,
            steered: false,
            idle_since: None,
            wait_since: None,
            work_since: None,
            dialog_since: None,
            moved: now,
            progress: None,
            boot_at: now,
        }
    }
    pub fn pending(&self) -> bool {
        !self.checkpoint.delivery.all_acknowledged()
    }
    pub fn offered(
        &mut self,
        id: &InputId,
        observation: &EngineObservation,
        now: Duration,
    ) -> Result<(), AgentFailure> {
        self.checkpoint.delivery.offer(id).map_err(invalid)?;
        self.base_turns = observation.turns_completed;
        self.base_starts = observation.turns_started;
        self.steered = observation.status == EngineStatus::Busy;
        self.awaiting = (!self.steered).then_some(now);
        self.idle_since = None;
        self.work_since = None;
        self.checkpoint.state = State::Delivering;
        Ok(())
    }
    /// A valid submission ends the session at once; the run (and the fn composing the agent,
    /// if any) then completes the step with the result.
    pub fn update(
        &mut self,
        now: Duration,
        o: &EngineObservation,
        background: &[String],
    ) -> Result<Action, AgentFailure> {
        if now >= self.limits.wall {
            return Err(failure(FailureKind::WallCap, "wall-clock cap exceeded"));
        }
        for id in &o.acknowledged {
            self.checkpoint
                .delivery
                .outcome(id, DeliveryOutcome::Acknowledged)
                .map_err(invalid)?;
        }
        for id in &o.not_accepted {
            self.checkpoint
                .delivery
                .outcome(id, DeliveryOutcome::NotAccepted)
                .map_err(invalid)?;
        }
        self.checkpoint.final_text.clone_from(&o.final_text);
        if o.session_id.as_ref().is_some_and(|s| !s.is_empty()) {
            self.checkpoint.session.clone_from(&o.session_id);
        }
        let missing: Vec<_> = self
            .required
            .iter()
            .filter(|n| !self.checkpoint.submissions.contains_key(*n))
            .cloned()
            .collect();
        // The store accepts only a valid submission, so any is the agent's last word.
        if !self.checkpoint.submissions.is_empty() && missing.is_empty() {
            self.checkpoint.state = State::Exiting;
            return Ok(Action::Finish);
        }
        if self.checkpoint.delivery.uncertain() {
            return Err(failure(
                FailureKind::UnknownAcceptance,
                "uncertain input delivery requires reconciliation; it will not be replayed",
            ));
        }
        if self.progress != Some(o.progress) {
            self.progress = Some(o.progress);
            self.moved = now;
        }
        if o.turns_started > self.base_starts || o.turns_completed > self.base_turns {
            self.awaiting = None;
            self.steered = false;
        } else if self.steered && o.status == EngineStatus::Idle {
            // The busy turn ended without completing, so the steered input needs a turn of its own.
            self.steered = false;
            self.awaiting = Some(now);
        } else if self.awaiting.is_some() && o.status == EngineStatus::Busy {
            // The engine went back to work before taking the input, which now waits behind that
            // work as steered input does.
            self.awaiting = None;
            self.steered = true;
        }
        let new_turn = o.turns_completed > self.base_turns;
        let complete = new_turn
            && missing.is_empty()
            && !self.pending()
            && self.awaiting.is_none()
            && matches!(o.status, EngineStatus::Idle | EngineStatus::Exited);
        let has_work = o.waiting.as_ref().is_some_and(|w| !w.is_empty())
            || !o.background_work.is_empty()
            || !background.is_empty();
        if let Some(error) = &o.error
            && !(complete && !has_work)
        {
            return if error.kind == EngineErrorKind::Transient {
                Ok(Action::Retry)
            } else {
                Err(error.clone().into())
            };
        }
        if o.status == EngineStatus::Exited && !(complete && !has_work) {
            return Err(if missing.is_empty() {
                failure(FailureKind::EngineExited, "engine exited before completion")
            } else {
                failure(
                    FailureKind::ExitedWithoutSubmit,
                    format!(
                        "the agent exited without submitting {}; last message: {}",
                        missing.join(", "),
                        self.checkpoint.final_text
                    ),
                )
            });
        }
        if o.status == EngineStatus::Starting {
            self.checkpoint.state = State::Boot;
            if now.saturating_sub(self.boot_at) >= self.limits.ready {
                return Err(failure(
                    FailureKind::ReadyTimeout,
                    "engine did not become ready",
                ));
            }
            return Ok(Action::None);
        }
        if let Some(sent) = self.awaiting
            && now.saturating_sub(sent) >= self.limits.turn_start
        {
            // Only an adapter's positive nonacceptance report permits a second offer.
            if self.checkpoint.delivery.next().is_none() {
                return Err(failure(
                    FailureKind::TurnStartTimeout,
                    "engine did not start a turn after acknowledged or pending delivery; refusing blind replay",
                ));
            }
        }
        if let Some(entry) = self.checkpoint.delivery.next() {
            if entry.tries >= 2 {
                return Err(failure(
                    FailureKind::TurnStartTimeout,
                    "engine did not accept input after one verified redelivery",
                ));
            }
            return Ok(Action::Send {
                id: entry.id.clone(),
                text: entry.text.clone(),
                steer: o.status == EngineStatus::Busy,
            });
        }
        if o.compactions > self.checkpoint.compactions && o.status != EngineStatus::Exited {
            return Ok(Action::Reprime {
                ordinal: o.compactions,
            });
        }
        if o.status != EngineStatus::Idle && now.saturating_sub(self.moved) >= self.limits.stall {
            return Err(failure(
                FailureKind::StallCap,
                "no transcript progress while non-idle",
            ));
        }
        if o.status == EngineStatus::Blocked {
            self.checkpoint.state = State::Waiting;
            let since = *self.dialog_since.get_or_insert(now);
            if now.saturating_sub(since) >= self.limits.dialog {
                self.dialog_since = None;
                return self.nudge("Nobody can answer here. Decide, or ask the orchestrator with `sluice tool ask` (to: \"orchestrator\") and continue the task.".into());
            }
            return Ok(Action::None);
        }
        self.dialog_since = None;
        if o.status == EngineStatus::Busy || !new_turn {
            self.checkpoint.state = if o.status == EngineStatus::Starting {
                State::Boot
            } else {
                State::Busy
            };
            self.idle_since = None;
            self.work_since = None;
            return Ok(Action::None);
        }
        if self.pending() {
            self.checkpoint.state = State::Delivering;
            return Ok(Action::None);
        }
        if has_work {
            self.checkpoint.state = if complete {
                State::Finishing
            } else {
                State::Waiting
            };
            if complete {
                let since = *self.work_since.get_or_insert(now);
                if now.saturating_sub(since) < self.limits.work {
                    return Ok(Action::None);
                }
                let note = "background work exceeded completion settle bound; cleanup will stop it";
                if !self.checkpoint.notes.iter().any(|n| n == note) {
                    self.checkpoint.notes.push(note.into());
                }
            } else {
                let since = *self.wait_since.get_or_insert(now);
                if now.saturating_sub(since) < self.limits.wait {
                    return Ok(Action::None);
                }
                self.wait_since = None;
                return self.nudge(format!("Your step is still waiting on {}. Stop or finish that background work, then submit: {}.", o.waiting.as_deref().unwrap_or("background work"), missing.join(", ")));
            }
        } else {
            self.wait_since = None;
        }
        self.checkpoint.state = State::Idle;
        let idle = *self.idle_since.get_or_insert(now);
        // Before its first nudge, an engine that does not report background work gets the
        // grace: that work may still be running toward the submission.
        let pause = if !complete && !self.reports_waiting && self.checkpoint.nudges == 0 {
            self.limits.grace
        } else {
            self.limits.settle
        };
        if now.saturating_sub(idle) < pause {
            return Ok(Action::None);
        }
        if complete {
            // Nothing to submit: the session ends once the agent is idle.
            self.checkpoint.state = State::Exiting;
            return Ok(Action::Finish);
        }
        self.nudge(format!("Your turn ended but these outputs are not submitted: {}. Finish and submit them with the command from your task (submitting ends your session), or explain the blocker.", missing.join(", ")))
    }
    fn nudge(&mut self, text: String) -> Result<Action, AgentFailure> {
        if self.checkpoint.nudges >= self.limits.nudges {
            return Err(failure(
                FailureKind::ExitedWithoutSubmit,
                format!(
                    "the agent stopped without a valid step_submit after {} nudges; last message: {}",
                    self.checkpoint.nudges, self.checkpoint.final_text
                ),
            ));
        }
        self.checkpoint.nudges += 1;
        let id = InputId::Nudge {
            ordinal: self.checkpoint.nudges,
        };
        self.checkpoint
            .delivery
            .enqueue(id.clone(), text.clone())
            .map_err(invalid)?;
        Ok(Action::Send {
            id,
            text,
            steer: false,
        })
    }
}

fn clock_ms() -> io::Result<u64> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_millis(),
    )
    .map_err(io::Error::other)
}
async fn persist<H: SupervisorHost>(
    machine: &Machine,
    config: &SupervisorConfig,
    host: &mut H,
) -> Result<(), AgentFailure> {
    machine.checkpoint.save(&config.run_dir).map_err(invalid)?;
    tokio::time::timeout(Duration::from_secs(2), host.checkpoint(&machine.checkpoint))
        .await
        .map_err(|_| invalid("checkpoint notification timed out"))?
        .map_err(invalid)
}
async fn bounded<T>(
    cancel: &CancellationToken,
    deadline: tokio::time::Instant,
    future: impl Future<Output = Result<T, EngineError>>,
) -> Result<T, AgentFailure> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(failure(FailureKind::Cancelled, "agent cancelled")),
        result = tokio::time::timeout_at(deadline, future) => result.map_err(|_| failure(FailureKind::WallCap, "engine operation deadline exceeded"))?.map_err(Into::into),
    }
}
async fn bounded_host<T>(
    cancel: &CancellationToken,
    deadline: tokio::time::Instant,
    future: impl Future<Output = io::Result<T>>,
) -> Result<T, AgentFailure> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(failure(FailureKind::Cancelled, "agent cancelled")),
        result = tokio::time::timeout_at(deadline, future) => result.map_err(|_| failure(FailureKind::WallCap, "wall-clock cap during host operation"))?.map_err(invalid),
    }
}
/// Called only in an admitted guardian invocation, never by a scheduler request future.
/// On outer-helper Transient, retain SessionGuard and the invocation's run directory, and
/// increment internal_attempt. The helper alone waits/owns the retry budget in that mode.
pub async fn supervise<E: EngineAdapter, H: SupervisorHost>(
    mut config: SupervisorConfig,
    engine: &mut E,
    host: &mut H,
    sessions: &mut SessionGuard,
    tmux: Option<&ApprovedTmux>,
    cancel: &CancellationToken,
) -> Result<AgentResult, AgentFailure> {
    config.cwd = fs::canonicalize(&config.cwd)
        .map_err(|e| failure(FailureKind::SessionCwd, format!("missing cwd: {e}")))?;
    if !config.cwd.is_dir() {
        return Err(failure(FailureKind::SessionCwd, "cwd is not a directory"));
    }
    if config.assigned.after > config.assigned.through || config.internal_attempt == 0 {
        return Err(invalid("invalid assigned range or internal attempt"));
    }
    fs::create_dir_all(&config.run_dir).map_err(invalid)?;
    fs::set_permissions(&config.run_dir, fs::Permissions::from_mode(0o700)).map_err(invalid)?;
    config.run_dir = fs::canonicalize(&config.run_dir).map_err(invalid)?;
    let profile = engine.profile();
    if profile.engine != config.engine {
        return Err(failure(
            FailureKind::CapabilityMismatch,
            "adapter engine differs from request",
        ));
    }
    let model = launch_model(engine, &profile, config.model.as_ref()).await?;
    let existing = Checkpoint::read(&config.run_dir).map_err(invalid)?;
    let mut checkpoint = if let Some(mut previous) = existing {
        if previous.version != 1
            || previous.run != config.run
            || previous.attempt != config.attempt
            || previous.invocation != config.invocation
            || previous.engine != config.engine
            || previous.cwd != config.cwd
            || previous.state != State::Backoff
            || config.internal_attempt <= previous.internal_attempt
        {
            return Err(invalid(
                "checkpoint does not match a same-run transient re-entry",
            ));
        }
        previous.delivery.recover();
        previous.compactions = 0;
        previous.internal_attempt = config.internal_attempt;
        previous
    } else {
        if config.internal_attempt != 1 {
            return Err(invalid("transient re-entry requires its checkpoint"));
        }
        let seed: Option<MessageContinuation> =
            match fs::read(config.run_dir.join("message-continuation.json")) {
                Ok(bytes) => Some(decode_json(&bytes).map_err(invalid)?),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(invalid(error)),
            };
        if seed.as_ref().is_some_and(|seed| {
            seed.run != config.run
                || seed
                    .delivery
                    .entries
                    .iter()
                    .any(|entry| !matches!(entry.id, InputId::Message { .. }))
        }) {
            return Err(invalid("message continuation does not belong to this run"));
        }
        let (mut delivery, live_after) = seed
            .map(|seed| (seed.delivery, seed.live_after.max(config.assigned.through)))
            .unwrap_or_else(|| (DeliveryLedger::default(), config.assigned.through));
        delivery.recover();
        Checkpoint {
            version: 1,
            run: config.run,
            attempt: config.attempt,
            invocation: config.invocation,
            engine: config.engine.clone(),
            cwd: config.cwd.clone(),
            internal_attempt: 1,
            session: select_session(
                config.session.as_deref(),
                config.previous.as_ref(),
                &config.engine,
                &config.cwd,
                config.assigned,
            ),
            head_before: git::head(&config.cwd).await.map_err(invalid)?,
            started_ms: clock_ms().map_err(invalid)?,
            state: State::Boot,
            delivery,
            submissions: BTreeMap::new(),
            reminded: false,
            nudges: 0,
            compactions: 0,
            live_after,
            final_text: String::new(),
            notes: vec![],
        }
    };
    fs::write(config.run_dir.join("task.md"), &config.task).map_err(invalid)?;
    let task_text = prompt::hand_over(&config.task, &config.run_dir.join("task.md"), "Your task")
        .map_err(invalid)?;
    checkpoint
        .delivery
        .enqueue(InputId::Task, task_text)
        .map_err(invalid)?;
    let mut assigned = config.messages.clone();
    assigned.sort_by_key(|m| m.id);
    for message in assigned {
        if message.id <= config.assigned.after || message.id > config.assigned.through {
            return Err(invalid("message outside assigned range"));
        }
        enqueue_message(&mut checkpoint, &message, &config.run_dir)?;
    }
    let now = Duration::from_millis(
        clock_ms()
            .map_err(invalid)?
            .saturating_sub(checkpoint.started_ms),
    );
    let deadline = tokio::time::Instant::now() + config.limits.wall.saturating_sub(now);
    let mut machine = Machine::new(
        checkpoint,
        config.limits.clone(),
        config.required.clone(),
        profile.reports_waiting,
        now,
    );
    let context = EngineContext {
        run_dir: config.run_dir.clone(),
        cwd: config.cwd.clone(),
        model: model.clone(),
        tmux_binary: tmux.map(|artifact| artifact.binary().into()),
    };
    let mut server: Option<PrivateTmux> = None;
    let mut quiet = QuietMonitor::default();
    let outcome = async {
        loop {
            if cancel.is_cancelled() { return Err(failure(FailureKind::Cancelled, "agent cancelled")); }
            if machine.checkpoint.delivery.uncertain() { return Err(failure(FailureKind::UnknownAcceptance, "checkpoint has uncertain delivery; refusing replay")); }
            if let Some(session) = machine.checkpoint.session.clone() {
                let metadata = match bounded(cancel, deadline, engine.session(&session)).await {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind == FailureKind::MissingSession && machine.checkpoint.delivery.entries.iter().all(|entry| entry.tries == 0) => {
                        clean(engine, host, &mut server, tmux, &context).await?;
                        machine.checkpoint.notes.push(format!("{}; starting fresh", error.message));
                        eprintln!("{}; starting fresh", error.message);
                        machine.checkpoint.session = None;
                        persist(&machine, &config, host).await?;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if let Some(metadata) = metadata {
                    if metadata.id.is_empty() { return Err(invalid("empty canonical session id")); }
                    let cwd = fs::canonicalize(&metadata.cwd).map_err(|e| failure(FailureKind::SessionCwd, format!("session cwd missing: {e}")))?;
                    if cwd != config.cwd { return Err(failure(FailureKind::SessionCwd, format!("session started in {}, not {}", cwd.display(), config.cwd.display()))); }
                    machine.checkpoint.session = Some(metadata.id);
                }
                sessions.acquire(&config.home, &config.engine, machine.checkpoint.session.as_deref().expect("session"), &config.project, config.run)?;
            }
            persist(&machine, &config, host).await?;
            refresh_me(host, &config.run_dir).await;
            let ready_deadline = deadline.min(tokio::time::Instant::now() + config.limits.ready);
            let startup = async {
                while pending_hooks(&context.run_dir).map_err(invalid)? {
                    process_hooks_inner(engine, None, &context.run_dir).map_err(invalid)?;
                }
                let launch = bounded(cancel, ready_deadline, engine.prepare(&context, machine.checkpoint.session.as_deref())).await?;
                if let Some(mut launch) = launch {
                    launch.env.insert("SLUICE_RUN_DIR".into(), context.run_dir.to_string_lossy().into_owned());
                    launch.env.insert("SLUICE_RUN_ID".into(), config.run.to_string());
                    let artifact = tmux.ok_or_else(|| failure(FailureKind::CapabilityMismatch, "pane launch requires approved private tmux"))?;
                    let mut private = PrivateTmux::spawn(artifact, &context.run_dir).await.map_err(invalid)?;
                    // Store ownership before awaiting pane creation.
                    private.create_pane(artifact, &context, launch).await.map_err(invalid)?;
                    server = Some(private);
                }
                let command = machine.checkpoint.session.clone().map(|session| EngineCommand::Resume { session }).unwrap_or(EngineCommand::StartFresh);
                bounded(cancel, ready_deadline, engine.execute(&context, command)).await.map(|_| ())
            }.await;
            let mut startup_failure = None;
            if let Err(mut error) = startup {
                if error.kind == FailureKind::WallCap && ready_deadline < deadline { error.kind = FailureKind::ReadyTimeout; }
                let missing = error.message.clone();
                // Only an explicit adapter missing-session error before any offer permits fallback.
                // Preserve its typed kind through startup (the public kind maps this below).
                if error.kind == FailureKind::MissingSession && machine.checkpoint.session.is_some() && machine.checkpoint.delivery.entries.iter().all(|entry| entry.tries == 0) {
                    clean(engine, host, &mut server, tmux, &context).await?;
                    machine.checkpoint.notes.push(format!("{missing}; starting fresh"));
                    eprintln!("{missing}; starting fresh");
                    machine.checkpoint.session = None;
                    persist(&machine, &config, host).await?;
                    continue;
                }
                startup_failure = Some(error);
            }
            if machine.checkpoint.internal_attempt > 1 && startup_failure.is_none() {
                machine.checkpoint.delivery.enqueue(InputId::Continue { attempt: machine.checkpoint.internal_attempt }, "Your session was interrupted by a rate limit or capacity error. Continue your task where you left off.".into()).map_err(invalid)?;
            }
            let result = if let Some(error) = startup_failure {
                Err(error)
            } else {
                machine.checkpoint.state = State::Delivering;
                run_invocation(&mut machine, &config, engine, host, sessions, &context, cancel, deadline, &mut quiet).await
            };
            match result {
                Ok(()) => return Ok(()),
                Err(error) if error.kind == FailureKind::Transient => {
                    machine.checkpoint.state = State::Backoff;
                    machine.checkpoint.delivery.recover();
                    persist(&machine, &config, host).await?;
                    clean(engine, host, &mut server, tmux, &context).await?;
                    if matches!(config.retry.owner, RetryOwner::OuterHelper) { return Err(error); }
                    if machine.checkpoint.internal_attempt > config.retry.additional_tries { return Err(error); }
                    if machine.checkpoint.session.is_none() { return Err(failure(FailureKind::UnknownAcceptance, "transient without a recorded session cannot start a fresh worker")); }
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return Err(failure(FailureKind::Cancelled, "cancelled during transient backoff")),
                        _ = tokio::time::sleep_until(deadline) => return Err(failure(FailureKind::WallCap, "wall-clock cap during backoff")),
                        _ = tokio::time::sleep(config.retry.backoff) => {},
                    }
                    machine.checkpoint.internal_attempt += 1;
                    machine.checkpoint.compactions = 0;
                    let now = Duration::from_millis(clock_ms().map_err(invalid)?.saturating_sub(machine.checkpoint.started_ms));
                    machine = Machine::new(machine.checkpoint.clone(), config.limits.clone(), config.required.clone(), profile.reports_waiting, now);
                    quiet.reset();
                }
                Err(error) => return Err(error),
            }
        }
    }.await;
    // This path runs on every ordinary error and cancellation. The guardian owns cleanup
    // when the whole future/process is destroyed. No success can precede this proof.
    let cleanup = clean(engine, host, &mut server, tmux, &context).await;
    if let Err(mut error) = cleanup.and(outcome) {
        error.session.clone_from(&machine.checkpoint.session);
        let _ = machine.checkpoint.save(&config.run_dir);
        return Err(error);
    }
    machine.checkpoint.state = State::Done;
    persist(&machine, &config, host).await?;
    Ok(AgentResult {
        final_text: machine.checkpoint.final_text,
        report: None,
        session: machine.checkpoint.session.unwrap_or_default(),
        git: git::facts(&config.cwd, machine.checkpoint.head_before.as_deref())
            .await
            .map_err(invalid)?,
        notes: machine.checkpoint.notes,
        model: model.map(|m| m.id),
    })
}
/// The model this launch runs: the requested object or the engine's default, composed and
/// checked against the engine's listing. An unknown id fails here, before anything starts.
async fn launch_model<E: EngineAdapter>(
    engine: &mut E,
    profile: &EngineProfile,
    requested: Option<&crate::model::ModelChoice>,
) -> Result<Option<crate::model::ResolvedModel>, AgentFailure> {
    let Some(choice) = requested.or(profile.default_model.as_ref()) else {
        return Ok(None);
    };
    let catalog = engine.models().await.map_err(AgentFailure::from)?;
    crate::model::resolve(&profile.engine, choice, &catalog)
        .map(Some)
        .map_err(invalid)
}
fn enqueue_message(
    checkpoint: &mut Checkpoint,
    message: &DeliveryMessage,
    directory: &Path,
) -> Result<(), AgentFailure> {
    let body = message.body.as_value();
    let text = if let Some(object) = body.as_object() {
        let field = |key: &str| object.get(key).and_then(Value::as_str);
        let from = field("from").unwrap_or("orchestrator");
        let thread = field("thread").unwrap_or("step");
        // A question says how to answer it: a reply to its id.
        let heading = match field("verb") {
            Some("ask") => format!(
                "Question {} from {from} on your sluice thread `{thread}` (answer it with sluice tool reply, to_message {})",
                message.id, message.id
            ),
            Some("reply") => format!(
                "Reply from {from} on your sluice thread `{thread}` to message {}",
                object
                    .get("to_message")
                    .map(Value::to_string)
                    .unwrap_or_default()
            ),
            _ => format!("Message from {from} on your sluice thread `{thread}`"),
        };
        format!(
            "{heading}: {}{}",
            field("body").unwrap_or(""),
            object
                .get("data")
                .filter(|v| !v.is_null())
                .map(|v| format!("\n\ndata: {v}"))
                .unwrap_or_default()
        )
    } else {
        body.as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| body.to_string())
    };
    let text = prompt::hand_over(
        &text,
        &directory
            .join("messages")
            .join(format!("{}.md", message.id)),
        "A message",
    )
    .map_err(invalid)?;
    checkpoint
        .delivery
        .enqueue(InputId::Message { id: message.id }, text)
        .map_err(invalid)
}
#[allow(clippy::too_many_arguments)]
async fn run_invocation<E: EngineAdapter, H: SupervisorHost>(
    machine: &mut Machine,
    config: &SupervisorConfig,
    engine: &mut E,
    host: &mut H,
    sessions: &mut SessionGuard,
    context: &EngineContext,
    cancel: &CancellationToken,
    deadline: tokio::time::Instant,
    quiet: &mut QuietMonitor,
) -> Result<(), AgentFailure> {
    loop {
        if hooks_need_context(&config.run_dir).map_err(invalid)? {
            refresh_me(host, &config.run_dir).await;
        }
        process_hooks(engine, config.run, &config.run_dir).map_err(invalid)?;
        let o = bounded(cancel, deadline, engine.observe(context)).await?;
        if engine
            .profile()
            .required_capabilities
            .iter()
            .any(|c| c == INLINE_COMPACTION_CONTEXT)
        {
            machine.checkpoint.compactions = o.compactions;
        }

        let snapshot = bounded_host(
            cancel,
            deadline,
            host.snapshot(machine.checkpoint.live_after),
        )
        .await?;
        // Snapshot is authoritative; removing a field cannot leave an earlier submission complete.
        machine.checkpoint.submissions = snapshot.submissions;
        let mut messages = snapshot.messages;
        messages.sort_by_key(|m| m.id);
        for message in messages {
            if message.id > machine.checkpoint.live_after {
                enqueue_message(&mut machine.checkpoint, &message, &config.run_dir)?;
                machine.checkpoint.live_after = message.id;
            }
        }
        let now = Duration::from_millis(
            clock_ms()
                .map_err(invalid)?
                .saturating_sub(machine.checkpoint.started_ms),
        );
        if o.status == EngineStatus::Busy {
            if quiet.due(now, config.limits.quiet) {
                let mark = bounded_host(cancel, deadline, git::sample(&config.cwd)).await?;
                let cpu = crate::quiet::descendant_cpu(&snapshot.roots).map_err(invalid)?;
                if let Some(note) = quiet.observe(now, config.limits.quiet, mark, cpu) {
                    machine.checkpoint.notes.push(note.clone());
                    if let Err(error) =
                        tokio::time::timeout(Duration::from_secs(2), host.note(&note))
                            .await
                            .unwrap_or_else(|_| Err(io::Error::other("quiet note timed out")))
                    {
                        eprintln!("quiet note not posted: {error}");
                    }
                }
            }
        } else {
            quiet.reset();
        }
        let action = machine.update(now, &o, &snapshot.background_work)?;
        if let Some(session) = &machine.checkpoint.session {
            sessions.acquire(
                &config.home,
                &config.engine,
                session,
                &config.project,
                config.run,
            )?;
        }
        persist(machine, config, host).await?;
        // The guardian answers control requests in order and may first be waiting on a hook
        // reply that only this loop writes; keep answering hooks while the ack is in flight.
        {
            let acknowledged = machine.checkpoint.delivery.acknowledged_messages();
            let ack = bounded_host(cancel, deadline, host.acknowledge(&acknowledged));
            tokio::pin!(ack);
            loop {
                tokio::select! {
                    result = &mut ack => break result?,
                    _ = tokio::time::sleep(Duration::from_millis(5)) => {
                        process_hooks(engine, config.run, &config.run_dir).map_err(invalid)?;
                    }
                }
            }
        }
        match action {
            Action::Send { id, text, steer } => {
                machine.offered(&id, &o, now)?;
                persist(machine, config, host).await?;
                let command = if steer {
                    EngineCommand::Steer {
                        id: id.clone(),
                        text,
                    }
                } else {
                    EngineCommand::DeliverText {
                        id: id.clone(),
                        text,
                    }
                };
                let outcome = match bounded(
                    cancel,
                    deadline.min(tokio::time::Instant::now() + config.limits.turn_start),
                    engine.execute(context, command),
                )
                .await
                {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        machine
                            .checkpoint
                            .delivery
                            .outcome(&id, DeliveryOutcome::Uncertain)
                            .map_err(invalid)?;
                        persist(machine, config, host).await?;
                        return Err(if error.kind == FailureKind::Cancelled {
                            error
                        } else {
                            failure(
                                FailureKind::UnknownAcceptance,
                                format!("input command failed with unknown acceptance: {error}"),
                            )
                        });
                    }
                };
                machine
                    .checkpoint
                    .delivery
                    .outcome(&id, outcome)
                    .map_err(invalid)?;
                persist(machine, config, host).await?;
            }
            Action::Reprime { ordinal } => {
                let me = tokio::time::timeout(Duration::from_secs(2), host.me())
                    .await
                    .unwrap_or_else(|_| Err(io::Error::other("snapshot timed out")));
                let error = me.as_ref().err().map(ToString::to_string);
                let text = reprime::context(
                    &config.run_dir.join("task.md"),
                    me.as_deref()
                        .map_err(|_| error.as_deref().unwrap_or("snapshot unavailable")),
                );
                let text = prompt::hand_over(
                    &text,
                    &config
                        .run_dir
                        .join("messages")
                        .join(format!("compact-{ordinal}.md")),
                    "Your compacted context",
                )
                .map_err(invalid)?;
                machine
                    .checkpoint
                    .delivery
                    .enqueue(
                        InputId::Reprime {
                            ordinal: ordinal
                                + ((u64::from(machine.checkpoint.internal_attempt) - 1) << 32),
                        },
                        text,
                    )
                    .map_err(invalid)?;
                machine.checkpoint.compactions = ordinal;
                persist(machine, config, host).await?;
            }
            Action::Retry => {
                return Err(failure(
                    FailureKind::Transient,
                    o.error
                        .map(|e| e.message)
                        .unwrap_or_else(|| "engine transient".into()),
                ));
            }
            Action::Finish => {
                let _ = bounded(
                    cancel,
                    deadline.min(tokio::time::Instant::now() + config.limits.ready),
                    engine.execute(context, EngineCommand::RequestExit),
                )
                .await;
                return Ok(());
            }
            Action::None => {}
        }
        tokio::select! { biased; _ = cancel.cancelled() => return Err(failure(FailureKind::Cancelled, "agent cancelled")), _ = tokio::time::sleep(config.limits.poll) => {} }
    }
}
async fn clean<E: EngineAdapter, H: SupervisorHost>(
    engine: &mut E,
    host: &mut H,
    server: &mut Option<PrivateTmux>,
    tmux: Option<&ApprovedTmux>,
    context: &EngineContext,
) -> Result<(), AgentFailure> {
    let close = tokio::time::timeout(Duration::from_secs(10), engine.close()).await;
    let server_result = if let Some(mut server) = server.take() {
        server
            .stop(tmux.expect("approved server"), &context.run_dir)
            .await
    } else {
        Ok(())
    };
    let proof = tokio::time::timeout(Duration::from_secs(15), host.cleanup()).await;
    // Always run all three, including when an earlier cleanup operation failed.
    close
        .map_err(|_| failure(FailureKind::Cleanup, "adapter close timed out"))?
        .map_err(|e| failure(FailureKind::Cleanup, e.to_string()))?;
    server_result.map_err(|e| failure(FailureKind::Cleanup, e.to_string()))?;
    proof
        .map_err(|_| failure(FailureKind::Cleanup, "containment cleanup timed out"))?
        .map_err(|e| failure(FailureKind::Cleanup, e.to_string()))?;
    // Shutdown hooks belong to the stopped invocation, even when its session and
    // run will be reused. Settle them before another adapter starts observing.
    while pending_hooks(&context.run_dir).map_err(invalid)? {
        process_hooks_inner(engine, None, &context.run_dir).map_err(invalid)?;
    }
    Ok(())
}

/// Foreground server and control client always use the verified release artifact and a
/// relative private socket. Guardian containment also covers cancellation during setup.
pub struct PrivateTmux {
    server: tokio::process::Child,
    control: Option<tokio::process::Child>,
}
impl PrivateTmux {
    pub async fn spawn(artifact: &ApprovedTmux, directory: &Path) -> io::Result<Self> {
        let socket = directory.join("tmux.sock");
        if let Ok(metadata) = fs::symlink_metadata(&socket) {
            use std::os::unix::fs::FileTypeExt;
            if !metadata.file_type().is_socket()
                || std::os::unix::net::UnixStream::connect(&socket).is_ok()
            {
                return Err(io::Error::other(
                    "private tmux socket is still owned or is not a socket",
                ));
            }
            fs::remove_file(&socket)?;
        }
        let command = artifact.server_command(directory, None)?;
        let mut command = tokio::process::Command::from(command);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("TERM", "xterm-256color")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut owned = Self {
            server: command.spawn()?,
            control: None,
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !directory.join("tmux.sock").exists() {
            if owned.server.try_wait()?.is_some() || tokio::time::Instant::now() >= deadline {
                return Err(io::Error::other("private foreground tmux did not start"));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(owned)
    }
    pub async fn create_pane(
        &mut self,
        artifact: &ApprovedTmux,
        context: &EngineContext,
        launch: EngineLaunch,
    ) -> io::Result<()> {
        if launch.argv.is_empty() {
            return Err(io::Error::other("empty engine argv"));
        }
        let mut client = artifact.client_command(&context.run_dir)?;
        client.args(["new-session", "-d", "-s", "main", "-x", "220", "-y", "50"]);
        let mut env = launch.env;
        for (key, value) in [
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_EDITOR", "true"),
            ("GIT_MERGE_AUTOEDIT", "no"),
        ] {
            env.insert(key.into(), value.into());
        }
        for name in [
            "CLAUDECODE",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_MESSAGING_TOKEN",
            "AI_AGENT",
        ] {
            env.remove(name);
        }
        // tmux replaces the pane PATH with the unattached client's PATH after -e.
        client.env_clear().envs(&env);
        if let Some(term) = env.get("TERM") {
            let mut options = artifact.client_command(&context.run_dir)?;
            options.args(["set-option", "-g", "default-terminal", term]);
            command_output(options).await?;
        }
        for (name, value) in env {
            if name.is_empty() || name.contains(['=', '\0']) || value.contains('\0') {
                return Err(io::Error::other("invalid pane environment"));
            }
            client.arg("-e").arg(format!("{name}={value}"));
        }
        client
            .arg("-c")
            .arg(&context.cwd)
            .arg("--")
            .args(launch.argv);
        command_output(client).await?;
        let mut control =
            tokio::process::Command::from(artifact.control_command(&context.run_dir, "main")?);
        control
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        self.control = Some(control.spawn()?);
        eprintln!(
            "attach: cd {} && {} -S tmux.sock attach",
            prompt::shell_quote(&context.run_dir.to_string_lossy()),
            prompt::shell_quote(&artifact.binary().to_string_lossy())
        );
        Ok(())
    }
    pub async fn stop(&mut self, artifact: &ApprovedTmux, directory: &Path) -> io::Result<()> {
        let mut client = artifact.client_command(directory)?;
        client.arg("kill-server");
        let _ = command_output(client).await;
        self.server.kill().await?;
        if let Some(mut control) = self.control.take() {
            control.kill().await?;
        }
        Ok(())
    }
}
async fn command_output(command: std::process::Command) -> io::Result<std::process::Output> {
    let mut command = tokio::process::Command::from(command);
    command.stdin(Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .map_err(|_| io::Error::other("private tmux client timed out"))??;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "private tmux client: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(out)
}

/// Claim-before-call is the at-most-once boundary. A claimed request without a reply is
/// uncertain after a crash; neither the guardian nor supervisor executes it again. Each pass
/// first settles the claims a crash or an older release left (see `hook_journal`).
pub fn process_hooks<E: EngineAdapter>(
    engine: &mut E,
    run: RunId,
    directory: &Path,
) -> io::Result<()> {
    process_hooks_inner(engine, Some(run), directory)
}
fn pending_hooks(directory: &Path) -> io::Result<bool> {
    Ok(
        !hook_journal::Listing::read(&hook_journal::path(directory))?
            .requests
            .is_empty(),
    )
}
fn process_hooks_inner<E: EngineAdapter>(
    engine: &mut E,
    run: Option<RunId>,
    directory: &Path,
) -> io::Result<()> {
    use sluice_model::error::PublicError;
    use sluice_process::socket::EngineHookReply;
    let journal = hook_journal::path(directory);
    let listing = hook_journal::settle_claims(&journal)?;
    for id in listing.requests.iter().take(128) {
        let Some(request) = hook_journal::claim(&journal, id)? else {
            continue;
        };
        let result: Result<EngineHookReply, PublicError> = if request.engine
            != engine.profile().engine
            || Some(request.run) != run
            || request.event.len() > 128
            || request.event.is_empty()
        {
            Err(PublicError::BadRequest {
                message: if run.is_none() {
                    "hook belongs to a stopped invocation"
                } else {
                    "hook run/event mismatch"
                }
                .into(),
            })
        } else {
            engine
                .on_hook(HookEvent {
                    event: request.event,
                    payload: request.payload.into_value(),
                })
                .map_err(|error| PublicError::AgentFailure {
                    kind: format!("{:?}", error.kind),
                    message: error.to_string(),
                    session: None,
                })
                .and_then(|reply| {
                    if !(0..=255).contains(&reply.exit_code) {
                        return Err(PublicError::BadRequest {
                            message: "invalid hook exit code".into(),
                        });
                    }
                    let stdout = reply
                        .stdout
                        .map(sluice_model::rpc::JsonValue::try_from)
                        .transpose()?;
                    Ok(EngineHookReply {
                        stdout,
                        exit_code: reply.exit_code,
                    })
                })
        };
        hook_journal::answer(&journal, id, &result)?;
    }
    Ok(())
}

fn hooks_need_context(directory: &Path) -> io::Result<bool> {
    Ok(hook_journal::requests(directory)?.iter().any(|request| {
        request.event == "SessionStart"
            && request
                .payload
                .as_value()
                .get("source")
                .and_then(Value::as_str)
                == Some("compact")
    }))
}
async fn refresh_me<H: SupervisorHost>(host: &mut H, directory: &Path) {
    let path = directory.join("me.md");
    if let Ok(Ok(text)) = tokio::time::timeout(Duration::from_secs(2), host.me()).await
        && text.len() <= 1024 * 1024
        && fs::write(directory.join("me.md.tmp"), text).is_ok()
        && fs::rename(directory.join("me.md.tmp"), &path).is_ok()
    {
        return;
    }
    let _ = fs::remove_file(path);
}
