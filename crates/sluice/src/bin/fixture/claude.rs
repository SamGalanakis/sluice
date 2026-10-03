pub const CLI_NAME: Option<&str> = Some("claude");
use std::process::ExitCode;
pub fn matches() -> bool {
    std::env::args().nth(1).as_deref() == Some("claude")
}
pub fn run() -> ExitCode {
    match sluice_agents::engines::claude::fixture::main(std::env::args().skip(2).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
