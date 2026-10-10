//! The public command line (SPEC §9): argument grammar plus the modes that only
//! exist for a person or an agent at a shell — `tool`, `next`, `watch`, `drain`,
//! `query`, `backup` and `docs`. Everything else registers from modes/.
use crate::modes::ModeFuture;
use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use sluice_model::{
    commands::{CommandReply, CommandRequest, Next, Query, RuntimeApi, Settles},
    error::PublicError,
    ids::{ProjectId, ProjectSelector, RecordSeq},
    rpc::{JsonValue, decode_json},
};
use sluice_runtime::{client::ensure_coordinator, registry::FnRegistry, watch};
use sluice_store::{ReadPool, records};
use std::{
    collections::BTreeMap,
    io::{Read, Write as _},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Parser)]
#[command(name = "sluice", version, about = "Run typed plans of fns.")]
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
        /// Default: config.json's http.port, else 3065.
        #[arg(long)]
        port: Option<u16>,
        /// Default: config.json's http.host, else 127.0.0.1.
        #[arg(long)]
        host: Option<String>,
    },
    Install {
        #[command(subcommand)]
        command: InstallCommand,
    },
    Loop {},
    Guardian(RunArgs),
    PayloadExec(RunArgs),
    /// `sluice tool [<name> [JSON] [--field VALUE]...]`; `sluice tool <name> --help` lists
    /// the tool's fields.
    #[command(disable_help_flag = true)]
    Tool {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    Next {
        #[arg(short = 'p', long = "project", value_name = "P")]
        projects: Vec<String>,
        #[arg(long, conflicts_with = "cursor")]
        since_seq: Option<i64>,
        #[arg(long, value_name = "FILE")]
        cursor: Option<PathBuf>,
        #[arg(long, default_value = "orchestrator", value_name = "NAME")]
        me: String,
        #[arg(long, default_value_t = 300, value_name = "S")]
        timeout: u64,
        #[arg(long, default_value_t = 20, value_name = "S")]
        settle: u64,
        #[arg(long, default_value_t = 120, value_name = "S")]
        settle_max: u64,
        #[arg(long)]
        all: bool,
        #[arg(long, default_value = "short", value_parser = ["short", "full", "none"])]
        settles: String,
        #[arg(long, default_value_t = 600, value_name = "N")]
        cut: usize,
        #[arg(long)]
        json: bool,
    },
    Watch {
        #[arg(short = 'p', long = "project", value_name = "P")]
        project: Option<String>,
        #[arg(long, value_delimiter = ',', value_name = "K1,K2")]
        kinds: Vec<String>,
        #[arg(long, value_delimiter = ',', value_name = "A,B")]
        threads: Vec<String>,
        #[arg(long, value_name = "N")]
        since_seq: Option<i64>,
        /// questions: hold messages that are not questions (say, reply) and print
        /// them with the next record that is not one.
        #[arg(long, default_value = "any", value_parser = ["any", "questions"])]
        wake: String,
    },
    Drain {
        #[arg(short = 'p', long = "project", value_name = "P")]
        projects: Vec<String>,
        #[arg(long)]
        no_wait: bool,
        #[arg(long)]
        release: bool,
    },
    Me {
        #[arg(long, value_name = "P")]
        project: Option<String>,
        #[arg(long, value_name = "S")]
        step: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Doctor {
        #[arg(long)]
        json: bool,
    },
    Query {
        sql: Option<String>,
        params: Vec<String>,
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
        #[arg(long)]
        table: bool,
        #[arg(long, default_value_t = 60, requires = "table", value_name = "N")]
        width: usize,
    },
    Backup {
        path: PathBuf,
        #[arg(long)]
        force: bool,
    },
    Docs {
        topic: Option<String>,
    },
    /// The MCP server over stdio (the tools `serve` offers at /mcp).
    Mcp {},
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
            Self::Install { .. } => "install",
            Self::Loop { .. } => "loop",
            Self::Guardian(_) => "guardian",
            Self::PayloadExec(_) => "payload-exec",
            Self::Tool { .. } => "tool",
            Self::Next { .. } => "next",
            Self::Watch { .. } => "watch",
            Self::Drain { .. } => "drain",
            Self::Me { .. } => "me",
            Self::Doctor { .. } => "doctor",
            Self::Query { .. } => "query",
            Self::Backup { .. } => "backup",
            Self::Docs { .. } => "docs",
            Self::Mcp { .. } => "mcp",
            Self::Agent { .. } => "agent hook",
        }
    }
}

/// Rust keeps SIGPIPE ignored, so a closed stdout panics; `| head` on the
/// tool listing or `watch` should exit quietly instead.
pub(crate) fn out(args: std::fmt::Arguments) {
    use std::io::Write;
    if let Err(error) = writeln!(std::io::stdout().lock(), "{args}")
        && error.kind() == std::io::ErrorKind::BrokenPipe
    {
        std::process::exit(0);
    }
}
macro_rules! println {
    ($($arg:tt)*) => {
        crate::cli::out(::std::format_args!($($arg)*))
    };
}

/// The public modes this module owns; modes/mod.rs registers one line each.
pub fn run(mode: Mode, home: PathBuf) -> ModeFuture {
    Box::pin(async move {
        match mode {
            Mode::Tool { args } => tool(&home, args).await,
            Mode::Next {
                projects,
                since_seq,
                cursor,
                me,
                timeout,
                settle,
                settle_max,
                all,
                settles,
                cut,
                json,
            } => {
                next(
                    &home,
                    NextFlags {
                        projects,
                        since_seq,
                        cursor,
                        me,
                        timeout,
                        settle,
                        settle_max,
                        all,
                        settles,
                        cut,
                        json,
                    },
                )
                .await
            }
            Mode::Watch {
                project,
                kinds,
                threads,
                since_seq,
                wake,
            } => watch_log(&home, project, &kinds, &threads, since_seq, wake).await,
            Mode::Drain {
                projects,
                no_wait,
                release,
            } => drain(&home, &projects, no_wait, release).await,
            Mode::Query {
                sql,
                params,
                limit,
                table,
                width,
            } => query(&home, sql, &params, limit, table, width).await,
            Mode::Backup { path, force } => backup(&home, &path, force),
            Mode::Docs { topic } => docs_mode(topic),
            other => Err(PublicError::not_implemented(other.name())),
        }
    })
}

pub fn bad_request(message: impl Into<String>) -> PublicError {
    PublicError::BadRequest {
        message: message.into(),
    }
}
fn storage(error: impl std::fmt::Display) -> PublicError {
    PublicError::Storage {
        message: error.to_string(),
    }
}
/// A first run creates SLUICE_HOME with the default config.json.
pub fn ensure_home(home: &Path) -> Result<(), PublicError> {
    std::fs::create_dir_all(home).map_err(storage)?;
    let config = home.join("config.json");
    if !config.exists() {
        let text = json!({
            "fn_dirs": [],
            "http": {"host": "127.0.0.1", "port": 3065},
            "log_max": 10000,
        });
        std::fs::write(&config, format!("{text}\n")).map_err(storage)?;
    }
    Ok(())
}
fn reads(home: &Path) -> Result<ReadPool, PublicError> {
    // A fresh home has no sluice.db and readers open read-only; a Writer
    // creates it. If the lock is held, a coordinator is already initializing.
    if !home.join("sluice.db").exists() {
        match sluice_store::Writer::open(home) {
            Ok(_) | Err(sluice_store::StoreError::WriterLocked) => {}
            Err(error) => return Err(error.into_public(true)),
        }
    }
    ReadPool::open(home, 2).map_err(|e| e.into_public(true))
}
/// ProjectSelector from argv: a name, or `id:<uuid>`.
fn selector(value: &str) -> Result<ProjectSelector, PublicError> {
    value
        .parse::<ProjectSelector>()
        .map_err(|e| bad_request(e.to_string()))
}
fn selectors(values: &[String]) -> Result<Vec<ProjectSelector>, PublicError> {
    values.iter().map(|v| selector(v)).collect()
}

