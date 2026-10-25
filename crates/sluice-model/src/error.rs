use crate::ids::Revision;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Path-bearing diagnostics, such as `steps.work.in.engine: required`.
pub type PathErrors = Vec<String>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum PublicError {
    #[error("{message}")]
    BadRequest { message: String },
    #[error("{message}")]
    NotFound { message: String },
    #[error("{message}")]
    Conflict {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_rev: Option<Revision>,
    },
    #[error("{message}")]
    Invalid { message: String, errors: PathErrors },
    #[error("{message}")]
    Busy { message: String, retryable: bool },
    #[error("{message}")]
    Storage { message: String },
    #[error("{message}")]
    CursorExpired { message: String },
    #[error("{message}")]
    ProcessLost { message: String },
    #[error("{message}")]
    ResourceExhausted {
        resource: crate::limits::ExhaustedResource,
        limit: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peak: Option<u64>,
        message: String,
    },
    #[error("{message}")]
    Cancelled { message: String },
    #[error("{message}")]
    FnFailure { message: String },
    #[error("{message}")]
    Transient { message: String },
    #[error("{message}")]
    Rejected { message: String },
    #[error("{message}")]
    AgentFailure {
        /// Earliest safe retry, as a Unix second (for example a usage-limit reset).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_at: Option<u64>,
        #[serde(default = "agent_failure_kind")]
        kind: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// A step's work ended (its agent exited or stopped) without a valid `step_submit`
    /// of the outputs it must submit.
    #[error("{message}")]
    ExitedWithoutSubmit {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
}
impl PublicError {
    pub fn not_implemented(mode: &str) -> Self {
        Self::BadRequest {
            message: format!("{mode} is not implemented in this build"),
        }
    }
}

fn agent_failure_kind() -> String {
    "AgentFailure".into()
}

/// Operational cause of an unsuccessful run, independent of its task and model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    ProviderQuota,
    ProviderUnavailable,
    Stall,
    WallCap,
    ResourceKill,
    NoSubmission,
    TaskRejected,
    Harness,
}
impl PublicError {
    pub fn failure_class(&self) -> FailureClass {
        use FailureClass::*;
        match self {
            Self::AgentFailure { kind, message, .. } => match kind.as_str() {
                "QuotaExhausted" => ProviderQuota,
                "AuthFailed" => ProviderUnavailable,
                "StallCap" => Stall,
                "WallCap" => WallCap,
                "ExitedWithoutSubmit" => NoSubmission,
                // These channels carry provider error text rather than a more specific kind.
                "Transient" | "EngineExited" | "AgentFailure" => transport_class(message),
                _ => Harness,
            },
            Self::ResourceExhausted { .. } => ResourceKill,
            Self::ExitedWithoutSubmit { .. } => NoSubmission,
            Self::Rejected { .. } => TaskRejected,
            Self::Transient { message } | Self::FnFailure { message } => transport_class(message),
            _ => Harness,
        }
    }
}
fn transport_class(message: &str) -> FailureClass {
    let text = message.to_ascii_lowercase();
    if [
        "no space left on device",
        "disk full",
        "out of memory",
        "oom-kill",
        "oom kill",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        FailureClass::ResourceKill
    } else if text.split(|c: char| !c.is_ascii_digit()).any(|s| {
        s.parse::<u16>()
            .is_ok_and(|n| matches!(n, 401 | 403 | 429 | 500..=599))
    }) || [
        "unauthorized",
        "authentication",
        "proxy",
        "connection refused",
        "connection reset",
        "service unavailable",
        "bad gateway",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        FailureClass::ProviderUnavailable
    } else {
        FailureClass::Harness
    }
}
/// Enrich only a known error-bearing tool row; persisted errors retain their old shape.
pub fn show_failure(row: &mut serde_json::Value) {
    if let Some(error) = row.get("error").filter(|e| e.is_object())
        && let Ok(error) = serde_json::from_value::<PublicError>(error.clone())
    {
        let class = serde_json::to_value(error.failure_class()).expect("failure class");
        row["failure_class"] = class.clone();
        row["error"]["failure_class"] = class;
    }
}
