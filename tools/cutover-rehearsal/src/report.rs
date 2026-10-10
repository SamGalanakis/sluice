//! The cutover report (`plan_rows::CutoverReport` in the schema-3 release; fixture
//! `cutover.report`), written here against the old release, which has no `plan_rows`. The
//! fields and their JSON are the same; `tests/rehearsal.rs` round-trips the contract's
//! fixtures through these types, and the integration branch decodes this tool's output with
//! the pinned type.

use serde::{Deserialize, Serialize};
use sluice_model::{
    commands::StepStatus,
    error::PublicError,
    ids::{AttemptId, RunId, StepId},
};

/// What the cutover asked of a live run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopRequest {
    Cancel,
    Stop,
}
/// How a run ended, read after the zero-blocker check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunOutcome {
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
/// What the orchestrator should do, from the actual outcome, never from the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetryAdvice {
    None,
    Retry,
    ReadThenRetry,
    CallAgain,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoppedRun {
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<StepId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    pub run: RunId,
    pub requested: StopRequest,
    pub outcome: RunOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_status: Option<StepStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_error: Option<String>,
    pub advice: RetryAdvice,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelRefusal {
    pub project: String,
    pub step: StepId,
    pub runs: Vec<RunId>,
    pub attempts: Vec<AttemptId>,
    pub error: PublicError,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CutoverReport {
    pub sha: String,
    pub schema: u32,
    pub deadline: String,
    pub reason: String,
    pub projects: u64,
    pub revisions: u64,
    pub stopped: Vec<StoppedRun>,
    pub refused: Vec<CancelRefusal>,
}

/// §10.1's advice: a succeeded run, or a step that succeeded as a whole, needs nothing; a
/// call is called again; a step failed `cancelled` is retried; any other failure is the
/// step's own and is read first.
pub fn advice(
    call: bool,
    outcome: &RunOutcome,
    step_status: Option<&StepStatus>,
    step_error: Option<&str>,
) -> RetryAdvice {
    if call {
        return match outcome.status {
            StepStatus::Succeeded => RetryAdvice::None,
            _ => RetryAdvice::CallAgain,
        };
    }
    if outcome.status == StepStatus::Succeeded || step_status == Some(&StepStatus::Succeeded) {
        return RetryAdvice::None;
    }
    match (step_status, step_error) {
        (Some(StepStatus::Failed), Some("cancelled")) => RetryAdvice::Retry,
        _ => RetryAdvice::ReadThenRetry,
    }
}

/// The one line per stopped run that the cutover appends to `deploy.log`.
pub fn line(run: &StoppedRun) -> String {
    let what = match (&run.step, &run.call) {
        (Some(step), _) => step.to_string(),
        (None, Some(call)) => format!("call:{call}"),
        (None, None) => "-".into(),
    };
    let outcome = match &run.outcome.error {
        Some(error) => format!("failed:{error}"),
        None => "succeeded".into(),
    };
    let word = |value: &dyn erased::Word| value.word();
    format!(
        "stopped {} {what} {} requested={} outcome={outcome} advice={}",
        run.project.as_deref().unwrap_or("-"),
        run.run,
        word(&run.requested),
        word(&run.advice),
    )
}
mod erased {
    pub trait Word {
        fn word(&self) -> String;
    }
    impl<T: serde::Serialize> Word for T {
        fn word(&self) -> String {
            match serde_json::to_value(self) {
                Ok(serde_json::Value::String(word)) => word,
                other => format!("{other:?}"),
            }
        }
    }
}