// ---- sluice tool --------------------------------------------------------------

/// Tool names are the command enum's snake_case tags; the one-line text is what
/// `sluice tool` lists. Names absent here are still decoded, so a newer server's
/// commands keep working — but everything in CommandRequest must be named.
const TOOLS: &[(&str, &str)] = &[
    ("project_create", "create a project (plan starts empty)"),
    (
        "project_update",
        "rename, describe, icon, resources, pause or archive",
    ),
    (
        "project_delete",
        "delete a project (confirm_name, expected_settings_rev)",
    ),
    ("projects_list", "every project, oldest first"),
    (
        "board_set",
        "set or clear (program null) the project's board",
    ),
    ("board_get", "the project's board program and its rev"),
    (
        "board_doc_read",
        "the board's document, its rev and its lines numbered",
    ),
    ("board_doc_write", "replace the board's whole document"),
    (
        "board_doc_edit",
        "replace line ranges of the board's document at a rev",
    ),
    ("plan_get", "the plan document and its revision"),
    ("plan_history", "the plan's edit history"),
    (
        "plan_edit",
        "edit the plan with an atomic batch of typed operations",
    ),
    ("plan_read", "read selected steps, with paging"),
    ("step_get", "read one step and its references"),
    ("unit_get", "read one unit and its steps"),
    ("unit_update", "change a unit's member steps atomically"),
    ("unit_remove", "remove every member of a unit"),
    ("plan_prune", "drop settled units by tag/age"),
    ("plan_set_input", "set a plan input"),
    ("plan_view", "the plan as mermaid text (or html)"),
    ("step_add", "add a step to the plan"),
    ("step_update", "change a step's declaration"),
    ("step_remove", "remove steps from the plan"),
    ("step_pause", "pause or resume steps"),
    ("step_set_input", "set a step's extra inputs"),
    ("step_set_output", "set a step's outputs by hand"),
    ("step_retry", "retry failed, stale or skipped steps"),
    ("step_cancel", "cancel running or pending steps"),
    (
        "step_submit",
        "a run's outputs, submitted by the step itself",
    ),
    (
        "step_progress",
        "a running step's latest values, never final",
    ),
    ("step_settle", "settle a finishing step on its submission"),
    (
        "step_context",
        "where a step stands, for its agent (sluice me)",
    ),
    ("submission", "a run's submitted fields so far"),
    ("unit_add", "add a recipe unit's steps"),
    ("unit_tag", "tag or untag a unit"),
    ("edge_add", "add `after` ordering to a step"),
    ("edge_remove", "remove `after` ordering"),
    (
        "ask",
        "ask a step, the orchestrator or the owner a question",
    ),
    (
        "say",
        "tell a step, the orchestrator or the owner something",
    ),
    ("reply", "reply to a message (answers an open question)"),
    ("messages", "read a view of a project's messages"),
    ("mark_read", "record how far a thread's reader has read"),
    ("fn_call", "call a fn directly (or wait on it)"),
    ("call_status", "one call's status"),
    ("fn_list", "every fn a scope sees"),
    ("fn_get", "one fn's manifest"),
    ("fn_save", "write a Python fn (manifest + main.py)"),
    ("recipe_list", "the project's recipes"),
    ("log_read", "read the log after a seq"),
    ("log_wait", "wait for new log records"),
    ("step_wait", "wait until steps reach a status"),
    ("next", "the next records an orchestrator acts on"),
    ("query", "one read-only SELECT over the home"),
    ("backup", "an online copy of the database"),
    ("docs", "the agent docs (a topic, or the index)"),
    ("status", "a project's state at a glance"),
    ("verify", "every problem the home knows about"),
    ("drain", "pause projects for maintenance"),
    ("release", "undo a drain"),
    ("builtin", "run a builtin step's payload (internal)"),
];

/// Commands the CLI answers itself — read-only or one-process work the tool path
/// would resolve to the same local function anyway. Everything else goes through
/// the coordinator's dispatch, the same path the MCP server takes.
fn local_tool(request: &CommandRequest) -> bool {
    matches!(
        request,
        CommandRequest::Docs { .. }
            | CommandRequest::Query(_)
            | CommandRequest::Backup { .. }
            | CommandRequest::StepContext { .. }
            | CommandRequest::FnGet { .. }
            | CommandRequest::LogRead(_)
    )
}

fn cli_author() -> String {
    if let Ok(author) = std::env::var("SLUICE_AUTHOR")
        && !author.trim().is_empty()
    {
        return author;
    }
    match std::env::var("SLUICE_STEP") {
        Ok(step) if !step.trim().is_empty() => format!("step:{step}"),
        _ => "cli".into(),
    }
}
/// The author rule (§4): explicit arg, else SLUICE_AUTHOR, else `step:<id>` in a
/// run, else "cli". Only fills fields the command already carries.
fn fill_author(request: &mut CommandRequest, author: &str) {
    use sluice_model::commands::EditOptions;
    let fill = |slot: &mut Option<String>| {
        if slot.is_none() {
            *slot = Some(author.to_string());
        }
    };
    let fill_edit = |edit: &mut EditOptions| fill(&mut edit.author);
    match request {
        CommandRequest::ProjectUpdate(r) => fill(&mut r.author),
        CommandRequest::ProjectDelete(r) => fill(&mut r.author),
        CommandRequest::BoardSet(r) => fill(&mut r.author),
        CommandRequest::BoardDocWrite(r) => fill(&mut r.author),
        CommandRequest::BoardDocEdit(r) => fill(&mut r.author),
        CommandRequest::PlanEdit(r) => fill(&mut r.author),
        CommandRequest::UnitUpdate(r) => fill(&mut r.author),
        CommandRequest::UnitRemove(r) => fill(&mut r.author),
        CommandRequest::StepAdd(r) => fill_edit(&mut r.edit),
        CommandRequest::UnitAdd(r) => fill_edit(&mut r.edit),
        CommandRequest::StepUpdate(r) => fill_edit(&mut r.edit),
        CommandRequest::StepRemove(r) => fill_edit(&mut r.edit),
        CommandRequest::StepPause(r) => fill_edit(&mut r.edit),
        CommandRequest::UnitTag(r) => fill_edit(&mut r.edit),
        CommandRequest::PlanPrune(r) => fill_edit(&mut r.edit),
        CommandRequest::PlanSetInput(r) => fill_edit(&mut r.edit),
        CommandRequest::StepSetInput(r) => fill_edit(&mut r.edit),
        CommandRequest::EdgeAdd(r) | CommandRequest::EdgeRemove(r) => fill_edit(&mut r.edit),
        CommandRequest::StepSetOutput(r) => fill(&mut r.author),
        CommandRequest::StepRetry(r) => fill(&mut r.author),
        CommandRequest::StepCancel(r) => fill(&mut r.author),
        CommandRequest::StepSubmit(r) => fill(&mut r.author),
        CommandRequest::StepSettle(r) => fill(&mut r.author),
        CommandRequest::FnCall(r) => fill(&mut r.author),
        CommandRequest::Drain { author: a, .. } | CommandRequest::Release { author: a } => fill(a),
        CommandRequest::ProjectCreate { author: a, .. } => fill(a),
        _ => {}
    }
}

