pub const CLI_NAME: Option<&str> = Some("devin");
use std::process::ExitCode;
pub fn matches() -> bool {
    let devin_args: Vec<String> = std::env::args().skip(1).collect();
    devin_args.first().is_some_and(|s| {
        s == "devin"
            || s == "devin-submit"
            || (s == "agent-hook" && devin_args.get(1).is_some_and(|engine| engine == "devin"))
    })
}
pub fn run() -> ExitCode {
    let devin_args: Vec<String> = std::env::args().skip(1).collect();
    let args = if devin_args[0] == "devin" {
        &devin_args[1..]
    } else {
        &devin_args[..]
    };
    match sluice_agents::engines::devin::fixture::main(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
