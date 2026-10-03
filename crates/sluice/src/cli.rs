use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "sluice", version, about = "Sluice Rust foundation")]
pub struct Cli {
    #[command(subcommand)]
    pub mode: Mode,
}
#[derive(Debug, Subcommand)]
pub enum Mode {
    Coordinator {
        #[arg(long)]
        maintenance: bool,
    },
    Serve {
        #[arg(long)]
        no_runner: bool,
        #[arg(long, default_value_t = 3065)]
        port: u16,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
    },
    Loop,
    Guardian(RunArgs),
    PayloadExec(RunArgs),
    Tool {
        name: String,
        json: Option<String>,
    },
    Me {
        #[arg(long)]
        json: bool,
    },
    Doctor {
        #[arg(long)]
        json: bool,
    },
    ImportPythonHome {
        src: PathBuf,
        dst: PathBuf,
    },
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
}
#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(long)]
    pub run: sluice_model::ids::RunId,
    #[arg(long)]
    pub attempt: sluice_model::ids::AttemptId,
    #[arg(long)]
    pub socket: PathBuf,
}
#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    Hook {
        #[arg(long)]
        engine: Engine,
        #[arg(long)]
        event: String,
        #[arg(long)]
        run: Option<sluice_model::ids::RunId>,
    },
}
#[derive(Debug, Clone, clap::ValueEnum)]
pub enum Engine {
    Codex,
    Claude,
    Devin,
}
impl Mode {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Coordinator { .. } => "coordinator",
            Self::Serve { .. } => "serve",
            Self::Loop => "loop",
            Self::Guardian(_) => "guardian",
            Self::PayloadExec(_) => "payload-exec",
            Self::Tool { .. } => "tool",
            Self::Me { .. } => "me",
            Self::Doctor { .. } => "doctor",
            Self::ImportPythonHome { .. } => "import-python-home",
            Self::Agent { .. } => "agent hook",
        }
    }
}