/// `sluice tool` takes the MCP tools' flat signatures (SPEC §8): the optional
/// fields stay out, and a few are renamed or nested. The wire structs want
/// `edit`, `selection` and `read` objects, `*_seconds` names and no missing
/// required fields — this rewrites the argv object into that shape before it
/// decodes. Keys a command does not know are left for `deny_unknown_fields`
/// to refuse.
/// `sluice tool unit_add`'s shorthands, before the shared plan-tool decoder: the unit may be
/// given as `params.unit`, and a single step id for a recipe step suffix in `after` is a
/// one-item list.
fn unit_add_shorthands(name: &str, args: &mut serde_json::Map<String, Value>) {
    if name != "unit_add" {
        return;
    }
    if let Some(Value::Object(after)) = args.get_mut("after") {
        for ids in after.values_mut() {
            if let Value::String(_) = ids {
                *ids = Value::Array(vec![ids.take()]);
            }
        }
    }
    if let Some(unit) = args.get("params").and_then(|p| p.get("unit")).cloned() {
        args.entry("unit").or_insert(unit);
    }
}

async fn normalize_args(
    home: &Path,
    name: &str,
    args: &mut serde_json::Map<String, Value>,
) -> Result<(), PublicError> {
    /// `after`, `steps`, `tags` or `projects` given as one value mean a
    /// one-element list, as the Python tools accepted.
    fn listify(args: &mut serde_json::Map<String, Value>, key: &str) {
        if let Some(value) = args.get_mut(key)
            && !matches!(value, Value::Array(_) | Value::Null)
        {
            *value = Value::Array(vec![value.take()]);
        }
    }
    /// steps/tags become the wire's `selection` object.
    fn selection(args: &mut serde_json::Map<String, Value>) {
        listify(args, "steps");
        listify(args, "tags");
        let mut selection = serde_json::Map::new();
        for key in ["steps", "tags"] {
            if let Some(value) = args.remove(key) {
                selection.insert(key.into(), value);
            }
        }
        args.insert("selection".into(), Value::Object(selection));
    }
    /// A command whose `project` is a bare ProjectId (not a selector) resolves
    /// a name or `id:` here.
    async fn project_id_arg(
        home: &Path,
        args: &mut serde_json::Map<String, Value>,
    ) -> Result<(), PublicError> {
        let Some(value) = args.get("project").and_then(Value::as_str) else {
            return Ok(());
        };
        let selector = selector(value)?;
        let id = project_id(home, &selector).await?;
        args.insert("project".into(), Value::String(id.to_string()));
        Ok(())
    }
    // MCP's public names for wire fields (`rev` for an edit's `expected`, `wait`, `older_than`,
    // ...) are taken here too, so a call copied from the tool descriptions works.
    for (public, wire) in sluice_web::mcp::renames(name) {
        if !args.contains_key(*wire)
            && let Some(value) = args.remove(*public)
        {
            args.insert((*wire).into(), value);
        }
    }
    let seconds = |args: &mut serde_json::Map<String, Value>, from: &str, to: &str| {
        if let Some(value) = args.remove(from) {
            args.insert(to.into(), value);
        }
    };
    // `project`/`projects` on argv are name or `id:` strings; the wire's
    // ProjectSelector is a tagged object. The commands taking a bare id
    // resolve through `project_id_arg` instead.
    let to_selector = |value: &mut Value| -> Result<(), PublicError> {
        if let Value::String(text) = value {
            *value = serde_json::to_value(selector(text)?).map_err(storage)?;
        }
        Ok(())
    };
    if !matches!(name, "step_submit" | "mark_read")
        && let Some(project) = args.get_mut("project")
    {
        to_selector(project)?;
    }
    if args.contains_key("projects") {
        listify(args, "projects");
        if let Some(Value::Array(items)) = args.get_mut("projects") {
            for item in items.iter_mut() {
                to_selector(item)?;
            }
        }
    }
    match name {
        "project_create" => {
            args.entry("description")
                .or_insert(Value::String(String::new()));
            args.entry("resources")
                .or_insert(Value::Object(Default::default()));
        }
        "project_update" => {
            if let Some(value) = args.remove("name") {
                args.entry("project").or_insert(value);
            }
            if let Some(hours) = args.remove("prune_done_after_hours") {
                let seconds = hours
                    .as_f64()
                    .map(|h| json!((h * 3600.0).round().max(0.0) as u64));
                args.insert("prune_done_after".into(), seconds.unwrap_or(Value::Null));
            }
            if let Some(project) = args.get_mut("project") {
                to_selector(project)?;
            }
        }
        "project_delete" => {
            let name = args.remove("name");
            if let Some(value) = name.clone() {
                args.entry("project").or_insert(value);
            }
            if let Some(Value::String(name)) = name
                && !name.starts_with("id:")
            {
                args.entry("confirm_name").or_insert(Value::String(name));
            }
            if args.get("expected_settings_rev").is_none()
                && let Some(selector) = args.get("project").and_then(Value::as_str)
            {
                let id = project_id(home, &self::selector(selector)?).await?;
                let rev: i64 = reads(home)?
                    .snapshot(move |sql| {
                        Ok(sql.query_row(
                            "SELECT settings_rev FROM projects WHERE project_id=?1",
                            [id.to_string()],
                            |r| r.get(0),
                        )?)
                    })
                    .await
                    .map_err(|e| e.into_public(true))?;
                args.insert("expected_settings_rev".into(), json!(rev));
            }
            if let Some(project) = args.get_mut("project") {
                to_selector(project)?;
            }
        }
        "board_set" if !args.contains_key("program") => {
            return Err(bad_request(
                "board_set needs program: the board program, or null to clear the board",
            ));
        }
        "board_doc_write" if !args.contains_key("markdown") => {
            return Err(bad_request(
                "board_doc_write needs markdown: the whole document (--markdown-file reads it from a file)",
            ));
        }
        "step_cancel" | "step_retry" | "status" => {
            selection(args);
            if name == "status" {
                listify(args, "state");
            }
            if name == "step_cancel" {
                args.entry("reason").or_insert(Value::String(String::new()));
            }
        }
        "step_set_output" => {
            args.entry("force").or_insert(Value::Bool(false));
            args.entry("reason").or_insert(Value::String(String::new()));
        }
        "step_submit" => project_id_arg(home, args).await?,
        // The owner is the dashboard's alone.
        "ask" | "say" | "reply" if args.contains_key("owner") => {
            return Err(bad_request(format!(
                "{name} takes no argument 'owner': the dashboard speaks as the owner"
            )));
        }
        "mark_read" => project_id_arg(home, args).await?,
        "fn_save" => {
            if let Some(value) = args.remove("fn") {
                args.entry("manifest").or_insert(value);
            }
        }
        "fn_call" => {
            args.entry("inputs")
                .or_insert(Value::Object(Default::default()));
            args.entry("direct").or_insert(Value::Bool(false));
            seconds(args, "wait", "wait_seconds");
        }
        "log_read" => {
            args.entry("limit").or_insert(json!(200));
        }
        "log_wait" => {
            args.entry("limit").or_insert(json!(200));
            let mut read = serde_json::Map::new();
            for key in [
                "project",
                "since_seq",
                "kinds",
                "threads",
                "statuses",
                "recipients",
                "limit",
            ] {
                if let Some(value) = args.remove(key) {
                    read.insert(key.into(), value);
                }
            }
            args.insert("read".into(), Value::Object(read));
            if let Some(timeout) = args.remove("timeout") {
                args.insert("timeout_seconds".into(), timeout);
            } else {
                args.entry("timeout_seconds").or_insert(json!(300));
            }
            if let Some(value) = args.get_mut("timeout_seconds")
                && value.as_u64().is_some_and(|v| v > 3600)
            {
                *value = json!(3600);
            }
            // `wake` arrives renamed: "any" or "questions" (a boolean passes as is).
            let wake = args.remove("questions_only").unwrap_or(json!("any"));
            let questions = match wake {
                Value::Bool(questions) => questions,
                Value::String(wake) if wake == "any" => false,
                Value::String(wake) if wake == "questions" => true,
                _ => return Err(bad_request("wake must be any or questions")),
            };
            args.insert("questions_only".into(), Value::Bool(questions));
        }
        "step_wait" => {
            selection(args);
            seconds(args, "timeout", "timeout_seconds");
            let timeout = args.entry("timeout_seconds").or_insert(json!(300));
            if timeout.as_u64().is_some_and(|v| v > 3600) {
                *timeout = json!(3600);
            }
        }
        "next" => {
            listify(args, "projects");
            args.entry("me").or_insert(json!("orchestrator"));
            args.entry("all").or_insert(Value::Bool(false));
            args.entry("settles").or_insert(json!("short"));
            for (from, to, default) in [
                ("timeout", "timeout_seconds", 300),
                ("settle", "settle_seconds", 20),
                ("settle_max", "settle_max_seconds", 120),
            ] {
                if let Some(value) = args.remove(from) {
                    args.insert(to.into(), value);
                } else {
                    args.entry(to).or_insert(json!(default));
                }
            }
            for key in ["timeout_seconds", "settle_max_seconds"] {
                if let Some(value) = args.get_mut(key)
                    && value.as_u64().is_some_and(|v| v > 3600)
                {
                    *value = json!(3600);
                }
            }
        }
        "drain" => listify(args, "projects"),
        "query" => {
            args.entry("params").or_insert(Value::Array(vec![]));
            args.entry("limit").or_insert(json!(200));
        }
        "plan_view" => {
            args.entry("format").or_insert(json!("mermaid"));
        }
        _ => {}
    }
    Ok(())
}

