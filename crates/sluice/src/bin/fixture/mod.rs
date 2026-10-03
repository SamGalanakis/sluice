use std::process::ExitCode;
struct Registration {
    cli_name: Option<&'static str>,
    matches: fn() -> bool,
    run: fn() -> ExitCode,
}
macro_rules! register_fixtures {
    ($($module:ident),* $(,)?) => {
        $(mod $module;)*
        const MODES: &[Registration] = &[$(Registration { cli_name: $module::CLI_NAME, matches: $module::matches, run: $module::run }),*];
    };
}
register_fixtures! {
    fn_stub,
    engine,
    codex,
    claude,
    devin,
    payload_exec,
    claude_hook,
    claude_submit,
}
fn parse() -> clap::ArgMatches {
    clap::Command::new("fixture")
        .arg(
            clap::Arg::new("kind")
                .required(true)
                .value_name("KIND")
                .value_parser(clap::builder::PossibleValuesParser::new(
                    MODES.iter().filter_map(|m| m.cli_name),
                )),
        )
        .arg(
            clap::Arg::new("script")
                .value_name("SCRIPT")
                .value_parser(clap::value_parser!(std::path::PathBuf)),
        )
        .get_matches()
}
pub fn run() -> ExitCode {
    if let Some(mode) = MODES.iter().find(|mode| (mode.matches)()) {
        return (mode.run)();
    }
    parse();
    fn_stub::run()
}
