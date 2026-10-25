//! Shared state machine and admitted-invocation driver.
use crate::{
    delivery::{DeliveryLedger, DeliveryState},
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
    park::{ParkCondition, ParkOp, ParkRequest, ParkView},
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
    /// Minimum time between reminders, injected directly by tests.
    pub reminder: Duration,
    pub stall: Duration,
    /// Idle time before a session with nothing to submit ends, or an unfinished one is nudged.
    pub settle: Duration,
    /// The longer idle time before the first nudge of an engine that does not report
    /// background work, which may still be running toward the submission.
    pub grace: Duration,
    pub poll: Duration,
    pub ready: Duration,
    pub turn_start: Duration,
    /// Idle time to receive acceptance evidence for input left pending after a turn.
    /// Injected directly by tests, never read from home settings.
    pub delivery_ack: Duration,
    /// Legacy wait timeout, accepted for compatibility; live work no longer triggers nudges.
    pub wait: Duration,
    pub dialog: Duration,
    pub quiet: Duration,
    /// Legacy background cleanup timeout, accepted for compatibility.
    pub work: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            nudges: 3,
            reminder: Duration::from_secs(3600),
            stall: Duration::from_secs(30 * 60),
            settle: Duration::from_secs(10),
            grace: Duration::from_secs(10 * 60),
            poll: Duration::from_millis(500),
            ready: Duration::from_secs(180),
            turn_start: Duration::from_secs(60),
            delivery_ack: Duration::from_secs(60),
            wait: Duration::from_secs(90 * 60),
            dialog: Duration::from_secs(60),
            quiet: Duration::from_secs(60 * 60),
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
            reminder: short,
            stall: Duration::from_secs(1),
            settle: short,
            grace: short,
            poll: Duration::from_millis(2),
            ready: Duration::from_secs(1),
            turn_start: Duration::from_millis(100),
            // Native fixture turns can take longer under gate load. Tests of the
            // acknowledgement deadline inject their own bound.
            delivery_ack: Self::default().delivery_ack,
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
    /// Parked (`sluice tool park`): the session idles, its lane released, until the
    /// condition fires.
    Parked,
    Exiting,
    Done,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureKind {
    Transient,
    MissingSession,
    /// The agent exited, or stopped after its nudges, without a valid step_submit.
    ExitedWithoutSubmit,
    /// One engine or host operation, or one retry wait, did not finish in time.
    OperationTimeout,
    StallCap,
    ReadyTimeout,
    TurnStartTimeout,
    LockConflict,
    SessionCwd,
    CapabilityMismatch,
    UnknownAcceptance,
    /// The engine hit a hard usage cap (see `engines::account`); never retried or replayed here.
    QuotaExhausted,
    /// The engine cannot authenticate (see `engines::account`); never retried or replayed here.
    AuthFailed,
    /// The engine stopped on an interactive screen it cannot pass (see `engines::screen`);
    /// never retried or replayed here.
    BlockedScreen,
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
    /// An engine limit's reported reset (or the end of Codex's network backoff): the
    /// Unix second the retry starts at, in place of the policy's fixed backoff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at: Option<u64>,
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
        retry_at: None,
    }
}
fn invalid(error: impl std::fmt::Display) -> AgentFailure {
    failure(FailureKind::Invalid, error.to_string())
}
impl From<EngineError> for AgentFailure {
    fn from(e: EngineError) -> Self {
        let retry_at = e.retry_at;
        let failure = failure(
            match e.kind {
                EngineErrorKind::Transient | EngineErrorKind::ObservationUnavailable => {
                    FailureKind::Transient
                }
                EngineErrorKind::MissingSession => FailureKind::MissingSession,
                EngineErrorKind::CapabilityMismatch => FailureKind::CapabilityMismatch,
                EngineErrorKind::UnknownAcceptance => FailureKind::UnknownAcceptance,
                EngineErrorKind::QuotaExhausted => FailureKind::QuotaExhausted,
                EngineErrorKind::AuthFailed => FailureKind::AuthFailed,
                EngineErrorKind::BlockedScreen => FailureKind::BlockedScreen,
                EngineErrorKind::Fatal => FailureKind::EngineExited,
            },
            e.message,
        );
        AgentFailure {
            retry_at,
            ..failure
        }
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
    /// The session was given messages. They are in it and are never assigned again, so a
    /// fresh session would never see them.
    #[serde(default)]
    pub messaged: bool,
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
    /// The `workspace` input: where git facts are read when the work lands outside `cwd`.
    pub workspace: Option<PathBuf>,
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
    /// Time spent parked, left out of the active-time clock used by idle detection.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub parked_ms: u64,
}
fn is_zero(n: &u64) -> bool {
    *n == 0
}
impl Checkpoint {
    /// Run time so far, parked time left out.
    fn elapsed(&self) -> io::Result<Duration> {
        Ok(Duration::from_millis(
            clock_ms()?
                .saturating_sub(self.started_ms)
                .saturating_sub(self.parked_ms),
        ))
    }
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
    /// A held or waiting section lease protects work just as a live child does.
    pub leases_active: bool,
    pub roots: Vec<ProcessIdentity>,
    /// The run's park (`sluice tool park`), once asked for.
    pub park: Option<ParkRequest>,
}
/// How long a host call keeps retrying a peer (the coordinator or the run's guardian) that
/// answers `busy` (retryable) or does not answer in time, before the run fails `Transient`.
/// A coordinator applying a burst of plan edits stalls for minutes at most, and a deploy
/// restarts it in seconds; ten minutes covers both many times over while still ending a run
/// whose peer is gone for good.
pub const HOST_PATIENCE: Duration = Duration::from_secs(600);
/// Each engine or host operation gets its own budget, regardless of run age. A host's
/// retry patience fits inside it, including the last transport attempt.
pub const OPERATION_TIMEOUT: Duration = Duration::from_secs(660);
/// Bound one transient wait, including a reported reset, without limiting session duration.
pub const BACKOFF_TIMEOUT: Duration = Duration::from_secs(3600);
/// A host call whose peer stayed unavailable for its whole patience. The supervisor fails
/// the run `Transient` on it, so the helper's retry resumes the session.
#[derive(Debug)]
pub struct HostUnavailable(pub String);
impl std::fmt::Display for HostUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for HostUnavailable {}
/// Calls `call` until it answers anything but a retryable `busy`, the run is cancelled
/// (`Interrupted`), or `patience` (none: no limit) has passed since the first try
/// ([`HostUnavailable`]). Waits between tries grow from 50 ms to 1 s. Another refusal is
/// returned as it is: it is definite.
pub async fn patiently<T, F>(
    cancel: &CancellationToken,
    patience: Option<Duration>,
    mut call: impl FnMut() -> F,
) -> io::Result<T>
where
    F: Future<Output = Result<T, sluice_model::error::PublicError>>,
{
    let cancelled = || io::Error::new(io::ErrorKind::Interrupted, "agent cancelled");
    let started = tokio::time::Instant::now();
    let mut wait = Duration::from_millis(50);
    loop {
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(cancelled()),
            result = call() => result,
        };
        let message = match result {
            Ok(reply) => return Ok(reply),
            Err(sluice_model::error::PublicError::Busy {
                message,
                retryable: true,
            }) => message,
            Err(error) => return Err(io::Error::other(error)),
        };
        if let Some(patience) = patience
            && started.elapsed() >= patience
        {
            return Err(io::Error::other(HostUnavailable(format!(
                "{message}; still unavailable after {}s of retrying",
                started.elapsed().as_secs()
            ))));
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(cancelled()),
            _ = tokio::time::sleep(wait) => {},
        }
        wait = (wait * 2).min(Duration::from_secs(1));
    }
}
/// Latest observed agent event and words, kept apart from strict delivery checkpoints.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentObservation {
    pub engine: String,
    pub model: Option<String>,
    pub last_activity_ms: Option<u64>,
    pub last_message: String,
}
/// The guardian supplies durable observations and holds invocation containment authority.
/// cleanup must prove all engine-owned descendants gone, including detached work. It must
/// retain the supervisor/control process and remain callable after an interrupted adapter.
///
/// A host call whose peer is only busy or slow keeps retrying (see [`patiently`]); it fails
/// with [`HostUnavailable`] once its patience is spent, which fails the run `Transient`. Any
/// other error is definite and fails the run `Invalid`.
pub trait SupervisorHost: Send {
    fn observation(
        &mut self,
        observation: &AgentObservation,
    ) -> impl Future<Output = io::Result<()>> + Send {
        let _ = observation;
        async { Ok(()) }
    }

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
    /// A mark that moves when the host may have news for the session (a message handed to
    /// the run). While nothing else changes, the supervisor watches it between snapshots and
    /// takes them less often. A host without one (`None`) is snapshotted every `poll`.
    fn news(&mut self) -> Option<u64> {
        None
    }
    /// Reads or moves the run's park (see [`ParkOp`]). A host without parks has none.
    fn park(&mut self, op: ParkOp) -> impl Future<Output = io::Result<ParkView>> + Send {
        let _ = op;
        async { Ok(ParkView::default()) }
    }
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
            .filter(|p| {
                p.engine == engine
                    && p.cwd == cwd
                    && (assigned.through > assigned.after || p.messaged)
            })
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
    /// Inputs steered into work that had completed this many turns at the offer.
    steered_inputs: BTreeMap<InputId, u64>,
    idle_delivery_since: Option<Duration>,
    base_starts: u64,
    awaiting: Option<Duration>,
    /// Input steered into a busy turn rides that turn, so it starts no turn-start wait.
    steered: bool,
    idle_since: Option<Duration>,
    last_nudge: Option<Duration>,
    dialog_since: Option<Duration>,
    moved: Duration,
    progress: Option<u64>,
    boot_at: Duration,
    /// The most turns this invocation's engine has reported completed.
    completed: u64,
    /// The agent asked to park; its turn's end parks the run.
    pub park: bool,
    /// The checkpoint as last persisted: an unchanged one is not written again.
    persisted: Option<Vec<u8>>,
    /// How many times a changed checkpoint was persisted.
    writes: u64,
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
    /// The agent asked to park and its turn is over.
    Park,
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
        // A re-entry cannot reconstruct the send time from an old checkpoint. Waiting a
        // full interval again is conservative and needs no checkpoint format change.
        let last_nudge = (checkpoint.nudges > 0).then_some(now);
        Self {
            checkpoint,
            limits,
            required,
            reports_waiting,
            base_turns: 0,
            steered_inputs: BTreeMap::new(),
            idle_delivery_since: None,
            base_starts: 0,
            awaiting: None,
            steered: false,
            idle_since: None,
            last_nudge,
            dialog_since: None,
            moved: now,
            progress: None,
            boot_at: now,
            completed: 0,
            park: false,
            persisted: None,
            writes: 0,
        }
    }
    /// The turns this invocation's engine has completed, as last observed.
    pub fn turns_completed(&self) -> u64 {
        self.completed
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
        if self.steered {
            self.steered_inputs
                .insert(id.clone(), observation.turns_completed);
        } else {
            self.steered_inputs.remove(id);
        }
        self.awaiting = (!self.steered).then_some(now);
        self.idle_since = None;
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
        leases_active: bool,
    ) -> Result<Action, AgentFailure> {
        self.completed = self.completed.max(o.turns_completed);
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
            || !background.is_empty()
            || leases_active;
        // A failed turn cannot become success just because no outputs were required.
        if let Some(error) = &o.error {
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
        // Turn counters describe work, not acceptance. Stop can leave a busy-turn
        // steer Offered. Permit late evidence for the delivery-ack interval, then
        // fail by name. Fresh idle-composer deliveries keep their turn-start wait.
        self.steered_inputs.retain(|id, _| {
            self.checkpoint
                .delivery
                .entries
                .iter()
                .any(|entry| entry.id == *id && entry.state == DeliveryState::Offered)
        });
        if o.status == EngineStatus::Idle
            && self
                .steered_inputs
                .values()
                .any(|completed| o.turns_completed > *completed)
        {
            let since = *self.idle_delivery_since.get_or_insert(now);
            if now.saturating_sub(since) >= self.limits.delivery_ack {
                return Err(failure(
                    FailureKind::TurnStartTimeout,
                    format!(
                        "engine left input(s) {:?} unacknowledged after its turn ended; refusing blind replay",
                        self.steered_inputs.keys().collect::<Vec<_>>()
                    ),
                ));
            }
        } else {
            self.idle_delivery_since = None;
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
            if has_work && matches!(entry.id, InputId::Nudge { .. }) {
                return Ok(Action::None);
            }
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
        // A park waits out its condition in place of any nudge or wait, background work and
        // its own queued lease included: they are often what it waits for.
        if self.park
            && new_turn
            && o.status == EngineStatus::Idle
            && !self.pending()
            && self.awaiting.is_none()
        {
            return Ok(Action::Park);
        }
        if !has_work
            && o.status != EngineStatus::Idle
            && now.saturating_sub(self.moved) >= self.limits.stall
        {
            return Err(failure(
                FailureKind::StallCap,
                "no transcript progress while non-idle",
            ));
        }
        // A gate can be queued and consume no CPU for hours. Neither elapsed wait time nor
        // an idle/blocked composer is evidence it should be interrupted or abandoned.
        if has_work {
            self.checkpoint.state = State::Waiting;
            self.idle_since = None;
            self.dialog_since = None;
            return Ok(Action::None);
        }
        if o.status == EngineStatus::Blocked {
            self.checkpoint.state = State::Waiting;
            let since = *self.dialog_since.get_or_insert(now);
            if now.saturating_sub(since) >= self.limits.dialog {
                self.dialog_since = None;
                return self.nudge(now, "Nobody can answer here. Decide, or ask with `sluice tool ask` (your unit's owner answers) and continue the task.".into());
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
            return Ok(Action::None);
        }
        if self.pending() {
            self.checkpoint.state = State::Delivering;
            return Ok(Action::None);
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
        self.nudge(now, format!("Your turn ended but these outputs are not submitted: {}. Finish and submit them with the command from your task (submitting ends your session), or explain the blocker; if you are waiting on a job, keep waiting.", missing.join(", ")))
    }
    fn nudge(&mut self, now: Duration, text: String) -> Result<Action, AgentFailure> {
        if self.checkpoint.nudges >= self.limits.nudges {
            return Err(failure(
                FailureKind::ExitedWithoutSubmit,
                format!(
                    "the agent stopped without a valid step_submit after {} nudges; last message: {}",
                    self.checkpoint.nudges, self.checkpoint.final_text
                ),
            ));
        }
        if self
            .last_nudge
            .is_some_and(|last| now.saturating_sub(last) < self.limits.reminder)
        {
            return Ok(Action::None);
        }
        self.last_nudge = Some(now);
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

/// A rate limit that reported its reset waits until just after it; any other transient
/// waits the policy's fixed backoff.
fn backoff(error: &AgentFailure, fixed: Duration) -> Duration {
    error.retry_at.map_or(fixed, |at| {
        Duration::from_secs(at.saturating_sub(crate::engines::account::now()))
    })
}
fn resume_text(previous: Option<&AgentFailure>) -> String {
    let cause = previous
        .filter(|error| error.kind == FailureKind::Transient)
        .map(|error| {
            let message = error.message.to_lowercase();
            if message.starts_with("codex: network error (") {
                "Codex lost its network connection after its own retries"
            } else if message.starts_with("claude: the pasted input did not show in the composer") {
                "your message could not be delivered and is being sent again"
            } else if [
                "rate limit",
                "rate_limit",
                "http status 429",
                "http 429",
                "too many requests",
            ]
            .iter()
            .any(|word| message.contains(word))
            {
                if error.retry_at.is_some() {
                    "a rate limit; it has reset"
                } else {
                    "a rate limit"
                }
            } else if [
                "capacity",
                "overloaded",
                "http status 529",
                "http 529",
                "http status 503",
                "http 503",
            ]
            .iter()
            .any(|word| message.contains(word))
            {
                "the engine was temporarily at capacity"
            } else if message.contains("engine exited") {
                "the engine exited"
            } else if message.contains("network error") || message.contains("connection error") {
                "a network connection failed"
            } else {
                "a temporary failure"
            }
        });
    match cause {
        Some(cause) => format!(
            "Your session was interrupted ({cause}). Continue your task where you left off."
        ),
        None => "Your session was interrupted. Continue your task where you left off.".into(),
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
    machine: &mut Machine,
    config: &SupervisorConfig,
    host: &mut H,
) -> Result<(), AgentFailure> {
    // Most ticks change nothing; rewriting (and syncing) the same checkpoint each one was
    // most of an idle session's cost.
    let bytes = serde_json::to_vec(&machine.checkpoint).map_err(invalid)?;
    if machine.persisted.as_ref() == Some(&bytes) {
        return Ok(());
    }
    machine.checkpoint.save(&config.run_dir).map_err(invalid)?;
    machine.persisted = Some(bytes);
    machine.writes += 1;
    // The local checkpoint is authoritative. A mirror notification is bookkeeping.
    let timing = sluice_process::probe::ProbeTiming::current();
    let mut delay = timing.backoff;
    for attempt in 0..3 {
        let result = tokio::time::timeout(
            timing.timeout.min(Duration::from_secs(2)),
            host.checkpoint(&machine.checkpoint),
        )
        .await
        .unwrap_or_else(|_| {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "checkpoint notification timed out",
            ))
        });
        match result {
            Ok(()) => return Ok(()),
            Err(error) if attempt == 2 => {
                observation_warning(&mut machine.checkpoint.notes, host, "checkpoint", &error)
                    .await;
            }
            Err(_) => {
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
        }
    }
    Ok(())
}
async fn observation_warning<H: SupervisorHost>(
    notes: &mut Vec<String>,
    host: &mut H,
    name: &str,
    error: &io::Error,
) {
    let prefix = format!("{name} observation unavailable");
    if notes.iter().any(|note| note.starts_with(&prefix)) {
        return;
    }
    let note = format!("{prefix}; fact is unknown, run continues: {error}");
    eprintln!("warning: {note}");
    notes.push(note.clone());
    if !matches!(
        tokio::time::timeout(Duration::from_secs(2), host.note(&note)).await,
        Ok(Ok(()))
    ) {
        eprintln!("observation warning could not reach the owner: {name}");
    }
}
async fn startup_host<T, H: SupervisorHost>(
    cancel: &CancellationToken,
    timeout: Duration,
    host: &mut H,
    notes: &mut Vec<String>,
    operation: impl Future<Output = io::Result<T>>,
) -> Result<T, AgentFailure> {
    let waiting = bounded_host(cancel, timeout, operation);
    tokio::pin!(waiting);
    tokio::select! {
        result = &mut waiting => result,
        _ = tokio::time::sleep(Duration::from_secs(10)) => {
            observation_warning(notes, host, "private tmux start", &io::Error::new(io::ErrorKind::TimedOut, "still queued; waiting for the same start")).await;
            waiting.await
        }
    }
}

async fn bounded<T>(
    cancel: &CancellationToken,
    timeout: Duration,
    future: impl Future<Output = Result<T, EngineError>>,
) -> Result<T, AgentFailure> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(failure(FailureKind::Cancelled, "agent cancelled")),
        result = tokio::time::timeout(timeout, future) => result.map_err(|_| failure(FailureKind::OperationTimeout, "engine operation timed out"))?.map_err(Into::into),
    }
}
async fn bounded_host<T>(
    cancel: &CancellationToken,
    timeout: Duration,
    future: impl Future<Output = io::Result<T>>,
) -> Result<T, AgentFailure> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(failure(FailureKind::Cancelled, "agent cancelled")),
        result = tokio::time::timeout(timeout, future) => result.map_err(|_| failure(FailureKind::OperationTimeout, "host operation timed out"))?.map_err(host_failure),
    }
}
/// A host error fails the run `Transient` when its peer was only unavailable, else `Invalid`.
fn host_failure(error: io::Error) -> AgentFailure {
    if error
        .get_ref()
        .is_some_and(|inner| inner.is::<HostUnavailable>())
    {
        failure(FailureKind::Transient, error.to_string())
    } else {
        invalid(error)
    }
}
/// Awaits a host call while answering the engine's hooks: the guardian waits on a hook's
/// decision for three seconds, and a host call can wait on a stalled peer far longer.
async fn answering_hooks<E: EngineAdapter, T>(
    engine: &mut E,
    config: &SupervisorConfig,
    future: impl Future<Output = Result<T, AgentFailure>>,
) -> Result<T, AgentFailure> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return result,
            _ = tokio::time::sleep(Duration::from_millis(5)) => {
                process_hooks(engine, config.run, &config.run_dir).map_err(invalid)?;
            }
        }
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
    let existing = Checkpoint::read(&config.run_dir).map_err(invalid)?;
    let mut preflight_notes = existing
        .as_ref()
        .map(|checkpoint| checkpoint.notes.clone())
        .unwrap_or_default();
    let model = launch_model(
        engine,
        &profile,
        config.model.as_ref(),
        host,
        &mut preflight_notes,
        cancel,
    )
    .await?;
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
        let mut notes = vec![];
        let head_before = match git::baseline(&config.cwd, config.workspace.as_deref()).await {
            Ok(head) => head,
            Err(error) => {
                observation_warning(&mut notes, host, "git", &error).await;
                None
            }
        };
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
            head_before,
            started_ms: clock_ms().map_err(invalid)?,
            state: State::Boot,
            delivery,
            submissions: BTreeMap::new(),
            reminded: false,
            nudges: 0,
            compactions: 0,
            live_after,
            final_text: String::new(),
            notes,
            parked_ms: 0,
        }
    };
    for note in preflight_notes {
        if !checkpoint.notes.contains(&note) {
            checkpoint.notes.push(note);
        }
    }
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
    let now = checkpoint.elapsed().map_err(invalid)?;
    let mut machine = Machine::new(
        checkpoint,
        config.limits.clone(),
        config.required.clone(),
        profile.reports_waiting,
        now,
    );
    let observation = AgentObservation {
        engine: config.engine.clone(),
        model: model.as_ref().map(|m| m.id.clone()),
        ..Default::default()
    };
    let _ = host.observation(&observation).await;
    let context = EngineContext {
        run_dir: config.run_dir.clone(),
        cwd: config.cwd.clone(),
        model: model.clone(),
        tmux_binary: tmux.map(|artifact| artifact.binary().into()),
    };
    let mut server: Option<PrivateTmux> = None;
    let mut quiet = QuietMonitor::default();
    // Checkpoints reject unknown fields in pinned releases, so the cause stays in memory.
    let mut previous_failure = None;
    let outcome = async {
        loop {
            if cancel.is_cancelled() { return Err(failure(FailureKind::Cancelled, "agent cancelled")); }
            if machine.checkpoint.delivery.uncertain() { return Err(failure(FailureKind::UnknownAcceptance, "checkpoint has uncertain delivery; refusing replay")); }
            if let Some(session) = machine.checkpoint.session.clone() {
                let mut delay = Duration::from_millis(100);
                let metadata_result = loop {
                    match bounded(cancel, OPERATION_TIMEOUT, async { Ok(engine.session(&session).await) }).await? {
                        Err(error) if error.kind == EngineErrorKind::ObservationUnavailable => {
                            observation_warning(&mut machine.checkpoint.notes, host, "session metadata", &io::Error::other(error.message)).await;
                            bounded(cancel, OPERATION_TIMEOUT, async { tokio::time::sleep(delay).await; Ok(()) }).await?;
                            delay = (delay * 2).min(Duration::from_secs(30));
                        }
                        result => break result.map_err(AgentFailure::from),
                    }
                };
                let metadata = match metadata_result {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind == FailureKind::MissingSession && machine.checkpoint.delivery.entries.iter().all(|entry| entry.tries == 0) => {
                        clean(engine, host, &mut server, tmux, &context).await?;
                        machine.checkpoint.notes.push(format!("{}; starting fresh", error.message));
                        eprintln!("{}; starting fresh", error.message);
                        machine.checkpoint.session = None;
                        persist(&mut machine, &config, host).await?;
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
            persist(&mut machine, &config, host).await?;
            refresh_me(host, &config.run_dir).await;
            let startup: Result<(), AgentFailure> = async {
                while pending_hooks(&context.run_dir).map_err(invalid)? {
                    process_hooks_inner(engine, None, &context.run_dir).map_err(invalid)?;
                }
                let mut probe_delay = Duration::from_millis(100);
                let launch = loop {
                    match bounded(cancel, OPERATION_TIMEOUT, async { Ok(engine.prepare(&context, machine.checkpoint.session.as_deref()).await) }).await? {
                        Ok(launch) => break launch,
                        Err(error) if error.kind == EngineErrorKind::ObservationUnavailable => {
                            observation_warning(&mut machine.checkpoint.notes, host, "engine probe", &io::Error::other(error.message)).await;
                            bounded(cancel, OPERATION_TIMEOUT, async { tokio::time::sleep(probe_delay).await; Ok(()) }).await?;
                            probe_delay = (probe_delay * 2).min(Duration::from_secs(30));
                        }
                        Err(error) => return Err(error.into()),
                    }
                };
                // Host preflight delay is not time spent waiting for an engine to become ready.
                machine.boot_at = Duration::from_millis(clock_ms().map_err(invalid)?.saturating_sub(machine.checkpoint.started_ms));
                if let Some(mut launch) = launch {
                    launch.env.insert("SLUICE_RUN_DIR".into(), context.run_dir.to_string_lossy().into_owned());
                    launch.env.insert("SLUICE_RUN_ID".into(), config.run.to_string());
                    let artifact = tmux.ok_or_else(|| failure(FailureKind::CapabilityMismatch, "pane launch requires approved private tmux"))?;
                    let private = startup_host(cancel, OPERATION_TIMEOUT, host, &mut machine.checkpoint.notes, PrivateTmux::spawn(artifact, &context.run_dir)).await?;
                    // Store ownership before awaiting pane creation.
                    server = Some(private);
                    startup_host(cancel, OPERATION_TIMEOUT, host, &mut machine.checkpoint.notes, server.as_mut().expect("private server").create_pane(artifact, &context, launch)).await?;
                    machine.boot_at = Duration::from_millis(clock_ms().map_err(invalid)?.saturating_sub(machine.checkpoint.started_ms));
                }
                let command = machine.checkpoint.session.clone().map(|session| EngineCommand::Resume { session }).unwrap_or(EngineCommand::StartFresh);
                let mut probe_delay = Duration::from_millis(100);
                loop {
                    match bounded(cancel, config.limits.ready, async { Ok(engine.execute(&context, command.clone()).await) }).await.map_err(|mut error| { if error.kind == FailureKind::OperationTimeout { error.kind = FailureKind::ReadyTimeout; } error })? {
                        Ok(_) => break Ok(()),
                        Err(error) if error.kind == EngineErrorKind::ObservationUnavailable => {
                            observation_warning(&mut machine.checkpoint.notes, host, "engine probe", &io::Error::other(error.message)).await;
                            bounded(cancel, OPERATION_TIMEOUT, async { tokio::time::sleep(probe_delay).await; Ok(()) }).await?;
                            probe_delay = (probe_delay * 2).min(Duration::from_secs(30));
                            machine.boot_at = Duration::from_millis(clock_ms().map_err(invalid)?.saturating_sub(machine.checkpoint.started_ms));
                        }
                        Err(error) => break Err(error.into()),
                    }
                }
            }.await;
            // An untested engine version runs, with a warning in the log and a note in the run.
            if let Some(note) = engine.version().and_then(|v| v.note())
                && !machine.checkpoint.notes.contains(&note)
            {
                tracing::warn!(engine = %config.engine, run = %config.run, "{note}");
                eprintln!("{note}");
                machine.checkpoint.notes.push(note);
            }
            let mut startup_failure = None;
            if let Err(error) = startup {
                let missing = error.message.clone();
                // Only an explicit adapter missing-session error before any offer permits fallback.
                // Preserve its typed kind through startup (the public kind maps this below).
                if error.kind == FailureKind::MissingSession && machine.checkpoint.session.is_some() && machine.checkpoint.delivery.entries.iter().all(|entry| entry.tries == 0) {
                    clean(engine, host, &mut server, tmux, &context).await?;
                    machine.checkpoint.notes.push(format!("{missing}; starting fresh"));
                    eprintln!("{missing}; starting fresh");
                    machine.checkpoint.session = None;
                    persist(&mut machine, &config, host).await?;
                    continue;
                }
                startup_failure = Some(error);
            }
            if machine.checkpoint.internal_attempt > 1 && startup_failure.is_none() {
                machine.checkpoint.delivery.enqueue(InputId::Continue { attempt: machine.checkpoint.internal_attempt }, resume_text(previous_failure.as_ref())).map_err(invalid)?;
            }
            let result = if let Some(error) = startup_failure {
                Err(error)
            } else {
                machine.checkpoint.state = State::Delivering;
                run_invocation(&mut machine, &config, engine, host, sessions, &context, cancel, &mut quiet).await
            };
            match result {
                Ok(()) => return Ok(()),
                Err(mut error) if error.kind == FailureKind::Transient => {
                    machine.checkpoint.state = State::Backoff;
                    machine.checkpoint.delivery.recover();
                    persist(&mut machine, &config, host).await?;
                    let last = matches!(config.retry.owner, RetryOwner::OuterHelper) || machine.checkpoint.internal_attempt > config.retry.additional_tries;
                    if last && keeps_pane(error.kind, machine.turns_completed()) {
                        keep_pane(&mut error, server.is_some(), tmux, &context.run_dir).await;
                    }
                    clean(engine, host, &mut server, tmux, &context).await?;
                    if last { return Err(error); }
                    if machine.checkpoint.session.is_none() { return Err(failure(FailureKind::UnknownAcceptance, "transient without a recorded session cannot start a fresh worker")); }
                    let wait = backoff(&error, config.retry.backoff);
                    eprintln!("agent transient, resuming in {}s: {}", wait.as_secs(), error.message);
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return Err(failure(FailureKind::Cancelled, "cancelled during transient backoff")),
                        _ = tokio::time::sleep(BACKOFF_TIMEOUT) => return Err(AgentFailure { retry_at: error.retry_at, ..failure(FailureKind::OperationTimeout, "transient backoff timed out") }),
                        _ = tokio::time::sleep(wait) => {},
                    }
                    machine.checkpoint.internal_attempt += 1;
                    previous_failure = Some(error);
                    machine.checkpoint.compactions = 0;
                    let now = machine.checkpoint.elapsed().map_err(invalid)?;
                    machine = Machine::new(machine.checkpoint.clone(), config.limits.clone(), config.required.clone(), profile.reports_waiting, now);
                    quiet.reset();
                }
                Err(error) => return Err(error),
            }
        }
    }.await;
    let mut outcome = outcome;
    // A failure that a protocol change could explain names an untested version as its cause.
    if let Err(error) = &mut outcome
        && let Some(hint) = engine.version().and_then(|v| v.hint())
        && let Some(likelihood) = version_cause(error.kind)
        && !error.message.contains(&hint)
    {
        error.message = format!("{}; {likelihood} cause: {hint}", error.message);
    }
    // The pane goes with the teardown below, so its text is kept first.
    if let Err(error) = &mut outcome
        && keeps_pane(error.kind, machine.turns_completed())
    {
        keep_pane(error, server.is_some(), tmux, &context.run_dir).await;
    }
    // This path runs on every ordinary error and cancellation. The guardian owns cleanup
    // when the whole future/process is destroyed. No success can precede this proof.
    let cleanup = clean(engine, host, &mut server, tmux, &context).await;
    if let Err(mut error) = cleanup.and(outcome) {
        error.session.clone_from(&machine.checkpoint.session);
        let _ = machine.checkpoint.save(&config.run_dir);
        // The run's log carries the step error's own text, under its kind.
        if error.kind != FailureKind::Cancelled {
            let class = sluice_model::error::PublicError::AgentFailure {
                kind: format!("{:?}", error.kind),
                message: error.message.clone(),
                session: None,
                retry_at: None,
            }
            .failure_class();
            eprintln!(
                "agent failed: {error}; failure_class={}",
                serde_json::to_value(class).expect("class")
            );
        }
        return Err(error);
    }
    let before = machine.checkpoint.head_before.clone();
    let before = before.as_deref();
    let facts = match &config.workspace {
        Some(workspace) => git::workspace_facts(workspace, before).await,
        None => git::facts(&config.cwd, before).await,
    };
    let git = match facts {
        Ok(facts) => facts,
        Err(error) => {
            observation_warning(&mut machine.checkpoint.notes, host, "git", &error).await;
            None
        }
    };
    machine.checkpoint.state = State::Done;
    persist(&mut machine, &config, host).await?;
    Ok(AgentResult {
        final_text: machine.checkpoint.final_text,
        report: None,
        session: machine.checkpoint.session.unwrap_or_default(),
        git,
        notes: machine.checkpoint.notes,
        model: model.map(|m| m.id),
    })
}
/// How likely an untested engine version caused a failure of this kind: an engine refusing
/// or failing a request, or exiting, is likely a protocol change; a lost or unstarted turn or a
/// stall may be one. Account, screen, session and sluice-side failures never are.
fn version_cause(kind: FailureKind) -> Option<&'static str> {
    match kind {
        FailureKind::CapabilityMismatch | FailureKind::EngineExited => Some("likely"),
        FailureKind::UnknownAcceptance
        | FailureKind::ReadyTimeout
        | FailureKind::TurnStartTimeout
        | FailureKind::StallCap => Some("possible"),
        _ => None,
    }
}
/// Whether a failure keeps the pane's text: one before the engine completed a turn, or a
/// timeout or stall; never a cancellation.
fn keeps_pane(kind: FailureKind, turns_completed: u64) -> bool {
    kind != FailureKind::Cancelled
        && (turns_completed == 0
            || matches!(
                kind,
                FailureKind::TurnStartTimeout
                    | FailureKind::StallCap
                    | FailureKind::ReadyTimeout
                    | FailureKind::OperationTimeout
            ))
}
/// Keeps the private pane's text at a failure, before the teardown destroys it: its screen (and
/// up to 50 rows above it) in `pane-at-failure.txt` (0600) in the invocation's directory, and
/// its last non-empty rows at the end of the message, anything token-like masked in both. A blocked screen's
/// message already quotes the screen, so it only names the file. A capture or write that
/// fails leaves the failure as it was.
async fn keep_pane(
    error: &mut AgentFailure,
    pane: bool,
    tmux: Option<&ApprovedTmux>,
    run_dir: &Path,
) {
    let (true, Some(artifact)) = (pane, tmux) else {
        return;
    };
    let Ok(Ok(pane)) = tokio::time::timeout(
        Duration::from_secs(5),
        PrivateTmux::capture(artifact, run_dir),
    )
    .await
    else {
        return;
    };
    let text = screen::evidence(&pane);
    let path = run_dir.join(screen::PANE_AT_FAILURE);
    let written = write_private(&path, text.as_bytes()).is_ok();
    let file = if written {
        format!("{}", path.display())
    } else {
        "not saved".into()
    };
    let rows = screen::tail(&text);
    if error.kind == FailureKind::BlockedScreen {
        error
            .message
            .push_str(&format!("\npane at failure: {file}"));
    } else if rows.is_empty() {
        error
            .message
            .push_str(&format!("\npane at failure was blank ({file})"));
    } else {
        error.message.push_str(&format!(
            "\npane at failure (last rows; whole screen: {file}):"
        ));
        for row in rows {
            error.message.push_str("\n  ");
            error.message.push_str(&row);
        }
    }
}
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()
}
/// The model this launch runs: the requested object or the engine's default, composed and
/// checked against the engine's listing. An unknown id fails here, before anything starts.
async fn launch_model<E: EngineAdapter, H: SupervisorHost>(
    engine: &mut E,
    profile: &EngineProfile,
    requested: Option<&crate::model::ModelChoice>,
    host: &mut H,
    notes: &mut Vec<String>,
    cancel: &CancellationToken,
) -> Result<Option<crate::model::ResolvedModel>, AgentFailure> {
    let Some(choice) = requested.or(profile.default_model.as_ref()) else {
        return Ok(None);
    };
    let mut delay = Duration::from_millis(100);
    let catalog = loop {
        let result = tokio::select! {
            _ = cancel.cancelled() => return Err(failure(FailureKind::Cancelled, "cancelled during model observation")),
            result = engine.models() => result,
        };
        match result {
            Ok(models) => break models,
            Err(error) if error.kind == EngineErrorKind::ObservationUnavailable => {
                observation_warning(
                    notes,
                    host,
                    "model listing",
                    &io::Error::other(error.message),
                )
                .await;
                tokio::select! {
                    _ = cancel.cancelled() => return Err(failure(FailureKind::Cancelled, "cancelled during model observation")),
                    _ = tokio::time::sleep(delay) => {},
                }
                delay = (delay * 2).min(Duration::from_secs(30));
            }
            Err(error) => return Err(error.into()),
        }
    };
    crate::model::resolve(&profile.engine, choice, &catalog)
        .map(Some)
        .map_err(invalid)
}
fn enqueue_message(
    checkpoint: &mut Checkpoint,
    message: &DeliveryMessage,
    directory: &Path,
) -> Result<(), AgentFailure> {
    enqueue_message_with_prefix(checkpoint, message, directory, "")
}
fn enqueue_message_with_prefix(
    checkpoint: &mut Checkpoint,
    message: &DeliveryMessage,
    directory: &Path,
    prefix: &str,
) -> Result<(), AgentFailure> {
    let body = message.body.as_value();
    let text = if let Some(object) = body.as_object() {
        let field = |key: &str| object.get(key).and_then(Value::as_str);
        let from = field("from").unwrap_or("someone");
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
        .enqueue(
            InputId::Message { id: message.id },
            format!("{prefix}{text}"),
        )
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
    quiet: &mut QuietMonitor,
) -> Result<(), AgentFailure> {
    let mut probe_delay = Duration::from_millis(100);
    let mut idle = IdleTicks::new(config.limits.poll);
    let mut last: Option<EngineObservation> = None;
    loop {
        let writes = machine.writes;
        if hooks_need_context(&config.run_dir).map_err(invalid)? {
            refresh_me(host, &config.run_dir).await;
        }
        process_hooks(engine, config.run, &config.run_dir).map_err(invalid)?;

        let snapshot = answering_hooks(
            engine,
            config,
            bounded_host(
                cancel,
                OPERATION_TIMEOUT,
                host.snapshot(machine.checkpoint.live_after),
            ),
        )
        .await?;
        // Host reads can answer engine hooks while they wait. Observe afterward so a
        // submission is judged with the acknowledgements and errors those hooks produced.
        let o = match bounded(cancel, OPERATION_TIMEOUT, async {
            Ok(engine.observe(context).await)
        })
        .await?
        {
            Ok(observation) => {
                probe_delay = Duration::from_millis(100);
                observation
            }
            Err(error) if error.kind == EngineErrorKind::ObservationUnavailable => {
                observation_warning(
                    &mut machine.checkpoint.notes,
                    host,
                    "engine probe",
                    &io::Error::other(error.message),
                )
                .await;
                bounded(cancel, OPERATION_TIMEOUT, async {
                    tokio::time::sleep(probe_delay).await;
                    Ok(())
                })
                .await?;
                probe_delay = (probe_delay * 2).min(Duration::from_secs(30));
                // A missing observation is no evidence of a stalled or unstarted turn.
                let now = machine.checkpoint.elapsed().map_err(invalid)?;
                machine.moved = now;
                machine.boot_at = now;
                for since in [
                    &mut machine.awaiting,
                    &mut machine.idle_delivery_since,
                    &mut machine.idle_since,
                    &mut machine.dialog_since,
                ] {
                    if since.is_some() {
                        *since = Some(now);
                    }
                }
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if engine
            .profile()
            .required_capabilities
            .iter()
            .any(|c| c == INLINE_COMPACTION_CONTEXT)
        {
            machine.checkpoint.compactions = o.compactions;
        }

        // Snapshot is authoritative; removing a field cannot leave an earlier submission complete.
        machine.checkpoint.submissions = snapshot.submissions;
        machine.park = snapshot.park.is_some();
        let mut messages = snapshot.messages;
        messages.sort_by_key(|m| m.id);
        for message in messages {
            if message.id > machine.checkpoint.live_after {
                enqueue_message(&mut machine.checkpoint, &message, &config.run_dir)?;
                machine.checkpoint.live_after = message.id;
            }
        }
        let now = machine.checkpoint.elapsed().map_err(invalid)?;
        let quiet_period = config.limits.quiet.max(config.limits.reminder);
        if o.status == EngineStatus::Busy
            && o.waiting.as_ref().is_none_or(String::is_empty)
            && o.background_work.is_empty()
            && snapshot.background_work.is_empty()
            && !snapshot.leases_active
        {
            if quiet.due(now, quiet_period) {
                let mark = answering_hooks(engine, config, async {
                    match git::sample(&config.cwd).await {
                        Ok(mark) => Ok(mark),
                        Err(error) => {
                            observation_warning(&mut machine.checkpoint.notes, host, "git", &error)
                                .await;
                            Ok(None)
                        }
                    }
                })
                .await?;
                let cpu = match sluice_process::probe::retry(|| {
                    std::future::ready(crate::quiet::descendant_cpu(&snapshot.roots))
                })
                .await
                {
                    Ok(cpu) => cpu,
                    Err(error) => {
                        observation_warning(&mut machine.checkpoint.notes, host, "CPU", &error)
                            .await;
                        // Unknown CPU activity cannot justify a quiet warning.
                        quiet.reset();
                        Default::default()
                    }
                };
                if let Some(note) = quiet.observe(now, quiet_period, mark, cpu) {
                    machine.checkpoint.notes.push(note.clone());
                    // A note is advice: one its peer never took is logged, never fatal.
                    if let Err(error) = answering_hooks(
                        engine,
                        config,
                        bounded_host(cancel, OPERATION_TIMEOUT, host.note(&note)),
                    )
                    .await
                    {
                        eprintln!("quiet note not posted: {error}");
                    }
                }
            }
        } else {
            quiet.reset();
        }
        if o.activity > 0
            && last
                .as_ref()
                .is_none_or(|prior: &EngineObservation| prior.activity != o.activity)
        {
            let observation = AgentObservation {
                engine: config.engine.clone(),
                model: context.model.as_ref().map(|m| m.id.clone()),
                last_activity_ms: clock_ms().ok(),
                last_message: crate::engines::account::redact(&o.final_text)
                    .chars()
                    .take(4000)
                    .collect(),
            };
            let _ = host.observation(&observation).await;
        }
        let action = machine.update(now, &o, &snapshot.background_work, snapshot.leases_active)?;
        let observed = last.as_ref() != Some(&o);
        let acted = action != Action::None;
        last = Some(o.clone());
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
        let acknowledged = machine.checkpoint.delivery.acknowledged_messages();
        answering_hooks(
            engine,
            config,
            bounded_host(cancel, OPERATION_TIMEOUT, host.acknowledge(&acknowledged)),
        )
        .await?;
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
                    config.limits.turn_start,
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
                // An error the session can carry on from in place (Codex's network loss) costs
                // a backoff and a continuation, within the transient budget; any other goes
                // back to `supervise`, which tears the engine down.
                if let Some(error) = &o.error
                    && machine.checkpoint.internal_attempt <= config.retry.additional_tries
                    && engine.recover_in_place(error)
                {
                    continue_in_place(machine, config, engine, host, cancel, error.clone().into())
                        .await?;
                    quiet.reset();
                    continue;
                }
                return Err(match o.error {
                    Some(error) => error.into(),
                    None => failure(FailureKind::Transient, "engine transient"),
                });
            }
            Action::Park => {
                park(machine, config, engine, host, context, cancel).await?;
                quiet.reset();
                continue;
            }
            Action::Finish => {
                let _ = bounded(
                    cancel,
                    config.limits.ready,
                    engine.execute(context, EngineCommand::RequestExit),
                )
                .await;
                return Ok(());
            }
            Action::None => {}
        }
        let changed = acted || observed || machine.writes != writes;
        let wait = idle.after(changed, host.news().is_some());
        if tick_wait(config, host, cancel, wait).await? {
            idle.reset();
        }
    }
}
/// An idle session's tick backs off to this many polls while nothing it observes changes.
const IDLE_POLLS: u32 = 10;
/// How often a tick's wait looks for a new hook request or news from the host.
const WAKE_CHECK: Duration = Duration::from_millis(100);
/// The wait before a session's next tick: `poll` after a tick that changed anything, doubling
/// up to [`IDLE_POLLS`] polls after each that changed nothing. A host that cannot say when it
/// has news keeps the tick at `poll`.
struct IdleTicks {
    poll: Duration,
    next: Duration,
}
impl IdleTicks {
    fn new(poll: Duration) -> Self {
        Self { poll, next: poll }
    }
    fn after(&mut self, changed: bool, news: bool) -> Duration {
        if changed || !news {
            self.reset();
            return self.poll;
        }
        let wait = self.next;
        self.next = (self.next * 2).min(self.poll * IDLE_POLLS);
        wait
    }
    fn reset(&mut self) {
        self.next = self.poll;
    }
}
/// Waits `wait` for the next tick, ending early when the engine sends a new hook request or
/// the host's news moves; returns whether it ended early.
async fn tick_wait<H: SupervisorHost>(
    config: &SupervisorConfig,
    host: &mut H,
    cancel: &CancellationToken,
    wait: Duration,
) -> Result<bool, AgentFailure> {
    let until = tokio::time::Instant::now() + wait;
    let news = host.news();
    let hooks = hook_requests(&config.run_dir).map_err(invalid)?;
    loop {
        let left = until.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return Ok(false);
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(failure(FailureKind::Cancelled, "agent cancelled")),
            _ = tokio::time::sleep(left.min(WAKE_CHECK)) => {}
        }
        if host.news() != news
            || !hook_requests(&config.run_dir)
                .map_err(invalid)?
                .is_subset(&hooks)
        {
            return Ok(true);
        }
    }
}
/// Carries the session on in the same engine process after a transient the adapter took back
/// (`EngineAdapter::recover_in_place`): waits out its backoff in `Backoff`, then queues the
/// next attempt's `Continue` input, which reaches the engine as any input does (never
/// replayed when its acceptance is unknown).
async fn continue_in_place<E: EngineAdapter, H: SupervisorHost>(
    machine: &mut Machine,
    config: &SupervisorConfig,
    engine: &mut E,
    host: &mut H,
    cancel: &CancellationToken,
    error: AgentFailure,
) -> Result<(), AgentFailure> {
    machine.checkpoint.state = State::Backoff;
    persist(machine, config, host).await?;
    let wait = backoff(&error, config.retry.backoff);
    eprintln!(
        "agent transient, continuing the same session in {}s: {}",
        wait.as_secs(),
        error.message
    );
    answering_hooks(engine, config, async {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(failure(FailureKind::Cancelled, "cancelled during transient backoff")),
            _ = tokio::time::sleep(BACKOFF_TIMEOUT) => Err(AgentFailure { retry_at: error.retry_at, ..failure(FailureKind::OperationTimeout, "transient backoff timed out") }),
            _ = tokio::time::sleep(wait) => Ok(()),
        }
    })
    .await?;
    machine.checkpoint.internal_attempt += 1;
    let attempt = machine.checkpoint.internal_attempt;
    machine
        .checkpoint
        .delivery
        .enqueue(
            InputId::Continue { attempt },
            "Your turn was cut off by a network error. Continue your task where you left off."
                .into(),
        )
        .map_err(invalid)?;
    let now = machine.checkpoint.elapsed().map_err(invalid)?;
    *machine = Machine::new(
        machine.checkpoint.clone(),
        config.limits.clone(),
        config.required.clone(),
        engine.profile().reports_waiting,
        now,
    );
    machine.checkpoint.state = State::Delivering;
    persist(machine, config, host).await
}
/// The agent asked to park (`sluice tool park`) and its turn is over. The session stays up,
/// idle; the coordinator releases the step's needs while the run waits for the condition (or
/// its timeout), then takes them back in the queue by priority, and the same session resumes
/// with one "woke:" message. Idle detection starts afresh after the park.
async fn park<E: EngineAdapter, H: SupervisorHost>(
    machine: &mut Machine,
    config: &SupervisorConfig,
    engine: &mut E,
    host: &mut H,
    context: &EngineContext,
    cancel: &CancellationToken,
) -> Result<(), AgentFailure> {
    machine.checkpoint.state = State::Parked;
    persist(machine, config, host).await?;
    let started = tokio::time::Instant::now();
    let Some(park) = parked_call(engine, config, host, cancel, ParkOp::Enter)
        .await?
        .park
    else {
        machine.park = false;
        return Ok(());
    };
    eprintln!("agent parked until {}", park.condition);
    let timeout = Duration::from_secs(
        park.timeout_seconds
            .unwrap_or(sluice_model::park::DEFAULT_PARK_TIMEOUT_SECONDS),
    );
    // A recovered supervisor must not restart the park's budget.
    let already_waited = time::OffsetDateTime::parse(
        park.parked_at.as_deref().unwrap_or(&park.requested_at),
        &time::format_description::well_known::Rfc3339,
    )
    .ok()
    .and_then(|at| std::time::Duration::try_from(time::OffsetDateTime::now_utc() - at).ok())
    .unwrap_or_default();
    let mut messages: Vec<DeliveryMessage>;
    let mut idle = IdleTicks::new(config.limits.poll);
    let reason = loop {
        let o = bounded(cancel, config.limits.ready, engine.observe(context)).await?;
        if o.status == EngineStatus::Exited {
            return Err(failure(
                FailureKind::EngineExited,
                "engine exited while parked",
            ));
        }
        if let Some(error) = o.error {
            return Err(error.into());
        }
        let view = parked_call(engine, config, host, cancel, ParkOp::Read).await?;
        if view.cancelled {
            cancel.cancel();
            return Err(failure(FailureKind::Cancelled, "cancelled while parked"));
        }
        let snapshot = answering_hooks(
            engine,
            config,
            bounded_host(
                cancel,
                OPERATION_TIMEOUT,
                host.snapshot(machine.checkpoint.live_after),
            ),
        )
        .await?;
        messages = snapshot
            .messages
            .into_iter()
            .filter(|m| m.id > machine.checkpoint.live_after)
            .collect();
        messages.sort_by_key(|m| m.id);
        if !messages.is_empty() {
            break "woken by a message".to_owned();
        }
        let fired = match &park.condition {
            ParkCondition::Lease { .. } => view.lease_held,
            condition => condition_holds(condition).map_err(invalid)?,
        };
        if fired {
            break park.condition.to_string();
        }
        if already_waited.saturating_add(started.elapsed()) >= timeout {
            break format!(
                "your park condition did not happen within {}s: {}",
                timeout.as_secs(),
                park.condition
            );
        }
        wait_parked(engine, config, host, cancel, &mut idle).await?;
    };
    let view = loop {
        let view = parked_call(
            engine,
            config,
            host,
            cancel,
            ParkOp::Wake {
                reason: reason.clone(),
            },
        )
        .await?;
        if view.cancelled {
            cancel.cancel();
            return Err(failure(FailureKind::Cancelled, "cancelled while parked"));
        }
        if view.woke {
            break view;
        }
        wait_parked(engine, config, host, cancel, &mut idle).await?;
    };
    let parked = started.elapsed();
    machine.checkpoint.parked_ms = machine
        .checkpoint
        .parked_ms
        .saturating_add(u64::try_from(parked.as_millis()).unwrap_or(u64::MAX));
    let text = match park.lease {
        Some(lease) if view.lease_held => format!("woke: {reason}, lease {lease}"),
        Some(lease) => format!("woke: {reason}; lease {lease} is still queued"),
        None => format!("woke: {reason}"),
    };
    eprintln!("agent {text}");
    if messages.is_empty() {
        let ordinal = machine
            .checkpoint
            .delivery
            .entries
            .iter()
            .filter(|entry| matches!(entry.id, InputId::Wake { .. }))
            .count();
        machine
            .checkpoint
            .delivery
            .enqueue(
                InputId::Wake {
                    ordinal: u32::try_from(ordinal + 1).map_err(invalid)?,
                },
                text,
            )
            .map_err(invalid)?;
    } else {
        let prefix = format!("{text}\n\n");
        for (index, message) in messages.iter().enumerate() {
            enqueue_message_with_prefix(
                &mut machine.checkpoint,
                message,
                &config.run_dir,
                if index == 0 { &prefix } else { "" },
            )?;
            machine.checkpoint.live_after = message.id;
        }
    }
    let now = machine.checkpoint.elapsed().map_err(invalid)?;
    *machine = Machine::new(
        machine.checkpoint.clone(),
        config.limits.clone(),
        config.required.clone(),
        engine.profile().reports_waiting,
        now,
    );
    machine.checkpoint.state = State::Delivering;
    persist(machine, config, host).await
}
/// A park call to the host, answering the engine's hooks meanwhile.
async fn parked_call<E: EngineAdapter, H: SupervisorHost>(
    engine: &mut E,
    config: &SupervisorConfig,
    host: &mut H,
    cancel: &CancellationToken,
    op: ParkOp,
) -> Result<ParkView, AgentFailure> {
    answering_hooks(
        engine,
        config,
        bounded_host(cancel, OPERATION_TIMEOUT, host.park(op)),
    )
    .await
}
/// A parked session is idle by definition: its checks back off as an idle tick does, and a
/// hook request ends the wait to be answered at once.
async fn wait_parked<E: EngineAdapter, H: SupervisorHost>(
    engine: &mut E,
    config: &SupervisorConfig,
    host: &mut H,
    cancel: &CancellationToken,
    idle: &mut IdleTicks,
) -> Result<(), AgentFailure> {
    let wait = idle.after(false, host.news().is_some());
    let woke = tick_wait(config, host, cancel, wait)
        .await
        .map_err(|error| {
            if error.kind == FailureKind::Cancelled {
                failure(FailureKind::Cancelled, "cancelled while parked")
            } else {
                error
            }
        })?;
    if woke {
        idle.reset();
    }
    process_hooks(engine, config.run, &config.run_dir).map_err(invalid)
}
/// Whether a `pid:` or `file:` park condition holds; a `lease:` one is the coordinator's.
pub fn condition_holds(condition: &ParkCondition) -> io::Result<bool> {
    match condition {
        ParkCondition::Lease { .. } => Ok(false),
        ParkCondition::Pid { pid, start } => sluice_process::identity::process_gone(*pid, *start),
        ParkCondition::File { path, marker } => match fs::read(path) {
            Ok(bytes) => Ok(marker.as_ref().is_none_or(|marker| {
                bytes
                    .windows(marker.len().max(1))
                    .any(|window| window == marker.as_bytes())
            })),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        },
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
        let mut delay = Duration::from_millis(100);
        let server = loop {
            match command.spawn() {
                Ok(child) => break child,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
                Err(error) => return Err(error),
            }
        };
        let mut owned = Self {
            server,
            control: None,
        };
        while !directory.join("tmux.sock").exists() {
            if owned.server.try_wait()?.is_some() {
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
        // An engine that exits leaves its last screen (and its pane, dead) until the teardown,
        // so the adapter reads its exit and a failure keeps what it showed.
        let mut options = artifact.client_command(&context.run_dir)?;
        options.args(["set-option", "-g", "remain-on-exit", "on"]);
        command_output(options).await?;
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
        let mut delay = Duration::from_millis(100);
        self.control = Some(loop {
            match control.spawn() {
                Ok(child) => break child,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
                Err(error) => return Err(error),
            }
        });
        eprintln!(
            "attach: cd {} && {} -S tmux.sock attach",
            prompt::shell_quote(&context.run_dir.to_string_lossy()),
            prompt::shell_quote(&artifact.binary().to_string_lossy())
        );
        Ok(())
    }
    /// The engine pane's screen and up to 50 rows above it: when the engine exits, tmux
    /// scrolls the dead pane's top row away to print `Pane is dead`.
    pub async fn capture(artifact: &ApprovedTmux, directory: &Path) -> io::Result<String> {
        let mut client = artifact.client_command(directory)?;
        client.args(["capture-pane", "-p", "-S", "-50", "-t", "main"]);
        let out = command_output(client).await?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
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
    let starts_pane = command
        .as_std()
        .get_args()
        .any(|arg| arg == "new-session" || arg == "set-option");
    let out = if starts_pane {
        // Retry only a failed fork. Once spawned, wait for this same command:
        // repeating new-session after uncertain acceptance could dispatch twice.
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut delay = Duration::from_millis(100);
        let child = loop {
            match command.spawn() {
                Ok(child) => break child,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
                Err(error) => return Err(error),
            }
        };
        child.wait_with_output().await?
    } else {
        sluice_process::probe::output(&mut command).await?
    };
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
    Ok(!hook_requests(directory)?.is_empty())
}
fn hook_requests(directory: &Path) -> io::Result<std::collections::BTreeSet<String>> {
    Ok(hook_journal::Listing::read(&hook_journal::path(directory))?.requests)
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
                    retry_at: error.retry_at,
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
