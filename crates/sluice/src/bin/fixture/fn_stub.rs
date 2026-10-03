use std::process::ExitCode;
pub const CLI_NAME: Option<&str> = Some("fn");
pub fn matches() -> bool {
    false
}
pub fn run() -> ExitCode {
    eprintln!(
        "{}",
        sluice::error_json(&sluice_model::error::PublicError::not_implemented(
            "fake fn/engine"
        ))
    );
    ExitCode::FAILURE
}
