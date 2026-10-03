use super::{Mode, ModeFuture};
use std::path::PathBuf;
pub fn structured(mode: &Mode) -> Result<(), sluice_model::error::PublicError> {
    use crate::cli::{AgentCommand, Engine};

    if let Mode::Agent {
        command: AgentCommand::Hook { engine, event, run },
    } = mode
    {
        let engine = match engine {
            Engine::Codex => "codex",
            Engine::Claude => "claude",
            Engine::Devin => "devin",
        };
        let code = sluice_agents::engine_hook_cli(engine, event, *run)?;
        std::process::exit(code);
    }
    Ok(())
}

pub fn run(mode: Mode, _home: PathBuf) -> ModeFuture {
    Box::pin(async move { structured(&mode) })
}

pub fn early_dispatch() -> Result<(), sluice_model::error::PublicError> {
    use sluice_model::error::PublicError;
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "agent-hook") {
        if args.len() != 4 {
            return Err(PublicError::BadRequest {
                message: "usage: sluice agent-hook <engine> <event>".into(),
            });
        }
        let code = sluice_agents::engine_hook_cli(&args[2], &args[3], None)?;
        std::process::exit(code);
    }
    Ok(())
}
