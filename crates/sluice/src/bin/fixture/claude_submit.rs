pub const CLI_NAME: Option<&str> = None;
use std::process::ExitCode;
pub fn matches() -> bool {
    std::env::args().nth(1).as_deref() == Some("claude-submit")
}
pub fn run() -> ExitCode {
    match sluice_agents::engines::claude::fixture::submit(std::env::args().skip(2).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