const TOOL_USAGE: &str = "\
usage: sluice tool [<name> [JSON] [--field VALUE]...]

Without a name, lists the tools; `sluice tool <name> --help` lists one tool's fields.
";
/// How `sluice tool` reads a tool's arguments (SPEC §14), printed with every usage.
const TOOL_ARGUMENTS: &str = "\
A tool's arguments are one JSON object (the first argument; `-` reads it from stdin)
and/or one flag per field: --field VALUE or --field=VALUE, `-` or `_` alike, a
boolean's flag alone meaning true. A value is the text as given for a field that takes
only strings; any other value is parsed as JSON when it parses, else taken as text (as
a one-item list for a list of strings: --statuses failed).
--<field>-file PATH reads the value from a file (`-` for stdin). Flags win over the
JSON object. In a run, project, run and (step_submit, step_progress, step_context) step
default to the run's own.
";

/// The names only `sluice tool` takes, rewritten by `normalize_args`, with their schema.
fn cli_aliases(name: &str) -> Vec<(&'static str, Value)> {
    let text = |doc: &str| json!({"type": "string", "description": doc});
    match name {
        "project_update" => vec![
            ("name", text("the project's current name (as project)")),
            (
                "prune_done_after_hours",
                json!({"type": ["number", "null"], "description": "prune_done_after, in hours (null turns it off)"}),
            ),
        ],
        "project_delete" => vec![(
            "name",
            text(
                "the project's current name: fills project, confirm_name and expected_settings_rev",
            ),
        )],
        "plan_prune" => vec![(
            "older_than_hours",
            json!({"type": "number", "description": "older_than, in hours"}),
        )],
        "step_set_input" => vec![
            ("step", text("one step to select (as steps)")),
            ("input", text("one input to bind, to value (as inputs)")),
            ("value", json!({"description": "the value of input"})),
        ],
        _ => vec![],
    }
}

