//! sluice: executable composition.

pub mod cli;
pub mod doctor;
pub mod logging;
pub mod me;
pub mod modes;

pub fn error_json(error: &sluice_model::error::PublicError) -> String {
    serde_json::to_string(error)
        .unwrap_or_else(|_| "{\"error\":\"storage\",\"message\":\"error encoding failed\"}".into())
}

pub mod install;
pub mod release;
