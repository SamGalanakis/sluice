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
    Cancelled { message: String },
    #[error("{message}")]
    FnFailure { message: String },
    #[error("{message}")]
    Transient { message: String },
    #[error("{message}")]
    Rejected { message: String },
    #[error("{message}")]
    AgentFailure {
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
