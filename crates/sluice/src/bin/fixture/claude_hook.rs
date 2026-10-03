pub const CLI_NAME: Option<&str> = None;
use std::process::ExitCode;
pub fn matches() -> bool {
    std::env::args().nth(1).as_deref() == Some("agent-hook")
        && std::env::args().nth(2).as_deref() == Some("claude")
}
pub fn run() -> ExitCode {
    match std::env::args()
        .nth(3)
        .ok_or_else(|| std::io::Error::other("missing Claude fixture event"))
        .and_then(|event| sluice_agents::engines::claude::fixture::hook_proxy(&event))
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
