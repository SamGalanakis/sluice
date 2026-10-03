//! sluice: executable composition.

pub mod cli;
pub mod doctor;
pub mod me;

pub fn error_json(error: &sluice_model::error::PublicError) -> String {
    serde_json::to_string(error)
        .unwrap_or_else(|_| "{\"error\":\"storage\",\"message\":\"error encoding failed\"}".into())
}