/// Every field `sluice tool <name>` takes, each with its JSON schema: the command's public
/// fields (MCP's, from the command schema), the few names only the CLI takes, and the wire
/// fields flat (`expected`, `timeout_seconds`, ...), which it has always taken too.
struct CliFields {
    /// `{"properties", "$defs"}`, the shape `coerce_integers` reads.
    schema: Value,
    /// The names it offers (suggested, and listed by --help): public, then CLI-only.
    public: Vec<String>,
    required: Vec<String>,
}
impl CliFields {
    fn of(name: &str) -> Self {
        let mut properties = serde_json::Map::new();
        let mut defs = sluice_web::mcp::command_schema()["$defs"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let mut public = Vec::new();
        let mut required = Vec::new();
        if let Some(schema) = sluice_web::mcp::tool_schema(name) {
            for (key, value) in schema["properties"].as_object().into_iter().flatten() {
                properties.insert(key.clone(), value.clone());
                public.push(key.clone());
            }
            for (key, value) in schema["$defs"].as_object().into_iter().flatten() {
                defs.insert(key.clone(), value.clone());
            }
            required = schema["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
        }
        for (key, value) in cli_aliases(name) {
            properties.insert(key.into(), value);
            public.push(key.into());
        }
        for (key, value) in sluice_web::mcp::wire_fields(name) {
            properties.entry(key).or_insert(value);
        }
        Self {
            schema: json!({"properties": properties, "$defs": defs}),
            public,
            required,
        }
    }
    fn field(&self, key: &str) -> Option<&Value> {
        self.schema["properties"].get(key)
    }
    fn accepts(&self, key: &str) -> sluice_web::tool_args::Accepts {
        sluice_web::tool_args::accepts(&self.schema["properties"][key], &self.schema["$defs"])
    }
    fn unknown(&self, tool: &str, key: &str) -> PublicError {
        sluice_web::tool_args::unknown_argument(tool, key, self.public.iter().map(String::as_str))
    }
    /// `sluice tool <name> --help`: the description, then each field with its type,
    /// whether it is required or its default, and what it is.
    fn help(&self, tool: &str) -> String {
        let description = sluice_web::mcp::tools()
            .iter()
            .find(|t| t.name == tool)
            .and_then(|t| t.description.as_deref().map(str::to_owned))
            .or_else(|| {
                TOOLS
                    .iter()
                    .find(|(name, _)| *name == tool)
                    .map(|(_, doc)| format!("{doc}."))
            })
            .unwrap_or_default();
        let (text, described) = sluice_web::tool_args::described_args(&description);
        let mut out =
            format!("usage: sluice tool {tool} [JSON] [--field VALUE]...\n\n{text}\n\nfields:\n");
        let mut names: Vec<&String> = self.public.iter().collect();
        names.sort_by_key(|name| !self.required.contains(name));
        for name in names {
            let schema = &self.schema["properties"][name.as_str()];
            let kind = sluice_web::tool_args::type_text(schema, &self.schema["$defs"]);
            let mut notes = Vec::new();
            if self.required.contains(name) {
                notes.push("required".to_owned());
            } else if let Some(default) = schema.get("default") {
                notes.push(format!("default {default}"));
            }
            let in_run = match name.as_str() {
                "project" => Some("id:$SLUICE_PROJECT_ID"),
                "run" => Some("$SLUICE_RUN_ID"),
                "step" if matches!(tool, "step_submit" | "step_progress" | "step_context") => {
                    Some("$SLUICE_STEP")
                }
                _ => None,
            };
            if let Some(value) = in_run {
                notes.push(format!("in a run: {value}"));
            }
            let doc = described
                .iter()
                .find(|(field, _)| field == name)
                .map(|(_, doc)| doc.clone())
                .or_else(|| schema["description"].as_str().map(str::to_owned))
                .unwrap_or_default();
            let notes = if notes.is_empty() {
                String::new()
            } else {
                format!("  ({})", notes.join("; "))
            };
            out.push_str(&format!("  --{} <{kind}>{notes}\n", name.replace('_', "-")));
            if !doc.is_empty() {
                out.push_str(&format!("      {doc}\n"));
            }
        }
        out.push('\n');
        out.push_str(TOOL_ARGUMENTS);
        out
    }
}

/// A flag's value: the text itself for a field that takes only strings (`--to owner`,
/// `--body 42`); else its JSON when it parses (`--to-message 24771`, `--outputs '{..}'`);
/// else, for a list of strings that takes no string, a list of the text (`--kinds message`,
/// `--statuses failed`); else the text. A field that takes no string or scalar at all must
/// be JSON.
fn flag_value(
    flag: &str,
    accepts: sluice_web::tool_args::Accepts,
    items: Option<sluice_web::tool_args::Accepts>,
    text: String,
) -> Result<Value, PublicError> {
    if accepts.only_string() {
        return Ok(Value::String(text));
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => Ok(value),
        Err(_)
            if accepts.array
                && !accepts.string
                && !accepts.any
                && items.is_some_and(|items| items.string || items.any) =>
        {
            Ok(Value::Array(vec![Value::String(text)]))
        }
        Err(error)
            if !(accepts.any
                || accepts.string
                || accepts.integer
                || accepts.number
                || accepts.boolean) =>
        {
            Err(bad_request(format!("--{flag}: not JSON: {error}")))
        }
        Err(_) => Ok(Value::String(text)),
    }
}

/// Whether stdin is a regular file (`< args.json`), as opposed to a pipe or a terminal.
fn stdin_is_file() -> bool {
    use std::os::fd::AsFd;
    std::io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .ok()
        .and_then(|fd| std::fs::File::from(fd).metadata().ok())
        .is_some_and(|meta| meta.is_file())
}

fn read_stdin() -> Result<String, PublicError> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(storage)?;
    Ok(text)
}

/// The arguments of `sluice tool <name> [JSON] [--field VALUE]...`: the JSON object (the
/// first argument, `-` reading it from stdin; with no argument and no flag, a file stdin is
/// redirected from), then each flag over it. Every field must be one the tool takes. None
/// asks for its help.
fn parse_tool_args(
    tool: &str,
    fields: &CliFields,
    rest: Vec<String>,
) -> Result<Option<serde_json::Map<String, Value>>, PublicError> {
    let mut rest = rest.into_iter().peekable();
    let json_text = rest.next_if(|first| !first.starts_with("--") && first != "-h");
    let mut flags: Vec<(String, Value)> = Vec::new();
    let mut stdin_read = false;
    while let Some(arg) = rest.next() {
        if arg == "--help" || arg == "-h" {
            return Ok(None);
        }
        let Some(flag) = arg.strip_prefix("--").filter(|flag| !flag.is_empty()) else {
            return Err(bad_request(format!(
                "unexpected argument {arg:?}: give one JSON object first, then --field VALUE flags"
            )));
        };
        let (flag, inline) = match flag.split_once('=') {
            Some((flag, value)) => (flag.to_owned(), Some(value.to_owned())),
            None => (flag.to_owned(), None),
        };
        let key = flag.replace('-', "_");
        let (field, from_file) = if fields.field(&key).is_some() {
            (key, false)
        } else if let Some(base) = key.strip_suffix("_file")
            && fields.field(base).is_some()
        {
            (base.to_owned(), true)
        } else {
            return Err(fields.unknown(tool, key.strip_suffix("_file").unwrap_or(&key)));
        };
        let accepts = fields.accepts(&field);
        let items = sluice_web::tool_args::item_accepts(
            &fields.schema["properties"][field.as_str()],
            &fields.schema["$defs"],
        );
        let text = inline.or_else(|| rest.next_if(|next| !next.starts_with("--")));
        let value = match (text, from_file) {
            (Some(path), true) => {
                let content = if path == "-" {
                    if stdin_read || json_text.as_deref() == Some("-") {
                        return Err(bad_request(
                            "stdin is read once: by `-` or by one --<field>-file -",
                        ));
                    }
                    stdin_read = true;
                    read_stdin()?
                } else {
                    std::fs::read_to_string(&path)
                        .map_err(|e| bad_request(format!("--{flag}: {path}: {e}")))?
                };
                flag_value(&flag, accepts, items, content)?
            }
            (Some(text), false) => flag_value(&flag, accepts, items, text)?,
            (None, false) if accepts.boolean => Value::Bool(true),
            (None, _) => return Err(bad_request(format!("--{flag} needs a value"))),
        };
        flags.push((field, value));
    }
    let text = match json_text {
        Some(text) if text == "-" => {
            if stdin_read {
                return Err(bad_request(
                    "stdin is read once: by `-` or by one --<field>-file -",
                ));
            }
            read_stdin()?
        }
        Some(text) => text,
        // A redirected file is read as the object; an open pipe or a terminal never is, so a
        // call with no arguments never waits on a stdin nobody writes to.
        None if flags.is_empty() && stdin_is_file() => read_stdin()?,
        None => String::new(),
    };
    let text = text.trim();
    let args_value: Value = serde_json::from_str(if text.is_empty() { "{}" } else { text })
        .map_err(|e| bad_request(format!("args: not JSON: {e}")))?;
    let mut args = match args_value {
        Value::Object(map) => map,
        _ => return Err(bad_request("args: expected a JSON object")),
    };
    for (field, value) in flags {
        args.insert(field, value);
    }
    if let Some(key) = args.keys().find(|key| fields.field(key).is_none()) {
        return Err(fields.unknown(tool, key));
    }
    Ok(Some(args))
}

/// In a run (SLUICE_RUN_ID set), a call that leaves out `project` or `run` gets the run's
/// own, `id:$SLUICE_PROJECT_ID` and `$SLUICE_RUN_ID`, on every tool that takes it, and
/// `step_submit`, `step_progress` and `step_context` get `$SLUICE_STEP` as `step`. A field the call gives,
/// even as null, is kept, and project_update or project_delete given `name` names its
/// project that way.
fn run_defaults(tool: &str, fields: &CliFields, args: &mut serde_json::Map<String, Value>) {
    let var = |key: &str| {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let Some(run) = var("SLUICE_RUN_ID") else {
        return;
    };
    if fields.field("run").is_some() {
        args.entry("run").or_insert(Value::String(run));
    }
    let named = matches!(tool, "project_update" | "project_delete") && args.contains_key("name");
    if fields.field("project").is_some()
        && !named
        && let Some(project) = var("SLUICE_PROJECT_ID")
    {
        args.entry("project")
            .or_insert(Value::String(sluice_agents::prompt::project_selector(
                &project,
            )));
    }
    if matches!(tool, "step_submit" | "step_progress" | "step_context")
        && let Some(step) = var("SLUICE_STEP")
    {
        args.entry("step").or_insert(Value::String(step));
    }
}

async fn tool(home: &Path, argv: Vec<String>) -> Result<(), PublicError> {
    ensure_home(home)?;
    let mut argv = argv.into_iter();
    let name = match argv.next() {
        None => None,
        Some(flag) if flag == "--help" || flag == "-h" => {
            print!("{TOOL_USAGE}\n{TOOL_ARGUMENTS}\n");
            None
        }
        Some(name) => Some(name),
    };
    let Some(name) = name else {
        let mut tools: Vec<(&str, &str)> = TOOLS.to_vec();
        tools.sort_unstable();
        tools.dedup();
        for (name, doc) in tools {
            println!("{name:<20} {doc}");
        }
        return Ok(());
    };
    let rest: Vec<String> = argv.collect();
    if name == "rpc" {
        return rpc(home, rest.into_iter().next()).await;
    }
    // message_post is retired and unlisted; runs started on an older release still
    // call it, and the coordinator translates it for them.
    if !TOOLS.iter().any(|(tool, _)| *tool == name) && name != "message_post" {
        return Err(sluice_web::tool_args::unknown_tool(
            &name,
            TOOLS.iter().map(|(tool, _)| *tool),
            " (sluice tool lists them)",
        ));
    }
    let fields = CliFields::of(&name);
    let Some(mut args) = parse_tool_args(&name, &fields, rest)? else {
        print!("{}", fields.help(&name));
        return Ok(());
    };
    sluice_web::tool_args::coerce_integers(&fields.schema, &mut args)?;
    run_defaults(&name, &fields, &mut args);
    let request = if crate::plan_tools::handles(&name) {
        unit_add_shorthands(&name, &mut args);
        crate::plan_tools::decode(&name, args, &cli_author())?
    } else {
        normalize_args(home, &name, &mut args).await?;
        let body = if name == "projects_list" {
            json!({"command": name})
        } else {
            json!({"command": name, "args": Value::Object(args)})
        };
        let mut request: CommandRequest = decode_json(body.to_string().as_bytes())
            .map_err(|e| bad_request(format!("args: {e}")))?;
        fill_author(&mut request, &cli_author());
        request
    };
    if !local_tool(&request) {
        let program = std::env::current_exe().map_err(storage)?;
        let client = ensure_coordinator(home, &program).await?;
        let reply = client.command(request).await?;
        return print_reply(&name, reply);
    }
    let data = match request {
        CommandRequest::Docs { topic } => {
            return docs_page(topic.as_deref()).map(|text| print!("{text}"));
        }
        CommandRequest::Query(Query { sql, params, limit }) => {
            let params: Vec<Value> = params.iter().map(|p| p.as_value().clone()).collect();
            let result = run_query(home, &sql, &params, limit as usize)?;
            println!("{}", String::from_utf8_lossy(result.encoded()));
            return Ok(());
        }
        CommandRequest::Backup { destination } => {
            return backup(home, Path::new(&destination), false);
        }
        CommandRequest::StepContext { project, step } => {
            let context = crate::me::context(home, &project, &step, None).await?;
            serde_json::to_value(context).map_err(storage)?
        }
        CommandRequest::FnGet { name, project } => fn_get(home, &name, project.as_ref()).await?,
        CommandRequest::LogRead(read) => {
            serde_json::to_value(log_read_page(home, &read).await?).map_err(storage)?
        }
        _ => unreachable!("local_tool covered"),
    };
    print_reply(
        &name,
        CommandReply::Data(JsonValue::try_from(data).map_err(storage)?),
    )
}

/// `sluice tool rpc '<request>'`: one raw coordinator request, its reply printed
/// unshaped. Host gates and diagnostics drive the coordinator through it with no
/// argument normalisation or author defaulting.
async fn rpc(home: &Path, request: Option<String>) -> Result<(), PublicError> {
    let text = match request {
        Some(text) => text,
        None => read_stdin()?,
    };
    let request: CommandRequest =
        decode_json(text.trim().as_bytes()).map_err(|e| bad_request(format!("request: {e}")))?;
    let program = std::env::current_exe().map_err(storage)?;
    let client = ensure_coordinator(home, &program).await?;
    let reply = client.command(request).await?;
    println!("{}", serde_json::to_string(&reply).map_err(storage)?);
    Ok(())
}

/// A tool result prints pretty JSON (text for docs/plan_view), the value MCP returns: the
/// reply's data, `{"ok": true}` for an acknowledgement. A result object with `"ok": false`
/// still prints, but exits 1.
fn print_reply(name: &str, reply: CommandReply) -> Result<(), PublicError> {
    let data = sluice_web::mcp::reply_value(reply)?;
    if matches!(name, "docs" | "plan_view") && data.is_string() {
        println!("{}", data.as_str().unwrap_or_default());
        return Ok(());
    }
    println!("{}", serde_json::to_string_pretty(&data).map_err(storage)?);
    if data.get("ok").is_some_and(|ok| ok == false) {
        std::process::exit(1);
    }
    Ok(())
}

// ---- local tool bodies --------------------------------------------------------

fn run_query(
    home: &Path,
    sql: &str,
    params: &[Value],
    limit: usize,
) -> Result<sluice_store::query::QueryResult, PublicError> {
    let home = home.to_path_buf();
    let sql = sql.to_string();
    let params = params.to_vec();
    let limit = (limit != 0).then_some(limit);
    reads(&home)?;
    let params: Vec<rusqlite::types::Value> = params
        .iter()
        .map(|param| match param {
            Value::Null => Ok(rusqlite::types::Value::Null),
            Value::Bool(b) => Ok(rusqlite::types::Value::Integer(*b as i64)),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(rusqlite::types::Value::Integer(i))
                } else if let Some(u) = n.as_u64() {
                    Ok(rusqlite::types::Value::Integer(
                        i64::try_from(u).map_err(|_| bad_request("param: integer out of range"))?,
                    ))
                } else {
                    Ok(rusqlite::types::Value::Real(n.as_f64().unwrap_or_default()))
                }
            }
            Value::String(s) => Ok(rusqlite::types::Value::Text(s.clone())),
            _ => Err(bad_request("param: arrays and objects cannot bind")),
        })
        .collect::<Result<_, PublicError>>()?;
    tokio::task::block_in_place(|| sluice_store::query::query(&home, &sql, Some(&params), limit))
}
/// An `id:` selector is the id itself and never reads the store: a run's task names its project
/// that way, so its submit does not depend on this binary reading the home's schema.
async fn project_id(home: &Path, selector: &ProjectSelector) -> Result<ProjectId, PublicError> {
    if let ProjectSelector::Id(id) = selector {
        return Ok(*id);
    }
    let selector = selector.clone();
    reads(home)?
        .snapshot(move |sql| sluice_store::messages::resolve_project(sql, &selector))
        .await
        .map_err(|e| e.into_public(true))
}
async fn fn_get(
    home: &Path,
    name: &str,
    selector: Option<&ProjectSelector>,
) -> Result<Value, PublicError> {
    let registry = FnRegistry::configured(home)?;
    let project = match selector {
        Some(selector) => Some(project_id(home, selector).await?),
        None => None,
    };
    let function = registry.get(name, project)?;
    Ok(function.detail())
}
/// `log_read`: one page of matching records (the last `limit` without
/// --since-seq), straight from the store like `sluice watch` reads it.
async fn log_read_page(
    home: &Path,
    read: &sluice_model::commands::LogRead,
) -> Result<sluice_model::commands::RecordPage, PublicError> {
    let filter = records::RecordFilter::from(read);
    let read = read.clone();
    reads(home)?
        .snapshot(move |sql| {
            let project = read
                .project
                .as_ref()
                .map(|s| sluice_store::messages::resolve_project(sql, s))
                .transpose()?;
            records::read_records(sql, project, &filter)?.into_page()
        })
        .await
        .map_err(|e| e.into_public(true))
}

