pub const CLI_NAME: Option<&str> = Some("codex");
use std::process::ExitCode;
pub fn matches() -> bool {
    std::env::args().nth(1).as_deref() == Some("codex")
}
pub fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(2).collect();
    match sluice_agents::engines::codex::protocol::fixture_main(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
