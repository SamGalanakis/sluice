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
    Tool {
        name: Option<String>,
        json: Option<String>,
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
        /// questions: hold notes (needs_reply false) and print them with the next
        /// record that is not one.
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
            Mode::Tool { name, json } => tool(&home, name, json).await,
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
    ("plan_get", "the plan document and its revision"),
    ("plan_history", "the plan's edit history"),
    ("plan_patch", "JSON-patch the plan at a required rev"),
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
        "step_context",
        "where a step stands, for its agent (sluice me)",
    ),
    ("submission", "a run's submitted fields so far"),
    ("unit_add", "add a recipe unit's steps"),
    ("unit_tag", "tag or untag a unit"),
    ("edge_add", "add `after` ordering to a step"),
    ("edge_remove", "remove `after` ordering"),
    ("message_post", "post a message or question on a thread"),
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
        CommandRequest::PlanPatch(r) => fill(&mut r.author),
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
        CommandRequest::MessagePost(r) => {
            fill(&mut r.author);
            if r.from.is_none() {
                r.from = r.author.clone();
            }
        }
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
    /// expected/dry_run/reason/author become the wire's `edit` object.
    fn edit(args: &mut serde_json::Map<String, Value>) {
        let mut edit = serde_json::Map::new();
        for key in ["expected", "dry_run", "reason", "author"] {
            if let Some(value) = args.remove(key) {
                edit.insert(key.into(), value);
            }
        }
        edit.entry("dry_run").or_insert(Value::Bool(false));
        edit.entry("reason").or_insert(Value::String(String::new()));
        args.insert("edit".into(), Value::Object(edit));
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
        "plan_patch" => {
            args.entry("dry_run").or_insert(Value::Bool(false));
        }
        "step_add" | "step_update" | "unit_tag" | "edge_add" | "edge_remove" | "plan_set_input"
        | "plan_prune" => {
            listify(args, "after");
            edit(args);
            if name == "unit_tag" {
                args.entry("add").or_insert(Value::Array(vec![]));
                args.entry("remove").or_insert(Value::Array(vec![]));
            }
            if name == "plan_prune"
                && let Some(hours) = args.remove("older_than_hours")
            {
                let secs = hours.as_f64().unwrap_or(0.0) * 3600.0;
                args.insert("older_than_seconds".into(), json!(secs.max(0.0) as u64));
            }
        }
        "unit_add" => {
            listify(args, "after");
            listify(args, "tags");
            edit(args);
            args.entry("after")
                .or_insert(Value::Object(Default::default()));
            args.entry("inputs")
                .or_insert(Value::Object(Default::default()));
            if let Some(unit) = args.get("params").and_then(|p| p.get("unit")).cloned() {
                args.entry("unit").or_insert(unit);
            }
        }
        "step_remove" | "step_pause" => {
            selection(args);
            edit(args);
            if name == "step_pause" {
                args.entry("paused").or_insert(Value::Bool(true));
            }
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
        "step_set_input" => {
            let step = args.remove("step");
            let input = args.remove("input");
            let value = args.remove("value");
            if let Some(step) = step {
                listify(args, "steps");
                match args.get_mut("steps") {
                    Some(Value::Array(steps)) => steps.push(step),
                    _ => {
                        args.insert("steps".into(), json!([step]));
                    }
                }
            }
            selection(args);
            if let (Some(Value::String(input)), Some(value)) = (input, value) {
                args.insert("inputs".into(), json!({input: value}));
            }
            if let Some(rev) = args.remove("rev") {
                args.insert("expected".into(), rev);
            }
            edit(args);
        }
        "step_set_output" => {
            args.entry("force").or_insert(Value::Bool(false));
            args.entry("reason").or_insert(Value::String(String::new()));
        }
        "step_submit" => {
            project_id_arg(home, args).await?;
            if args.get("run").is_none()
                && let Ok(run) = std::env::var("SLUICE_RUN_ID")
            {
                args.insert("run".into(), Value::String(run));
            }
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
            for key in ["project", "since_seq", "kinds", "threads", "limit"] {
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
            let wake = args.remove("wake").unwrap_or(json!("any"));
            args.insert(
                "questions_only".into(),
                Value::Bool(wake.as_str() == Some("questions")),
            );
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

async fn tool(home: &Path, name: Option<String>, args: Option<String>) -> Result<(), PublicError> {
    ensure_home(home)?;
    let Some(name) = name else {
        let mut tools: Vec<(&str, &str)> = TOOLS.to_vec();
        tools.sort_unstable();
        tools.dedup();
        for (name, doc) in tools {
            println!("{name:<20} {doc}");
        }
        return Ok(());
    };
    if name == "rpc" {
        return rpc(home, args).await;
    }
    if !TOOLS.iter().any(|(tool, _)| *tool == name) {
        return Err(bad_request(format!(
            "unknown tool {name:?} (sluice tool lists them)"
        )));
    }
    let text = match args {
        Some(text) => text,
        None => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(storage)?;
            text
        }
    };
    let text = text.trim();
    let args_value: Value = serde_json::from_str(if text.is_empty() { "{}" } else { text })
        .map_err(|e| bad_request(format!("args: not JSON: {e}")))?;
    let mut args = match args_value {
        Value::Object(map) => map,
        _ => return Err(bad_request("args: expected a JSON object")),
    };
    normalize_args(home, &name, &mut args).await?;
    let body = if name == "projects_list" {
        json!({"command": name})
    } else {
        json!({"command": name, "args": Value::Object(args)})
    };
    let mut request: CommandRequest =
        decode_json(body.to_string().as_bytes()).map_err(|e| bad_request(format!("args: {e}")))?;
    fill_author(&mut request, &cli_author());
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
        None => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(storage)?;
            text
        }
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
async fn project_id(home: &Path, selector: &ProjectSelector) -> Result<ProjectId, PublicError> {
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
    let filter = records::RecordFilter {
        since: read.since_seq,
        kinds: read.kinds.clone().unwrap_or_default(),
        threads: read.threads.clone().unwrap_or_default(),
        limit: read.limit,
    };
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
/// --wake questions, notes (messages with needs_reply false) are held and print
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
                    sluice_model::events::Event::Message(message) if !message.needs_reply
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