// ---- sluice docs --------------------------------------------------------------

/// The pages the MCP `docs` tool serves (docs/agent), compiled into the binary.
const DOCS: &[(&str, &str)] = sluice_runtime::docs::PAGES;

/// `sluice docs <topic>` prints a page; no topic prints the index (each topic's
/// first heading), the same index the docs tool returns.
pub fn docs_page(topic: Option<&str>) -> Result<String, PublicError> {
    match topic {
        None => {
            let mut out = String::new();
            for (name, page) in DOCS {
                let heading = page
                    .lines()
                    .find(|line| line.starts_with('#'))
                    .unwrap_or("")
                    .trim_start_matches('#')
                    .trim();
                out.push_str(&format!("{name:<14} {heading}\n"));
            }
            Ok(out)
        }
        Some(topic) => DOCS
            .iter()
            .find(|(name, _)| *name == topic)
            .map(|(_, page)| format!("{}\n", page.trim_end()))
            .ok_or_else(|| {
                let names: Vec<&str> = DOCS.iter().map(|(n, _)| *n).collect();
                bad_request(format!(
                    "no docs topic {topic:?} (topics: {})",
                    names.join(", ")
                ))
            }),
    }
}
fn docs_mode(topic: Option<String>) -> Result<(), PublicError> {
    let text = docs_page(topic.as_deref())?;
    print!("{text}");
    Ok(())
}

