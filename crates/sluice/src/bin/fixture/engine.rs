pub const CLI_NAME: Option<&str> = Some("engine");
use std::process::ExitCode;
pub fn matches() -> bool {
    std::env::args().nth(1).as_deref() == Some("engine")
}
pub fn run() -> ExitCode {
    let fixture = super::parse();
    let result = fixture
        .get_one::<std::path::PathBuf>("script")
        .cloned()
        .ok_or_else(|| std::io::Error::other("engine fixture requires a scripted event file"))
        .and_then(|path| sluice_agents::fixture_engine_main(&path));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
