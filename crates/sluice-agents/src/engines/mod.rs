//! Engine contract v1. Adapters report facts; only the shared supervisor judges success.
use serde::{Deserialize, Serialize};
use sluice_model::ids::MessageId;
use std::{collections::BTreeMap, future::Future, io, path::PathBuf};

pub mod claude;
pub mod codex;
pub mod devin;
pub mod environment;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputId {
    Task,
    Message { id: MessageId },
    Reprime { ordinal: u64 },
    Nudge { ordinal: u32 },
    Reminder,
    Continue { attempt: u32 },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineStatus {
    Starting,
    Busy,
    Idle,
    Blocked,
    Exited,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineErrorKind {
    Transient,
    MissingSession,
    CapabilityMismatch,
    UnknownAcceptance,
    Fatal,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineError {
    pub kind: EngineErrorKind,
    pub message: String,
}
impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for EngineError {}

/// Counters are monotonic within one invocation, start at zero, and exclude resumed history.
/// A completed turn is evidence even when no separate start notification was available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineObservation {
    pub status: EngineStatus,
    pub turns_started: u64,
    pub turns_completed: u64,
    pub waiting: Option<String>,
    pub background_work: Vec<String>,
    pub compactions: u64,
    pub final_text: String,
    pub session_id: Option<String>,
    pub acknowledged: Vec<InputId>,
    /// Positive evidence the engine did not accept these inputs. Mere silence is insufficient.
    pub not_accepted: Vec<InputId>,
    pub progress: u64,
    pub error: Option<EngineError>,
}
impl Default for EngineObservation {
    fn default() -> Self {
        Self {
            status: EngineStatus::Starting,
            turns_started: 0,
            turns_completed: 0,
            waiting: None,
            background_work: vec![],
            compactions: 0,
            final_text: String::new(),
            session_id: None,
            acknowledged: vec![],
            not_accepted: vec![],
            progress: 0,
            error: None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum EngineCommand {
    StartFresh,
    Resume { session: String },
    DeliverText { id: InputId, text: String },
    Steer { id: InputId, text: String },
    RequestExit,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryOutcome {
    Acknowledged,
    NotAccepted,
    Pending,
    Uncertain,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineProfile {
    pub engine: String,
    pub version_range: String,
    pub required_capabilities: Vec<String>,
    pub models: BTreeMap<String, String>,
    pub efforts: Vec<String>,
    pub reports_waiting: bool,
}
impl EngineProfile {
    pub fn validate_selection(
        &self,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<(), EngineError> {
        if model.is_some_and(|m| !self.models.contains_key(m))
            || effort.is_some_and(|e| !self.efforts.iter().any(|v| v == e))
        {
            return Err(EngineError {
                kind: EngineErrorKind::CapabilityMismatch,
                message: format!("{}: unsupported model or effort", self.engine),
            });
        }
        Ok(())
    }
}
/// Immutable paths belong to the current invocation. No prompt appears on argv.
#[derive(Debug, Clone)]
pub struct EngineContext {
    pub run_dir: PathBuf,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub tmux_binary: Option<PathBuf>,
}
#[derive(Debug, Clone)]
pub struct EngineLaunch {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
}
/// All methods run inside the admitted invocation. Implementations must tolerate cancellation
/// by future drop; cleanup must reap auxiliary children. Session aliases resolve before locking.
pub trait EngineAdapter: Send {
    /// Synchronous hook decisions must stay bounded and perform no provider calls.
    fn on_hook(&mut self, _hook: HookEvent) -> Result<HookReply, EngineError> {
        Err(EngineError {
            kind: EngineErrorKind::CapabilityMismatch,
            message: "engine hooks are unsupported".into(),
        })
    }
    fn profile(&self) -> EngineProfile;
    fn session(
        &mut self,
        session: &str,
    ) -> impl Future<Output = Result<Option<SessionMetadata>, EngineError>> + Send;
    fn prepare(
        &mut self,
        context: &EngineContext,
        session: Option<&str>,
    ) -> impl Future<Output = Result<Option<EngineLaunch>, EngineError>> + Send;
    fn execute(
        &mut self,
        context: &EngineContext,
        command: EngineCommand,
    ) -> impl Future<Output = Result<DeliveryOutcome, EngineError>> + Send;
    fn observe(
        &mut self,
        context: &EngineContext,
    ) -> impl Future<Output = Result<EngineObservation, EngineError>> + Send;
    fn close(&mut self) -> impl Future<Output = io::Result<()>> + Send;
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMetadata {
    pub id: String,
    pub cwd: PathBuf,
}

/// Executable fixture protocol. One matching command consumes a frame; frames without a
/// command advance on observe. Unmatched commands are recorded but consume no frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptFrame {
    pub command: Option<EngineCommand>,
    pub outcome: DeliveryOutcome,
    pub observation: EngineObservation,
    pub error: Option<EngineError>,
    pub delay_ms: u64,
}

/// Profile marker: the engine supplies context in the compaction hook reply itself.
pub const INLINE_COMPACTION_CONTEXT: &str = "sluice.compaction_context_inline";
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookEvent {
    pub event: String,
    pub payload: serde_json::Value,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookReply {
    pub stdout: Option<serde_json::Value>,
    pub exit_code: i32,
}