// ---- sluice next ---------------------------------------------------------------

struct NextFlags {
    projects: Vec<String>,
    since_seq: Option<i64>,
    cursor: Option<PathBuf>,
    me: String,
    timeout: u64,
    settle: u64,
    settle_max: u64,
    all: bool,
    settles: String,
    cut: usize,
    json: bool,
}

async fn next(home: &Path, flags: NextFlags) -> Result<(), PublicError> {
    ensure_home(home)?;
    if flags.cut == 0 {
        return Err(bad_request(
            "--cut: expected a positive number of characters",
        ));
    }
    let projects = selectors(&flags.projects)?;
    let since = match (&flags.cursor, flags.since_seq) {
        (Some(path), _) => watch::load_cursor(path)?,
        (None, seq) => seq.map(RecordSeq),
    };
    if since.is_some_and(|seq| seq.0 < 0) {
        return Err(bad_request("--since-seq: expected a nonnegative sequence"));
    }
    // "From now" is the top of the selected projects' logs; the command carries
    // an explicit seq, so the CLI reads the same bounds the tool would start at.
    let reads = reads(home)?;
    let selected = {
        let wanted = projects.clone();
        reads
            .snapshot(move |sql| {
                let ids: Vec<ProjectId> = if wanted.is_empty() {
                    let mut stmt = sql.prepare(
                        "SELECT project_id FROM projects WHERE deleted_at IS NULL AND archived=0 ORDER BY name",
                    )?;
                    let rows = stmt
                        .query_map([], |r| r.get::<_, String>(0))?
                        .collect::<Result<Vec<_>, _>>()?;
                    rows.iter()
                        .map(|id| {
                            id.parse::<ProjectId>()
                                .map_err(|e| {
                                    sluice_store::StoreError::from(bad_request(e.to_string()))
                                })
                        })
                        .collect::<Result<Vec<_>, _>>()?
                } else {
                    wanted
                        .iter()
                        .map(|s| sluice_store::messages::resolve_project(sql, s))
                        .collect::<Result<Vec<_>, _>>()?
                };
                let mut top = RecordSeq(0);
                for id in &ids {
                    top = RecordSeq(top.0.max(records::bounds(sql, Some(*id))?.1.0));
                }
                Ok((ids, top))
            })
            .await
            .map_err(|e| e.into_public(true))?
    };
    let (ids, top) = selected;
    if ids.is_empty() && projects.is_empty() {
        return Err(bad_request(
            "no projects (none not archived); pass -p to name them",
        ));
    }
    let since_seq = since.unwrap_or(top);
    let settles = match flags.settles.as_str() {
        "short" => Settles::Short,
        "full" => Settles::Full,
        "none" => Settles::None,
        _ => unreachable!("clap restricted"),
    };
    let request = CommandRequest::Next(Next {
        projects,
        since_seq,
        me: flags.me,
        timeout_seconds: flags.timeout.min(3600),
        all: flags.all,
        settle_seconds: flags.settle,
        settle_max_seconds: flags.settle_max.min(3600),
        settles: settles.clone(),
    });
    let program = std::env::current_exe().map_err(storage)?;
    let client = ensure_coordinator(home, &program).await?;
    let reply = client.command(request).await?;
    let CommandReply::Next(result) = reply else {
        return Err(storage("unexpected reply to next"));
    };
    if let Some(path) = &flags.cursor {
        watch::save_cursor(path, result.last_seq)?;
    }
    let out = watch::render(&result, settles, flags.cut, flags.json)?;
    print!("{out}");
    std::io::stdout().flush().map_err(storage)?;
    Ok(())
}

// ---- sluice watch ---------------------------------------------------------------

/// Follow the log: one JSON line per matching record, starting at the end unless
/// --since-seq says where. Reads come straight from the store — a watcher must
/// not need the coordinator (or take its writer lock) to observe. With
/// --wake questions, messages that are not questions (say, reply) are held and print
/// just before the next record that is not one.
async fn watch_log(
    home: &Path,
    project: Option<String>,
    kinds: &[String],
    threads: &[String],
    since_seq: Option<i64>,
    wake: String,
) -> Result<(), PublicError> {
    ensure_home(home)?;
    let questions = wake == "questions";
    let reads = reads(home)?;
    let (project, mut cursor) = {
        let selector = project.as_deref().map(selector).transpose()?;
        reads
            .snapshot(move |sql| {
                let id = selector
                    .as_ref()
                    .map(|s| sluice_store::messages::resolve_project(sql, s))
                    .transpose()?;
                let end = records::bounds(sql, id)?.1;
                Ok((id, end))
            })
            .await
            .map_err(|e| e.into_public(true))?
    };
    if let Some(seq) = since_seq {
        cursor = RecordSeq(seq);
    }
    if cursor.0 < 0 {
        return Err(bad_request("--since-seq: expected a nonnegative sequence"));
    }
    let kinds = kinds.to_vec();
    let threads = threads.to_vec();
    let mut held: Vec<u8> = Vec::new();
    loop {
        let filter = records::RecordFilter {
            since: Some(cursor),
            kinds: kinds.clone(),
            threads: threads.clone(),
            limit: 1000,
            ..Default::default()
        };
        let page = reads
            .snapshot(move |sql| {
                records::read_records(sql, project, &filter).and_then(|r| r.into_page())
            })
            .await
            .map_err(|e| e.into_public(true))?;
        let mut chunk = Vec::new();
        for record in &page.records {
            let mut line = serde_json::to_vec(record).map_err(storage)?;
            line.push(b'\n');
            let note = questions
                && matches!(
                    &record.event,
                    sluice_model::events::Event::Message(message) if !message.is_question()
                );
            if note {
                held.append(&mut line);
            } else {
                chunk.append(&mut held);
                chunk.append(&mut line);
            }
            cursor = record.seq;
        }
        if !chunk.is_empty() {
            let mut out = std::io::stdout().lock();
            out.write_all(&chunk)
                .and_then(|()| out.flush())
                .map_err(storage)?;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

// ---- sluice drain ---------------------------------------------------------------

async fn drain(
    home: &Path,
    projects: &[String],
    no_wait: bool,
    release: bool,
) -> Result<(), PublicError> {
    ensure_home(home)?;
    let author = cli_author();
    let program = std::env::current_exe().map_err(storage)?;
    let client = ensure_coordinator(home, &program).await?;
    if release {
        let reply = client
            .command(CommandRequest::Release {
                author: Some(author),
            })
            .await?;
        let CommandReply::Data(data) = reply else {
            return Err(storage("unexpected reply to release"));
        };
        let released: Vec<String> =
            serde_json::from_value(data.as_value()["released"].clone()).map_err(storage)?;
        if released.is_empty() {
            println!("released nothing (no drain ownership)");
        } else {
            let names = project_names(home, &released).await?;
            println!("released {}", names.join(", "));
        }
        return Ok(());
    }
    let projects = selectors(projects)?;
    let reply = client
        .command(CommandRequest::Drain {
            projects: (!projects.is_empty()).then_some(projects),
            author: Some(author),
        })
        .await?;
    let CommandReply::Data(data) = reply else {
        return Err(storage("unexpected reply to drain"));
    };
    let paused: Vec<String> =
        serde_json::from_value(data.as_value()["paused"].clone()).map_err(storage)?;
    if paused.is_empty() {
        println!("nothing to pause (all already paused)");
    } else {
        let names = project_names(home, &paused).await?;
        println!("paused {}", names.join(", "));
    }
    if no_wait {
        return Ok(());
    }
    let reads = reads(home)?;
    let mut last = String::new();
    loop {
        let status = sluice_runtime::drain::status(&reads).await?;
        let owned = status.paused.clone();
        let calls = status.pending_calls.len();
        let running = reads
            .snapshot(move |sql| {
                let mut running = BTreeMap::<String, Vec<String>>::new();
                for id in &owned {
                    let name: String = sql.query_row(
                        "SELECT name FROM projects WHERE project_id=?1",
                        [id.to_string()],
                        |r| r.get(0),
                    )?;
                    let mut stmt = sql.prepare(
                        "SELECT step_id FROM steps WHERE project_id=?1 AND status='running' ORDER BY step_id",
                    )?;
                    let steps = stmt
                        .query_map([id.to_string()], |r| r.get::<_, String>(0))?
                        .collect::<Result<Vec<_>, _>>()?;
                    running.insert(name, steps);
                }
                Ok(running)
            })
            .await
            .map_err(|e| e.into_public(true))?;
        if calls == 0 && running.values().all(|steps| steps.is_empty()) {
            println!("drained");
            return Ok(());
        }
        let text = format!(
            "running: {}; calls {}",
            running
                .iter()
                .map(|(name, steps)| {
                    if steps.is_empty() {
                        format!("{name} 0")
                    } else {
                        format!("{name} {} ({})", steps.len(), steps.join(", "))
                    }
                })
                .collect::<Vec<_>>()
                .join(", "),
            calls
        );
        if text != last {
            println!("{text}");
            last = text;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
async fn project_names(home: &Path, ids: &[String]) -> Result<Vec<String>, PublicError> {
    let ids = ids.to_vec();
    reads(home)?
        .snapshot(move |sql| {
            Ok(ids
                .iter()
                .map(|id| {
                    sql.query_row(
                        "SELECT name FROM projects WHERE project_id=?1",
                        [id.as_str()],
                        |r| r.get::<_, String>(0),
                    )
                })
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .map_err(|e| e.into_public(true))
}
// ---- sluice query ---------------------------------------------------------------

/// One read-only SELECT: the {columns, rows, truncated} JSON, one row per line,
/// or an aligned table. Without SQL, the tables and views with their columns.
async fn query(
    home: &Path,
    sql: Option<String>,
    params: &[String],
    limit: Option<usize>,
    table: bool,
    width: usize,
) -> Result<(), PublicError> {
    ensure_home(home)?;
    let Some(sql) = sql else {
        // The guarded SELECT path: PRAGMA is denied, so column names come
        // from a zero-row SELECT's cursor rather than pragma_table_info.
        let objects = run_query(
            home,
            "SELECT type,name FROM sqlite_schema WHERE type IN ('table','view') \
             AND name NOT LIKE 'sqlite_%' ORDER BY type,name",
            &[],
            0,
        )?;
        for row in objects.rows() {
            let (
                sluice_store::query::QueryCell::Text(kind),
                sluice_store::query::QueryCell::Text(name),
            ) = (&row[0], &row[1])
            else {
                continue;
            };
            let columns = run_query(
                home,
                &format!("SELECT * FROM \"{}\" LIMIT 0", name.replace('"', "\"\"")),
                &[],
                0,
            )?
            .columns()
            .to_vec();
            println!("{kind:<5} {name}({})", columns.join(", "));
        }
        return Ok(());
    };
    let params: Vec<Value> = params
        .iter()
        .map(|text| serde_json::from_str(text).unwrap_or_else(|_| json!(text)))
        .collect();
    let result = run_query(home, &sql, &params, limit.unwrap_or(0))?;
    if !table {
        println!("{}", String::from_utf8_lossy(result.encoded()));
        return Ok(());
    }
    let columns: Vec<String> = result.columns().to_vec();
    let rows: Vec<Vec<Value>> = result
        .rows()
        .iter()
        .map(|row| row.iter().map(cell_json).collect())
        .collect();
    let cell = |value: &Value| -> String {
        let text = if value.is_null() {
            String::new()
        } else {
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string())
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        if width == 0 || text.chars().count() <= width {
            return text;
        }
        let mut chars = text.chars();
        let mut cut: String = chars.by_ref().take(width.saturating_sub(1)).collect();
        cut.push('…');
        cut
    };
    let grid: Vec<Vec<String>> = std::iter::once(columns.clone())
        .chain(rows.iter().map(|row| row.iter().map(cell).collect()))
        .collect();
    let widths: Vec<usize> = (0..columns.len())
        .map(|i| {
            grid.iter()
                .map(|row| row[i].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let lines: Vec<String> = grid
        .iter()
        .map(|row| {
            row.iter()
                .zip(&widths)
                .map(|(cell, w)| format!("{cell:<w$}"))
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
                .to_string()
        })
        .collect();
    println!("{}", lines[0]);
    println!(
        "{}",
        widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("  ")
    );
    for line in &lines[1..] {
        println!("{line}");
    }
    let n = rows.len();
    println!(
        "({} row{}{})",
        n,
        if n == 1 { "" } else { "s" },
        if result.truncated() {
            ", truncated"
        } else {
            ""
        }
    );
    Ok(())
}

fn cell_json(cell: &sluice_store::query::QueryCell) -> Value {
    use sluice_store::query::QueryCell;
    match cell {
        QueryCell::Null => Value::Null,
        QueryCell::Integer(v) => json!(v),
        QueryCell::Real(v) => json!(v),
        QueryCell::Text(v) => json!(v),
    }
}

// ---- sluice backup ---------------------------------------------------------------

fn backup(home: &Path, path: &Path, force: bool) -> Result<(), PublicError> {
    ensure_home(home)?;
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(storage)?.join(path)
    };
    let real_db = std::fs::canonicalize(home.join("sluice.db")).ok();
    if real_db.is_some() && real_db == std::fs::canonicalize(&path).ok() {
        return Err(bad_request(
            "refusing to back up over the home's own database",
        ));
    }
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_dir() => {
            return Err(bad_request(format!("{} is a directory", path.display())));
        }
        Ok(_) if !force => {
            return Err(bad_request(format!(
                "{} exists (pass --force)",
                path.display()
            )));
        }
        Ok(_) => std::fs::remove_file(&path).map_err(storage)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(storage(e)),
    }
    let info = sluice_store::backup::backup(home, &path).map_err(|e| e.into_public(true))?;
    println!("{} {} bytes", info.path.display(), info.bytes);
    Ok(())
}

#[derive(Debug, Subcommand)]
pub enum InstallCommand {
    Fence { reason: String },
    Unfence,
    Select { release_dir: PathBuf, home: PathBuf },
    Status,
}
